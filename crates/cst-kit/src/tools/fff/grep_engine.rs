use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use encoding_rs::{CoderResult, Decoder, GB18030, UTF_8, UTF_16BE, UTF_16LE};
use globset::GlobSet;
use ignore::{WalkBuilder, WalkState};
use memchr::memmem::Finder;
use regex::bytes::RegexBuilder as BytesRegexBuilder;
use tokio_util::sync::CancellationToken;

use super::super::path_guard::{PathGuardSession, SearchGuardError, VerifiedSearchEntry};
use super::{
    FfgrepMatch, MAX_LINE_BYTES, MAX_TOTAL_READ_BYTES, TruncatedReason, dir_is_excluded,
    normalize_path, path_within_modified_window, worker_threads,
};

pub(crate) const STREAM_CHUNK_BYTES: usize = 256 * 1024;
pub(crate) const MAX_DIR_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub(crate) const MAX_GREP_VISITED: usize = 500_000;
const MAX_STAGED_COLLECTOR_BYTES: usize = 256 * 1024;
const MAX_GLOBAL_STAGED_BYTES: usize = 512 * 1024;

pub(crate) enum ContentMatcher {
    LiteralSensitive(Box<Finder<'static>>),
    LiteralInsensitive(regex::bytes::Regex),
    Regex(regex::bytes::Regex),
}

impl ContentMatcher {
    pub(crate) fn build(pattern: &str, literal: bool, sensitive: bool) -> Result<Self, String> {
        let is_literal = literal || regex::escape(pattern) == pattern;
        if is_literal && sensitive {
            return Ok(Self::LiteralSensitive(Box::new(
                Finder::new(pattern.as_bytes()).into_owned(),
            )));
        }
        let expression = if is_literal {
            regex::escape(pattern)
        } else {
            pattern.to_owned()
        };
        let re = BytesRegexBuilder::new(&expression)
            .case_insensitive(!sensitive)
            .size_limit(4 * 1024 * 1024)
            .build()
            .map_err(|_| "内容检索正则表达式无效或过大。")?;
        if is_literal {
            Ok(Self::LiteralInsensitive(re))
        } else {
            Ok(Self::Regex(re))
        }
    }

    pub(crate) fn matches_line(&self, line: &[u8]) -> bool {
        match self {
            Self::LiteralSensitive(finder) => finder.find(line).is_some(),
            Self::LiteralInsensitive(re) | Self::Regex(re) => re.is_match(line),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StagedRow {
    pub path: Arc<str>,
    pub line: usize,
    pub text: String,
    pub matched: bool,
}

#[derive(Default)]
struct LineStreamCollector {
    rows: Vec<StagedRow>,
    staged_bytes: usize,
    matched_count: usize,
    more_matches_omitted: bool,
    before: VecDeque<(usize, String)>,
    after_remaining: usize,
    line_buf: Vec<u8>,
    dropping_oversized: bool,
    hit_line_limit: bool,
    line_no: usize,
    saw_nul: bool,
    last_emitted_line: usize,
}

impl LineStreamCollector {
    fn new(context: usize) -> Self {
        Self {
            before: VecDeque::with_capacity(context),
            line_no: 1,
            ..Default::default()
        }
    }

    fn push_row(&mut self, row: StagedRow) -> bool {
        let row_bytes = row.text.len().saturating_add(32);
        if self.staged_bytes.saturating_add(row_bytes) > MAX_STAGED_COLLECTOR_BYTES {
            // Try evicting non-matched context rows to make room for a matched row.
            if row.matched {
                while self.staged_bytes.saturating_add(row_bytes) > MAX_STAGED_COLLECTOR_BYTES {
                    let Some(idx) = self.rows.iter().rposition(|r| !r.matched) else {
                        break;
                    };
                    let removed = self.rows.remove(idx);
                    self.staged_bytes = self
                        .staged_bytes
                        .saturating_sub(removed.text.len().saturating_add(32));
                }
            }
            if self.staged_bytes.saturating_add(row_bytes) > MAX_STAGED_COLLECTOR_BYTES {
                return false;
            }
        }
        self.staged_bytes = self.staged_bytes.saturating_add(row_bytes);
        self.rows.push(row);
        true
    }

    fn feed_utf8(
        &mut self,
        chunk: &[u8],
        path: &Arc<str>,
        matcher: &ContentMatcher,
        context: usize,
        limit: usize,
    ) {
        if self.saw_nul || chunk.is_empty() {
            return;
        }
        if memchr::memchr(0, chunk).is_some() {
            self.saw_nul = true;
            self.rows.clear();
            self.staged_bytes = 0;
            self.before.clear();
            return;
        }
        let mut pos = 0usize;
        while pos < chunk.len() {
            if let Some(nl_rel) = memchr::memchr(b'\n', &chunk[pos..]) {
                let seg = &chunk[pos..pos + nl_rel];
                pos += nl_rel + 1;
                self.push_line_bytes(seg);
                self.finish_line(path, matcher, context, limit, true);
            } else {
                let seg = &chunk[pos..];
                pos = chunk.len();
                self.push_line_bytes(seg);
            }
        }
    }

    fn push_line_bytes(&mut self, seg: &[u8]) {
        if self.dropping_oversized {
            return;
        }
        // Account for up to 1 trailing '\r' that may be stripped on CRLF.
        if self.line_buf.len().saturating_add(seg.len()) > MAX_LINE_BYTES + 1 {
            let trimmed_new_len = self.line_buf.len() + seg.len();
            if trimmed_new_len > MAX_LINE_BYTES + 1
                || (trimmed_new_len == MAX_LINE_BYTES + 1 && seg.last() != Some(&b'\r'))
            {
                self.dropping_oversized = true;
                self.hit_line_limit = true;
                self.line_buf.clear();
                self.before.clear();
                self.after_remaining = 0;
                return;
            }
        }
        self.line_buf.extend_from_slice(seg);
    }

    fn finish_line(
        &mut self,
        path: &Arc<str>,
        matcher: &ContentMatcher,
        context: usize,
        limit: usize,
        saw_lf: bool,
    ) {
        let cur_line = self.line_no;
        self.line_no += 1;

        if self.dropping_oversized {
            self.dropping_oversized = false;
            self.line_buf.clear();
            return;
        }
        // Only strip a SINGLE trailing CR immediately preceding LF (preserve literal CR in content).
        if saw_lf && self.line_buf.last() == Some(&b'\r') {
            self.line_buf.pop();
        }
        if self.line_buf.len() > MAX_LINE_BYTES {
            self.hit_line_limit = true;
            self.line_buf.clear();
            self.before.clear();
            self.after_remaining = 0;
            return;
        }

        let matched = matcher.matches_line(&self.line_buf);
        if matched {
            if self.matched_count >= limit {
                self.more_matches_omitted = true;
                self.line_buf.clear();
                self.before.clear();
                self.after_remaining = 0;
                return;
            }
            while let Some((prev_no, prev_text)) = self.before.pop_front() {
                if prev_no > self.last_emitted_line
                    && self.push_row(StagedRow {
                        path: Arc::clone(path),
                        line: prev_no,
                        text: prev_text,
                        matched: false,
                    })
                {
                    self.last_emitted_line = prev_no;
                }
            }
            let text = String::from_utf8_lossy(&self.line_buf).into_owned();
            if self.push_row(StagedRow {
                path: Arc::clone(path),
                line: cur_line,
                text,
                matched: true,
            }) {
                self.matched_count += 1;
                self.last_emitted_line = cur_line;
                self.after_remaining = context;
            } else {
                self.more_matches_omitted = true;
                self.after_remaining = 0;
            }
        } else if self.after_remaining > 0 {
            let text = String::from_utf8_lossy(&self.line_buf).into_owned();
            if cur_line > self.last_emitted_line
                && self.push_row(StagedRow {
                    path: Arc::clone(path),
                    line: cur_line,
                    text,
                    matched: false,
                })
            {
                self.last_emitted_line = cur_line;
            }
            self.after_remaining -= 1;
        } else if context > 0
            && self.matched_count < limit
            && self.staged_bytes < MAX_STAGED_COLLECTOR_BYTES
        {
            if self.before.len() == context {
                self.before.pop_front();
            }
            let text = String::from_utf8_lossy(&self.line_buf).into_owned();
            self.before.push_back((cur_line, text));
        }
        self.line_buf.clear();
    }

    fn finish_eof(
        &mut self,
        path: &Arc<str>,
        matcher: &ContentMatcher,
        context: usize,
        limit: usize,
    ) {
        if self.saw_nul {
            return;
        }
        if self.dropping_oversized {
            self.dropping_oversized = false;
            self.line_buf.clear();
            return;
        }
        if !self.line_buf.is_empty() {
            self.finish_line(path, matcher, context, limit, false);
        }
    }
}

enum DetectedStreamMode {
    ProbingBom(Vec<u8>),
    SingleDecoder {
        decoder: Decoder,
        collector: LineStreamCollector,
        decode_buf: Vec<u8>,
    },
    NoBomDual {
        utf8_valid: bool,
        utf8_carry: Vec<u8>,
        utf8_collector: LineStreamCollector,
        gb_decoder: Decoder,
        gb_collector: LineStreamCollector,
        gb_decode_buf: Vec<u8>,
    },
}

pub(crate) struct StreamScanOutcome {
    pub rows: Vec<StagedRow>,
    pub is_binary: bool,
    pub hit_line_limit: bool,
    pub more_matches_omitted: bool,
    pub budget_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub bytes_read: u64,
}

fn decode_chunk_with_decoder(
    decoder: &mut Decoder,
    src: &[u8],
    last: bool,
    dst: &mut Vec<u8>,
    mut on_utf8: impl FnMut(&[u8]),
) {
    if src.is_empty() && !last {
        return;
    }
    let min_cap = 32 * 1024;
    if dst.len() < min_cap {
        dst.resize(min_cap, 0);
    }
    let mut offset = 0usize;
    loop {
        let (res, read, written, _) = decoder.decode_to_utf8(&src[offset..], dst, last);
        offset += read;
        if written > 0 {
            on_utf8(&dst[..written]);
        }
        match res {
            CoderResult::InputEmpty => break,
            CoderResult::OutputFull => {
                if written == 0 {
                    let next_len = dst.len().saturating_mul(2).max(64 * 1024);
                    dst.resize(next_len, 0);
                }
            }
        }
    }
}

/// Feed bytes into a strict incremental UTF-8 validator/splitter without treating
/// an incomplete trailing sequence at a chunk/budget boundary as invalid UTF-8.
fn feed_strict_utf8_incremental(
    carry: &mut Vec<u8>,
    input: &[u8],
    is_true_eof: bool,
    mut on_valid: impl FnMut(&[u8]),
) -> bool {
    if carry.is_empty() {
        let slice = input;
        match std::str::from_utf8(slice) {
            Ok(_) => {
                if !slice.is_empty() {
                    on_valid(slice);
                }
                true
            }
            Err(err) => {
                let valid_len = err.valid_up_to();
                if valid_len > 0 {
                    on_valid(&slice[..valid_len]);
                }
                if err.error_len().is_none() {
                    let rem = &slice[valid_len..];
                    if is_true_eof {
                        return false;
                    }
                    carry.extend_from_slice(rem);
                    return true;
                }
                false
            }
        }
    } else {
        // Combine carry (at most 3 bytes) with up to 4 bytes of input to finish the split char.
        let take = (4 - carry.len()).min(input.len());
        carry.extend_from_slice(&input[..take]);
        let rest = &input[take..];
        let mut combined_consumed = 0usize;
        match std::str::from_utf8(carry) {
            Ok(_) => {
                on_valid(carry);
                carry.clear();
                feed_strict_utf8_incremental(carry, rest, is_true_eof, on_valid)
            }
            Err(err) => {
                let valid_len = err.valid_up_to();
                if valid_len > 0 {
                    on_valid(&carry[..valid_len]);
                    combined_consumed = valid_len;
                }
                if err.error_len().is_some() {
                    return false;
                }
                // Still incomplete.
                if combined_consumed > 0 {
                    carry.drain(..combined_consumed);
                }
                if rest.is_empty() {
                    return !is_true_eof;
                }
                // More input available; continue recursively.
                feed_strict_utf8_incremental(carry, rest, is_true_eof, on_valid)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_reader_streaming(
    mut reader: impl Read,
    path_display: &str,
    max_file_bytes: u64,
    remaining_budget: &AtomicUsize,
    matcher: &ContentMatcher,
    context: usize,
    limit: usize,
    deadline: Instant,
    cancel: &CancellationToken,
    chunk_size: usize,
) -> io::Result<StreamScanOutcome> {
    let path_arc: Arc<str> = Arc::from(path_display);
    let chunk_size = chunk_size.max(1);
    let mut mode = DetectedStreamMode::ProbingBom(Vec::with_capacity(4));
    let mut read_buf = vec![0u8; chunk_size.min(STREAM_CHUNK_BYTES)];
    let mut bytes_read_total = 0u64;
    let mut reached_eof = false;
    let mut budget_truncated = false;
    let mut timed_out = false;
    let mut cancelled = false;

    loop {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        if bytes_read_total >= max_file_bytes {
            // Confirm true EOF vs a file that grew after open (1-byte probe without consuming
            // additional lines): if more bytes exist, keep `reached_eof = false` so incomplete
            // trailing lines/characters are not mis-evaluated as EOF.
            let mut probe = [0u8; 1];
            match reader.read(&mut probe) {
                Ok(0) => reached_eof = true,
                Ok(_) => reached_eof = false,
                Err(_) => reached_eof = false,
            }
            break;
        }

        let left_in_file = (max_file_bytes - bytes_read_total) as usize;
        let want = left_in_file.min(read_buf.len());
        let reserved = reserve_up_to_budget(remaining_budget, want);
        if reserved == 0 {
            budget_truncated = true;
            break;
        }
        let n = match reader.read(&mut read_buf[..reserved]) {
            Ok(n) => {
                if n < reserved {
                    remaining_budget.fetch_add(reserved - n, AtomicOrdering::AcqRel);
                }
                n
            }
            Err(err) => {
                remaining_budget.fetch_add(reserved, AtomicOrdering::AcqRel);
                return Err(err);
            }
        };
        if n == 0 {
            reached_eof = true;
            break;
        }
        bytes_read_total += n as u64;
        let chunk = &read_buf[..n];

        if let DetectedStreamMode::ProbingBom(ref mut probe) = mode {
            probe.extend_from_slice(chunk);
            if probe.len() < 3 {
                continue;
            }
            let data = std::mem::take(probe);
            mode = init_mode_from_probe(&data, &path_arc, matcher, context, limit);
            if stream_is_binary(&mode) {
                return Ok(StreamScanOutcome {
                    rows: Vec::new(),
                    is_binary: true,
                    hit_line_limit: false,
                    more_matches_omitted: false,
                    budget_truncated: false,
                    timed_out: false,
                    cancelled: false,
                    bytes_read: bytes_read_total,
                });
            }
            continue;
        }

        feed_stream_mode(&mut mode, chunk, &path_arc, matcher, context, limit);
        if stream_is_binary(&mode) {
            return Ok(StreamScanOutcome {
                rows: Vec::new(),
                is_binary: true,
                hit_line_limit: false,
                more_matches_omitted: false,
                budget_truncated: false,
                timed_out: false,
                cancelled: false,
                bytes_read: bytes_read_total,
            });
        }
    }

    if cancelled {
        return Ok(StreamScanOutcome {
            rows: Vec::new(),
            is_binary: false,
            hit_line_limit: false,
            more_matches_omitted: false,
            budget_truncated: false,
            timed_out: false,
            cancelled: true,
            bytes_read: bytes_read_total,
        });
    }

    if let DetectedStreamMode::ProbingBom(ref mut probe) = mode {
        let data = std::mem::take(probe);
        mode = init_mode_from_probe(&data, &path_arc, matcher, context, limit);
    }

    let true_eof = reached_eof && !budget_truncated && !timed_out;
    finalize_stream_mode(
        mode,
        true_eof,
        &path_arc,
        matcher,
        context,
        limit,
        budget_truncated,
        timed_out,
        bytes_read_total,
    )
}

fn init_mode_from_probe(
    probe: &[u8],
    path_display: &Arc<str>,
    matcher: &ContentMatcher,
    context: usize,
    limit: usize,
) -> DetectedStreamMode {
    if let Some(rest) = probe.strip_prefix(b"\xFF\xFE") {
        let mut mode = DetectedStreamMode::SingleDecoder {
            decoder: UTF_16LE.new_decoder_without_bom_handling(),
            collector: LineStreamCollector::new(context),
            decode_buf: Vec::new(),
        };
        if !rest.is_empty() {
            feed_stream_mode(&mut mode, rest, path_display, matcher, context, limit);
        }
        return mode;
    }
    if let Some(rest) = probe.strip_prefix(b"\xFE\xFF") {
        let mut mode = DetectedStreamMode::SingleDecoder {
            decoder: UTF_16BE.new_decoder_without_bom_handling(),
            collector: LineStreamCollector::new(context),
            decode_buf: Vec::new(),
        };
        if !rest.is_empty() {
            feed_stream_mode(&mut mode, rest, path_display, matcher, context, limit);
        }
        return mode;
    }
    if let Some(rest) = probe.strip_prefix(b"\xEF\xBB\xBF") {
        let mut mode = DetectedStreamMode::SingleDecoder {
            decoder: UTF_8.new_decoder_without_bom_handling(),
            collector: LineStreamCollector::new(context),
            decode_buf: Vec::new(),
        };
        if !rest.is_empty() {
            feed_stream_mode(&mut mode, rest, path_display, matcher, context, limit);
        }
        return mode;
    }
    let mut mode = DetectedStreamMode::NoBomDual {
        utf8_valid: true,
        utf8_carry: Vec::with_capacity(4),
        utf8_collector: LineStreamCollector::new(context),
        gb_decoder: GB18030.new_decoder_without_bom_handling(),
        gb_collector: LineStreamCollector::new(context),
        gb_decode_buf: Vec::new(),
    };
    if !probe.is_empty() {
        feed_stream_mode(&mut mode, probe, path_display, matcher, context, limit);
    }
    mode
}

fn feed_stream_mode(
    mode: &mut DetectedStreamMode,
    chunk: &[u8],
    path_display: &Arc<str>,
    matcher: &ContentMatcher,
    context: usize,
    limit: usize,
) {
    match mode {
        DetectedStreamMode::ProbingBom(_) => {}
        DetectedStreamMode::SingleDecoder {
            decoder,
            collector,
            decode_buf,
        } => {
            decode_chunk_with_decoder(decoder, chunk, false, decode_buf, |utf8| {
                collector.feed_utf8(utf8, path_display, matcher, context, limit);
            });
        }
        DetectedStreamMode::NoBomDual {
            utf8_valid,
            utf8_carry,
            utf8_collector,
            gb_decoder,
            gb_collector,
            gb_decode_buf,
        } => {
            if memchr::memchr(0, chunk).is_some() {
                utf8_collector.saw_nul = true;
                utf8_collector.rows.clear();
                utf8_collector.staged_bytes = 0;
                gb_collector.saw_nul = true;
                gb_collector.rows.clear();
                gb_collector.staged_bytes = 0;
                return;
            }
            if *utf8_valid {
                let ok = feed_strict_utf8_incremental(utf8_carry, chunk, false, |valid| {
                    utf8_collector.feed_utf8(valid, path_display, matcher, context, limit);
                });
                if !ok {
                    *utf8_valid = false;
                    utf8_collector.rows.clear();
                    utf8_collector.staged_bytes = 0;
                    utf8_collector.before.clear();
                }
            }
            decode_chunk_with_decoder(gb_decoder, chunk, false, gb_decode_buf, |utf8| {
                gb_collector.feed_utf8(utf8, path_display, matcher, context, limit);
            });
        }
    }
}

fn stream_is_binary(mode: &DetectedStreamMode) -> bool {
    match mode {
        DetectedStreamMode::ProbingBom(_) => false,
        DetectedStreamMode::SingleDecoder { collector, .. } => collector.saw_nul,
        DetectedStreamMode::NoBomDual {
            utf8_valid,
            utf8_collector,
            gb_collector,
            ..
        } => gb_collector.saw_nul || (*utf8_valid && utf8_collector.saw_nul),
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_stream_mode(
    mode: DetectedStreamMode,
    true_eof: bool,
    path_display: &Arc<str>,
    matcher: &ContentMatcher,
    context: usize,
    limit: usize,
    budget_truncated: bool,
    timed_out: bool,
    bytes_read: u64,
) -> io::Result<StreamScanOutcome> {
    match mode {
        DetectedStreamMode::ProbingBom(_) => Ok(StreamScanOutcome {
            rows: Vec::new(),
            is_binary: false,
            hit_line_limit: false,
            more_matches_omitted: false,
            budget_truncated,
            timed_out,
            cancelled: false,
            bytes_read,
        }),
        DetectedStreamMode::SingleDecoder {
            mut decoder,
            mut collector,
            mut decode_buf,
        } => {
            if true_eof {
                decode_chunk_with_decoder(&mut decoder, &[], true, &mut decode_buf, |utf8| {
                    collector.feed_utf8(utf8, path_display, matcher, context, limit);
                });
                collector.finish_eof(path_display, matcher, context, limit);
            }
            if collector.saw_nul {
                return Ok(StreamScanOutcome {
                    rows: Vec::new(),
                    is_binary: true,
                    hit_line_limit: false,
                    more_matches_omitted: false,
                    budget_truncated: false,
                    timed_out: false,
                    cancelled: false,
                    bytes_read,
                });
            }
            Ok(StreamScanOutcome {
                rows: collector.rows,
                is_binary: false,
                hit_line_limit: collector.hit_line_limit,
                more_matches_omitted: collector.more_matches_omitted,
                budget_truncated,
                timed_out,
                cancelled: false,
                bytes_read,
            })
        }
        DetectedStreamMode::NoBomDual {
            mut utf8_valid,
            utf8_carry,
            mut utf8_collector,
            mut gb_decoder,
            mut gb_collector,
            mut gb_decode_buf,
        } => {
            if true_eof {
                if utf8_valid && !utf8_carry.is_empty() {
                    // Trailing incomplete UTF-8 at true EOF invalidates UTF-8 -> fall back to GB18030.
                    utf8_valid = false;
                }
                if utf8_valid {
                    utf8_collector.finish_eof(path_display, matcher, context, limit);
                }
                decode_chunk_with_decoder(&mut gb_decoder, &[], true, &mut gb_decode_buf, |utf8| {
                    gb_collector.feed_utf8(utf8, path_display, matcher, context, limit);
                });
                gb_collector.finish_eof(path_display, matcher, context, limit);
            }
            // Note: on budget truncation (true_eof == false), ignore partial trailing utf8_carry
            // and do NOT flush incomplete last line (avoids false `$` match and false UTF-8 error).
            let chosen = if utf8_valid {
                utf8_collector
            } else {
                gb_collector
            };
            if chosen.saw_nul {
                return Ok(StreamScanOutcome {
                    rows: Vec::new(),
                    is_binary: true,
                    hit_line_limit: false,
                    more_matches_omitted: false,
                    budget_truncated: false,
                    timed_out: false,
                    cancelled: false,
                    bytes_read,
                });
            }
            Ok(StreamScanOutcome {
                rows: chosen.rows,
                is_binary: false,
                hit_line_limit: chosen.hit_line_limit,
                more_matches_omitted: chosen.more_matches_omitted,
                budget_truncated,
                timed_out,
                cancelled: false,
                bytes_read,
            })
        }
    }
}

fn reserve_up_to_budget(remaining: &AtomicUsize, want: usize) -> usize {
    if want == 0 {
        return 0;
    }
    let mut cur = remaining.load(AtomicOrdering::Relaxed);
    loop {
        let grant = cur.min(want);
        if grant == 0 {
            return 0;
        }
        match remaining.compare_exchange_weak(
            cur,
            cur - grant,
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
        ) {
            Ok(_) => return grant,
            Err(next) => cur = next,
        }
    }
}

/// Merge staged rows across files, deduplicating overlapping context lines and
/// prioritizing `matched == true` rows over context rows when enforcing `limit`.
pub(crate) fn merge_and_limit_grep_rows(
    mut rows: Vec<StagedRow>,
    limit: usize,
) -> (Vec<FfgrepMatch>, bool) {
    // Deduplicate by (path, line), upgrading `matched` to true if either occurrence matched.
    let mut dedup: BTreeMap<(Arc<str>, usize), (String, bool)> = BTreeMap::new();
    for row in rows.drain(..) {
        dedup
            .entry((row.path, row.line))
            .and_modify(|(_, m)| {
                if row.matched {
                    *m = true;
                }
            })
            .or_insert((row.text, row.matched));
    }

    if dedup.len() <= limit {
        let out = dedup
            .into_iter()
            .map(|((path, line), (text, matched))| FfgrepMatch {
                path: path.to_string(),
                line,
                text,
                matched,
            })
            .collect();
        return (out, false);
    }

    // Prioritize matched rows first, then fill remaining slots with context rows in order.
    type DedupEntry = ((Arc<str>, usize), (String, bool));
    let mut kept_matched: Vec<DedupEntry> = Vec::with_capacity(limit);
    let mut ctx_candidates: Vec<DedupEntry> = Vec::new();
    for entry in dedup {
        if entry.1.1 {
            if kept_matched.len() < limit {
                kept_matched.push(entry);
            }
        } else {
            ctx_candidates.push(entry);
        }
    }
    let remaining_slots = limit.saturating_sub(kept_matched.len());
    let mut combined = kept_matched;
    combined.extend(ctx_candidates.into_iter().take(remaining_slots));
    combined.sort_by(|a, b| a.0.cmp(&b.0));

    let out = combined
        .into_iter()
        .map(|((path, line), (text, matched))| FfgrepMatch {
            path: path.to_string(),
            line,
            text,
            matched,
        })
        .collect();
    (out, true)
}

pub(crate) struct GrepRunOutcome {
    pub matches: Vec<FfgrepMatch>,
    pub truncated: bool,
    pub truncated_reason: Option<TruncatedReason>,
    pub degraded: bool,
    pub skipped: usize,
    pub skipped_large_files: usize,
    pub notices: Vec<String>,
}

struct GrepShared {
    session: PathGuardSession,
    verified_root: VerifiedSearchEntry,
    root_path: PathBuf,
    include: Option<GlobSet>,
    excludes: GlobSet,
    matcher: Arc<ContentMatcher>,
    modified_window: Option<(i64, i64)>,
    limit: usize,
    context: usize,
    deadline: Instant,
    cancel: CancellationToken,
    visited: AtomicUsize,
    remaining_bytes: AtomicUsize,
    skipped: AtomicUsize,
    skipped_large_files: AtomicUsize,
    read_budget_exceeded: AtomicBool,
    visited_limit_hit: AtomicBool,
    timed_out: AtomicBool,
    cancelled: AtomicBool,
    hit_line_limit: AtomicBool,
    more_matches_omitted: AtomicBool,
    stop: AtomicBool,
    collected: Mutex<Vec<StagedRow>>,
    matched_collected: AtomicUsize,
}

impl GrepShared {
    fn mark_cancelled(&self) {
        self.cancelled.store(true, AtomicOrdering::Relaxed);
        self.stop.store(true, AtomicOrdering::Relaxed);
    }

    fn mark_timed_out(&self) {
        self.timed_out.store(true, AtomicOrdering::Relaxed);
        self.stop.store(true, AtomicOrdering::Relaxed);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_grep(
    session: &PathGuardSession,
    verified_root: &VerifiedSearchEntry,
    include: Option<GlobSet>,
    excludes: GlobSet,
    matcher: Arc<ContentMatcher>,
    modified_window: Option<(i64, i64)>,
    limit: usize,
    context: usize,
    timeout_sec: u64,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<GrepRunOutcome, String> {
    if !verified_root.is_dir {
        return run_grep_single_file(
            session,
            verified_root,
            &matcher,
            modified_window,
            limit,
            context,
            timeout_sec,
            deadline,
            cancel,
        );
    }

    let root = &verified_root.canonical_path;
    let shared = Arc::new(GrepShared {
        session: session.clone(),
        verified_root: verified_root.clone(),
        root_path: root.clone(),
        include,
        excludes,
        matcher,
        modified_window,
        limit,
        context,
        deadline,
        cancel: cancel.clone(),
        visited: AtomicUsize::new(0),
        remaining_bytes: AtomicUsize::new(MAX_TOTAL_READ_BYTES),
        skipped: AtomicUsize::new(0),
        skipped_large_files: AtomicUsize::new(0),
        read_budget_exceeded: AtomicBool::new(false),
        visited_limit_hit: AtomicBool::new(false),
        timed_out: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        hit_line_limit: AtomicBool::new(false),
        more_matches_omitted: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        collected: Mutex::new(Vec::new()),
        matched_collected: AtomicUsize::new(0),
    });

    let walker = build_grep_walker(session, root, &shared.excludes);
    walker.build_parallel().run(|| {
        let worker_shared = Arc::clone(&shared);
        Box::new(move |entry| visit_grep_entry(&worker_shared, entry))
    });

    if shared.cancelled.load(AtomicOrdering::Relaxed) || cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }

    let raw_rows = std::mem::take(&mut *shared.collected.lock().unwrap_or_else(|e| e.into_inner()));
    let (matches, limit_truncated_merge) = merge_and_limit_grep_rows(raw_rows, limit);

    let timed_out = shared.timed_out.load(AtomicOrdering::Relaxed);
    let budget_exceeded = shared.read_budget_exceeded.load(AtomicOrdering::Relaxed);
    let visited_hit = shared.visited_limit_hit.load(AtomicOrdering::Relaxed);
    let limit_hit =
        limit_truncated_merge || shared.more_matches_omitted.load(AtomicOrdering::Relaxed);
    let line_limit_hit = shared.hit_line_limit.load(AtomicOrdering::Relaxed);
    let skipped = shared.skipped.load(AtomicOrdering::Relaxed);
    let skipped_large = shared.skipped_large_files.load(AtomicOrdering::Relaxed);

    let reason = pick_grep_reason(
        timed_out,
        budget_exceeded,
        visited_hit,
        limit_hit,
        line_limit_hit,
    );
    let truncated = reason.is_some();
    let degraded = timed_out || budget_exceeded || skipped_large > 0;
    let mut notices = Vec::new();
    if timed_out {
        notices.push(format!(
            "内容检索达到 {timeout_sec} 秒返回期限，已保留已提交的命中结果；可缩小 path、补充 include 与 modifiedWithinHours 或调高 timeoutSec（最高 {} 秒）。",
            super::MAX_TIMEOUT_SECS
        ));
    }
    if budget_exceeded {
        notices.push(
            "内容检索达到 256 MiB 读取预算，已保留此前命中的结果；请缩小 path 或补充 include 与 modifiedWithinHours。"
                .to_owned(),
        );
    }
    if skipped_large > 0 {
        notices.push(format!(
            "另有 {skipped_large} 个大于 10 MiB 的文件被跳过，可直接将单个文件路径传入 ffgrep 流式扫描或用 read 分段查看。"
        ));
    }
    if line_limit_hit {
        notices.push("部分超过 8 KiB 的长行已跳过。".to_owned());
    }

    Ok(GrepRunOutcome {
        matches,
        truncated,
        truncated_reason: reason,
        degraded,
        skipped,
        skipped_large_files: skipped_large,
        notices,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_grep_single_file(
    session: &PathGuardSession,
    verified_root: &VerifiedSearchEntry,
    matcher: &ContentMatcher,
    modified_window: Option<(i64, i64)>,
    limit: usize,
    context: usize,
    timeout_sec: u64,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<GrepRunOutcome, String> {
    let (file, verified) = session
        .open_grep_file(&verified_root.canonical_path, verified_root)
        .map_err(|err| match err {
            SearchGuardError::PolicyDenied => "权限拒绝：禁止读取受保护文件或链接目标。".to_owned(),
            SearchGuardError::IoOrAccess => "目标文件无法打开。".to_owned(),
        })?;
    if !path_within_modified_window(verified.modified_secs, modified_window) {
        return Ok(GrepRunOutcome {
            matches: Vec::new(),
            truncated: false,
            truncated_reason: None,
            degraded: false,
            skipped: 0,
            skipped_large_files: 0,
            notices: Vec::new(),
        });
    }
    let file_size = verified.size_bytes.unwrap_or(0);
    if file_size == 0 {
        return Ok(GrepRunOutcome {
            matches: Vec::new(),
            truncated: false,
            truncated_reason: None,
            degraded: false,
            skipped: 0,
            skipped_large_files: 0,
            notices: Vec::new(),
        });
    }
    let remaining = AtomicUsize::new(MAX_TOTAL_READ_BYTES);
    let path_display = normalize_path(&verified.canonical_path);
    let outcome = scan_reader_streaming(
        file,
        &path_display,
        file_size,
        &remaining,
        matcher,
        context,
        limit,
        deadline,
        cancel,
        STREAM_CHUNK_BYTES,
    )
    .map_err(|_| "目标文件读取失败。".to_owned())?;

    if outcome.cancelled {
        return Err("文件检索已取消。".into());
    }
    if outcome.is_binary {
        return Err("目标文件包含二进制空字节，不支持文本检索。".into());
    }

    let (matches, limit_truncated_merge) = merge_and_limit_grep_rows(outcome.rows, limit);
    let limit_hit = limit_truncated_merge || outcome.more_matches_omitted;
    let reason = pick_grep_reason(
        outcome.timed_out,
        outcome.budget_truncated,
        false,
        limit_hit,
        outcome.hit_line_limit,
    );
    let truncated = reason.is_some();
    let degraded = outcome.timed_out || outcome.budget_truncated;
    let mut notices = Vec::new();
    if outcome.timed_out {
        notices.push(format!(
            "单文件流式检索达到 {timeout_sec} 秒返回期限，仅覆盖文件头前 {} 字节，已保留已命中行；可调高 timeoutSec（最高 {} 秒）重试。",
            outcome.bytes_read,
            super::MAX_TIMEOUT_SECS
        ));
    }
    if outcome.budget_truncated {
        notices.push(format!(
            "单文件流式检索达到 256 MiB 读取预算（文件总长 {} 字节），结果仅覆盖文件头前缀，已保留已命中行。",
            file_size
        ));
    }
    if outcome.hit_line_limit {
        notices.push("文件中部分超过 8 KiB 的长行已跳过。".to_owned());
    }

    Ok(GrepRunOutcome {
        matches,
        truncated,
        truncated_reason: reason,
        degraded,
        skipped: 0,
        skipped_large_files: 0,
        notices,
    })
}

fn pick_grep_reason(
    timed_out: bool,
    read_budget: bool,
    visited_limit: bool,
    limit: bool,
    line_limit: bool,
) -> Option<TruncatedReason> {
    if timed_out {
        Some(TruncatedReason::Timeout)
    } else if read_budget {
        Some(TruncatedReason::ReadBudget)
    } else if visited_limit {
        Some(TruncatedReason::VisitedLimit)
    } else if limit {
        Some(TruncatedReason::Limit)
    } else if line_limit {
        Some(TruncatedReason::LineLimit)
    } else {
        None
    }
}

fn build_grep_walker(session: &PathGuardSession, root: &Path, excludes: &GlobSet) -> WalkBuilder {
    let filter_session = session.clone();
    let filter_root = root.to_path_buf();
    let filter_excludes = excludes.clone();
    let mut walker = WalkBuilder::new(root);
    walker
        .threads(worker_threads())
        .standard_filters(false)
        .hidden(false)
        .follow_links(false);
    walker.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let name = entry.file_name();
        if super::is_git_dir_name(name) {
            return false;
        }
        let Some(kind) = entry.file_type() else {
            return false;
        };
        if kind.is_symlink() {
            return false;
        }
        if kind.is_dir() {
            let path = entry.path();
            if !filter_session.allow_walk_dir(path) {
                return false;
            }
            if !filter_excludes.is_empty() && dir_is_excluded(path, &filter_root, &filter_excludes)
            {
                return false;
            }
        }
        true
    });
    walker
}

fn visit_grep_entry(
    shared: &GrepShared,
    entry: Result<ignore::DirEntry, ignore::Error>,
) -> WalkState {
    if shared.stop.load(AtomicOrdering::Relaxed) {
        return WalkState::Quit;
    }
    if shared.cancel.is_cancelled() {
        shared.mark_cancelled();
        return WalkState::Quit;
    }
    if Instant::now() >= shared.deadline {
        shared.mark_timed_out();
        return WalkState::Quit;
    }

    let entry = match entry {
        Ok(e) => e,
        Err(_) => {
            shared.skipped.fetch_add(1, AtomicOrdering::Relaxed);
            return WalkState::Continue;
        }
    };
    if entry.depth() == 0 || !entry.file_type().is_some_and(|k| k.is_file()) {
        return WalkState::Continue;
    }

    let path = entry.path();
    let rel_key = path
        .strip_prefix(&shared.root_path)
        .ok()
        .map(normalize_path)
        .unwrap_or_else(|| normalize_path(path));
    let file_name = entry.file_name().to_string_lossy();
    if !passes_grep_globs(
        &rel_key,
        &file_name,
        shared.include.as_ref(),
        &shared.excludes,
    ) {
        return WalkState::Continue;
    }

    // Pre-open skip of symlinks, unhydrated cloud files, and out-of-window modified times.
    if entry_pre_skip(path, &entry, shared.modified_window) {
        return WalkState::Continue;
    }

    let (file, verified) = match shared.session.open_grep_file(path, &shared.verified_root) {
        Ok(pair) => pair,
        Err(SearchGuardError::PolicyDenied) => {
            // Protected secrets / `.state/` / reparse links are never counted in `skipped` or `visited`.
            return WalkState::Continue;
        }
        Err(SearchGuardError::IoOrAccess) => {
            shared.skipped.fetch_add(1, AtomicOrdering::Relaxed);
            return WalkState::Continue;
        }
    };

    // Charge `visited` strictly AFTER policy authorization so sensitive files never shift `visited_limit`.
    if shared.visited.fetch_add(1, AtomicOrdering::Relaxed) >= MAX_GREP_VISITED {
        shared
            .visited_limit_hit
            .store(true, AtomicOrdering::Relaxed);
        shared.stop.store(true, AtomicOrdering::Relaxed);
        return WalkState::Quit;
    }

    // Re-verify modified time on the opened handle.
    if !path_within_modified_window(verified.modified_secs, shared.modified_window) {
        return WalkState::Continue;
    }
    let file_size = verified.size_bytes.unwrap_or(0);
    if file_size == 0 {
        return WalkState::Continue;
    }
    if file_size > MAX_DIR_FILE_BYTES {
        shared
            .skipped_large_files
            .fetch_add(1, AtomicOrdering::Relaxed);
        return WalkState::Continue;
    }
    if shared.stop.load(AtomicOrdering::Relaxed) {
        return WalkState::Quit;
    }

    let display_path = normalize_path(&verified.canonical_path);
    let outcome = match scan_reader_streaming(
        file,
        &display_path,
        file_size,
        &shared.remaining_bytes,
        &shared.matcher,
        shared.context,
        shared.limit,
        shared.deadline,
        &shared.cancel,
        STREAM_CHUNK_BYTES,
    ) {
        Ok(o) => o,
        Err(_) => {
            shared.skipped.fetch_add(1, AtomicOrdering::Relaxed);
            return WalkState::Continue;
        }
    };

    if outcome.cancelled {
        shared.mark_cancelled();
        return WalkState::Quit;
    }
    if outcome.timed_out {
        shared.mark_timed_out();
    }
    if outcome.budget_truncated {
        shared
            .read_budget_exceeded
            .store(true, AtomicOrdering::Relaxed);
        shared.stop.store(true, AtomicOrdering::Relaxed);
    }
    if outcome.hit_line_limit {
        shared.hit_line_limit.store(true, AtomicOrdering::Relaxed);
    }
    if outcome.more_matches_omitted {
        shared
            .more_matches_omitted
            .store(true, AtomicOrdering::Relaxed);
    }
    if !outcome.is_binary && !outcome.rows.is_empty() {
        let added_matches = outcome.rows.iter().filter(|r| r.matched).count();
        let mut guard = shared.collected.lock().unwrap_or_else(|e| e.into_inner());
        let mut cur_bytes: usize = guard.iter().map(|r| r.text.len().saturating_add(32)).sum();
        for row in outcome.rows {
            let sz = row.text.len().saturating_add(32);
            if cur_bytes.saturating_add(sz) > MAX_GLOBAL_STAGED_BYTES {
                if row.matched {
                    while cur_bytes.saturating_add(sz) > MAX_GLOBAL_STAGED_BYTES {
                        let Some(idx) = guard.iter().rposition(|r| !r.matched) else {
                            break;
                        };
                        let removed = guard.remove(idx);
                        cur_bytes = cur_bytes.saturating_sub(removed.text.len().saturating_add(32));
                    }
                }
                if cur_bytes.saturating_add(sz) > MAX_GLOBAL_STAGED_BYTES {
                    shared
                        .more_matches_omitted
                        .store(true, AtomicOrdering::Relaxed);
                    continue;
                }
            }
            cur_bytes = cur_bytes.saturating_add(sz);
            guard.push(row);
        }
        let total_matched = shared
            .matched_collected
            .fetch_add(added_matches, AtomicOrdering::Relaxed)
            + added_matches;
        if total_matched > shared.limit {
            shared
                .more_matches_omitted
                .store(true, AtomicOrdering::Relaxed);
            shared.stop.store(true, AtomicOrdering::Relaxed);
        }
    }

    if shared.stop.load(AtomicOrdering::Relaxed) {
        WalkState::Quit
    } else {
        WalkState::Continue
    }
}

fn passes_grep_globs(
    rel_path: &str,
    file_name: &str,
    include: Option<&GlobSet>,
    excludes: &GlobSet,
) -> bool {
    if !excludes.is_empty() && (excludes.is_match(rel_path) || excludes.is_match(file_name)) {
        return false;
    }
    if let Some(inc) = include {
        return inc.is_match(rel_path) || inc.is_match(file_name);
    }
    true
}

fn entry_pre_skip(
    _path: &Path,
    entry: &ignore::DirEntry,
    modified_window: Option<(i64, i64)>,
) -> bool {
    if entry.path_is_symlink() {
        return true;
    }
    if let Ok(meta) = entry.metadata() {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if (meta.file_attributes() & super::super::path_guard::FILE_ATTRIBUTE_UNHYDRATED_MASK)
                != 0
            {
                return true;
            }
        }
        if modified_window.is_some() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            if !path_within_modified_window(mtime, modified_window) {
                return true;
            }
        }
    }
    false
}

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use encoding_rs::{CoderResult, Encoding, GB18030, UTF_8, UTF_16BE, UTF_16LE};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};
use memchr::memmem::Finder;
use pi_session::ToolDefinition;
use regex::bytes::RegexBuilder as BytesRegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::path_guard::{PathGuard, PathGuardSession};
use super::{MAX_OUTPUT_BYTES, json_result};

const DEFAULT_LIMIT: usize = 30;
const MAX_LIMIT: usize = 1000;
const MAX_LINE_BYTES: usize = 8 * 1024;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_TOTAL_READ_BYTES: usize = 256 * 1024 * 1024;
const MAX_VISITED: usize = 500_000;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_BUF_INIT: usize = 64 * 1024;
const WORKER_BUF_SHRINK_THRESHOLD: usize = 1024 * 1024;
const WEAK_MATCH_SCORE_THRESHOLD: i32 = 400;

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub context: Option<usize>,
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    #[serde(default)]
    pub literal: bool,
}

pub fn definitions(guard: PathGuard) -> Vec<ToolDefinition> {
    [false, true]
        .into_iter()
        .map(|content_search| {
            let guard = guard.clone();
            let name = if content_search { "ffgrep" } else { "fffind" };
            let mut definition = ToolDefinition::from_typed::<SearchInput, _, _>(
                name,
                name,
                if content_search {
                    "检索文件内容，遵守忽略规则并跳过凭据。"
                } else {
                    "模糊查找文件路径，遵守忽略规则并跳过凭据。"
                },
                move |_, input, cancel, _, ctx| {
                    let guard = guard.clone();
                    async move {
                        let cancel = cancel.unwrap_or_default();
                        let cwd = PathBuf::from(ctx.cwd());
                        let value = tokio::task::spawn_blocking(move || {
                            search(&guard, &cwd, input, content_search, &cancel)
                        })
                        .await
                        .map_err(|_| "文件检索任务异常。".to_owned())??;
                        json_result(value)
                    }
                },
            );
            definition.prompt_snippet = Some(definition.description.clone());
            definition
        })
        .collect()
}

pub fn search(
    guard: &PathGuard,
    cwd: &Path,
    input: SearchInput,
    content_search: bool,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    check_search_time(cancel, deadline)?;
    let session = guard.session();
    search_with_session(&session, cwd, input, content_search, cancel, deadline)
}

fn search_with_session(
    session: &PathGuardSession,
    cwd: &Path,
    input: SearchInput,
    content_search: bool,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Value, String> {
    if input.pattern.len() > 4096 || (content_search && input.pattern.is_empty()) {
        return Err("检索表达式为空或超过 4096 字节。".into());
    }
    check_search_time(cancel, deadline)?;
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let context = input.context.unwrap_or(0).min(20);
    let (raw_root, include) = search_root(session, cwd, input.path.as_deref())?;
    let root = session.check_path(raw_root)?;
    let canonical_cwd = cwd.canonicalize().ok();
    let relative_root = canonical_cwd.as_deref().unwrap_or(cwd).to_path_buf();
    let excludes = globs(input.exclude.as_deref().unwrap_or(&[]))?;
    let sensitive = input
        .case_sensitive
        .unwrap_or_else(|| input.pattern.chars().any(char::is_uppercase));

    if content_search {
        let matcher = ContentMatcher::build(&input.pattern, input.literal, sensitive)?;
        run_grep(
            session,
            &root,
            &relative_root,
            include,
            excludes,
            Arc::new(matcher),
            limit,
            context,
            deadline,
            cancel,
        )
    } else {
        let query = QueryPlan::new(&input.pattern, sensitive);
        run_find(
            session,
            &root,
            &relative_root,
            include,
            excludes,
            Arc::new(query),
            limit,
            deadline,
            cancel,
        )
    }
}

fn worker_threads() -> usize {
    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    cores.saturating_sub(2).clamp(1, 8)
}

fn build_walker(
    session: &PathGuardSession,
    root: &Path,
    relative_root: &Path,
    excludes: &GlobSet,
) -> WalkBuilder {
    let filter_session = session.clone();
    let filter_root = root.to_path_buf();
    let filter_relative = relative_root.to_path_buf();
    let filter_excludes = excludes.clone();
    let mut walker = WalkBuilder::new(root);
    walker
        .threads(worker_threads())
        .hidden(false)
        .follow_links(false)
        .require_git(false);
    walker.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        if entry.file_name() == ".git" {
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
            if !filter_excludes.is_empty()
                && dir_is_excluded(path, &filter_relative, &filter_root, &filter_excludes)
            {
                return false;
            }
        }
        true
    });
    walker
}

fn dir_is_excluded(
    path: &Path,
    relative_root: &Path,
    search_root: &Path,
    excludes: &GlobSet,
) -> bool {
    let keys = PathMatchKeys::new(path, relative_root, search_root, true);
    keys.any(|key| excludes.is_match(key) || excludes.is_match(format!("{key}/")))
}

struct PathMatchKeys {
    display: String,
    absolute: Option<String>,
    relative_to_search: Option<String>,
}

impl PathMatchKeys {
    fn new(path: &Path, relative_root: &Path, search_root: &Path, filtered: bool) -> Self {
        let relative = path.strip_prefix(relative_root).ok();
        Self {
            display: normalize_path(relative.unwrap_or(path)),
            absolute: (filtered && relative.is_some()).then(|| normalize_path(path)),
            relative_to_search: if filtered && search_root != relative_root {
                path.strip_prefix(search_root).ok().map(normalize_path)
            } else {
                None
            },
        }
    }

    fn any(&self, mut matches: impl FnMut(&str) -> bool) -> bool {
        matches(&self.display)
            || self.relative_to_search.as_deref().is_some_and(&mut matches)
            || self.absolute.as_deref().is_some_and(matches)
    }

    fn passes(&self, include: Option<&GlobSet>, excludes: &GlobSet) -> bool {
        include.is_none_or(|glob| self.any(|key| glob.is_match(key)))
            && (excludes.is_empty() || !self.any(|key| excludes.is_match(key)))
    }
}

fn normalize_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    if let Some(rest) = raw.strip_prefix("//?/UNC/") {
        return format!("//{rest}");
    }
    #[cfg(windows)]
    if let Some(rest) = raw.strip_prefix("//?/") {
        return rest.to_owned();
    }
    raw
}

// ============================================================================
// fffind: Parallel Traversal + Pure-Rust Relevance Scoring + Top-K Selection
// ============================================================================

struct QueryPlan {
    unfiltered: bool,
    sensitive: bool,
    terms: Vec<String>,
}

impl QueryPlan {
    fn new(pattern: &str, sensitive: bool) -> Self {
        let trimmed = pattern.trim();
        if trimmed.is_empty() || trimmed == "*" {
            return Self {
                unfiltered: true,
                sensitive,
                terms: Vec::new(),
            };
        }
        let mut terms: Vec<String> = trimmed
            .split_whitespace()
            .map(|term| {
                if sensitive {
                    term.to_owned()
                } else {
                    term.to_lowercase()
                }
            })
            .collect();
        // Evaluate longest term first so selective tokens reject misses earliest.
        terms.sort_by_key(|term| std::cmp::Reverse(term.chars().count()));
        Self {
            unfiltered: false,
            sensitive,
            terms,
        }
    }

    fn score_path(&self, display: &str, scratch: &mut String) -> Option<i32> {
        if self.unfiltered {
            return Some(0);
        }
        let haystack: &str = if self.sensitive {
            display
        } else {
            lowercase_into(display, scratch);
            scratch.as_str()
        };
        let filename_start = haystack.rfind('/').map_or(0, |i| i + 1);
        let mut total_score = 0i32;
        for term in &self.terms {
            let score = score_term(term, haystack, display, filename_start)?;
            total_score = total_score.saturating_add(score);
        }
        let avg = total_score / (self.terms.len() as i32);
        let depth = display.bytes().filter(|&b| b == b'/').count() as i32;
        let len_penalty = (display.len().min(400) as i32) / 4;
        Some(avg - depth * 8 - len_penalty)
    }
}

fn lowercase_into(input: &str, out: &mut String) {
    out.clear();
    if input.is_ascii() {
        out.reserve(input.len());
        for b in input.bytes() {
            out.push(b.to_ascii_lowercase() as char);
        }
    } else {
        out.push_str(&input.to_lowercase());
    }
}

fn score_term(term: &str, haystack: &str, original: &str, filename_start: usize) -> Option<i32> {
    let filename = &haystack[filename_start..];
    let orig_filename = original.rsplit('/').next().unwrap_or(original);

    // 1. Exact filename or exact filename stem match
    if filename == term {
        return Some(10_000);
    }
    if let Some(stem) = filename.rsplit_once('.').map(|(s, _)| s)
        && stem == term
    {
        return Some(9_500);
    }
    // 2. Filename prefix match
    if filename.starts_with(term) {
        return Some(6_000 + (term.len().min(64) as i32) * 8);
    }
    // 3. Contiguous substring in filename
    if let Some(pos) = filename.find(term) {
        let boundary_bonus = if is_boundary_at(orig_filename, pos) {
            800
        } else {
            0
        };
        return Some(4_000 + boundary_bonus - (pos.min(128) as i32) * 4);
    }
    // 4. Contiguous substring in full path
    if let Some(pos) = haystack.find(term) {
        let boundary_bonus = if is_boundary_at(original, pos) {
            500
        } else {
            0
        };
        return Some(2_500 + boundary_bonus - (pos.min(256) as i32) * 2);
    }
    // 5. Subsequence inside filename (preferred over full-path subsequence)
    if let Some(sub_score) = subsequence_score(term, filename, orig_filename) {
        return Some(1_200 + sub_score.clamp(-500, 700));
    }
    // Keep long full-path subsequences below stronger match classes.
    subsequence_score(term, haystack, original).map(|score| score.min(600))
}

fn is_boundary_at(text: &str, byte_idx: usize) -> bool {
    if byte_idx == 0 {
        return true;
    }
    let Some(prev) = text.get(..byte_idx).and_then(|s| s.chars().next_back()) else {
        return false;
    };
    if matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ') {
        return true;
    }
    let Some(curr) = text.get(byte_idx..).and_then(|s| s.chars().next()) else {
        return false;
    };
    prev.is_lowercase() && curr.is_uppercase()
}

fn subsequence_score(term: &str, haystack: &str, original: &str) -> Option<i32> {
    let mut term_chars = term.chars();
    let mut target = term_chars.next()?;
    let mut score = 0i32;
    let mut prev_match_char_idx: Option<usize> = None;
    let mut run_len = 0i32;
    let mut orig_iter = original.chars();
    let mut prev_orig: Option<char> = None;

    for (char_idx, ch) in haystack.chars().enumerate() {
        let orig_ch = orig_iter.next().unwrap_or(ch);
        if ch == target {
            score += 16;
            if prev_match_char_idx.is_some_and(|p| p + 1 == char_idx) {
                run_len += 1;
                score += 24 + (run_len.min(8) * 4);
            } else {
                if let Some(prev_idx) = prev_match_char_idx {
                    let gap = (char_idx - prev_idx - 1).min(32) as i32;
                    score -= gap * 2;
                }
                run_len = 0;
            }
            if char_idx == 0
                || prev_orig.is_some_and(|p| {
                    matches!(p, '/' | '\\' | '_' | '-' | '.' | ' ')
                        || (p.is_lowercase() && orig_ch.is_uppercase())
                })
            {
                score += 20;
            }
            prev_match_char_idx = Some(char_idx);
            match term_chars.next() {
                Some(next) => target = next,
                None => return Some(score),
            }
        }
        prev_orig = Some(orig_ch);
    }
    None
}

#[derive(Clone, Debug)]
struct FindCandidate {
    score: i32,
    path: String,
}

fn cmp_find_candidate(a: &FindCandidate, b: &FindCandidate) -> Ordering {
    b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path))
}

struct FindShared {
    session: PathGuardSession,
    root: PathBuf,
    relative_root: PathBuf,
    include: Option<GlobSet>,
    excludes: GlobSet,
    query: Arc<QueryPlan>,
    limit: usize,
    deadline: Instant,
    cancel: CancellationToken,
    visited: AtomicUsize,
    matched_total: AtomicUsize,
    skipped: AtomicUsize,
    truncated: AtomicBool,
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    collected: Mutex<Vec<FindCandidate>>,
}

impl FindShared {
    fn set_error(&self, message: &str) {
        let mut slot = self.error.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(message.to_owned());
        }
        self.stop.store(true, AtomicOrdering::Relaxed);
    }
}

struct FindWorker {
    shared: Arc<FindShared>,
    scratch: String,
    local: Vec<FindCandidate>,
    cutoff_score: i32,
    local_skipped: usize,
}

impl FindWorker {
    fn new(shared: Arc<FindShared>) -> Self {
        let init_cap = shared.limit.min(256);
        Self {
            shared,
            scratch: String::with_capacity(256),
            local: Vec::with_capacity(init_cap),
            cutoff_score: i32::MIN,
            local_skipped: 0,
        }
    }

    fn visit(&mut self, entry: Result<ignore::DirEntry, ignore::Error>) -> WalkState {
        if self.shared.stop.load(AtomicOrdering::Relaxed) {
            return WalkState::Quit;
        }
        if self.shared.cancel.is_cancelled() {
            self.shared.set_error("文件检索已取消。");
            return WalkState::Quit;
        }
        if Instant::now() >= self.shared.deadline {
            self.shared
                .set_error("文件检索超过 30 秒，请缩小检索范围。");
            return WalkState::Quit;
        }
        if self.shared.visited.fetch_add(1, AtomicOrdering::Relaxed) >= MAX_VISITED {
            self.shared.truncated.store(true, AtomicOrdering::Relaxed);
            self.shared.stop.store(true, AtomicOrdering::Relaxed);
            return WalkState::Quit;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                self.local_skipped += 1;
                return WalkState::Continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            return WalkState::Continue;
        }

        let path = entry.path();
        let keys = PathMatchKeys::new(
            path,
            &self.shared.relative_root,
            &self.shared.root,
            self.shared.include.is_some() || !self.shared.excludes.is_empty(),
        );
        if !keys.passes(self.shared.include.as_ref(), &self.shared.excludes) {
            return WalkState::Continue;
        }

        let Some(score) = self
            .shared
            .query
            .score_path(&keys.display, &mut self.scratch)
        else {
            return WalkState::Continue;
        };

        let keep_cap = self.shared.limit;
        if !self.shared.query.unfiltered
            && self.local.len() >= keep_cap
            && score < self.cutoff_score
        {
            self.shared.truncated.store(true, AtomicOrdering::Relaxed);
            return WalkState::Continue;
        }

        if self.shared.session.check_path(path).is_err() {
            return WalkState::Continue;
        }

        let total_now = self
            .shared
            .matched_total
            .fetch_add(1, AtomicOrdering::Relaxed)
            + 1;
        if self.shared.query.unfiltered && total_now > keep_cap {
            self.shared.truncated.store(true, AtomicOrdering::Relaxed);
            self.shared.stop.store(true, AtomicOrdering::Relaxed);
            return WalkState::Quit;
        }

        self.local.push(FindCandidate {
            score,
            path: keys.display,
        });
        if self.local.len() >= keep_cap.saturating_mul(2) {
            self.local
                .select_nth_unstable_by(keep_cap - 1, cmp_find_candidate);
            self.local.truncate(keep_cap);
            self.cutoff_score = self.local.iter().map(|c| c.score).min().unwrap_or(i32::MIN);
            self.shared.truncated.store(true, AtomicOrdering::Relaxed);
        }
        WalkState::Continue
    }
}

impl Drop for FindWorker {
    fn drop(&mut self) {
        if self.local_skipped > 0 {
            self.shared
                .skipped
                .fetch_add(self.local_skipped, AtomicOrdering::Relaxed);
        }
        if !self.local.is_empty() {
            let mut collected = self
                .shared
                .collected
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            collected.append(&mut self.local);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_find(
    session: &PathGuardSession,
    root: &Path,
    relative_root: &Path,
    include: Option<GlobSet>,
    excludes: GlobSet,
    query: Arc<QueryPlan>,
    limit: usize,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let walker = build_walker(session, root, relative_root, &excludes);
    let shared = Arc::new(FindShared {
        session: session.clone(),
        root: root.to_path_buf(),
        relative_root: relative_root.to_path_buf(),
        include,
        excludes,
        query: Arc::clone(&query),
        limit,
        deadline,
        cancel: cancel.clone(),
        visited: AtomicUsize::new(0),
        matched_total: AtomicUsize::new(0),
        skipped: AtomicUsize::new(0),
        truncated: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        error: Mutex::new(None),
        collected: Mutex::new(Vec::new()),
    });

    walker.build_parallel().run(|| {
        let mut worker = FindWorker::new(Arc::clone(&shared));
        Box::new(move |entry| worker.visit(entry))
    });

    if let Some(err) = shared
        .error
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        return Err(err);
    }

    check_search_time(cancel, deadline)?;
    let mut candidates =
        std::mem::take(&mut *shared.collected.lock().unwrap_or_else(|e| e.into_inner()));
    let mut truncated = shared.truncated.load(AtomicOrdering::Relaxed);
    if candidates.len() > limit {
        candidates.select_nth_unstable_by(limit - 1, cmp_find_candidate);
        candidates.truncate(limit);
        truncated = true;
    }
    candidates.sort_by(cmp_find_candidate);

    let top_score = candidates.first().map(|c| c.score);
    let skipped = shared.skipped.load(AtomicOrdering::Relaxed);
    let mut obj = Map::new();
    obj.insert("truncated".into(), Value::Bool(truncated));
    obj.insert("skipped".into(), json!(skipped));
    if !query.unfiltered && top_score.is_some_and(|score| score < WEAK_MATCH_SCORE_THRESHOLD) {
        obj.insert(
            "notice".into(),
            Value::String("仅找到弱相关路径匹配，建议补充文件名关键词或缩小 path 范围。".into()),
        );
    }
    Ok(bounded_output(
        obj,
        candidates
            .into_iter()
            .map(|candidate| (json!({"path": candidate.path}), None)),
        limit,
    ))
}

enum ContentMatcher {
    LiteralSensitive(Box<Finder<'static>>),
    LiteralInsensitive(regex::bytes::Regex),
    Regex(regex::bytes::Regex),
}

impl ContentMatcher {
    fn build(pattern: &str, literal: bool, sensitive: bool) -> Result<Self, String> {
        let literal = literal || regex::escape(pattern) == pattern;
        if literal && sensitive {
            return Ok(Self::LiteralSensitive(Box::new(
                Finder::new(pattern.as_bytes()).into_owned(),
            )));
        }
        let expression = if literal {
            regex::escape(pattern)
        } else {
            pattern.to_owned()
        };
        let re = BytesRegexBuilder::new(&expression)
            .case_insensitive(!sensitive)
            .size_limit(4 * 1024 * 1024)
            .build()
            .map_err(|_| "内容检索正则表达式无效或过大。")?;
        if literal {
            Ok(Self::LiteralInsensitive(re))
        } else {
            Ok(Self::Regex(re))
        }
    }

    // Only substring tests are safe on the whole file. Regexes can observe
    // boundaries introduced by splitting lines and removing their terminators.
    fn may_match_whole_file(&self, content: &[u8]) -> bool {
        match self {
            Self::LiteralSensitive(finder) => finder.find(content).is_some(),
            Self::LiteralInsensitive(re) => re.is_match(content),
            Self::Regex(_) => true,
        }
    }

    fn matches_line(&self, line: &[u8]) -> bool {
        match self {
            Self::LiteralSensitive(finder) => finder.find(line).is_some(),
            Self::LiteralInsensitive(re) | Self::Regex(re) => re.is_match(line),
        }
    }
}

fn decode_into_buf(encoding: &'static Encoding, src: &[u8], dst: &mut Vec<u8>) {
    dst.clear();
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let max_len = decoder
        .max_utf8_buffer_length(src.len())
        .unwrap_or_else(|| src.len().saturating_mul(3).saturating_add(16));
    dst.resize(max_len, 0);
    let mut read_total = 0usize;
    let mut write_total = 0usize;
    loop {
        let (result, read, written, _) =
            decoder.decode_to_utf8(&src[read_total..], &mut dst[write_total..], true);
        read_total += read;
        write_total += written;
        match result {
            CoderResult::InputEmpty => break,
            CoderResult::OutputFull => {
                let extra = dst.len().max(4096);
                dst.resize(dst.len().saturating_add(extra), 0);
            }
        }
    }
    dst.truncate(write_total);
}

/// Detect BOM / binary / UTF-8 / GB18030 and return searchable UTF-8 bytes without
/// copying valid UTF-8 files. Returns `None` for binary files.
fn decode_file_content<'a>(raw: &'a [u8], decode_buf: &'a mut Vec<u8>) -> Option<&'a [u8]> {
    if let Some((encoding, rest)) = raw
        .strip_prefix(b"\xFF\xFE")
        .map(|rest| (UTF_16LE, rest))
        .or_else(|| raw.strip_prefix(b"\xFE\xFF").map(|rest| (UTF_16BE, rest)))
    {
        decode_into_buf(encoding, rest, decode_buf);
        return memchr::memchr(0, decode_buf)
            .is_none()
            .then_some(decode_buf.as_slice());
    }

    let utf8_bom = raw.strip_prefix(b"\xEF\xBB\xBF");
    let body = utf8_bom.unwrap_or(raw);
    if memchr::memchr(0, body).is_some() {
        return None;
    }
    if std::str::from_utf8(body).is_ok() {
        return Some(body);
    }
    let encoding = if utf8_bom.is_some() { UTF_8 } else { GB18030 };
    decode_into_buf(encoding, body, decode_buf);
    Some(decode_buf.as_slice())
}

#[derive(Default)]
struct GrepResults {
    rows: Vec<(Value, usize)>,
    bytes: usize,
}

impl GrepResults {
    fn push(&mut self, row: Value, limit: usize) -> bool {
        if self.rows.len() >= limit {
            return false;
        }
        let size = row.to_string().len();
        let bytes = self.bytes + size + usize::from(!self.rows.is_empty());
        if bytes > MAX_OUTPUT_BYTES {
            return false;
        }
        self.bytes = bytes;
        self.rows.push((row, size));
        true
    }
}

struct GrepShared {
    session: PathGuardSession,
    root: PathBuf,
    relative_root: PathBuf,
    include: Option<GlobSet>,
    excludes: GlobSet,
    matcher: Arc<ContentMatcher>,
    limit: usize,
    context: usize,
    deadline: Instant,
    cancel: CancellationToken,
    visited: AtomicUsize,
    remaining_bytes: AtomicUsize,
    skipped: AtomicUsize,
    skipped_large_files: AtomicUsize,
    truncated: AtomicBool,
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    collected: Mutex<GrepResults>,
}

impl GrepShared {
    fn set_error(&self, message: &str) {
        let mut slot = self.error.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(message.to_owned());
        }
        self.stop.store(true, AtomicOrdering::Relaxed);
    }
}

fn reserve_read_budget(remaining: &AtomicUsize, need: usize) -> bool {
    let mut current = remaining.load(AtomicOrdering::Relaxed);
    loop {
        if need > current {
            return false;
        }
        match remaining.compare_exchange_weak(
            current,
            current - need,
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(next) => current = next,
        }
    }
}

fn read_with_budget(
    reader: impl Read,
    size: usize,
    remaining: &AtomicUsize,
    buf: &mut Vec<u8>,
) -> Option<io::Result<usize>> {
    if !reserve_read_budget(remaining, size) {
        return None;
    }
    // The size verified on the open handle bounds this read, including when
    // another process appends to the file. Refund bytes never read on short reads.
    buf.clear();
    buf.reserve(size);
    let result = reader.take(size as u64).read_to_end(buf);
    remaining.fetch_add(size - buf.len(), AtomicOrdering::Relaxed);
    Some(result)
}

struct GrepWorker {
    shared: Arc<GrepShared>,
    raw_buf: Vec<u8>,
    decode_buf: Vec<u8>,
    local_skipped: usize,
    local_skipped_large: usize,
}

impl GrepWorker {
    fn new(shared: Arc<GrepShared>) -> Self {
        Self {
            shared,
            raw_buf: Vec::with_capacity(WORKER_BUF_INIT),
            decode_buf: Vec::with_capacity(WORKER_BUF_INIT),
            local_skipped: 0,
            local_skipped_large: 0,
        }
    }

    fn visit(&mut self, entry: Result<ignore::DirEntry, ignore::Error>) -> WalkState {
        if self.shared.stop.load(AtomicOrdering::Relaxed) {
            return WalkState::Quit;
        }
        if self.shared.cancel.is_cancelled() {
            self.shared.set_error("文件检索已取消。");
            return WalkState::Quit;
        }
        if Instant::now() >= self.shared.deadline {
            self.shared
                .set_error("文件检索超过 30 秒，请缩小检索范围。");
            return WalkState::Quit;
        }
        if self.shared.visited.fetch_add(1, AtomicOrdering::Relaxed) >= MAX_VISITED {
            self.shared.truncated.store(true, AtomicOrdering::Relaxed);
            self.shared.stop.store(true, AtomicOrdering::Relaxed);
            return WalkState::Quit;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                self.local_skipped += 1;
                return WalkState::Continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            return WalkState::Continue;
        }

        let path = entry.path();
        let keys = PathMatchKeys::new(
            path,
            &self.shared.relative_root,
            &self.shared.root,
            self.shared.include.is_some() || !self.shared.excludes.is_empty(),
        );
        if !keys.passes(self.shared.include.as_ref(), &self.shared.excludes) {
            return WalkState::Continue;
        }
        let display = keys.display;

        let (file, file_size) = match self.shared.session.open_file(path) {
            Ok(pair) => pair,
            Err(_) => {
                self.local_skipped += 1;
                return WalkState::Continue;
            }
        };
        if file_size == 0 {
            return WalkState::Continue;
        }
        if file_size > MAX_FILE_BYTES {
            self.local_skipped += 1;
            self.local_skipped_large += 1;
            return WalkState::Continue;
        }
        if self.shared.stop.load(AtomicOrdering::Relaxed) {
            return WalkState::Quit;
        }
        match read_with_budget(
            file,
            file_size as usize,
            &self.shared.remaining_bytes,
            &mut self.raw_buf,
        ) {
            Some(Ok(_)) => (),
            Some(Err(_)) => {
                self.local_skipped += 1;
                shrink_if_large(&mut self.raw_buf);
                return WalkState::Continue;
            }
            None => {
                if !self.shared.stop.swap(true, AtomicOrdering::Relaxed) {
                    self.shared
                        .set_error("内容检索超过 256 MiB，请缩小检索范围。");
                }
                return WalkState::Quit;
            }
        }

        let shared = &self.shared;
        if let Some(content) = decode_file_content(&self.raw_buf, &mut self.decode_buf) {
            if let Err(error) = check_search_time(&shared.cancel, shared.deadline) {
                shared.set_error(&error);
            } else if !shared.stop.load(AtomicOrdering::Relaxed) {
                self.local_skipped += scan_content(
                    content,
                    &shared.matcher,
                    shared.context,
                    |line| {
                        if shared.stop.load(AtomicOrdering::Relaxed) {
                            return false;
                        }
                        if line.is_multiple_of(256)
                            && let Err(error) = check_search_time(&shared.cancel, shared.deadline)
                        {
                            shared.set_error(&error);
                            return false;
                        }
                        true
                    },
                    |line, text, matched| {
                        let mut collected =
                            shared.collected.lock().unwrap_or_else(|e| e.into_inner());
                        if shared.stop.load(AtomicOrdering::Relaxed) {
                            return false;
                        }
                        if !collected.push(
                            json!({"path": display, "line": line, "text": String::from_utf8_lossy(text), "matched": matched}),
                            shared.limit,
                        ) {
                            shared.truncated.store(true, AtomicOrdering::Relaxed);
                            shared.stop.store(true, AtomicOrdering::Relaxed);
                            return false;
                        }
                        true
                    },
                );
            }
        }

        shrink_if_large(&mut self.raw_buf);
        shrink_if_large(&mut self.decode_buf);
        if shared.stop.load(AtomicOrdering::Relaxed) {
            WalkState::Quit
        } else {
            WalkState::Continue
        }
    }
}

impl Drop for GrepWorker {
    fn drop(&mut self) {
        if self.local_skipped > 0 {
            self.shared
                .skipped
                .fetch_add(self.local_skipped, AtomicOrdering::Relaxed);
        }
        if self.local_skipped_large > 0 {
            self.shared
                .skipped_large_files
                .fetch_add(self.local_skipped_large, AtomicOrdering::Relaxed);
        }
    }
}

fn shrink_if_large(buf: &mut Vec<u8>) {
    if buf.capacity() > WORKER_BUF_SHRINK_THRESHOLD {
        buf.clear();
        buf.shrink_to(WORKER_BUF_INIT);
    }
}

fn scan_content(
    content: &[u8],
    matcher: &ContentMatcher,
    context: usize,
    mut should_continue: impl FnMut(usize) -> bool,
    mut emit: impl FnMut(usize, &[u8], bool) -> bool,
) -> usize {
    let may_match = matcher.may_match_whole_file(content);
    if !may_match && content.len() <= MAX_LINE_BYTES {
        return 0;
    }
    let mut before = VecDeque::with_capacity(context);
    let mut after = 0usize;
    let mut skipped = 0;
    let mut start = 0;

    let ends = memchr::memchr_iter(b'\n', content)
        .map(|nl| nl + 1)
        .chain(std::iter::once(content.len()));
    for (index, end) in ends.enumerate() {
        if start == end {
            break;
        }
        let line_no = index + 1;
        if !should_continue(line_no) {
            break;
        }
        let raw_line = &content[start..end];
        start = end;
        if raw_line.len() > MAX_LINE_BYTES {
            skipped += 1;
            before.clear();
            after = 0;
            continue;
        }
        if !may_match {
            continue;
        }
        let line = trim_line_end(raw_line);
        let matched = matcher.matches_line(line);
        if matched || after > 0 {
            if matched {
                while let Some((prev_num, prev_bytes)) = before.pop_front() {
                    if !emit(prev_num, prev_bytes, false) {
                        return skipped;
                    }
                }
            }
            if !emit(line_no, line, matched) {
                break;
            }
            after = if matched { context } else { after - 1 };
        } else if context > 0 {
            if before.len() == context {
                before.pop_front();
            }
            before.push_back((line_no, line));
        }
    }
    skipped
}

#[inline]
fn trim_line_end(mut bytes: &[u8]) -> &[u8] {
    while let Some(&last) = bytes.last() {
        if last == b'\n' || last == b'\r' {
            bytes = &bytes[..bytes.len() - 1];
        } else {
            break;
        }
    }
    bytes
}

#[allow(clippy::too_many_arguments)]
fn run_grep(
    session: &PathGuardSession,
    root: &Path,
    relative_root: &Path,
    include: Option<GlobSet>,
    excludes: GlobSet,
    matcher: Arc<ContentMatcher>,
    limit: usize,
    context: usize,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let walker = build_walker(session, root, relative_root, &excludes);
    let shared = Arc::new(GrepShared {
        session: session.clone(),
        root: root.to_path_buf(),
        relative_root: relative_root.to_path_buf(),
        include,
        excludes,
        matcher,
        limit,
        context,
        deadline,
        cancel: cancel.clone(),
        visited: AtomicUsize::new(0),
        remaining_bytes: AtomicUsize::new(MAX_TOTAL_READ_BYTES),
        skipped: AtomicUsize::new(0),
        skipped_large_files: AtomicUsize::new(0),
        truncated: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        error: Mutex::new(None),
        collected: Mutex::new(GrepResults::default()),
    });

    walker.build_parallel().run(|| {
        let mut worker = GrepWorker::new(Arc::clone(&shared));
        Box::new(move |entry| worker.visit(entry))
    });

    if let Some(err) = shared
        .error
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        return Err(err);
    }

    check_search_time(cancel, deadline)?;
    let mut collected = std::mem::take(
        &mut shared
            .collected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rows,
    );
    collected.sort_by(|(a, _), (b, _)| {
        a["path"]
            .as_str()
            .cmp(&b["path"].as_str())
            .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
    });

    let skipped = shared.skipped.load(AtomicOrdering::Relaxed);
    let skipped_large = shared.skipped_large_files.load(AtomicOrdering::Relaxed);
    let mut obj = Map::new();
    obj.insert(
        "truncated".into(),
        Value::Bool(shared.truncated.load(AtomicOrdering::Relaxed)),
    );
    obj.insert("skipped".into(), json!(skipped));
    if skipped_large > 0 {
        obj.insert("skippedLargeFiles".into(), json!(skipped_large));
        obj.insert(
            "notice".into(),
            Value::String(format!(
                "已跳过 {skipped_large} 个超过 10 MiB 的大文件，可通过 read 分段查看指定文件。"
            )),
        );
    }
    Ok(bounded_output(
        obj,
        collected.into_iter().map(|(row, size)| (row, Some(size))),
        limit,
    ))
}

// ============================================================================
// Shared Helpers
// ============================================================================

fn search_root(
    session: &PathGuardSession,
    cwd: &Path,
    filter: Option<&str>,
) -> Result<(PathBuf, Option<GlobSet>), String> {
    let Some(filter) = filter.filter(|s| !s.is_empty()) else {
        return Ok((cwd.into(), None));
    };
    let path = cwd.join(filter);
    session.verify_requested(&path)?;
    if path.exists() {
        return Ok((path, None));
    }

    let normalized = normalize_path(Path::new(filter));
    if let Some(wildcard) = normalized.find(['*', '?', '[', '{']) {
        if Path::new(filter).is_absolute()
            && let Some(separator) = normalized[..wildcard].rfind('/')
        {
            let suffix = &normalized[separator + 1..];
            return Ok((
                PathBuf::from(&normalized[..=separator]),
                Some(globs(&[suffix.into()])?),
            ));
        }
        return Ok((cwd.into(), Some(globs(&[normalized])?)));
    }
    Err("检索路径不存在。".into())
}

fn globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = normalize_path(Path::new(pattern));
        let glob = GlobBuilder::new(&pattern)
            .case_insensitive(cfg!(windows))
            .literal_separator(false)
            .build()
            .map_err(|_| "路径过滤表达式无效。".to_owned())?;
        builder.add(glob);
        if pattern.ends_with('/') {
            builder.add(
                GlobBuilder::new(&format!("{pattern}**"))
                    .case_insensitive(cfg!(windows))
                    .literal_separator(false)
                    .build()
                    .map_err(|_| "路径排除表达式无效。".to_owned())?,
            );
        }
    }
    builder.build().map_err(|_| "路径过滤表达式无效。".into())
}

fn check_search_time(cancel: &CancellationToken, deadline: Instant) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    if Instant::now() >= deadline {
        return Err("文件检索超过 30 秒，请缩小检索范围。".into());
    }
    Ok(())
}

fn bounded_output(
    mut obj: Map<String, Value>,
    rows: impl IntoIterator<Item = (Value, Option<usize>)>,
    limit: usize,
) -> Value {
    obj.insert("matches".into(), Value::Array(Vec::new()));
    let mut output = Value::Object(obj);
    let mut bytes = output.to_string().len();
    let matches = output["matches"]
        .as_array_mut()
        .expect("matches is an array");
    for (row, size) in rows {
        let next_bytes = bytes
            + size.unwrap_or_else(|| row.to_string().len())
            + usize::from(!matches.is_empty());
        if matches.len() == limit || next_bytes > MAX_OUTPUT_BYTES {
            output["truncated"] = Value::Bool(true);
            break;
        }
        bytes = next_bytes;
        matches.push(row);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn fff_matches_relative_globs_with_unextended_windows_cwd() {
        let temp = std::env::temp_dir().join(format!("cst-fff-cwd-{}", uuid::Uuid::new_v4()));
        let cwd = PathBuf::from(crate::diagnostics::volume_index::display_path(&temp));
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::write(cwd.join("src/log.txt"), b"needle").unwrap();
        let guard = PathGuard::new(&cwd).unwrap();
        for content in [false, true] {
            let input = serde_json::from_value(json!({
                "pattern": if content { "needle" } else { "log" }, "path": "src/*.txt"
            }))
            .unwrap();
            let value = search(&guard, &cwd, input, content, &CancellationToken::new()).unwrap();
            assert_eq!(value["matches"][0]["path"], "src/log.txt");
        }
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn fff_leaves_non_candidates_out_of_path_guard_work() {
        let temp = std::env::temp_dir().join(format!("cst-fff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp).unwrap();
        let cwd = temp.canonicalize().unwrap();
        let guard = PathGuard::new(&cwd).unwrap();
        for i in 0..32 {
            std::fs::write(cwd.join(format!("noise-{i}.txt")), b"needle").unwrap();
        }
        std::fs::write(cwd.join("needle.txt"), b"needle").unwrap();
        for (content, max_checks) in [(false, 3), (true, 3)] {
            let session = guard.session();
            let input = serde_json::from_value(json!({
                "pattern": "needle", "path": "needle.*"
            }))
            .unwrap();
            let value = search_with_session(
                &session,
                &cwd,
                input,
                content,
                &CancellationToken::new(),
                Instant::now() + SEARCH_TIMEOUT,
            )
            .unwrap();
            assert_eq!(value["matches"].as_array().unwrap().len(), 1);
            assert!(
                session.check_count() <= max_checks,
                "content={content}, checks={}",
                session.check_count()
            );
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn fffind_ranks_exact_and_prefix_filenames_above_scattered_subsequences() {
        let temp = std::env::temp_dir().join(format!("cst-fff-rank-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(temp.join("a/b/c")).unwrap();
        std::fs::create_dir_all(temp.join("z/y")).unwrap();
        let cwd = temp.canonicalize().unwrap();
        std::fs::write(
            cwd.join("a/old_target_x_diag_y_report_backup.log"),
            b"scattered",
        )
        .unwrap();
        std::fs::write(cwd.join("a/b/c/target_diag_report_extra.log"), b"prefix").unwrap();
        std::fs::write(cwd.join("z/y/target_diag_report.log"), b"exact").unwrap();

        let guard = PathGuard::new(&cwd).unwrap();
        let input = serde_json::from_value(json!({"pattern": "target_diag_report"})).unwrap();
        let value = search(&guard, &cwd, input, false, &CancellationToken::new()).unwrap();
        let paths: Vec<&str> = value["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths[0], "z/y/target_diag_report.log");
        assert_eq!(paths[1], "a/b/c/target_diag_report_extra.log");
        assert_eq!(paths[2], "a/old_target_x_diag_y_report_backup.log");
        assert!(value.get("notice").is_none());

        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn fffind_flags_weak_only_matches_with_notice() {
        let temp = std::env::temp_dir().join(format!("cst-fff-weak-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(temp.join("xdir/ydir")).unwrap();
        let cwd = temp.canonicalize().unwrap();
        std::fs::write(cwd.join("xdir/ydir/zfile.txt"), b"ok").unwrap();
        let guard = PathGuard::new(&cwd).unwrap();
        let input = serde_json::from_value(json!({"pattern": "xyz"})).unwrap();
        let value = search(&guard, &cwd, input, false, &CancellationToken::new()).unwrap();
        assert_eq!(value["matches"][0]["path"], "xdir/ydir/zfile.txt");
        assert!(value["notice"].as_str().unwrap().contains("弱相关"));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn ffgrep_decodes_utf16le_utf16be_and_gb18030_while_skipping_binary() {
        let temp = std::env::temp_dir().join(format!("cst-fff-enc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let cwd = temp.canonicalize().unwrap();

        // UTF-16 LE BOM
        let mut utf16le = vec![0xFF, 0xFE];
        for unit in "line1\r\nCRITICAL_DISK_TIMEOUT 磁盘超时\r\n".encode_utf16() {
            utf16le.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(cwd.join("dism_le.log"), &utf16le).unwrap();

        // UTF-16 BE BOM
        let mut utf16be = vec![0xFE, 0xFF];
        for unit in "header\nCRITICAL_DISK_TIMEOUT_BE\n".encode_utf16() {
            utf16be.extend_from_slice(&unit.to_be_bytes());
        }
        std::fs::write(cwd.join("dism_be.log"), &utf16be).unwrap();

        // GB18030 Chinese log
        let (gb_bytes, _, _) = GB18030.encode("第一行\r\n蓝屏异常代码 0x000000EF\r\n");
        std::fs::write(cwd.join("zh_gbk.log"), gb_bytes.as_ref()).unwrap();

        // Binary file with NUL bytes and matching ASCII payload
        std::fs::write(cwd.join("blob.bin"), b"\x00\x01CRITICAL_DISK_TIMEOUT\x00").unwrap();

        let guard = PathGuard::new(&cwd).unwrap();

        let le = search(
            &guard,
            &cwd,
            serde_json::from_value(json!({"pattern": "CRITICAL_DISK_TIMEOUT", "literal": true}))
                .unwrap(),
            true,
            &CancellationToken::new(),
        )
        .unwrap();
        let le_paths: Vec<&str> = le["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap())
            .collect();
        assert_eq!(le_paths, vec!["dism_be.log", "dism_le.log"]);
        assert_eq!(le["matches"][1]["line"], 2);
        assert!(
            le["matches"][1]["text"]
                .as_str()
                .unwrap()
                .contains("磁盘超时")
        );

        let gb = search(
            &guard,
            &cwd,
            serde_json::from_value(json!({"pattern": "蓝屏异常代码", "literal": true})).unwrap(),
            true,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(gb["matches"].as_array().unwrap().len(), 1);
        assert_eq!(gb["matches"][0]["path"], "zh_gbk.log");
        assert_eq!(gb["matches"][0]["line"], 2);

        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn ffgrep_skips_files_over_10mib_with_notice() {
        let temp = std::env::temp_dir().join(format!("cst-fff-large-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let cwd = temp.canonicalize().unwrap();
        let big = vec![b'a'; (MAX_FILE_BYTES as usize) + 128];
        std::fs::write(cwd.join("big.log"), &big).unwrap();
        std::fs::write(cwd.join("small.log"), b"needle_here\n").unwrap();

        let guard = PathGuard::new(&cwd).unwrap();
        let value = search(
            &guard,
            &cwd,
            serde_json::from_value(json!({"pattern": "needle_here", "literal": true})).unwrap(),
            true,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(value["matches"].as_array().unwrap().len(), 1);
        assert_eq!(value["skippedLargeFiles"], 1);
        assert_eq!(value["skipped"], 1);
        assert!(value["notice"].as_str().unwrap().contains("10 MiB"));

        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn regex_preserves_line_anchors_crlf_and_unicode_case_folding() {
        let content = b"header\r\nneedle\r\r\nneedle\nfinal";
        for pattern in [
            r"^needle$",
            r"(?-m:^needle$)",
            r"\Aneedle\z",
            r"(?mR:^needle$)",
        ] {
            let matcher = ContentMatcher::build(pattern, false, true).unwrap();
            let mut matches = Vec::new();
            assert_eq!(
                scan_content(
                    content,
                    &matcher,
                    0,
                    |_| true,
                    |line, text, matched| {
                        assert!(matched);
                        assert_eq!(text, b"needle");
                        matches.push(line);
                        true
                    }
                ),
                0
            );
            assert_eq!(matches, [2, 3]);
        }
        let matcher = ContentMatcher::build("k", true, false).unwrap();
        assert!(matcher.may_match_whole_file("K".as_bytes()));
        assert!(matcher.matches_line("K".as_bytes()));
        assert!(
            !ContentMatcher::build("K", true, true)
                .unwrap()
                .matches_line(b"k")
        );
    }

    #[test]
    fn literal_prefilter_keeps_oversized_line_accounting() {
        let mut content = vec![b'a'; MAX_LINE_BYTES];
        content.extend_from_slice(b"\r\nshort\n");
        let matcher = ContentMatcher::build("needle", true, true).unwrap();
        assert_eq!(
            scan_content(
                &content,
                &matcher,
                0,
                |_| true,
                |_, _, _| panic!("unexpected match")
            ),
            1,
        );
    }

    #[test]
    fn decoding_handles_four_byte_characters_nul_and_invalid_sequences() {
        let mut buffer = Vec::with_capacity(1);
        for (little_endian, bom) in [(true, [0xff, 0xfe]), (false, [0xfe, 0xff])] {
            let encode = |text: &str| {
                bom.into_iter()
                    .chain(text.encode_utf16().flat_map(|unit| {
                        if little_endian {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    }))
                    .collect::<Vec<u8>>()
            };
            let raw = encode("A𠮷\r\n");
            assert_eq!(
                decode_file_content(&raw, &mut buffer),
                Some("A𠮷\r\n".as_bytes())
            );
            let with_nul = encode(&format!("{}\0", "a".repeat(MAX_LINE_BYTES + 1)));
            assert!(decode_file_content(&with_nul, &mut buffer).is_none());
            let mut odd = encode("A");
            odd.push(0x61);
            assert_eq!(
                decode_file_content(&odd, &mut buffer),
                Some("A\u{fffd}".as_bytes())
            );
        }
        let (gb, _, had_errors) = GB18030.encode("A𠮷");
        assert!(!had_errors);
        assert_eq!(
            decode_file_content(&gb, &mut buffer),
            Some("A𠮷".as_bytes())
        );
        assert_eq!(
            decode_file_content(b"\xef\xbb\xbfA\xff", &mut buffer),
            Some("A\u{fffd}".as_bytes())
        );
    }

    #[test]
    fn verified_length_bounds_reads_and_refunds_unused_bytes() {
        let remaining = AtomicUsize::new(5);
        let mut buffer = Vec::new();
        assert!(read_with_budget(&b"abcdef"[..], 6, &remaining, &mut buffer).is_none());
        assert_eq!(remaining.load(AtomicOrdering::Relaxed), 5);
        assert_eq!(
            read_with_budget(&b"abcdef"[..], 3, &remaining, &mut buffer)
                .unwrap()
                .unwrap(),
            3
        );
        assert_eq!(buffer, b"abc");
        assert_eq!(
            read_with_budget(&b"x"[..], 2, &remaining, &mut buffer)
                .unwrap()
                .unwrap(),
            1
        );
        assert_eq!(buffer, b"x");
        assert_eq!(remaining.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn concurrent_budget_reservations_never_overdraw() {
        let budget = AtomicUsize::new(3201);
        let granted = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    while reserve_read_budget(&budget, 3) {
                        granted.fetch_add(3, AtomicOrdering::Relaxed);
                    }
                });
            }
        });
        assert_eq!(granted.load(AtomicOrdering::Relaxed), 3201);
        assert_eq!(budget.load(AtomicOrdering::Relaxed), 0);
    }

    #[test]
    fn output_cap_includes_the_wrapper_and_json_escaping() {
        let mut obj = Map::new();
        obj.insert("truncated".into(), json!(false));
        let baseline = bounded_output(obj.clone(), [], 1).to_string().len();
        let padding = MAX_OUTPUT_BYTES - baseline - json!("\"").to_string().len();
        let row = json!(format!("\"{}", "a".repeat(padding)));
        let output = bounded_output(obj.clone(), [(row.clone(), None)], 1);
        assert_eq!(output.to_string().len(), MAX_OUTPUT_BYTES);
        assert_eq!(output["truncated"], false);
        let output = bounded_output(obj, [(row, None), (json!(null), None)], 2);
        assert_eq!(output["truncated"], true);
        assert_eq!(output["matches"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn fff_exact_limit_and_ranked_top_k() {
        let cwd = std::env::temp_dir().join(format!("cst-fff-limit-{}", uuid::Uuid::new_v4()));
        for dir in ["a", "b", "c"] {
            std::fs::create_dir_all(cwd.join(dir)).unwrap();
            for n in 0..6 {
                std::fs::write(cwd.join(dir).join(format!("match-{n}.txt")), b"match\r\n").unwrap();
            }
        }
        let guard = PathGuard::new(&cwd).unwrap();
        for (pattern, content) in [("", false), ("match", false), ("match", true)] {
            let run = |limit| {
                search(
                    &guard,
                    &cwd,
                    serde_json::from_value(json!({"pattern": pattern, "limit": limit})).unwrap(),
                    content,
                    &CancellationToken::new(),
                )
                .unwrap()
            };
            let exact = run(18);
            assert_eq!(exact["matches"].as_array().unwrap().len(), 18);
            assert_eq!(exact["truncated"], false);
            let limited = run(2);
            assert_eq!(limited["matches"].as_array().unwrap().len(), 2);
            assert_eq!(limited["truncated"], true);
            if !content && !pattern.is_empty() {
                assert_eq!(
                    limited["matches"].as_array().unwrap().as_slice(),
                    &exact["matches"].as_array().unwrap()[..2]
                );
            }
        }
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_cross_drive_filters_match_absolute_and_search_relative_keys() {
        let keys = PathMatchKeys::new(
            Path::new(r"\\?\D:\logs\nested\report.txt"),
            Path::new(r"\\?\C:\kit"),
            Path::new(r"\\?\D:\logs"),
            true,
        );
        let include = globs(&[r"nested\*.txt".into()]).unwrap();
        let empty = globs(&[]).unwrap();
        assert!(keys.passes(Some(&include), &empty));
        let exclude = globs(&[r"d:\LOGS\nested\".into()]).unwrap();
        assert!(!keys.passes(Some(&include), &exclude));
        assert_eq!(keys.display, "D:/logs/nested/report.txt");
    }

    #[test]
    fn fff_exclude_works_consistently_for_external_absolute_path() {
        let root_a = std::env::temp_dir().join(format!("cst-fff-ext-a-{}", uuid::Uuid::new_v4()));
        let root_b = std::env::temp_dir().join(format!("cst-fff-ext-b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(root_b.join("sub")).unwrap();
        std::fs::write(root_b.join("keep.txt"), b"needle").unwrap();
        std::fs::write(root_b.join("drop.log"), b"needle").unwrap();
        std::fs::write(root_b.join("sub/nested.txt"), b"needle").unwrap();

        let guard = PathGuard::new(&root_a).unwrap();
        for content in [false, true] {
            let input = serde_json::from_value(json!({
                "pattern": if content { "needle" } else { "e" },
                "path": root_b.to_string_lossy(),
                "exclude": ["*.log", "sub/**"]
            }))
            .unwrap();
            let value = search(&guard, &root_a, input, content, &CancellationToken::new()).unwrap();
            let matches = value["matches"].as_array().unwrap();
            assert_eq!(matches.len(), 1, "content={content}, matches={matches:?}");
            let path = matches[0]["path"].as_str().unwrap();
            assert!(path.ends_with("/keep.txt"), "unexpected path: {path}");
            assert!(!path.contains('\\'));
        }

        let absolute_pattern = format!("{}/*.txt", normalize_path(&root_b));
        let exclude = normalize_path(&root_b.join("sub")) + "/";
        for content in [false, true] {
            let input = serde_json::from_value(json!({
                "pattern": if content { "needle" } else { "e" },
                "path": absolute_pattern,
                "exclude": [exclude],
            }))
            .unwrap();
            let value = search(&guard, &root_a, input, content, &CancellationToken::new()).unwrap();
            let matches = value["matches"].as_array().unwrap();
            assert_eq!(matches.len(), 1);
            assert!(matches[0]["path"].as_str().unwrap().ends_with("/keep.txt"));
        }
        std::fs::remove_dir_all(root_a).unwrap();
        std::fs::remove_dir_all(root_b).unwrap();
    }
}

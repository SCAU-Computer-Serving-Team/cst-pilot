use std::cmp::Ordering;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use encoding_rs::{CoderResult, Encoding, GB18030, UTF_16BE, UTF_16LE};
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
const WRAPPER_RESERVE_BYTES: usize = 1024;
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
    let session = guard.session();
    search_with_session(&session, cwd, input, content_search, cancel)
}

fn search_with_session(
    session: &PathGuardSession,
    cwd: &Path,
    input: SearchInput,
    content_search: bool,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    if input.pattern.len() > 4096 || (content_search && input.pattern.is_empty()) {
        return Err("检索表达式为空或超过 4096 字节。".into());
    }
    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let context = input.context.unwrap_or(0).min(20);
    let (raw_root, include) = search_root(cwd, input.path.as_deref())?;
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
        .unwrap_or(4);
    cores.saturating_sub(2).clamp(2, 8)
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
    let (display, rel_to_root) = path_match_keys(path, relative_root, search_root);
    let display_dir = format!("{display}/");
    if excludes.is_match(&display) || excludes.is_match(&display_dir) {
        return true;
    }
    if let Some(rel) = rel_to_root {
        let rel_dir = format!("{rel}/");
        if excludes.is_match(&rel) || excludes.is_match(&rel_dir) {
            return true;
        }
    }
    false
}

fn path_match_keys(
    path: &Path,
    relative_root: &Path,
    search_root: &Path,
) -> (String, Option<String>) {
    let display = path
        .strip_prefix(relative_root)
        .map(normalize_path)
        .unwrap_or_else(|_| normalize_path(path));
    let rel_to_root = if search_root != relative_root {
        path.strip_prefix(search_root).ok().map(normalize_path)
    } else {
        None
    };
    (display, rel_to_root)
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

fn passes_path_filters(
    display: &str,
    rel_to_root: Option<&str>,
    include: Option<&GlobSet>,
    excludes: &GlobSet,
) -> bool {
    if let Some(filter) = include {
        let matched = filter.is_match(display) || rel_to_root.is_some_and(|r| filter.is_match(r));
        if !matched {
            return false;
        }
    }
    if !excludes.is_empty()
        && (excludes.is_match(display) || rel_to_root.is_some_and(|r| excludes.is_match(r)))
    {
        return false;
    }
    true
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
    let orig_filename = original.get(filename_start..).unwrap_or(filename);

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
        return Some(1_200 + sub_score);
    }
    // 6. Subsequence across full path
    subsequence_score(term, haystack, original)
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
    since_deadline_check: usize,
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
            since_deadline_check: 0,
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
        self.since_deadline_check += 1;
        if self.since_deadline_check.is_multiple_of(64) && Instant::now() >= self.shared.deadline {
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
        let (display, rel_to_root) =
            path_match_keys(path, &self.shared.relative_root, &self.shared.root);
        if !passes_path_filters(
            &display,
            rel_to_root.as_deref(),
            self.shared.include.as_ref(),
            &self.shared.excludes,
        ) {
            return WalkState::Continue;
        }

        let Some(score) = self.shared.query.score_path(&display, &mut self.scratch) else {
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
            path: display,
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
    let mut matches = Vec::with_capacity(candidates.len());
    let mut output_bytes = WRAPPER_RESERVE_BYTES;
    for candidate in candidates {
        if !push_bounded(
            &mut matches,
            &mut output_bytes,
            json!({"path": candidate.path}),
            limit,
        )? {
            truncated = true;
            break;
        }
    }

    let skipped = shared.skipped.load(AtomicOrdering::Relaxed);
    let mut obj = Map::new();
    obj.insert("matches".into(), Value::Array(matches));
    obj.insert("truncated".into(), Value::Bool(truncated));
    obj.insert("skipped".into(), json!(skipped));
    if !query.unfiltered && top_score.is_some_and(|score| score < WEAK_MATCH_SCORE_THRESHOLD) {
        obj.insert(
            "notice".into(),
            Value::String("仅找到弱相关路径匹配，建议补充文件名关键词或缩小 path 范围。".into()),
        );
    }
    Ok(Value::Object(obj))
}

// ============================================================================
// ffgrep: Parallel Walk + SIMD Prefilter + Bytes Regex + UTF-16/GB18030 Decode
// ============================================================================

enum ContentMatcher {
    LiteralSensitive(Box<Finder<'static>>),
    LiteralAsciiInsensitive {
        lower: Vec<u8>,
        first_lo: u8,
        first_hi: u8,
    },
    BytesRegex {
        re: regex::bytes::Regex,
        allow_whole_file_prefilter: bool,
    },
}

impl ContentMatcher {
    fn build(pattern: &str, literal: bool, sensitive: bool) -> Result<Self, String> {
        let is_plain = literal || regex::escape(pattern) == pattern;
        if is_plain {
            if sensitive {
                return Ok(Self::LiteralSensitive(Box::new(
                    Finder::new(pattern.as_bytes()).into_owned(),
                )));
            }
            if pattern.is_ascii() {
                let lower = pattern.to_ascii_lowercase().into_bytes();
                let first_lo = lower[0];
                let first_hi = first_lo.to_ascii_uppercase();
                return Ok(Self::LiteralAsciiInsensitive {
                    lower,
                    first_lo,
                    first_hi,
                });
            }
            let escaped = regex::escape(pattern);
            let re = BytesRegexBuilder::new(&escaped)
                .case_insensitive(true)
                .unicode(true)
                .multi_line(true)
                .crlf(true)
                .size_limit(4 * 1024 * 1024)
                .build()
                .map_err(|_| "内容检索正则表达式无效或过大。")?;
            return Ok(Self::BytesRegex {
                re,
                allow_whole_file_prefilter: true,
            });
        }

        let re = BytesRegexBuilder::new(pattern)
            .case_insensitive(!sensitive)
            .unicode(true)
            .multi_line(true)
            .crlf(true)
            .size_limit(4 * 1024 * 1024)
            .build()
            .map_err(|_| "内容检索正则表达式无效或过大。")?;
        let allow_whole_file_prefilter =
            !pattern.contains("\\A") && !pattern.contains("\\z") && !pattern.contains("\\Z");
        Ok(Self::BytesRegex {
            re,
            allow_whole_file_prefilter,
        })
    }

    #[inline]
    fn matches_whole_file(&self, content: &[u8]) -> bool {
        match self {
            Self::LiteralSensitive(finder) => finder.find(content).is_some(),
            Self::LiteralAsciiInsensitive {
                lower,
                first_lo,
                first_hi,
            } => ascii_ci_contains(content, lower, *first_lo, *first_hi),
            Self::BytesRegex {
                re,
                allow_whole_file_prefilter,
            } => !*allow_whole_file_prefilter || re.is_match(content),
        }
    }

    #[inline]
    fn matches_line(&self, line: &[u8]) -> bool {
        match self {
            Self::LiteralSensitive(finder) => finder.find(line).is_some(),
            Self::LiteralAsciiInsensitive {
                lower,
                first_lo,
                first_hi,
            } => ascii_ci_contains(line, lower, *first_lo, *first_hi),
            Self::BytesRegex { re, .. } => re.is_match(line),
        }
    }
}

#[inline]
fn ascii_ci_contains(haystack: &[u8], lower_needle: &[u8], first_lo: u8, first_hi: u8) -> bool {
    let needle_len = lower_needle.len();
    if needle_len == 0 {
        return true;
    }
    if haystack.len() < needle_len {
        return false;
    }
    let last_start = haystack.len() - needle_len;
    let search_slice = &haystack[..=last_start];
    if first_lo == first_hi {
        for pos in memchr::memchr_iter(first_lo, search_slice) {
            if haystack[pos..pos + needle_len].eq_ignore_ascii_case(lower_needle) {
                return true;
            }
        }
    } else {
        for pos in memchr::memchr2_iter(first_lo, first_hi, search_slice) {
            if haystack[pos..pos + needle_len].eq_ignore_ascii_case(lower_needle) {
                return true;
            }
        }
    }
    false
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
    if let Some(rest) = raw.strip_prefix(b"\xFF\xFE") {
        decode_into_buf(UTF_16LE, rest, decode_buf);
        let probe_len = decode_buf.len().min(8192);
        if memchr::memchr(0, &decode_buf[..probe_len]).is_some() {
            return None;
        }
        return Some(decode_buf.as_slice());
    }
    if let Some(rest) = raw.strip_prefix(b"\xFE\xFF") {
        decode_into_buf(UTF_16BE, rest, decode_buf);
        let probe_len = decode_buf.len().min(8192);
        if memchr::memchr(0, &decode_buf[..probe_len]).is_some() {
            return None;
        }
        return Some(decode_buf.as_slice());
    }

    let body = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(raw);
    if memchr::memchr(0, body).is_some() {
        return None;
    }
    if std::str::from_utf8(body).is_ok() {
        return Some(body);
    }
    decode_into_buf(GB18030, body, decode_buf);
    Some(decode_buf.as_slice())
}

#[derive(Clone, Debug)]
struct GrepMatch {
    path: String,
    line: usize,
    text: String,
    matched: bool,
}

fn cmp_grep_match(a: &GrepMatch, b: &GrepMatch) -> Ordering {
    a.path.cmp(&b.path).then_with(|| a.line.cmp(&b.line))
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
    matched_total: AtomicUsize,
    approx_output_bytes: AtomicUsize,
    skipped: AtomicUsize,
    skipped_large_files: AtomicUsize,
    truncated: AtomicBool,
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    collected: Mutex<Vec<GrepMatch>>,
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

struct GrepWorker {
    shared: Arc<GrepShared>,
    raw_buf: Vec<u8>,
    decode_buf: Vec<u8>,
    local_matches: Vec<GrepMatch>,
    local_skipped: usize,
    local_skipped_large: usize,
    since_deadline_check: usize,
}

impl GrepWorker {
    fn new(shared: Arc<GrepShared>) -> Self {
        Self {
            shared,
            raw_buf: Vec::with_capacity(WORKER_BUF_INIT),
            decode_buf: Vec::with_capacity(WORKER_BUF_INIT),
            local_matches: Vec::new(),
            local_skipped: 0,
            local_skipped_large: 0,
            since_deadline_check: 0,
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
        self.since_deadline_check += 1;
        if self.since_deadline_check.is_multiple_of(8) && Instant::now() >= self.shared.deadline {
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
        let (display, rel_to_root) =
            path_match_keys(path, &self.shared.relative_root, &self.shared.root);
        if !passes_path_filters(
            &display,
            rel_to_root.as_deref(),
            self.shared.include.as_ref(),
            &self.shared.excludes,
        ) {
            return WalkState::Continue;
        }

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
        if !reserve_read_budget(&self.shared.remaining_bytes, file_size as usize) {
            self.shared
                .set_error("内容检索超过 256 MiB，请缩小检索范围。");
            return WalkState::Quit;
        }

        self.raw_buf.clear();
        let mut reader = file.take(MAX_FILE_BYTES + 1);
        if reader.read_to_end(&mut self.raw_buf).is_err() {
            self.local_skipped += 1;
            return WalkState::Continue;
        }
        if self.raw_buf.len() as u64 > MAX_FILE_BYTES {
            self.local_skipped += 1;
            self.local_skipped_large += 1;
            shrink_if_large(&mut self.raw_buf);
            return WalkState::Continue;
        }

        let Some(content) = decode_file_content(&self.raw_buf, &mut self.decode_buf) else {
            shrink_if_large(&mut self.raw_buf);
            shrink_if_large(&mut self.decode_buf);
            return WalkState::Continue;
        };

        if self.shared.matcher.matches_whole_file(content) {
            let max_needed = self.shared.limit.saturating_add(1);
            let before_len = self.local_matches.len();
            let mut file_truncated = false;
            scan_content(
                content,
                &display,
                &self.shared.matcher,
                self.shared.context,
                max_needed,
                &mut self.local_matches,
                &mut self.local_skipped,
                &mut file_truncated,
            );
            let added = self.local_matches.len() - before_len;
            if added > 0 {
                let added_bytes: usize = self.local_matches[before_len..]
                    .iter()
                    .map(|m| m.path.len() + m.text.len() + 56)
                    .sum();
                let new_total = self
                    .shared
                    .matched_total
                    .fetch_add(added, AtomicOrdering::Relaxed)
                    + added;
                let new_bytes = self
                    .shared
                    .approx_output_bytes
                    .fetch_add(added_bytes, AtomicOrdering::Relaxed)
                    + added_bytes;
                if file_truncated || new_total > self.shared.limit || new_bytes >= MAX_OUTPUT_BYTES
                {
                    self.shared.truncated.store(true, AtomicOrdering::Relaxed);
                    self.shared.stop.store(true, AtomicOrdering::Relaxed);
                    shrink_if_large(&mut self.raw_buf);
                    shrink_if_large(&mut self.decode_buf);
                    return WalkState::Quit;
                }
            }
        }

        shrink_if_large(&mut self.raw_buf);
        shrink_if_large(&mut self.decode_buf);
        WalkState::Continue
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
        if !self.local_matches.is_empty() {
            let mut collected = self
                .shared
                .collected
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            collected.append(&mut self.local_matches);
        }
    }
}

fn shrink_if_large(buf: &mut Vec<u8>) {
    if buf.capacity() > WORKER_BUF_SHRINK_THRESHOLD {
        buf.clear();
        buf.shrink_to(WORKER_BUF_INIT);
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_content<'a>(
    content: &'a [u8],
    display: &str,
    matcher: &ContentMatcher,
    context: usize,
    max_file_matches: usize,
    out: &mut Vec<GrepMatch>,
    skipped: &mut usize,
    file_truncated: &mut bool,
) {
    let mut before: VecDeque<(usize, &'a [u8])> = VecDeque::with_capacity(context);
    let mut after = 0usize;
    let mut line_number = 0usize;
    let mut start = 0usize;
    let mut emitted_in_file = 0usize;

    let mut process_line = |raw_line: &'a [u8], out: &mut Vec<GrepMatch>| -> bool {
        line_number += 1;
        if raw_line.len() > MAX_LINE_BYTES {
            *skipped += 1;
            before.clear();
            after = 0;
            return true;
        }
        let line = trim_line_end(raw_line);
        let matched = matcher.matches_line(line);
        if matched || after > 0 {
            if matched {
                while let Some((prev_num, prev_bytes)) = before.pop_front() {
                    if emitted_in_file >= max_file_matches {
                        *file_truncated = true;
                        return false;
                    }
                    out.push(GrepMatch {
                        path: display.to_owned(),
                        line: prev_num,
                        text: String::from_utf8_lossy(prev_bytes).into_owned(),
                        matched: false,
                    });
                    emitted_in_file += 1;
                }
            }
            if emitted_in_file >= max_file_matches {
                *file_truncated = true;
                return false;
            }
            out.push(GrepMatch {
                path: display.to_owned(),
                line: line_number,
                text: String::from_utf8_lossy(line).into_owned(),
                matched,
            });
            emitted_in_file += 1;
            after = if matched { context } else { after - 1 };
        } else if context > 0 {
            before.push_back((line_number, line));
            if before.len() > context {
                before.pop_front();
            }
        }
        true
    };

    for nl_pos in memchr::memchr_iter(b'\n', content) {
        let end = nl_pos + 1;
        if !process_line(&content[start..end], out) {
            return;
        }
        start = end;
    }
    if start < content.len() {
        let _ = process_line(&content[start..], out);
    }
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
        matched_total: AtomicUsize::new(0),
        approx_output_bytes: AtomicUsize::new(WRAPPER_RESERVE_BYTES),
        skipped: AtomicUsize::new(0),
        skipped_large_files: AtomicUsize::new(0),
        truncated: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        error: Mutex::new(None),
        collected: Mutex::new(Vec::new()),
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

    let mut collected =
        std::mem::take(&mut *shared.collected.lock().unwrap_or_else(|e| e.into_inner()));
    collected.sort_by(cmp_grep_match);

    let mut matches = Vec::with_capacity(collected.len().min(limit));
    let mut output_bytes = WRAPPER_RESERVE_BYTES;
    let mut truncated = shared.truncated.load(AtomicOrdering::Relaxed);
    for item in collected {
        if !push_bounded(
            &mut matches,
            &mut output_bytes,
            json!({
                "path": item.path,
                "line": item.line,
                "text": item.text,
                "matched": item.matched
            }),
            limit,
        )? {
            truncated = true;
            break;
        }
    }

    let skipped = shared.skipped.load(AtomicOrdering::Relaxed);
    let skipped_large = shared.skipped_large_files.load(AtomicOrdering::Relaxed);
    let mut obj = Map::new();
    obj.insert("matches".into(), Value::Array(matches));
    obj.insert("truncated".into(), Value::Bool(truncated));
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
    Ok(Value::Object(obj))
}

// ============================================================================
// Shared Helpers
// ============================================================================

fn search_root(cwd: &Path, filter: Option<&str>) -> Result<(PathBuf, Option<GlobSet>), String> {
    let Some(filter) = filter.filter(|s| !s.is_empty()) else {
        return Ok((cwd.into(), None));
    };
    let path = cwd.join(filter);
    if path.exists() {
        return Ok((path, None));
    }
    if filter.contains(['*', '?', '[', '{']) {
        return Ok((cwd.into(), Some(globs(&[filter.into()])?)));
    }
    Err("检索路径不存在。".into())
}

fn globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = pattern.replace('\\', "/");
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(false)
            .build()
            .map_err(|_| "路径过滤表达式无效。".to_owned())?;
        builder.add(glob);
        if pattern.ends_with('/') {
            builder.add(
                GlobBuilder::new(&format!("{pattern}**"))
                    .literal_separator(false)
                    .build()
                    .map_err(|_| "路径排除表达式无效。".to_owned())?,
            );
        }
    }
    builder.build().map_err(|_| "路径过滤表达式无效。".into())
}

fn push_bounded(
    matches: &mut Vec<Value>,
    bytes: &mut usize,
    value: Value,
    limit: usize,
) -> Result<bool, String> {
    let size = serde_json::to_vec(&value)
        .map_err(|_| "检索结果无法编码。")?
        .len()
        + 1;
    if matches.len() >= limit || *bytes + size > MAX_OUTPUT_BYTES {
        return Ok(false);
    }
    *bytes += size;
    matches.push(value);
    Ok(true)
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
            let value =
                search_with_session(&session, &cwd, input, content, &CancellationToken::new())
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

        std::fs::remove_dir_all(root_a).unwrap();
        std::fs::remove_dir_all(root_b).unwrap();
    }
}

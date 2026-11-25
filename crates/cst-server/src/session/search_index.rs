use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use cst_os::fs_safe::atomic_write_json_sync;
use cst_search::{
    ContentSearchBestHitDto, ContentSearchMatchDto, SearchContentKind, SessionSearchResponseDto,
    TitleSearchMatchDto, build_snippet, collapse_whitespace, match_query,
};
use pi_agent::AgentMessage;
use pi_ai::{ContentBlock, UserContent, UserContentBlock, uuid_v7};
use pi_session::{
    FileEntry, SessionEntry, SessionHeader, SessionManager, format_iso_timestamp,
    parse_iso_timestamp_ms, parse_session_entry_line,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::session::UnflushedSessionSnapshot;
use crate::session::index::SessionIndexStore;
use crate::session::slot::SessionSlot;

const SEARCH_INDEX_SCHEMA_VERSION: u32 = 2;
const MAX_TOOL_RESULT_INDEX_CHARS: usize = 4096;
const MAX_QUERY_SNAPSHOT_CACHE: usize = 8;
const QUERY_SNAPSHOT_TTL: Duration = Duration::from_secs(60);
const DEFAULT_PAGE_LIMIT: usize = 10;
const MAX_PAGE_LIMIT: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedSearchIndex {
    schema_version: u32,
    sessions: BTreeMap<String, Arc<IndexedSession>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedChunk {
    pub entry_id: String,
    pub kind: SearchContentKind,
    pub timestamp_ms: u64,
    pub on_active_branch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_leaf_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSession {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub is_placeholder_title: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub mtime_ns: u64,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<String>,
    #[serde(default)]
    pub chunks: Vec<IndexedChunk>,
}

#[derive(Debug, Clone)]
pub struct SearchQueryParams<'a> {
    pub query: &'a str,
    pub active_session_id: Option<&'a str>,
    pub title_offset: usize,
    pub title_limit: usize,
    pub content_offset: usize,
    pub content_limit: usize,
    pub snapshot_token: Option<&'a str>,
}

impl<'a> SearchQueryParams<'a> {
    pub fn new(query: &'a str) -> Self {
        Self {
            query,
            active_session_id: None,
            title_offset: 0,
            title_limit: DEFAULT_PAGE_LIMIT,
            content_offset: 0,
            content_limit: DEFAULT_PAGE_LIMIT,
            snapshot_token: None,
        }
    }
}

struct CachedSearchSnapshot {
    token: String,
    query: String,
    active_session_id: Option<String>,
    created_at: Instant,
    title_matches: Vec<TitleSearchMatchDto>,
    content_matches: Vec<ContentSearchMatchDto>,
}

struct IndexState {
    synced_initial: bool,
    dirty: bool,
    sessions: BTreeMap<String, Arc<IndexedSession>>,
    // Live writes win over an in-flight startup scan, even if equal to the disk cache.
    startup_overrides: HashSet<String>,
    removed: HashSet<String>,
}

/// Full-tree session search index backed by an in-memory map + atomic disk cache
/// (`data/inbox/search-index.v1.json`), with mtime/size incremental startup sync
/// and short-lived snapshot pagination cache.
#[derive(Clone)]
pub struct SessionSearchIndex {
    sessions_root: PathBuf,
    cache_path: PathBuf,
    state: Arc<RwLock<IndexState>>,
    startup_sync: Arc<Mutex<()>>,
    query_cache: Arc<Mutex<VecDeque<CachedSearchSnapshot>>>,
    persist_notify: Arc<Notify>,
    persist_spawned: Arc<std::sync::atomic::AtomicBool>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl SessionSearchIndex {
    pub fn new(sessions_root: PathBuf, inbox_root: PathBuf) -> Self {
        let cache_path = inbox_root.join("search-index.v1.json");
        let loaded = load_persisted_cache(&cache_path).unwrap_or_default();
        Self {
            sessions_root,
            cache_path,
            state: Arc::new(RwLock::new(IndexState {
                synced_initial: false,
                dirty: false,
                sessions: loaded,
                startup_overrides: HashSet::new(),
                removed: HashSet::new(),
            })),
            startup_sync: Arc::new(Mutex::new(())),
            query_cache: Arc::new(Mutex::new(VecDeque::new())),
            persist_notify: Arc::new(Notify::new()),
            persist_spawned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.persist_notify.notify_one();
    }

    fn schedule_persist(&self) {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current()
            && !self
                .persist_spawned
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let this = self.clone();
            handle.spawn(async move {
                loop {
                    this.persist_notify.notified().await;
                    if this.closed.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    if this.closed.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let snapshot = {
                        let mut guard = this.state.write().expect("search index write lock");
                        if !guard.dirty {
                            continue;
                        }
                        guard.dirty = false;
                        PersistedSearchIndex {
                            schema_version: SEARCH_INDEX_SCHEMA_VERSION,
                            sessions: guard.sessions.clone(),
                        }
                    };
                    let path = this.cache_path.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        atomic_write_json_sync(&path, &snapshot, "保存会话搜索索引")
                    })
                    .await;
                }
            });
        }
        self.persist_notify.notify_one();
    }

    pub fn is_synced(&self) -> bool {
        self.state
            .read()
            .expect("search index read lock")
            .synced_initial
    }

    /// Reconciles the startup cache with disk once. Live slot updates publish independently.
    pub fn sync_all(&self, index_store: &SessionIndexStore) {
        let _sync = self.startup_sync.lock().expect("search index startup lock");
        let cached_by_path: HashMap<_, _> = {
            let guard = self.state.read().expect("search index read lock");
            if guard.synced_initial {
                return;
            }
            guard
                .sessions
                .values()
                .filter_map(|session| Some((session.path.clone()?, session.clone())))
                .collect()
        };
        // A duplicated session ID cannot be opened unambiguously by the session pool.
        let mut disk_sessions = BTreeMap::new();
        let mut duplicate_ids = HashSet::new();
        for path in collect_session_jsonl_files(&self.sessions_root) {
            let Some((mtime_ns, size)) = file_fingerprint(&path) else {
                continue;
            };
            let path_str = path.to_string_lossy();
            let session = cached_by_path
                .get(path_str.as_ref())
                .filter(|item| item.mtime_ns == mtime_ns && item.file_size == size)
                .cloned()
                .or_else(|| parse_jsonl_session_file(&path, mtime_ns, size).map(Arc::new));
            let Some(session) = session else { continue };
            let id = &session.id;
            if !index_store
                .is_deleted_sync(id)
                .is_ok_and(|deleted| !deleted)
            {
                continue;
            }
            if disk_sessions.insert(id.clone(), session.clone()).is_some() {
                duplicate_ids.insert(id.clone());
            }
        }
        for snap in index_store.list_all_unflushed_sync() {
            if index_store
                .is_deleted_sync(&snap.id)
                .is_ok_and(|deleted| !deleted)
            {
                disk_sessions
                    .entry(snap.id.clone())
                    .or_insert_with(|| Arc::new(indexed_from_unflushed(&snap)));
            }
        }
        for id in duplicate_ids {
            disk_sessions.remove(&id);
        }
        self.finish_startup_sync(disk_sessions);
    }

    fn finish_startup_sync(&self, mut disk_sessions: BTreeMap<String, Arc<IndexedSession>>) {
        let mut guard = self.state.write().expect("search index write lock");
        for id in std::mem::take(&mut guard.startup_overrides) {
            if let Some(live) = guard.sessions.get(&id) {
                disk_sessions.insert(id, live.clone());
            }
        }
        disk_sessions.retain(|id, _| !guard.removed.contains(id));
        guard.dirty |= guard.sessions != disk_sessions;
        guard.sessions = disk_sessions;
        guard.synced_initial = true;
        let dirty = guard.dirty;
        drop(guard);
        if dirty {
            self.schedule_persist();
        }
    }

    pub fn upsert_unflushed(&self, snap: &UnflushedSessionSnapshot) {
        self.upsert(indexed_from_unflushed(snap));
    }

    pub fn upsert_from_slot(&self, slot: &SessionSlot) {
        slot.session()
            .with_session_manager(|manager| self.upsert_from_manager(manager));
    }

    pub fn upsert_from_manager(&self, manager: &SessionManager) {
        self.upsert(extract_indexed_from_manager(manager));
    }

    fn upsert(&self, item: IndexedSession) {
        let mut guard = self.state.write().expect("search index write lock");
        if guard.removed.contains(&item.id) {
            return;
        }
        if !guard.synced_initial {
            guard.startup_overrides.insert(item.id.clone());
        }
        if guard.sessions.get(&item.id).map(Arc::as_ref) == Some(&item) {
            return;
        }
        guard.sessions.insert(item.id.clone(), Arc::new(item));
        guard.dirty = true;
        drop(guard);
        self.schedule_persist();
    }

    pub fn remove(&self, session_id: &str) {
        let mut guard = self.state.write().expect("search index write lock");
        guard.removed.insert(session_id.to_owned());
        let changed = guard.sessions.remove(session_id).is_some();
        // Existing pages cannot be compacted in place without shifting their offsets.
        let mut cache = self.query_cache.lock().expect("query cache lock");
        cache.retain(|snap| {
            !snap
                .title_matches
                .iter()
                .any(|item| item.session_id == session_id)
                && !snap
                    .content_matches
                    .iter()
                    .any(|item| item.session_id == session_id)
        });
        guard.dirty |= changed;
        drop(cache);
        drop(guard);
        if changed {
            self.schedule_persist();
        }
    }

    pub fn search(&self, params: SearchQueryParams<'_>) -> SessionSearchResponseDto {
        let trimmed_query = params.query.trim();
        if trimmed_query.is_empty() {
            return SessionSearchResponseDto::default();
        }

        let title_limit = params.title_limit.clamp(1, MAX_PAGE_LIMIT);
        let content_limit = params.content_limit.clamp(1, MAX_PAGE_LIMIT);

        // Reuse cached snapshot when client pages with a valid matching `snapshotToken`.
        let requested_token = params.snapshot_token.filter(|token| !token.is_empty());
        if let Some(req_token) = requested_token {
            let mut cache = self.query_cache.lock().expect("query cache lock");
            let now = Instant::now();
            cache.retain(|snap| now.duration_since(snap.created_at) <= QUERY_SNAPSHOT_TTL);
            if let Some(snap) = cache.iter().find(|s| {
                s.token == req_token
                    && s.query == trimmed_query
                    && s.active_session_id.as_deref() == params.active_session_id
            }) {
                return slice_snapshot(
                    snap,
                    params.title_offset,
                    title_limit,
                    params.content_offset,
                    content_limit,
                );
            }
        }

        // Compute full sorted matches over all indexed sessions.
        let sessions_snapshot: Vec<Arc<IndexedSession>> = {
            let guard = self.state.read().expect("search index read lock");
            guard.sessions.values().cloned().collect()
        };

        let mut title_scored: Vec<(bool, u32, u64, TitleSearchMatchDto)> = Vec::new();
        let mut content_scored: Vec<(bool, u32, u8, usize, u64, ContentSearchMatchDto)> =
            Vec::new();

        for sess in &sessions_snapshot {
            let is_active = params.active_session_id == Some(sess.id.as_str());

            // 1. Title match
            if (!sess.is_placeholder_title || trimmed_query == "新对话")
                && let Some(m) = match_query(&sess.title, trimmed_query)
            {
                title_scored.push((
                    is_active,
                    m.score,
                    sess.updated_at_ms,
                    TitleSearchMatchDto {
                        session_id: sess.id.clone(),
                        title: sess.title.clone(),
                        updated_at: sess.updated_at.clone(),
                        score: m.score,
                    },
                ));
            }

            // 2. Content match across active + historical branches (tool output included, thinking excluded)
            let mut hit_count = 0usize;
            let mut best_rank: Option<(u32, u8, bool, u64, &IndexedChunk)> = None;
            for chunk in &sess.chunks {
                if let Some(m) = match_query(&chunk.text, trimmed_query) {
                    hit_count += 1;
                    let candidate = (
                        m.score,
                        chunk.kind.rank_priority(),
                        chunk.on_active_branch,
                        chunk.timestamp_ms,
                        chunk,
                    );
                    let replace = match best_rank {
                        None => true,
                        Some((bs, bp, ba, bt, _)) => {
                            (candidate.0, candidate.1, candidate.2, candidate.3) > (bs, bp, ba, bt)
                        }
                    };
                    if replace {
                        best_rank = Some(candidate);
                    }
                }
            }

            if let Some((best_score, best_prio, _, _, best_chunk)) = best_rank {
                let prefix = best_chunk.kind.snippet_prefix(best_chunk.on_active_branch);
                let (snippet, spans) = build_snippet(&best_chunk.text, trimmed_query, prefix, 36);
                content_scored.push((
                    is_active,
                    best_score,
                    best_prio,
                    hit_count,
                    sess.updated_at_ms,
                    ContentSearchMatchDto {
                        session_id: sess.id.clone(),
                        title: sess.title.clone(),
                        updated_at: sess.updated_at.clone(),
                        hit_count,
                        score: best_score,
                        best_hit: ContentSearchBestHitDto {
                            entry_id: best_chunk.entry_id.clone(),
                            kind: best_chunk.kind,
                            snippet,
                            on_active_branch: best_chunk.on_active_branch,
                            branch_leaf_id: best_chunk.branch_leaf_id.clone(),
                            spans,
                        },
                    },
                ));
            }
        }

        title_scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.3.session_id.cmp(&b.3.session_id))
        });

        content_scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| b.3.cmp(&a.3))
                .then_with(|| b.4.cmp(&a.4))
                .then_with(|| a.5.session_id.cmp(&b.5.session_id))
        });

        // A concurrent delete either filters these results or invalidates the published snapshot.
        let guard = self.state.read().expect("search index read lock");
        title_scored.retain(|item| !guard.removed.contains(&item.3.session_id));
        content_scored.retain(|item| !guard.removed.contains(&item.5.session_id));
        let token = uuid_v7().replace('-', "");
        let snap = CachedSearchSnapshot {
            token,
            query: trimmed_query.to_owned(),
            active_session_id: params.active_session_id.map(str::to_owned),
            created_at: Instant::now(),
            title_matches: title_scored.into_iter().map(|x| x.3).collect(),
            content_matches: content_scored.into_iter().map(|x| x.5).collect(),
        };

        let response = if requested_token.is_some() {
            // A replaced snapshot always restarts both groups at page one.
            slice_snapshot(&snap, 0, DEFAULT_PAGE_LIMIT, 0, DEFAULT_PAGE_LIMIT)
        } else {
            slice_snapshot(
                &snap,
                params.title_offset,
                title_limit,
                params.content_offset,
                content_limit,
            )
        };

        let mut cache = self.query_cache.lock().expect("query cache lock");
        let now = Instant::now();
        cache.retain(|s| now.duration_since(s.created_at) <= QUERY_SNAPSHOT_TTL);
        while cache.len() >= MAX_QUERY_SNAPSHOT_CACHE {
            cache.pop_front();
        }
        cache.push_back(snap);

        response
    }
}

fn slice_snapshot(
    snap: &CachedSearchSnapshot,
    title_offset: usize,
    title_limit: usize,
    content_offset: usize,
    content_limit: usize,
) -> SessionSearchResponseDto {
    let title_total = snap.title_matches.len();
    let title_start = title_offset.min(title_total);
    let title_end = title_start.saturating_add(title_limit).min(title_total);
    let title_matches = snap.title_matches[title_start..title_end].to_vec();
    let title_has_more = title_end < title_total;

    let content_total = snap.content_matches.len();
    let content_start = content_offset.min(content_total);
    let content_end = content_start
        .saturating_add(content_limit)
        .min(content_total);
    let content_matches = snap.content_matches[content_start..content_end].to_vec();
    let content_has_more = content_end < content_total;

    SessionSearchResponseDto {
        query: snap.query.clone(),
        snapshot_token: snap.token.clone(),
        title_matches,
        title_total,
        title_has_more,
        content_matches,
        content_total,
        content_has_more,
    }
}

fn load_persisted_cache(path: &Path) -> Option<BTreeMap<String, Arc<IndexedSession>>> {
    let bytes = fs::read(path).ok()?;
    let parsed: PersistedSearchIndex = serde_json::from_slice(&bytes).ok()?;
    if parsed.schema_version != SEARCH_INDEX_SCHEMA_VERSION {
        return None;
    }
    Some(parsed.sessions)
}

fn collect_session_jsonl_files(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() && !dirs.contains(&p) {
                dirs.push(p);
            }
        }
    }
    let mut files = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(p);
            }
        }
    }
    files
}

fn indexed_from_unflushed(snap: &UnflushedSessionSnapshot) -> IndexedSession {
    let explicit = snap
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let updated_at_ms = parse_iso_timestamp_ms(&snap.created_at).unwrap_or(0);
    IndexedSession {
        id: snap.id.clone(),
        title: explicit.unwrap_or("新对话").to_owned(),
        is_placeholder_title: explicit.is_none(),
        updated_at: Some(snap.created_at.clone()),
        updated_at_ms,
        path: None,
        mtime_ns: 0,
        file_size: 0,
        leaf_id: None,
        chunks: Vec::new(),
    }
}

fn file_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((mtime.as_nanos().try_into().ok()?, meta.len()))
}

fn parse_jsonl_session_file(path: &Path, mtime_ns: u64, file_size: u64) -> Option<IndexedSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut header: Option<SessionHeader> = None;
    let mut entries: Vec<SessionEntry> = Vec::new();

    let mut complete = true;
    for line_res in reader.lines() {
        let Ok(line) = line_res else {
            complete = false;
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(file_entry) = parse_session_entry_line(trimmed) {
            match file_entry {
                FileEntry::Session(h) => {
                    if header.is_none() {
                        header = Some(h);
                    }
                }
                FileEntry::Entry(e) => entries.push(e),
            }
        }
    }

    let hdr = header?;
    if file_fingerprint(path) != Some((mtime_ns, file_size)) {
        return None;
    }
    let leaf_id = entries.last().map(|e| e.id().to_owned());
    let mut indexed = build_indexed_session(
        &hdr.id,
        &hdr.timestamp,
        None,
        leaf_id.as_deref(),
        &entries,
        mtime_ns / 1_000_000,
    );
    if complete {
        indexed.path = Some(path.to_string_lossy().into_owned());
        indexed.mtime_ns = mtime_ns;
        indexed.file_size = file_size;
    }
    Some(indexed)
}

fn extract_indexed_from_manager(sm: &SessionManager) -> IndexedSession {
    let id = sm.get_session_id().to_owned();
    let header_ts = sm
        .get_header()
        .map(|h| h.timestamp.clone())
        .unwrap_or_default();
    let entries = sm.get_entries();
    let leaf_id = sm.get_leaf_id().map(str::to_owned);
    let session_name = sm.get_session_name();
    let (path_str, mtime_ns, file_size) = sm
        .get_session_file()
        .and_then(|path| {
            let (mtime, size) = file_fingerprint(path)?;
            Some((Some(path.to_string_lossy().into_owned()), mtime, size))
        })
        .unwrap_or_default();

    let mut indexed = build_indexed_session(
        &id,
        &header_ts,
        session_name.as_deref(),
        leaf_id.as_deref(),
        &entries,
        mtime_ns / 1_000_000,
    );
    // An in-memory branch selection may differ from the leaf reconstructed from JSONL.
    if sm.is_flushed() && leaf_id.as_deref() == entries.last().map(SessionEntry::id) {
        indexed.path = path_str;
        indexed.mtime_ns = mtime_ns;
        indexed.file_size = file_size;
    }
    indexed
}

fn entry_timestamp_ms(entry: &SessionEntry) -> u64 {
    let raw = match entry {
        SessionEntry::Message(e) => {
            let msg_ts = e.message.timestamp();
            if msg_ts > 0 {
                return msg_ts;
            }
            &e.timestamp
        }
        SessionEntry::ThinkingLevelChange(e) => &e.timestamp,
        SessionEntry::ModelChange(e) => &e.timestamp,
        SessionEntry::Usage(e) => &e.timestamp,
        SessionEntry::Compaction(e) => &e.timestamp,
        SessionEntry::BranchSummary(e) => &e.timestamp,
        SessionEntry::Custom(e) => &e.timestamp,
        SessionEntry::CustomMessage(e) => &e.timestamp,
        SessionEntry::ContextEdit(e) => &e.timestamp,
        SessionEntry::Label(e) => &e.timestamp,
        SessionEntry::SessionInfo(e) => &e.timestamp,
    };
    parse_iso_timestamp_ms(raw).unwrap_or(0)
}

fn extract_user_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(s) => s.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(tc) => Some(tc.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn extract_user_blocks_text(blocks: &[UserContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(tc) => Some(tc.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_content_blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            // Thinking blocks, tool-call raw JSON, and base64 images are intentionally excluded.
            ContentBlock::Text(tc) => Some(tc.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut iter = text.char_indices();
    match iter.nth(max_chars) {
        Some((byte_idx, _)) => text[..byte_idx].to_owned(),
        None => text.to_owned(),
    }
}

/// Computes the newest leaf node reachable from each entry ID in the session tree so clicking
/// an off-branch search hit restores that full historical branch rather than truncating it.
fn compute_descendant_newest_leaves(
    entries: &[SessionEntry],
) -> (HashMap<String, Option<String>>, HashMap<String, String>) {
    let mut parent_map: HashMap<String, Option<String>> = HashMap::with_capacity(entries.len());
    let mut children_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut ts_map: HashMap<String, u64> = HashMap::with_capacity(entries.len());

    for entry in entries {
        let id = entry.id().to_owned();
        let pid = entry.parent_id().map(str::to_owned);
        if let Some(ref p) = pid {
            children_map.entry(p.clone()).or_default().push(id.clone());
        }
        parent_map.insert(id.clone(), pid);
        ts_map.insert(id, entry_timestamp_ms(entry));
    }

    // Bottom-up or memoized DFS to find (max_ts, leaf_id) per node.
    let mut memo: HashMap<String, (u64, String)> = HashMap::with_capacity(entries.len());
    for entry in entries {
        let root_id = entry.id().to_owned();
        if memo.contains_key(&root_id) {
            continue;
        }
        let mut stack: Vec<(String, bool)> = vec![(root_id, false)];
        let mut visiting: HashSet<String> = HashSet::new();
        while let Some((node, expanded)) = stack.pop() {
            if memo.contains_key(&node) {
                continue;
            }
            if expanded {
                visiting.remove(&node);
                let best = children_map
                    .get(&node)
                    .into_iter()
                    .flatten()
                    .filter_map(|kid| memo.get(kid))
                    .max_by_key(|(timestamp, _)| *timestamp)
                    .cloned()
                    .unwrap_or_else(|| (*ts_map.get(&node).unwrap_or(&0), node.clone()));
                memo.insert(node, best);
            } else {
                if !visiting.insert(node.clone()) {
                    memo.insert(node.clone(), (*ts_map.get(&node).unwrap_or(&0), node));
                    continue;
                }
                stack.push((node.clone(), true));
                if let Some(kids) = children_map.get(&node) {
                    for kid in kids {
                        if !memo.contains_key(kid) {
                            stack.push((kid.clone(), false));
                        }
                    }
                }
            }
        }
    }

    let newest_leaf_map = memo.into_iter().map(|(k, (_, leaf))| (k, leaf)).collect();
    (parent_map, newest_leaf_map)
}

fn build_indexed_session(
    id: &str,
    header_ts: &str,
    explicit_name_override: Option<&str>,
    leaf_id: Option<&str>,
    entries: &[SessionEntry],
    file_mtime_ms: u64,
) -> IndexedSession {
    let (parent_map, newest_leaf_map) = compute_descendant_newest_leaves(entries);

    let mut active_set: HashSet<String> = HashSet::new();
    let mut cur = leaf_id.map(str::to_owned);
    while let Some(nid) = cur {
        if !active_set.insert(nid.clone()) {
            break;
        }
        cur = parent_map.get(&nid).cloned().flatten();
    }

    let mut latest_session_name: Option<String> = explicit_name_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let mut first_user_title: Option<String> = None;
    let mut latest_ts_ms = parse_iso_timestamp_ms(header_ts)
        .unwrap_or(0)
        .max(file_mtime_ms);
    let mut chunks: Vec<IndexedChunk> = Vec::new();

    for entry in entries {
        let ts_ms = entry_timestamp_ms(entry);
        if ts_ms > latest_ts_ms {
            latest_ts_ms = ts_ms;
        }

        let eid = entry.id();
        let on_active_branch = active_set.contains(eid);
        let branch_leaf_id = if on_active_branch {
            None
        } else {
            newest_leaf_map
                .get(eid)
                .cloned()
                .or_else(|| Some(eid.to_owned()))
        };

        match entry {
            SessionEntry::SessionInfo(info) => {
                if let Some(name) = info
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    latest_session_name = Some(name.to_owned());
                }
            }
            SessionEntry::Message(m) => match &m.message {
                AgentMessage::User(u) => {
                    let raw = extract_user_text(&u.content);
                    let clean = collapse_whitespace(&raw);
                    if !clean.is_empty() {
                        if first_user_title.is_none() {
                            first_user_title = Some(clean.chars().take(160).collect());
                        }
                        chunks.push(IndexedChunk {
                            entry_id: eid.to_owned(),
                            kind: SearchContentKind::User,
                            timestamp_ms: ts_ms,
                            on_active_branch,
                            branch_leaf_id,
                            text: clean,
                        });
                    }
                }
                AgentMessage::Assistant(a) => {
                    let raw = extract_content_blocks_text(&a.content);
                    let clean = collapse_whitespace(&raw);
                    if !clean.is_empty() {
                        chunks.push(IndexedChunk {
                            entry_id: eid.to_owned(),
                            kind: SearchContentKind::Assistant,
                            timestamp_ms: ts_ms,
                            on_active_branch,
                            branch_leaf_id,
                            text: clean,
                        });
                    }
                }
                AgentMessage::ToolResult(tr) => {
                    // Never index BitLocker output to protect recovery credentials.
                    if tr.tool_name.eq_ignore_ascii_case("bitlocker") {
                        continue;
                    }
                    let raw = extract_user_blocks_text(&tr.content);
                    let clean = collapse_whitespace(&raw);
                    if !clean.is_empty() {
                        chunks.push(IndexedChunk {
                            entry_id: eid.to_owned(),
                            kind: SearchContentKind::Tool,
                            timestamp_ms: ts_ms,
                            on_active_branch,
                            branch_leaf_id,
                            text: truncate_chars(&clean, MAX_TOOL_RESULT_INDEX_CHARS),
                        });
                    }
                }
                AgentMessage::System(_) | AgentMessage::Custom(_) => {}
            },
            SessionEntry::Compaction(c) => {
                let clean = collapse_whitespace(&c.summary);
                if !clean.is_empty() {
                    chunks.push(IndexedChunk {
                        entry_id: eid.to_owned(),
                        kind: SearchContentKind::Summary,
                        timestamp_ms: ts_ms,
                        on_active_branch,
                        branch_leaf_id,
                        text: clean,
                    });
                }
            }
            SessionEntry::BranchSummary(bs) => {
                let clean = collapse_whitespace(&bs.summary);
                if !clean.is_empty() {
                    chunks.push(IndexedChunk {
                        entry_id: eid.to_owned(),
                        kind: SearchContentKind::Summary,
                        timestamp_ms: ts_ms,
                        on_active_branch,
                        branch_leaf_id,
                        text: clean,
                    });
                }
            }
            SessionEntry::ThinkingLevelChange(_)
            | SessionEntry::ModelChange(_)
            | SessionEntry::Usage(_)
            | SessionEntry::Custom(_)
            | SessionEntry::CustomMessage(_)
            | SessionEntry::ContextEdit(_)
            | SessionEntry::Label(_) => {}
        }
    }

    let resolved_title = latest_session_name.or(first_user_title);
    let is_placeholder_title = resolved_title.is_none();
    let title = resolved_title.unwrap_or_else(|| "新对话".to_owned());
    let updated_at = (latest_ts_ms > 0).then(|| format_iso_timestamp(latest_ts_ms));

    IndexedSession {
        id: id.to_owned(),
        title,
        is_placeholder_title,
        updated_at,
        updated_at_ms: latest_ts_ms,
        path: None,
        mtime_ns: 0,
        file_size: 0,
        leaf_id: leaf_id.map(str::to_owned),
        chunks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::{
        AssistantMessage, StopReason, TextContent, ThinkingContent, ToolResultMessage, Usage,
        UserMessage,
    };
    use pi_session::{CompactionEntry, SessionInfoEntry, SessionMessageEntry};

    fn make_user_entry(id: &str, parent: Option<&str>, ts: u64, text: &str) -> SessionEntry {
        SessionEntry::Message(SessionMessageEntry {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            timestamp: format_iso_timestamp(ts),
            message: AgentMessage::User(UserMessage {
                content: UserContent::Text(text.to_owned()),
                timestamp: ts,
            }),
        })
    }

    fn make_assistant_entry(
        id: &str,
        parent: Option<&str>,
        ts: u64,
        thinking: Option<&str>,
        text: &str,
    ) -> SessionEntry {
        let mut content = Vec::new();
        if let Some(th) = thinking {
            content.push(ContentBlock::Thinking(ThinkingContent {
                thinking: th.to_owned(),
                thinking_signature: None,
                redacted: None,
            }));
        }
        content.push(ContentBlock::Text(TextContent {
            text: text.to_owned(),
            text_signature: None,
        }));
        SessionEntry::Message(SessionMessageEntry {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            timestamp: format_iso_timestamp(ts),
            message: AgentMessage::Assistant(AssistantMessage {
                content,
                api: "openai-completions".to_owned(),
                provider: "deepseek".to_owned(),
                model: "deepseek-flash".to_owned(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: ts,
                duration_ms: None,
            }),
        })
    }

    fn make_tool_entry(
        id: &str,
        parent: Option<&str>,
        ts: u64,
        tool_name: &str,
        text: &str,
    ) -> SessionEntry {
        SessionEntry::Message(SessionMessageEntry {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            timestamp: format_iso_timestamp(ts),
            message: AgentMessage::ToolResult(ToolResultMessage {
                tool_call_id: format!("tc-{id}"),
                tool_name: tool_name.to_owned(),
                content: vec![UserContentBlock::Text(TextContent {
                    text: text.to_owned(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: ts,
                duration_ms: None,
            }),
        })
    }

    #[test]
    fn excludes_thinking_and_bitlocker_while_indexing_tools_summaries_and_off_branch() {
        let entries = vec![
            SessionEntry::SessionInfo(SessionInfoEntry {
                id: "info1".into(),
                parent_id: None,
                timestamp: format_iso_timestamp(1000),
                name: Some("C 盘空间与硬盘信息".into()),
            }),
            make_user_entry("u1", Some("info1"), 2000, "帮我看 C 盘空间"),
            // Historical branch off u1
            make_assistant_entry(
                "a_old",
                Some("u1"),
                2500,
                Some("绝密思考词_SECRET_THINKING"),
                "历史分支发现旧版驱动残留",
            ),
            make_user_entry("u_old_leaf", Some("a_old"), 2600, "先不用删驱动"),
            // Active branch off u1
            make_tool_entry("t1", Some("u1"), 3000, "disk", "驱动安装包缓存占用 4.8 GB"),
            make_tool_entry(
                "t_bl",
                Some("t1"),
                3100,
                "bitlocker",
                "恢复密码 111111-222222",
            ),
            make_assistant_entry(
                "a_cur",
                Some("t_bl"),
                3200,
                Some("思考中提及隐蔽词_HIDDEN_THOUGHT"),
                "空间主要由休眠文件与本地缓存占用",
            ),
            SessionEntry::Compaction(CompactionEntry {
                id: "cmp1".into(),
                parent_id: Some("a_cur".into()),
                timestamp: format_iso_timestamp(3300),
                summary: "已总结磁盘清理方案".into(),
                first_kept_entry_id: "a_cur".into(),
                first_kept_entry_index: None,
                tokens_before: 1000,
                details: None,
                usage: None,
                from_hook: None,
                system_message: None,
            }),
        ];

        let indexed = build_indexed_session(
            "sess-1",
            &format_iso_timestamp(1000),
            None,
            Some("cmp1"),
            &entries,
            3300,
        );
        assert_eq!(indexed.title, "C 盘空间与硬盘信息");
        // Thinking and BitLocker must never appear in indexed chunks
        assert!(
            !indexed
                .chunks
                .iter()
                .any(|c| c.text.contains("SECRET_THINKING"))
        );
        assert!(
            !indexed
                .chunks
                .iter()
                .any(|c| c.text.contains("HIDDEN_THOUGHT"))
        );
        assert!(
            !indexed
                .chunks
                .iter()
                .any(|c| c.text.contains("111111-222222"))
        );

        // Tool output and compaction summary are indexed on active branch
        assert!(indexed.chunks.iter().any(|c| c.entry_id == "t1"
            && c.kind == SearchContentKind::Tool
            && c.on_active_branch));
        assert!(indexed.chunks.iter().any(|c| c.entry_id == "cmp1"
            && c.kind == SearchContentKind::Summary
            && c.on_active_branch));

        // Off-branch hit resolves its newest descendant leaf (`u_old_leaf`)
        let old = indexed
            .chunks
            .iter()
            .find(|c| c.entry_id == "a_old")
            .expect("off-branch assistant indexed");
        assert!(!old.on_active_branch);
        assert_eq!(old.branch_leaf_id.as_deref(), Some("u_old_leaf"));
    }

    #[test]
    fn pins_active_session_and_supports_snapshot_pagination_of_10() {
        let tmp = std::env::temp_dir().join(format!("cst-search-idx-{}", uuid_v7()));
        let idx = SessionSearchIndex::new(tmp.join("sessions"), tmp.join("inbox"));
        {
            let mut guard = idx.state.write().unwrap();
            for i in 0..14u64 {
                let sid = format!("s-{i:02}");
                guard.sessions.insert(
                    sid.clone(),
                    Arc::new(IndexedSession {
                        id: sid,
                        title: format!("磁盘缓存清理案例 {i:02}"),
                        is_placeholder_title: false,
                        updated_at: Some(format_iso_timestamp(10_000 + i * 1000)),
                        updated_at_ms: 10_000 + i * 1000,
                        path: None,
                        mtime_ns: 0,
                        file_size: 0,
                        leaf_id: Some("e1".into()),
                        chunks: vec![IndexedChunk {
                            entry_id: "e1".into(),
                            kind: SearchContentKind::Assistant,
                            timestamp_ms: 10_000 + i * 1000,
                            on_active_branch: true,
                            branch_leaf_id: None,
                            text: format!("检测到本地缓存目录占用空间 {i} GB"),
                        }],
                    }),
                );
            }
            guard.synced_initial = true;
        }

        // Active session s-01 (older timestamp) must pin to index 0
        let page1 = idx.search(SearchQueryParams {
            query: "huancun",
            active_session_id: Some("s-01"),
            title_offset: 0,
            title_limit: 10,
            content_offset: 0,
            content_limit: 10,
            snapshot_token: None,
        });
        assert_eq!(page1.title_total, 14);
        assert_eq!(page1.title_matches.len(), 10);
        assert!(page1.title_has_more);
        assert_eq!(page1.title_matches[0].session_id, "s-01");
        assert_eq!(page1.content_total, 14);
        assert_eq!(page1.content_matches.len(), 10);
        assert!(page1.content_has_more);
        assert_eq!(page1.content_matches[0].session_id, "s-01");

        // Second page with snapshotToken returns remaining 4 items with zero duplicates
        let page2 = idx.search(SearchQueryParams {
            query: "huancun",
            active_session_id: Some("s-01"),
            title_offset: 10,
            title_limit: 10,
            content_offset: 10,
            content_limit: 10,
            snapshot_token: Some(&page1.snapshot_token),
        });
        assert_eq!(page2.title_matches.len(), 4);
        assert!(!page2.title_has_more);
        assert_eq!(page2.content_matches.len(), 4);
        assert!(!page2.content_has_more);
        let p1_ids: HashSet<_> = page1
            .content_matches
            .iter()
            .map(|m| m.session_id.as_str())
            .collect();
        for item in &page2.content_matches {
            assert!(!p1_ids.contains(item.session_id.as_str()));
        }
        idx.remove("s-01");
        let after_delete = idx.search(SearchQueryParams {
            active_session_id: Some("s-01"),
            snapshot_token: Some(&page1.snapshot_token),
            title_offset: 10,
            title_limit: 1,
            content_offset: 10,
            content_limit: 1,
            ..SearchQueryParams::new("huancun")
        });
        assert_ne!(after_delete.snapshot_token, page2.snapshot_token);
        assert_eq!(after_delete.title_total, 13);
        assert_eq!(after_delete.content_total, 13);
        assert_eq!(after_delete.title_matches.len(), 10);
        assert_eq!(after_delete.content_matches.len(), 10);
        assert_eq!(after_delete.title_matches[0].session_id, "s-13");
        assert!(
            !after_delete
                .content_matches
                .iter()
                .any(|item| item.session_id == "s-01")
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn startup_scan_excludes_duplicate_files_and_reads_current_unflushed_metadata() {
        let tmp = std::env::temp_dir().join(format!("cst-search-idx-{}", uuid_v7()));
        let sessions = tmp.join("sessions");
        let inbox = tmp.join("inbox");
        let index_store = SessionIndexStore::new(sessions.clone(), inbox.clone());
        let index = SessionSearchIndex::new(sessions.clone(), inbox);
        let mut manager = SessionManager::create("/search-index", sessions.clone(), None).unwrap();
        let duplicated = manager.get_session_id().to_owned();
        manager
            .append_message(AgentMessage::User(UserMessage::new("duplicated entry")))
            .unwrap();
        let path = manager.get_session_file().unwrap().to_path_buf();
        drop(manager);
        let other = sessions.join("another");
        fs::create_dir_all(&other).unwrap();
        fs::copy(path, other.join("copied.jsonl")).unwrap();

        let mut unflushed = UnflushedSessionSnapshot {
            id: "draft".to_owned(),
            created_at: format_iso_timestamp(1000),
            cwd: "/search-index".to_owned(),
            session_dir: sessions.to_string_lossy().into_owned(),
            parent_session: None,
            name: Some("outdated title".to_owned()),
            model: None,
            thinking_level: None,
        };
        index.state.write().unwrap().sessions.insert(
            unflushed.id.clone(),
            Arc::new(indexed_from_unflushed(&unflushed)),
        );
        unflushed.name = Some("renamed draft".to_owned());
        index_store.write_unflushed_sync(&unflushed).unwrap();
        index.sync_all(&index_store);

        let guard = index.state.read().unwrap();
        assert!(!guard.sessions.contains_key(&duplicated));
        assert_eq!(guard.sessions["draft"].title, "renamed draft");
        drop(guard);
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn startup_merge_cannot_undo_a_live_write_or_tombstone() {
        fn item(id: &str, name: &str) -> IndexedSession {
            build_indexed_session(id, "", Some(name), None, &[], 0)
        }
        let tmp = std::env::temp_dir().join(format!("cst-search-idx-{}", uuid_v7()));
        let idx = SessionSearchIndex::new(tmp.join("sessions"), tmp.join("inbox"));
        let scanned = BTreeMap::from([
            ("renamed".to_owned(), Arc::new(item("renamed", "old title"))),
            (
                "removed".to_owned(),
                Arc::new(item("removed", "deleted title")),
            ),
        ]);
        idx.upsert(item("renamed", "new title"));
        idx.upsert(item("created", "new session"));
        idx.remove("removed");
        idx.upsert(item("removed", "late callback"));
        idx.finish_startup_sync(scanned);

        let guard = idx.state.read().unwrap();
        assert!(guard.synced_initial);
        assert_eq!(guard.sessions["renamed"].title, "new title");
        assert!(guard.sessions.contains_key("created"));
        assert!(!guard.sessions.contains_key("removed"));
    }
}

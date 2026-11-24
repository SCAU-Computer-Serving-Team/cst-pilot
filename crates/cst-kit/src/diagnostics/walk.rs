use super::scan::{
    Ranked, STALE_AGE_SECS, STALE_MIN_BYTES, TopN, ext_key, format_modified_utc, gib,
    hardlink_rank_key, pct,
};
use super::volume_index::{display_path, path_key};
use super::win_native::{Control, rounded};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_GLOBAL_WALK_WORKERS: usize = 8;
const MAX_PER_SCAN_WORKERS: usize = 6;
const MAX_DIR_DEPTH: usize = 1024;
const MAX_DENIED_PATHS: usize = 50;
const MAX_EXTENSION_KEYS: usize = 65_536;
const TOP_FILES_LIMIT: usize = 100;
const EXT_AGG_MAX: usize = 40;
/// Leave room for deterministic aggregation and JSON serialization before the outer hard timeout.
const SOFT_BUDGET_MARGIN: Duration = Duration::from_millis(250);

const FILE_ATTRIBUTE_DIRECTORY_BITS: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_REPARSE_POINT_BITS: u32 = 0x0000_0400;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
const FILE_ATTRIBUTE_CLOUD_RECALL: u32 =
    FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

static ACTIVE_WALK_WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
pub struct WalkLimits {
    pub entries: usize,
    pub timeout: Duration,
}
impl Default for WalkLimits {
    fn default() -> Self {
        Self {
            entries: 500_000,
            timeout: Duration::from_secs(30),
        }
    }
}

pub struct WalkBudget {
    pub scanned: usize,
    pub incomplete: bool,
    pub exhausted: bool,
    pub denied: Vec<String>,
    start: Instant,
    limits: WalkLimits,
}
impl WalkBudget {
    pub fn new(limits: WalkLimits) -> Self {
        Self {
            scanned: 0,
            incomplete: false,
            exhausted: false,
            denied: Vec::new(),
            start: Instant::now(),
            limits,
        }
    }
    pub fn take(&mut self, control: &Control) -> Result<bool, String> {
        if control.cancel.is_cancelled() {
            control.check()?;
        }
        if self.exhausted
            || self.scanned >= self.limits.entries
            || self.start.elapsed() >= self.limits.timeout
            || control.remaining().is_zero()
        {
            self.exhausted = true;
            self.incomplete = true;
            return Ok(false);
        }
        self.scanned += 1;
        Ok(true)
    }
    pub fn failed(&mut self, path: &Path) {
        self.incomplete = true;
        if self.denied.len() < MAX_DENIED_PATHS {
            self.denied.push(display_path(path));
        }
    }
    fn remaining_entries(&self) -> usize {
        self.limits.entries.saturating_sub(self.scanned)
    }
    fn remaining_timeout(&self) -> Duration {
        self.limits.timeout.saturating_sub(self.start.elapsed())
    }
}

/// Identifies Microsoft Cloud Files / OneDrive reparse tags (`IO_REPARSE_TAG_CLOUD*` and
/// `IO_REPARSE_TAG_ONEDRIVE`).
pub(crate) fn is_cloud_reparse_tag(tag: u32) -> bool {
    (tag & 0xFFFF_0FFF) == 0x9000_001A || tag == 0x8000_0021
}

pub fn linked(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & (FILE_ATTRIBUTE_REPARSE_POINT_BITS | FILE_ATTRIBUTE_CLOUD_RECALL)
            != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WinEntryClass {
    Dir,
    File(u64),
    ReparseOrCloudSkipped,
}

/// Classifies a Windows directory entry consistently across `FileIdBothDirectoryInfo` and
/// `FindFirstFileExW`, distinguishing cloud placeholders, hydrated cloud files, and symlinks/junctions.
fn classify_win_entry(
    attrs: u32,
    reparse_tag: u32,
    logical: u64,
    allocation: Option<u64>,
) -> WinEntryClass {
    let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY_BITS != 0;
    let is_reparse = attrs & FILE_ATTRIBUTE_REPARSE_POINT_BITS != 0;
    let is_cloud_recall = attrs & FILE_ATTRIBUTE_CLOUD_RECALL != 0;
    if is_dir {
        if is_reparse || is_cloud_recall {
            return WinEntryClass::ReparseOrCloudSkipped;
        }
        return WinEntryClass::Dir;
    }
    if is_cloud_recall {
        // Cloud-only or partially-hydrated file: only report size when AllocationSize is known;
        // fallback paths without AllocationSize cannot confirm local bytes -> mark unknown/incomplete.
        return match allocation {
            Some(alloc) => WinEntryClass::File(logical.min(alloc)),
            None => WinEntryClass::ReparseOrCloudSkipped,
        };
    }
    if is_reparse {
        // Fully hydrated OneDrive/Cloud Files entries retain their cloud reparse tag without
        // recall bits; treat those as normal local files, while skipping symlinks/AppExecLinks.
        if is_cloud_reparse_tag(reparse_tag) {
            return WinEntryClass::File(logical);
        }
        return WinEntryClass::ReparseOrCloudSkipped;
    }
    WinEntryClass::File(logical)
}

/// Direct child metadata returned by a single directory-enumeration pass (used by `ls`).
#[derive(Debug, Clone)]
pub struct DirectChild {
    pub name: String,
    pub path: PathBuf,
    pub kind: DirectChildKind,
    pub bytes: Option<u64>,
    /// 64-bit NTFS FileId (48-bit record + 16-bit sequence) when available from Windows.
    pub file_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectChildKind {
    Dir,
    File,
    ReparseOrCloud,
    Unknown,
}

/// Result of enumerating direct children under a shared call-level budget.
pub struct DirectChildrenPage {
    pub entries: Vec<Result<DirectChild, ()>>,
    pub truncated: bool,
    pub had_error: bool,
}

/// Enumerates direct children of `dir` under the caller's single [`WalkBudget`] and [`Control`].
/// On Windows uses `FileIdBothDirectoryInfo` (single syscall per 64 KiB buffer, capturing size +
/// attributes + reparse tag + 64-bit `FileId`) and falls back to `FindFirstFileExW` only when the
/// info class is unsupported.
pub fn list_direct_children(
    dir: &Path,
    budget: &mut WalkBudget,
    control: &Control,
) -> Result<DirectChildrenPage, String> {
    control.check()?;
    #[cfg(windows)]
    {
        win_walk::list_direct_children_win(dir, budget, control)
    }
    #[cfg(not(windows))]
    {
        let entries = fs::read_dir(dir).map_err(|_| "无法列出目标目录。".to_owned())?;
        let mut out = Vec::new();
        let mut truncated = false;
        let mut had_error = false;
        for item in entries {
            if !budget.take(control)? {
                truncated = true;
                break;
            }
            let Ok(entry) = item else {
                had_error = true;
                out.push(Err(()));
                continue;
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let (kind, bytes) = match fs::symlink_metadata(&path) {
                Ok(meta) if linked(&meta) => (DirectChildKind::ReparseOrCloud, None),
                Ok(meta) if meta.is_dir() => (DirectChildKind::Dir, None),
                Ok(meta) if meta.is_file() => (DirectChildKind::File, Some(meta.len())),
                Ok(_) | Err(_) => (DirectChildKind::Unknown, None),
            };
            out.push(Ok(DirectChild {
                name,
                path,
                kind,
                bytes,
                file_id: None,
            }));
        }
        Ok(DirectChildrenPage {
            entries: out,
            truncated,
            had_error,
        })
    }
}

/// Safely parses a `FILE_ID_BOTH_DIR_INFO` byte buffer without reading past the variable-length
/// header (`104` bytes) or creating unaligned `u16` slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedDirEntry {
    pub name_u16: Vec<u16>,
    pub attrs: u32,
    pub end_of_file: i64,
    pub allocation_size: i64,
    pub last_write_time: i64,
    pub ea_size: u32,
    pub file_id: u64,
}

const FILE_ID_BOTH_HEADER_LEN: usize = 104;

pub(crate) fn parse_file_id_both_dir_buffer(buf: &[u8]) -> (Vec<ParsedDirEntry>, bool) {
    let mut entries = Vec::new();
    let mut off = 0usize;
    let mut corrupted = false;
    while off < buf.len() {
        let Some(header_end) = off.checked_add(FILE_ID_BOTH_HEADER_LEN) else {
            corrupted = true;
            break;
        };
        if header_end > buf.len() {
            corrupted = true;
            break;
        }
        let h = &buf[off..header_end];
        let next_entry_offset = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
        let last_write_time =
            i64::from_le_bytes([h[24], h[25], h[26], h[27], h[28], h[29], h[30], h[31]]);
        let end_of_file =
            i64::from_le_bytes([h[40], h[41], h[42], h[43], h[44], h[45], h[46], h[47]]);
        let allocation_size =
            i64::from_le_bytes([h[48], h[49], h[50], h[51], h[52], h[53], h[54], h[55]]);
        let attrs = u32::from_le_bytes([h[56], h[57], h[58], h[59]]);
        let name_len = u32::from_le_bytes([h[60], h[61], h[62], h[63]]) as usize;
        let ea_size = u32::from_le_bytes([h[64], h[65], h[66], h[67]]);
        let file_id =
            u64::from_le_bytes([h[96], h[97], h[98], h[99], h[100], h[101], h[102], h[103]]);
        if !name_len.is_multiple_of(2) {
            corrupted = true;
            break;
        }
        let Some(name_end) = header_end.checked_add(name_len) else {
            corrupted = true;
            break;
        };
        if name_end > buf.len() {
            corrupted = true;
            break;
        }
        let name_u16: Vec<u16> = buf[header_end..name_end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        entries.push(ParsedDirEntry {
            name_u16,
            attrs,
            end_of_file,
            allocation_size,
            last_write_time,
            ea_size,
            file_id,
        });
        if next_entry_offset == 0 {
            break;
        }
        if next_entry_offset < FILE_ID_BOTH_HEADER_LEN || !next_entry_offset.is_multiple_of(8) {
            corrupted = true;
            break;
        }
        let Some(next) = off.checked_add(next_entry_offset) else {
            corrupted = true;
            break;
        };
        if next < name_end || next > buf.len() {
            corrupted = true;
            break;
        }
        off = next;
    }
    (entries, corrupted)
}

/// Unified global worker permit tracker that accounts for both the baseline worker thread (always
/// granted to prevent deadlocks) and bounded extra parallel workers.
struct WorkerPermits {
    total: usize,
}
impl WorkerPermits {
    fn acquire(desired: usize) -> Self {
        let cap = desired.clamp(1, MAX_PER_SCAN_WORKERS);
        // Always reserve 1 baseline worker and count it in the global pool so concurrent scans
        // know the true active worker count.
        ACTIVE_WALK_WORKERS.fetch_add(1, Ordering::AcqRel);
        let mut acquired = 1usize;
        while acquired < cap {
            let cur = ACTIVE_WALK_WORKERS.load(Ordering::Relaxed);
            if cur >= MAX_GLOBAL_WALK_WORKERS {
                break;
            }
            if ACTIVE_WALK_WORKERS
                .compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                acquired += 1;
            }
        }
        Self { total: acquired }
    }
    fn total_threads(&self) -> usize {
        self.total
    }
}
impl Drop for WorkerPermits {
    fn drop(&mut self) {
        ACTIVE_WALK_WORKERS.fetch_sub(self.total, Ordering::AcqRel);
    }
}

struct DirRecord {
    parent: Option<usize>,
    path: PathBuf,
    depth: usize,
    root_slot: Option<usize>,
}

#[derive(Clone)]
struct FileRecord {
    file_id: u64,
    dir_id: usize,
    bytes: u64,
    modified_unix: i64,
    name: String,
}

/// Direct file seed passed from `ls` so direct files and sibling subtrees share one deterministic
/// hardlink deduplication scope.
#[derive(Clone)]
pub struct DirectFileSeed {
    pub file_id: u64,
    pub bytes: u64,
    pub name: String,
}

struct WalkQueue {
    state: Mutex<(Vec<usize>, usize, bool)>, // (pending_dir_ids, active_workers, stop)
    cv: Condvar,
}
impl WalkQueue {
    fn new(initial_ids: Vec<usize>) -> Self {
        Self {
            state: Mutex::new((initial_ids, 0, false)),
            cv: Condvar::new(),
        }
    }
    fn pop(&self) -> Option<usize> {
        let mut guard = self.state.lock().ok()?;
        loop {
            if guard.2 {
                return None;
            }
            if let Some(id) = guard.0.pop() {
                guard.1 += 1;
                return Some(id);
            }
            if guard.1 == 0 {
                return None;
            }
            guard = self.cv.wait(guard).ok()?;
        }
    }
    fn push_batch(&self, ids: &[usize]) {
        if ids.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.state.lock() {
            if guard.2 {
                return;
            }
            guard.0.extend_from_slice(ids);
            if guard.0.len() == 1 {
                self.cv.notify_one();
            } else {
                self.cv.notify_all();
            }
        }
    }
    fn done_one(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.1 = guard.1.saturating_sub(1);
            if guard.1 == 0 && (guard.0.is_empty() || guard.2) {
                self.cv.notify_all();
            }
        }
    }
    fn stop(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.2 = true;
            guard.0.clear();
            self.cv.notify_all();
        }
    }
}

/// RAII guard guaranteeing `done_one()` runs even if a worker panics, and halting the queue on unwind.
struct ActiveDirGuard<'a> {
    queue: &'a WalkQueue,
    panicked: &'a AtomicBool,
    completed: bool,
}
impl Drop for ActiveDirGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.panicked.store(true, Ordering::Release);
            self.queue.stop();
        }
        self.queue.done_one();
    }
}

struct WalkOutcome {
    total: u64,
    root_totals: Vec<Option<u64>>,
    scanned: usize,
    incomplete: bool,
    exhausted: bool,
    ext_overflow: bool,
    denied: Vec<String>,
    top_dirs: Vec<Ranked>,
    top_files: Vec<Ranked>,
    stale_files: Vec<Ranked>,
    extensions: HashMap<String, (u64, u64)>,
}

struct SharedState {
    dirs: Mutex<Vec<DirRecord>>,
    denied: Mutex<Vec<String>>,
    root_unreadable: Vec<AtomicBool>,
    scanned: AtomicUsize,
    incomplete: AtomicBool,
    exhausted: AtomicBool,
    cancelled: AtomicBool,
    worker_panicked: AtomicBool,
    entry_limit: usize,
    deadline: Instant,
    collect_analytics: bool,
    dedup_hardlinks: bool,
    expected_vol_serial: Option<u32>,
}
impl SharedState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        initial_dirs: Vec<DirRecord>,
        root_slots: usize,
        entry_limit: usize,
        deadline: Instant,
        collect_analytics: bool,
        dedup_hardlinks: bool,
        expected_vol_serial: Option<u32>,
    ) -> Self {
        let root_unreadable = (0..root_slots).map(|_| AtomicBool::new(false)).collect();
        Self {
            dirs: Mutex::new(initial_dirs),
            denied: Mutex::new(Vec::new()),
            root_unreadable,
            scanned: AtomicUsize::new(0),
            incomplete: AtomicBool::new(false),
            exhausted: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            worker_panicked: AtomicBool::new(false),
            entry_limit,
            deadline,
            collect_analytics,
            dedup_hardlinks,
            expected_vol_serial,
        }
    }
    fn take_entry(&self, control: &Control) -> bool {
        if control.cancel.is_cancelled() {
            self.cancelled.store(true, Ordering::Release);
            return false;
        }
        if self.exhausted.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            self.exhausted.store(true, Ordering::Release);
            self.incomplete.store(true, Ordering::Release);
            return false;
        }
        let prev = self.scanned.fetch_add(1, Ordering::Relaxed);
        if prev >= self.entry_limit {
            self.scanned.fetch_sub(1, Ordering::Relaxed);
            self.exhausted.store(true, Ordering::Release);
            self.incomplete.store(true, Ordering::Release);
            return false;
        }
        true
    }
    fn mark_incomplete(&self) {
        self.incomplete.store(true, Ordering::Release);
    }
    fn mark_root_unreadable(&self, root_slot: Option<usize>) {
        self.mark_incomplete();
        if let Some(slot) = root_slot
            && let Some(flag) = self.root_unreadable.get(slot)
        {
            flag.store(true, Ordering::Release);
        }
    }
    fn record_denied(&self, path: &Path) {
        self.mark_incomplete();
        if let Ok(mut list) = self.denied.lock()
            && list.len() < MAX_DENIED_PATHS
        {
            list.push(display_path(path));
        }
    }
    fn add_subdirs(&self, parent: usize, depth: usize, paths: Vec<PathBuf>) -> Vec<usize> {
        if paths.is_empty() {
            return Vec::new();
        }
        let Ok(mut dirs) = self.dirs.lock() else {
            return Vec::new();
        };
        let mut ids = Vec::with_capacity(paths.len());
        for path in paths {
            let id = dirs.len();
            dirs.push(DirRecord {
                parent: Some(parent),
                path,
                depth,
                root_slot: None,
            });
            ids.push(id);
        }
        ids
    }
}

fn should_dedup_hardlinks(root: &Path) -> bool {
    #[cfg(windows)]
    {
        win_walk::is_local_ntfs_volume(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        false
    }
}

fn volume_serial_of(root: &Path) -> Option<u32> {
    #[cfg(windows)]
    {
        win_walk::query_dir_volume_serial(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_forest(
    scope_root: &Path,
    initial_dirs: Vec<DirRecord>,
    initial_queue: Vec<usize>,
    seed_files: Vec<FileRecord>,
    root_slots: usize,
    entry_limit: usize,
    timeout: Duration,
    control: &Control,
    collect_analytics: bool,
    top_dirs_limit: usize,
) -> Result<WalkOutcome, String> {
    control.check()?;
    let effective_timeout = {
        let outer = control.remaining();
        let soft_outer = if outer > SOFT_BUDGET_MARGIN {
            outer - SOFT_BUDGET_MARGIN
        } else {
            outer
        };
        timeout.min(soft_outer)
    };
    if entry_limit == 0 || effective_timeout.is_zero() {
        return Ok(WalkOutcome {
            total: 0,
            root_totals: vec![None; root_slots],
            scanned: 0,
            incomplete: true,
            exhausted: true,
            ext_overflow: false,
            denied: Vec::new(),
            top_dirs: Vec::new(),
            top_files: Vec::new(),
            stale_files: Vec::new(),
            extensions: HashMap::new(),
        });
    }
    let deadline = Instant::now() + effective_timeout;
    let dedup = should_dedup_hardlinks(scope_root);
    let expected_vol_serial = volume_serial_of(scope_root);
    let desired = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(MAX_PER_SCAN_WORKERS);
    let permits = WorkerPermits::acquire(desired);
    let workers_n = permits.total_threads();

    let shared = SharedState::new(
        initial_dirs,
        root_slots,
        entry_limit,
        deadline,
        collect_analytics,
        dedup,
        expected_vol_serial,
    );
    let queue = WalkQueue::new(initial_queue);

    #[cfg(windows)]
    let cancel_handles = win_walk::CancelRegistry::new();

    let all_files: Mutex<Vec<FileRecord>> = Mutex::new(seed_files);
    let done_flag = AtomicBool::new(false);

    let any_worker_panicked = std::thread::scope(|scope| {
        // Watchdog stops the queue and repeatedly issues CancelSynchronousIo until every worker
        // exits, closing the check-to-syscall race window.
        let shared_ref = &shared;
        let queue_ref = &queue;
        let done_ref = &done_flag;
        #[cfg(windows)]
        let cancel_ref = &cancel_handles;
        let watcher = scope.spawn(move || {
            let mut cancelling = false;
            while !done_ref.load(Ordering::Acquire) {
                if !cancelling {
                    if control.cancel.is_cancelled() {
                        shared_ref.cancelled.store(true, Ordering::Release);
                        queue_ref.stop();
                        cancelling = true;
                    } else if Instant::now() >= shared_ref.deadline {
                        shared_ref.exhausted.store(true, Ordering::Release);
                        shared_ref.incomplete.store(true, Ordering::Release);
                        queue_ref.stop();
                        cancelling = true;
                    }
                }
                if cancelling {
                    #[cfg(windows)]
                    cancel_ref.cancel_all();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        let mut handles = Vec::with_capacity(workers_n);
        for _ in 0..workers_n {
            let shared_ref = &shared;
            let queue_ref = &queue;
            let all_files_ref = &all_files;
            #[cfg(windows)]
            let cancel_ref = &cancel_handles;
            handles.push(scope.spawn(move || {
                #[cfg(windows)]
                let _cancel_guard = cancel_ref.register_current_thread();
                #[cfg(windows)]
                let mut dir_buf = vec![0u64; 8192]; // 64 KiB, 8-byte aligned
                let mut local_files: Vec<FileRecord> = Vec::new();
                while let Some(dir_id) = queue_ref.pop() {
                    let mut active_guard = ActiveDirGuard {
                        queue: queue_ref,
                        panicked: &shared_ref.worker_panicked,
                        completed: false,
                    };
                    if shared_ref.cancelled.load(Ordering::Acquire)
                        || shared_ref.exhausted.load(Ordering::Acquire)
                    {
                        active_guard.completed = true;
                        break;
                    }
                    let (dir_path, depth, root_slot) = {
                        let dirs = shared_ref.dirs.lock().expect("dirs mutex");
                        (
                            dirs[dir_id].path.clone(),
                            dirs[dir_id].depth,
                            dirs[dir_id].root_slot,
                        )
                    };
                    #[cfg(windows)]
                    let subdirs = win_walk::scan_dir_win(
                        dir_id,
                        &dir_path,
                        depth,
                        root_slot,
                        &mut dir_buf,
                        shared_ref,
                        control,
                        &mut local_files,
                    );
                    #[cfg(not(windows))]
                    let subdirs = scan_dir_portable(
                        dir_id,
                        &dir_path,
                        depth,
                        root_slot,
                        shared_ref,
                        control,
                        &mut local_files,
                    );
                    let child_ids = shared_ref.add_subdirs(dir_id, depth + 1, subdirs);
                    queue_ref.push_batch(&child_ids);
                    active_guard.completed = true;
                }
                if !local_files.is_empty()
                    && let Ok(mut out) = all_files_ref.lock()
                {
                    out.extend(local_files);
                }
            }));
        }
        let mut panicked = false;
        for h in handles {
            if h.join().is_err() {
                panicked = true;
            }
        }
        done_flag.store(true, Ordering::Release);
        let _ = watcher.join();
        panicked
    });

    if any_worker_panicked || shared.worker_panicked.load(Ordering::Acquire) {
        return Err("目录遍历工作线程异常退出。".into());
    }
    if shared.cancelled.load(Ordering::Acquire) || control.cancel.is_cancelled() {
        control.check()?;
        return Err("诊断已取消。".into());
    }

    let dirs = shared.dirs.into_inner().map_err(|_| "目录遍历状态异常。")?;
    let files = all_files.into_inner().map_err(|_| "文件汇总状态异常。")?;
    let denied = shared
        .denied
        .into_inner()
        .map_err(|_| "权限拒绝记录状态异常。")?;
    let scanned = shared.scanned.load(Ordering::Relaxed);
    let incomplete = shared.incomplete.load(Ordering::Relaxed);
    let exhausted = shared.exhausted.load(Ordering::Relaxed);
    let root_unreadable: Vec<bool> = shared
        .root_unreadable
        .iter()
        .map(|f| f.load(Ordering::Acquire))
        .collect();

    aggregate_deterministic(
        &dirs,
        &root_unreadable,
        files,
        denied,
        scanned,
        incomplete,
        exhausted,
        shared.collect_analytics,
        shared.dedup_hardlinks,
        top_dirs_limit,
        control,
    )
}

#[allow(clippy::too_many_arguments)]
fn aggregate_deterministic(
    dirs: &[DirRecord],
    root_unreadable: &[bool],
    files: Vec<FileRecord>,
    denied: Vec<String>,
    scanned: usize,
    incomplete: bool,
    exhausted: bool,
    collect_analytics: bool,
    dedup_hardlinks: bool,
    top_dirs_limit: usize,
    control: &Control,
) -> Result<WalkOutcome, String> {
    let dir_count = dirs.len();
    let dir_keys: Vec<String> = if dedup_hardlinks {
        dirs.iter().map(|d| path_key(&d.path)).collect()
    } else {
        Vec::new()
    };

    // Deterministic representative selection for duplicate FileIds within visible scan scope,
    // using the exact same comparator shared with `mft.rs`.
    let winners: Vec<FileRecord> = if dedup_hardlinks {
        let mut by_id: HashMap<u64, FileRecord> = HashMap::new();
        let mut no_id: Vec<FileRecord> = Vec::new();
        for (idx, file) in files.into_iter().enumerate() {
            if idx & 0x3FFF == 0 {
                control.check()?;
            }
            if file.file_id == 0 {
                no_id.push(file);
                continue;
            }
            match by_id.entry(file.file_id) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(file);
                }
                std::collections::hash_map::Entry::Occupied(mut occ) => {
                    let existing = occ.get();
                    if hardlink_rank_key(&dir_keys[file.dir_id], &file.name)
                        < hardlink_rank_key(&dir_keys[existing.dir_id], &existing.name)
                    {
                        *occ.get_mut() = file;
                    }
                }
            }
        }
        let mut merged = Vec::with_capacity(by_id.len() + no_id.len());
        merged.extend(by_id.into_values());
        merged.extend(no_id);
        merged
    } else {
        files
    };

    let mut dir_totals = vec![0u64; dir_count];
    let mut top_files_heap = TopN::new(if collect_analytics {
        TOP_FILES_LIMIT
    } else {
        0
    });
    let mut stale_heap = TopN::new(if collect_analytics {
        TOP_FILES_LIMIT
    } else {
        0
    });
    let mut extensions: HashMap<String, (u64, u64)> = HashMap::new();
    let mut ext_overflow = false;
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let stale_cutoff = now_unix.saturating_sub(STALE_AGE_SECS);

    for (idx, file) in winners.iter().enumerate() {
        if idx & 0x3FFF == 0 {
            control.check()?;
        }
        if file.dir_id < dir_count {
            dir_totals[file.dir_id] = dir_totals[file.dir_id].saturating_add(file.bytes);
        }
        if collect_analytics {
            let ext = ext_key(&file.name);
            if extensions.contains_key(&ext) || extensions.len() < MAX_EXTENSION_KEYS {
                let agg = extensions.entry(ext).or_default();
                agg.0 = agg.0.saturating_add(file.bytes);
                agg.1 = agg.1.saturating_add(1);
            } else {
                ext_overflow = true;
            }
            let make_path = || display_path(&dirs[file.dir_id].path.join(&file.name));
            if top_files_heap.would_accept(file.bytes) {
                top_files_heap.add(Ranked {
                    bytes: file.bytes,
                    path: make_path(),
                    modified: format_modified_utc(file.modified_unix),
                });
            }
            if file.bytes >= STALE_MIN_BYTES
                && file.modified_unix > 0
                && file.modified_unix <= stale_cutoff
                && stale_heap.would_accept(file.bytes)
            {
                stale_heap.add(Ranked {
                    bytes: file.bytes,
                    path: make_path(),
                    modified: format_modified_utc(file.modified_unix),
                });
            }
        }
    }

    // Bottom-up roll-up sorted by descending depth (exact parent sums regardless of parallel visit order).
    let mut order: Vec<usize> = (0..dir_count).collect();
    order.sort_unstable_by(|&a, &b| dirs[b].depth.cmp(&dirs[a].depth));
    for id in order {
        if let Some(parent) = dirs[id].parent {
            dir_totals[parent] = dir_totals[parent].saturating_add(dir_totals[id]);
        }
    }

    let total = dir_totals.first().copied().unwrap_or(0);
    let mut root_totals = vec![None; root_unreadable.len()];
    for (id, rec) in dirs.iter().enumerate() {
        if let Some(slot) = rec.root_slot
            && slot < root_totals.len()
        {
            root_totals[slot] = (!root_unreadable[slot]).then_some(dir_totals[id]);
        }
    }

    let mut top_dirs_heap = TopN::new(top_dirs_limit);
    if top_dirs_limit > 0 {
        for (id, &bytes) in dir_totals.iter().enumerate() {
            if top_dirs_heap.would_accept(bytes) {
                top_dirs_heap.add(Ranked {
                    bytes,
                    path: display_path(&dirs[id].path),
                    modified: None,
                });
            }
        }
    }

    Ok(WalkOutcome {
        total,
        root_totals,
        scanned,
        incomplete,
        exhausted,
        ext_overflow,
        denied,
        top_dirs: top_dirs_heap.sorted(),
        top_files: top_files_heap.sorted(),
        stale_files: stale_heap.sorted(),
        extensions,
    })
}

#[cfg(not(windows))]
fn scan_dir_portable(
    dir_id: usize,
    dir_path: &Path,
    depth: usize,
    root_slot: Option<usize>,
    shared: &SharedState,
    control: &Control,
    local_files: &mut Vec<FileRecord>,
) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir_path) {
        Ok(e) => e,
        Err(err) => {
            if root_slot.is_some() {
                shared.mark_root_unreadable(root_slot);
            }
            if err.kind() == std::io::ErrorKind::PermissionDenied {
                shared.record_denied(dir_path);
            } else {
                shared.mark_incomplete();
            }
            return Vec::new();
        }
    };
    let mut subdirs = Vec::new();
    for entry in entries {
        if !shared.take_entry(control) {
            break;
        }
        let Ok(entry) = entry else {
            shared.mark_incomplete();
            continue;
        };
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(err) => {
                if err.kind() == std::io::ErrorKind::PermissionDenied {
                    shared.record_denied(&path);
                } else {
                    shared.mark_incomplete();
                }
                continue;
            }
        };
        if linked(&meta) {
            shared.mark_incomplete();
            continue;
        }
        if meta.is_dir() {
            if depth + 1 >= MAX_DIR_DEPTH {
                shared.mark_incomplete();
            } else {
                subdirs.push(path);
            }
        } else if meta.is_file() {
            let modified_unix = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            let name = if shared.collect_analytics || shared.dedup_hardlinks {
                entry.file_name().to_string_lossy().into_owned()
            } else {
                String::new()
            };
            local_files.push(FileRecord {
                file_id: 0,
                dir_id,
                bytes: meta.len(),
                modified_unix,
                name,
            });
        }
    }
    subdirs
}

/// Measures all direct subdirectories of `parent_root` in ONE parallel walk under a single
/// [`WalkBudget`], sharing deterministic hardlink deduplication across `direct_files` and every
/// sibling subtree. Returns `None` for any direct subdirectory whose root itself could not be
/// opened or was not started due to budget exhaustion.
pub fn measure_ls_subdirs(
    parent_root: &Path,
    direct_files: &[DirectFileSeed],
    subdir_paths: &[PathBuf],
    budget: &mut WalkBudget,
    control: &Control,
) -> Result<Vec<Option<u64>>, String> {
    if subdir_paths.is_empty() {
        return Ok(Vec::new());
    }
    let remaining_entries = budget.remaining_entries();
    let remaining_timeout = budget.remaining_timeout();
    if budget.exhausted || remaining_entries == 0 || remaining_timeout.is_zero() {
        budget.exhausted = true;
        budget.incomplete = true;
        return Ok(vec![None; subdir_paths.len()]);
    }
    // dir_id 0 is `parent_root` (holds direct file seeds for unified dedup);
    // dir_id 1..=subdir_paths.len() are the direct subdirectories queued for parallel traversal.
    let mut initial_dirs = Vec::with_capacity(1 + subdir_paths.len());
    initial_dirs.push(DirRecord {
        parent: None,
        path: parent_root.to_path_buf(),
        depth: 0,
        root_slot: None,
    });
    let mut initial_queue = Vec::with_capacity(subdir_paths.len());
    for (slot, path) in subdir_paths.iter().enumerate() {
        let id = initial_dirs.len();
        initial_dirs.push(DirRecord {
            parent: Some(0),
            path: path.clone(),
            depth: 1,
            root_slot: Some(slot),
        });
        initial_queue.push(id);
    }
    // Seed direct files into dir_id 0; because `path_key(parent_root)` is a strict prefix of every
    // child directory key, direct files win hardlink ties deterministically and sibling subtrees
    // never double-count them.
    let seed_files: Vec<FileRecord> = direct_files
        .iter()
        .map(|f| FileRecord {
            file_id: f.file_id,
            dir_id: 0,
            bytes: f.bytes,
            modified_unix: 0,
            name: f.name.clone(),
        })
        .collect();

    let outcome = walk_forest(
        parent_root,
        initial_dirs,
        initial_queue,
        seed_files,
        subdir_paths.len(),
        remaining_entries,
        remaining_timeout,
        control,
        false,
        0,
    )?;
    budget.scanned = budget.scanned.saturating_add(outcome.scanned);
    budget.incomplete |= outcome.incomplete;
    budget.exhausted |= outcome.exhausted;
    for path in outcome.denied {
        if budget.denied.len() < MAX_DENIED_PATHS {
            budget.denied.push(path);
        }
    }
    Ok(outcome.root_totals)
}

pub fn size(
    root: &Path,
    budget: &mut WalkBudget,
    control: &Control,
    top: &mut TopN,
) -> Result<u64, String> {
    let remaining_entries = budget.remaining_entries();
    let remaining_timeout = budget.remaining_timeout();
    if budget.exhausted || remaining_entries == 0 || remaining_timeout.is_zero() {
        budget.exhausted = true;
        budget.incomplete = true;
        return Ok(0);
    }
    let initial_dirs = vec![DirRecord {
        parent: None,
        path: root.to_path_buf(),
        depth: 0,
        root_slot: Some(0),
    }];
    let outcome = walk_forest(
        root,
        initial_dirs,
        vec![0],
        Vec::new(),
        1,
        remaining_entries,
        remaining_timeout,
        control,
        false,
        top.limit(),
    )?;
    budget.scanned = budget.scanned.saturating_add(outcome.scanned);
    budget.incomplete |= outcome.incomplete;
    budget.exhausted |= outcome.exhausted;
    for path in outcome.denied {
        if budget.denied.len() < MAX_DENIED_PATHS {
            budget.denied.push(path);
        }
    }
    for row in outcome.top_dirs {
        top.add(row);
    }
    Ok(outcome.total)
}

pub fn usage(
    root: &Path,
    top: usize,
    control: &Control,
    limits: WalkLimits,
) -> Result<Value, String> {
    let meta = fs::symlink_metadata(root).map_err(|_| "目标目录无法读取。")?;
    if linked(&meta) || !meta.is_dir() {
        return Err("目标必须是目录且不能是链接或重解析点。".into());
    }
    let start = Instant::now();
    let initial_dirs = vec![DirRecord {
        parent: None,
        path: root.to_path_buf(),
        depth: 0,
        root_slot: Some(0),
    }];
    let outcome = walk_forest(
        root,
        initial_dirs,
        vec![0],
        Vec::new(),
        1,
        limits.entries,
        limits.timeout,
        control,
        true,
        top + 1,
    )?;
    let root_key = path_key(root);
    let top_dirs = outcome
        .top_dirs
        .into_iter()
        .filter(|r| path_key(Path::new(&r.path)) != root_key)
        .take(top)
        .map(|r| {
            json!({
                "path": r.path,
                "sizeGB": gib(r.bytes),
                "pct": if outcome.incomplete { None } else { pct(r.bytes, outcome.total) }
            })
        })
        .collect::<Vec<_>>();
    // Strictly align with `doc/diagnostics/API.md`: `topFiles` items contain `{ path, sizeGB, modified }`.
    let top_files = outcome
        .top_files
        .into_iter()
        .take(top)
        .map(|r| {
            json!({
                "path": r.path,
                "sizeGB": gib(r.bytes),
                "modified": r.modified
            })
        })
        .collect::<Vec<_>>();
    let mut extensions: Vec<_> = outcome.extensions.into_iter().collect();
    extensions.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
    let ext_agg = extensions
        .into_iter()
        .take(top.min(EXT_AGG_MAX))
        .map(|(ext, (bytes, files))| json!({"ext": ext, "files": files, "sizeGB": gib(bytes)}))
        .collect::<Vec<_>>();
    let stale_files = outcome
        .stale_files
        .into_iter()
        .take(top)
        .map(|r| json!({"path": r.path, "sizeGB": gib(r.bytes), "modified": r.modified}))
        .collect::<Vec<_>>();
    let ext_note = if outcome.ext_overflow {
        "扩展名种类超过 65,536 上限，extAgg 可能为下界。"
    } else {
        ""
    };
    let notice = if outcome.incomplete {
        format!("扫描因预算、链接、云占位或访问限制未完成，大小为下界，占比未计算。{ext_note}")
    } else {
        format!(
            "原生并行目录遍历完成；{ext_note}目录排行包含不同深度，父子目录不可相加，陈旧文件为至少 50 MiB 且一年未修改。"
        )
    };
    Ok(json!({
        "method": "native-walk",
        "root": display_path(root),
        "totalGB": gib(outcome.total),
        "topDirs": top_dirs,
        "topFiles": top_files,
        "extAgg": ext_agg,
        "staleFiles": stale_files,
        "stats": {
            "filesScanned": outcome.scanned,
            "elapsedSec": rounded(start.elapsed().as_secs_f64(), 1),
            "budget": limits.entries
        },
        "truncated": outcome.incomplete,
        "denied": outcome.denied,
        "notice": notice
    }))
}

#[cfg(windows)]
mod win_walk {
    use super::*;
    use std::ffi::OsString;
    use std::mem::offset_of;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED,
        ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL,
        ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FIND_FIRST_EX_FLAGS, FIND_FIRST_EX_LARGE_FETCH, FileIdBothDirectoryInfo,
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        GetDriveTypeW, GetFileInformationByHandle, GetFileInformationByHandleEx,
        GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING, WIN32_FIND_DATAW,
    };
    use windows::Win32::System::IO::CancelSynchronousIo;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};
    use windows::core::PCWSTR;

    // Compile-time verification that `parse_file_id_both_dir_buffer` offsets match Windows SDK.
    const _: () = {
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, NextEntryOffset) == 0);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, LastWriteTime) == 24);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, EndOfFile) == 40);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, AllocationSize) == 48);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileAttributes) == 56);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength) == 60);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, EaSize) == 64);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileId) == 96);
        assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileName) == FILE_ID_BOTH_HEADER_LEN);
    };

    const DRIVE_REMOTE: u32 = 4;
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE && !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    pub(super) struct CancelRegistry {
        threads: Mutex<Vec<usize>>, // raw HANDLE values stored as usize for Send+Sync
    }
    impl CancelRegistry {
        pub(super) fn new() -> Self {
            Self {
                threads: Mutex::new(Vec::new()),
            }
        }
        pub(super) fn register_current_thread(&self) -> ThreadCancelGuard<'_> {
            let mut dup = HANDLE::default();
            let ok = unsafe {
                let proc = GetCurrentProcess();
                DuplicateHandle(
                    proc,
                    GetCurrentThread(),
                    proc,
                    &mut dup,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )
            }
            .is_ok();
            let raw = if ok { dup.0 as usize } else { 0 };
            if raw != 0
                && let Ok(mut list) = self.threads.lock()
            {
                list.push(raw);
            }
            ThreadCancelGuard {
                registry: self,
                raw,
            }
        }
        pub(super) fn cancel_all(&self) {
            if let Ok(list) = self.threads.lock() {
                for &raw in list.iter() {
                    if raw != 0 {
                        // SAFETY: handle remains open while worker holds `ThreadCancelGuard`.
                        let _ = unsafe { CancelSynchronousIo(HANDLE(raw as *mut _)) };
                    }
                }
            }
        }
    }
    pub(super) struct ThreadCancelGuard<'a> {
        registry: &'a CancelRegistry,
        raw: usize,
    }
    impl Drop for ThreadCancelGuard<'_> {
        fn drop(&mut self) {
            if self.raw != 0 {
                if let Ok(mut list) = self.registry.threads.lock() {
                    list.retain(|&h| h != self.raw);
                }
                unsafe {
                    let _ = CloseHandle(HANDLE(self.raw as *mut _));
                }
            }
        }
    }

    pub(super) fn is_local_ntfs_volume(root: &Path) -> bool {
        let disp = display_path(root);
        if disp.starts_with(r"\\") {
            return false;
        }
        let wide_in = to_wide_path(root);
        let mut vol_root = vec![0u16; wide_in.len().max(260) + 16];
        let ok = unsafe { GetVolumePathNameW(PCWSTR(wide_in.as_ptr()), &mut vol_root) };
        if ok.is_err() {
            return false;
        }
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(vol_root.as_ptr())) };
        if drive_type == DRIVE_REMOTE {
            return false;
        }
        let mut fs_name = [0u16; 64];
        let ok = unsafe {
            GetVolumeInformationW(
                PCWSTR(vol_root.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut fs_name),
            )
        };
        if ok.is_err() {
            return false;
        }
        let len = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        String::from_utf16_lossy(&fs_name[..len]).eq_ignore_ascii_case("NTFS")
    }

    pub(super) fn query_dir_volume_serial(dir: &Path) -> Option<u32> {
        let handle = open_dir_handle_raw(dir).ok()?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle.0, &mut info).ok()? };
        (info.dwVolumeSerialNumber != 0).then_some(info.dwVolumeSerialNumber)
    }

    fn to_wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn to_wide_pattern(dir: &Path) -> Vec<u16> {
        const BACKSLASH: u16 = b'\\' as u16;
        const COLON: u16 = b':' as u16;
        let mut w: Vec<u16> = dir.as_os_str().encode_wide().collect();
        if !matches!(w.last().copied(), Some(BACKSLASH | COLON)) {
            w.push(BACKSLASH);
        }
        w.push(b'*' as u16);
        w.push(0);
        w
    }

    fn is_dot_or_dotdot(name: &[u16]) -> bool {
        (name.len() == 1 && name[0] == b'.' as u16)
            || (name.len() == 2 && name[0] == b'.' as u16 && name[1] == b'.' as u16)
    }

    fn filetime_i64_to_unix(ticks: i64) -> i64 {
        if ticks < 0 {
            return 0;
        }
        let u = ticks as u64;
        if u >= EPOCH_DIFF_100NS {
            ((u - EPOCH_DIFF_100NS) / 10_000_000) as i64
        } else {
            0
        }
    }

    fn filetime_parts_to_unix(low: u32, high: u32) -> i64 {
        let ticks = ((high as u64) << 32) | (low as u64);
        if ticks >= EPOCH_DIFF_100NS {
            ((ticks - EPOCH_DIFF_100NS) / 10_000_000) as i64
        } else {
            0
        }
    }

    fn open_dir_handle_raw(dir: &Path) -> Result<OwnedHandle, windows::core::Error> {
        let w = to_wide_path(dir);
        let h = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }?;
        if h == INVALID_HANDLE_VALUE {
            return Err(windows::core::Error::from_thread());
        }
        Ok(OwnedHandle(h))
    }

    enum DirOpenError {
        AccessDenied,
        ReparseOrCrossVolume,
        Other,
    }

    /// Opens a directory with `FILE_FLAG_OPEN_REPARSE_POINT` and immediately re-verifies the
    /// handle's attributes and volume serial so a directory replaced by a junction/symlink or
    /// mount point between queueing and opening is never descended into.
    fn open_dir_handle_checked(
        dir: &Path,
        expected_vol_serial: Option<u32>,
    ) -> Result<OwnedHandle, DirOpenError> {
        let handle = open_dir_handle_raw(dir).map_err(|e| {
            if (e.code().0 as u32 & 0xFFFF) == ERROR_ACCESS_DENIED.0 {
                DirOpenError::AccessDenied
            } else {
                DirOpenError::Other
            }
        })?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle.0, &mut info) }.is_err() {
            return Err(DirOpenError::Other);
        }
        let attrs = info.dwFileAttributes;
        if attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0
            || attrs & (FILE_ATTRIBUTE_REPARSE_POINT.0 | FILE_ATTRIBUTE_CLOUD_RECALL) != 0
        {
            return Err(DirOpenError::ReparseOrCrossVolume);
        }
        if let Some(expected) = expected_vol_serial
            && info.dwVolumeSerialNumber != 0
            && info.dwVolumeSerialNumber != expected
        {
            return Err(DirOpenError::ReparseOrCrossVolume);
        }
        Ok(handle)
    }

    fn is_info_class_unsupported(err: &windows::core::Error) -> bool {
        let code = err.code().0 as u32 & 0xFFFF;
        code == ERROR_INVALID_FUNCTION.0
            || code == ERROR_NOT_SUPPORTED.0
            || code == ERROR_INVALID_PARAMETER.0
            || code == ERROR_INVALID_LEVEL.0
    }

    fn is_findfirst_flag_unsupported(err: &windows::core::Error) -> bool {
        let code = err.code().0 as u32 & 0xFFFF;
        code == ERROR_INVALID_PARAMETER.0
            || code == ERROR_NOT_SUPPORTED.0
            || code == ERROR_INVALID_FUNCTION.0
    }

    fn is_enum_eof(err: &windows::core::Error) -> bool {
        let code = err.code().0 as u32 & 0xFFFF;
        code == ERROR_NO_MORE_FILES.0 || code == ERROR_HANDLE_EOF.0
    }

    fn is_findfirst_empty(err: &windows::core::Error) -> bool {
        let code = err.code().0 as u32 & 0xFFFF;
        code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_NO_MORE_FILES.0
    }

    fn is_access_denied(err: &windows::core::Error) -> bool {
        (err.code().0 as u32 & 0xFFFF) == ERROR_ACCESS_DENIED.0
    }

    pub(super) fn list_direct_children_win(
        dir: &Path,
        budget: &mut WalkBudget,
        control: &Control,
    ) -> Result<DirectChildrenPage, String> {
        control.check()?;
        let cancel_reg = CancelRegistry::new();
        let done_flag = AtomicBool::new(false);
        let deadline = Instant::now() + budget.remaining_timeout().min(control.remaining());

        std::thread::scope(|scope| {
            let cancel_ref = &cancel_reg;
            let done_ref = &done_flag;
            let watcher = scope.spawn(move || {
                let mut cancelling = false;
                while !done_ref.load(Ordering::Acquire) {
                    if !cancelling && (control.cancel.is_cancelled() || Instant::now() >= deadline)
                    {
                        cancelling = true;
                    }
                    if cancelling {
                        cancel_ref.cancel_all();
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let _thread_guard = cancel_reg.register_current_thread();
            let result = list_direct_children_inner(dir, budget, control);
            done_flag.store(true, Ordering::Release);
            let _ = watcher.join();
            result
        })
    }

    fn list_direct_children_inner(
        dir: &Path,
        budget: &mut WalkBudget,
        control: &Control,
    ) -> Result<DirectChildrenPage, String> {
        let handle = match open_dir_handle_checked(dir, None) {
            Ok(h) => h,
            Err(DirOpenError::ReparseOrCrossVolume) => {
                return Err("目标必须是目录且不能是链接或重解析点。".into());
            }
            Err(DirOpenError::AccessDenied | DirOpenError::Other) => {
                return Err("无法列出目标目录。".into());
            }
        };
        let mut buf = vec![0u64; 8192]; // 64 KiB aligned
        let buf_bytes = std::mem::size_of_val(buf.as_slice());
        let mut out = Vec::new();
        let mut first_batch = true;
        let mut truncated = false;
        let mut had_error = false;
        loop {
            control.check()?;
            if budget.exhausted
                || budget.remaining_entries() == 0
                || budget.remaining_timeout().is_zero()
            {
                budget.exhausted = true;
                budget.incomplete = true;
                truncated = true;
                break;
            }
            let res = unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    FileIdBothDirectoryInfo,
                    buf.as_mut_ptr().cast(),
                    buf_bytes as u32,
                )
            };
            match res {
                Ok(()) => {
                    // SAFETY: `buf` is initialized and sized for `buf_bytes`.
                    let raw =
                        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf_bytes) };
                    let (parsed, corrupted) = parse_file_id_both_dir_buffer(raw);
                    if first_batch && corrupted && parsed.is_empty() {
                        return list_direct_children_findfirst(dir, budget, control);
                    }
                    first_batch = false;
                    for item in parsed {
                        if is_dot_or_dotdot(&item.name_u16) {
                            continue;
                        }
                        if !budget.take(control)? {
                            truncated = true;
                            return Ok(DirectChildrenPage {
                                entries: out,
                                truncated,
                                had_error,
                            });
                        }
                        let os_name = OsString::from_wide(&item.name_u16);
                        let name = os_name.to_string_lossy().into_owned();
                        let path = dir.join(&os_name);
                        let logical = item.end_of_file.max(0) as u64;
                        let alloc = item.allocation_size.max(0) as u64;
                        let (kind, bytes) = match classify_win_entry(
                            item.attrs,
                            item.ea_size,
                            logical,
                            Some(alloc),
                        ) {
                            WinEntryClass::Dir => (DirectChildKind::Dir, None),
                            WinEntryClass::File(b) => (DirectChildKind::File, Some(b)),
                            WinEntryClass::ReparseOrCloudSkipped => {
                                (DirectChildKind::ReparseOrCloud, None)
                            }
                        };
                        let file_id = (item.file_id != 0).then_some(item.file_id);
                        out.push(Ok(DirectChild {
                            name,
                            path,
                            kind,
                            bytes,
                            file_id,
                        }));
                    }
                    if corrupted {
                        had_error = true;
                        budget.incomplete = true;
                        out.push(Err(()));
                        break;
                    }
                }
                Err(e) if is_enum_eof(&e) => break,
                Err(e) if first_batch && is_info_class_unsupported(&e) => {
                    return list_direct_children_findfirst(dir, budget, control);
                }
                Err(_) => {
                    control.check()?;
                    if first_batch {
                        return Err("无法列出目标目录。".into());
                    }
                    had_error = true;
                    budget.incomplete = true;
                    out.push(Err(()));
                    break;
                }
            }
        }
        Ok(DirectChildrenPage {
            entries: out,
            truncated,
            had_error,
        })
    }

    fn list_direct_children_findfirst(
        dir: &Path,
        budget: &mut WalkBudget,
        control: &Control,
    ) -> Result<DirectChildrenPage, String> {
        control.check()?;
        let pattern = to_wide_pattern(dir);
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        let first = unsafe {
            FindFirstFileExW(
                PCWSTR(pattern.as_ptr()),
                FindExInfoBasic,
                (&raw mut data).cast(),
                FindExSearchNameMatch,
                None,
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        let h = match first {
            Ok(h) => Ok(h),
            Err(e) if is_findfirst_flag_unsupported(&e) && control.check().is_ok() => unsafe {
                FindFirstFileExW(
                    PCWSTR(pattern.as_ptr()),
                    FindExInfoBasic,
                    (&raw mut data).cast(),
                    FindExSearchNameMatch,
                    None,
                    FIND_FIRST_EX_FLAGS(0),
                )
            },
            Err(e) => Err(e),
        };
        let handle = match h {
            Ok(h) if h != INVALID_HANDLE_VALUE => h,
            Err(e) if is_findfirst_empty(&e) => {
                return Ok(DirectChildrenPage {
                    entries: Vec::new(),
                    truncated: false,
                    had_error: false,
                });
            }
            _ => {
                control.check()?;
                return Err("无法列出目标目录。".into());
            }
        };
        struct FindGuard(HANDLE);
        impl Drop for FindGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = FindClose(self.0);
                }
            }
        }
        let _guard = FindGuard(handle);
        let mut out = Vec::new();
        let mut truncated = false;
        let mut had_error = false;
        loop {
            control.check()?;
            let name_len = data
                .cFileName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(data.cFileName.len());
            let name_slice = &data.cFileName[..name_len];
            if !is_dot_or_dotdot(name_slice) {
                if !budget.take(control)? {
                    truncated = true;
                    break;
                }
                let os_name = OsString::from_wide(name_slice);
                let name = os_name.to_string_lossy().into_owned();
                let path = dir.join(&os_name);
                let logical = ((data.nFileSizeHigh as u64) << 32) | (data.nFileSizeLow as u64);
                let (kind, bytes) = match classify_win_entry(
                    data.dwFileAttributes,
                    data.dwReserved0,
                    logical,
                    None,
                ) {
                    WinEntryClass::Dir => (DirectChildKind::Dir, None),
                    WinEntryClass::File(b) => (DirectChildKind::File, Some(b)),
                    WinEntryClass::ReparseOrCloudSkipped => (DirectChildKind::ReparseOrCloud, None),
                };
                out.push(Ok(DirectChild {
                    name,
                    path,
                    kind,
                    bytes,
                    file_id: None,
                }));
            }
            let next = unsafe { FindNextFileW(handle, &mut data) };
            match next {
                Ok(()) => {}
                Err(e) if is_enum_eof(&e) => break,
                Err(_) => {
                    control.check()?;
                    had_error = true;
                    budget.incomplete = true;
                    out.push(Err(()));
                    break;
                }
            }
        }
        Ok(DirectChildrenPage {
            entries: out,
            truncated,
            had_error,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn scan_dir_win(
        dir_id: usize,
        dir_path: &Path,
        depth: usize,
        root_slot: Option<usize>,
        buf: &mut [u64],
        shared: &SharedState,
        control: &Control,
        local_files: &mut Vec<FileRecord>,
    ) -> Vec<PathBuf> {
        if shared.cancelled.load(Ordering::Acquire) || shared.exhausted.load(Ordering::Acquire) {
            return Vec::new();
        }
        let handle = match open_dir_handle_checked(dir_path, shared.expected_vol_serial) {
            Ok(h) => h,
            Err(DirOpenError::AccessDenied) => {
                shared.mark_root_unreadable(root_slot);
                shared.record_denied(dir_path);
                return Vec::new();
            }
            Err(DirOpenError::ReparseOrCrossVolume | DirOpenError::Other) => {
                shared.mark_root_unreadable(root_slot);
                shared.mark_incomplete();
                return Vec::new();
            }
        };
        let buf_bytes = std::mem::size_of_val(buf);
        let mut subdirs = Vec::new();
        let mut first_batch = true;
        loop {
            if shared.cancelled.load(Ordering::Acquire) || shared.exhausted.load(Ordering::Acquire)
            {
                break;
            }
            let res = unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    FileIdBothDirectoryInfo,
                    buf.as_mut_ptr().cast(),
                    buf_bytes as u32,
                )
            };
            match res {
                Ok(()) => {
                    // SAFETY: `buf` is initialized and sized for `buf_bytes`.
                    let raw =
                        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf_bytes) };
                    let (parsed, corrupted) = parse_file_id_both_dir_buffer(raw);
                    if first_batch && corrupted && parsed.is_empty() {
                        // Safe to fall back only because the directory handle itself was already
                        // verified to be a non-reparse directory on the expected volume.
                        return scan_dir_findfirst(
                            dir_id,
                            dir_path,
                            depth,
                            root_slot,
                            shared,
                            control,
                            local_files,
                        );
                    }
                    first_batch = false;
                    for item in parsed {
                        if is_dot_or_dotdot(&item.name_u16) {
                            continue;
                        }
                        if !shared.take_entry(control) {
                            return subdirs;
                        }
                        let logical = item.end_of_file.max(0) as u64;
                        let alloc = item.allocation_size.max(0) as u64;
                        match classify_win_entry(item.attrs, item.ea_size, logical, Some(alloc)) {
                            WinEntryClass::Dir => {
                                if depth + 1 >= MAX_DIR_DEPTH {
                                    shared.mark_incomplete();
                                } else {
                                    subdirs
                                        .push(dir_path.join(OsString::from_wide(&item.name_u16)));
                                }
                            }
                            WinEntryClass::File(bytes) => {
                                let name = if shared.collect_analytics || shared.dedup_hardlinks {
                                    OsString::from_wide(&item.name_u16)
                                        .to_string_lossy()
                                        .into_owned()
                                } else {
                                    String::new()
                                };
                                local_files.push(FileRecord {
                                    file_id: item.file_id,
                                    dir_id,
                                    bytes,
                                    modified_unix: filetime_i64_to_unix(item.last_write_time),
                                    name,
                                });
                            }
                            WinEntryClass::ReparseOrCloudSkipped => {
                                shared.mark_incomplete();
                            }
                        }
                    }
                    if corrupted {
                        shared.mark_incomplete();
                        break;
                    }
                }
                Err(e) if is_enum_eof(&e) => break,
                Err(e) if first_batch && is_info_class_unsupported(&e) => {
                    return scan_dir_findfirst(
                        dir_id,
                        dir_path,
                        depth,
                        root_slot,
                        shared,
                        control,
                        local_files,
                    );
                }
                Err(e) => {
                    if is_access_denied(&e) {
                        shared.record_denied(dir_path);
                    } else {
                        shared.mark_incomplete();
                    }
                    if first_batch {
                        shared.mark_root_unreadable(root_slot);
                    }
                    break;
                }
            }
        }
        subdirs
    }

    fn scan_dir_findfirst(
        dir_id: usize,
        dir_path: &Path,
        depth: usize,
        root_slot: Option<usize>,
        shared: &SharedState,
        control: &Control,
        local_files: &mut Vec<FileRecord>,
    ) -> Vec<PathBuf> {
        if shared.cancelled.load(Ordering::Acquire) || shared.exhausted.load(Ordering::Acquire) {
            return Vec::new();
        }
        let pattern = to_wide_pattern(dir_path);
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        let first = unsafe {
            FindFirstFileExW(
                PCWSTR(pattern.as_ptr()),
                FindExInfoBasic,
                (&raw mut data).cast(),
                FindExSearchNameMatch,
                None,
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        let h = match first {
            Ok(h) => Ok(h),
            Err(e)
                if is_findfirst_flag_unsupported(&e)
                    && !shared.cancelled.load(Ordering::Acquire)
                    && !shared.exhausted.load(Ordering::Acquire) =>
            unsafe {
                FindFirstFileExW(
                    PCWSTR(pattern.as_ptr()),
                    FindExInfoBasic,
                    (&raw mut data).cast(),
                    FindExSearchNameMatch,
                    None,
                    FIND_FIRST_EX_FLAGS(0),
                )
            },
            Err(e) => Err(e),
        };
        let handle = match h {
            Ok(h) if h != INVALID_HANDLE_VALUE => h,
            Ok(_) => {
                shared.mark_root_unreadable(root_slot);
                shared.mark_incomplete();
                return Vec::new();
            }
            Err(e) if is_findfirst_empty(&e) => return Vec::new(),
            Err(e) => {
                shared.mark_root_unreadable(root_slot);
                if is_access_denied(&e) {
                    shared.record_denied(dir_path);
                } else {
                    shared.mark_incomplete();
                }
                return Vec::new();
            }
        };
        struct FindGuard(HANDLE);
        impl Drop for FindGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = FindClose(self.0);
                }
            }
        }
        let _guard = FindGuard(handle);
        let mut subdirs = Vec::new();
        loop {
            if shared.cancelled.load(Ordering::Acquire) || shared.exhausted.load(Ordering::Acquire)
            {
                break;
            }
            let name_len = data
                .cFileName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(data.cFileName.len());
            let name_slice = &data.cFileName[..name_len];
            if !is_dot_or_dotdot(name_slice) {
                if !shared.take_entry(control) {
                    break;
                }
                let logical = ((data.nFileSizeHigh as u64) << 32) | (data.nFileSizeLow as u64);
                match classify_win_entry(data.dwFileAttributes, data.dwReserved0, logical, None) {
                    WinEntryClass::Dir => {
                        if depth + 1 >= MAX_DIR_DEPTH {
                            shared.mark_incomplete();
                        } else {
                            subdirs.push(dir_path.join(OsString::from_wide(name_slice)));
                        }
                    }
                    WinEntryClass::File(bytes) => {
                        let name = if shared.collect_analytics {
                            OsString::from_wide(name_slice)
                                .to_string_lossy()
                                .into_owned()
                        } else {
                            String::new()
                        };
                        local_files.push(FileRecord {
                            file_id: 0, // FindFirstFileExW does not expose FileId -> never dedup
                            dir_id,
                            bytes,
                            modified_unix: filetime_parts_to_unix(
                                data.ftLastWriteTime.dwLowDateTime,
                                data.ftLastWriteTime.dwHighDateTime,
                            ),
                            name,
                        });
                    }
                    WinEntryClass::ReparseOrCloudSkipped => {
                        shared.mark_incomplete();
                    }
                }
            }
            match unsafe { FindNextFileW(handle, &mut data) } {
                Ok(()) => {}
                Err(e) if is_enum_eof(&e) => break,
                Err(e) => {
                    if is_access_denied(&e) {
                        shared.record_denied(dir_path);
                    } else {
                        shared.mark_incomplete();
                    }
                    break;
                }
            }
        }
        subdirs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_dir_entry(
        next_off: u32,
        eof: i64,
        alloc: i64,
        attrs: u32,
        ea_size: u32,
        file_id: u64,
        name: &str,
    ) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let name_bytes = units.len() * 2;
        let len = if next_off == 0 {
            FILE_ID_BOTH_HEADER_LEN + name_bytes
        } else {
            next_off as usize
        };
        let mut rec = vec![0u8; len.max(FILE_ID_BOTH_HEADER_LEN + name_bytes)];
        rec[0..4].copy_from_slice(&next_off.to_le_bytes());
        rec[40..48].copy_from_slice(&eof.to_le_bytes());
        rec[48..56].copy_from_slice(&alloc.to_le_bytes());
        rec[56..60].copy_from_slice(&attrs.to_le_bytes());
        rec[60..64].copy_from_slice(&(name_bytes as u32).to_le_bytes());
        rec[64..68].copy_from_slice(&ea_size.to_le_bytes());
        rec[96..104].copy_from_slice(&file_id.to_le_bytes());
        for (i, u) in units.iter().enumerate() {
            rec[FILE_ID_BOTH_HEADER_LEN + i * 2..FILE_ID_BOTH_HEADER_LEN + i * 2 + 2]
                .copy_from_slice(&u.to_le_bytes());
        }
        rec
    }

    #[test]
    fn dir_buffer_parser_handles_tight_106_byte_tail_without_overread() {
        // 104 header + 2-byte UTF-16 name ("a") = 106 bytes (< 112-byte sizeof struct!)
        let buf = encode_dir_entry(0, 1234, 4096, 0x20, 0, 0x0001_0000_0000_002A, "a");
        assert_eq!(buf.len(), 106);
        let (entries, corrupted) = parse_file_id_both_dir_buffer(&buf);
        assert!(!corrupted);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].end_of_file, 1234);
        assert_eq!(entries[0].file_id, 0x0001_0000_0000_002A);
        assert_eq!(entries[0].name_u16, vec![b'a' as u16]);
    }

    #[test]
    fn dir_buffer_parser_rejects_unaligned_overlapping_and_truncated_records() {
        // Odd/unaligned NextEntryOffset must be flagged as corrupted
        let mut bad_align = encode_dir_entry(113, 10, 10, 0x20, 0, 1, "x");
        bad_align.resize(240, 0);
        let (entries, corrupted) = parse_file_id_both_dir_buffer(&bad_align);
        assert!(corrupted);
        assert_eq!(entries.len(), 1);

        // NextEntryOffset overlapping FileName must be flagged as corrupted
        let mut overlap = encode_dir_entry(112, 10, 10, 0x20, 0, 1, "longfilename.txt");
        overlap[0..4].copy_from_slice(&112u32.to_le_bytes());
        overlap.resize(256, 0);
        let (_, corrupted_overlap) = parse_file_id_both_dir_buffer(&overlap);
        assert!(corrupted_overlap);

        // Truncated FileNameLength past buffer end must be flagged as corrupted
        let mut trunc = encode_dir_entry(0, 10, 10, 0x20, 0, 1, "ok");
        trunc[60..64].copy_from_slice(&500u32.to_le_bytes());
        let (entries_trunc, corrupted_trunc) = parse_file_id_both_dir_buffer(&trunc);
        assert!(corrupted_trunc);
        assert!(entries_trunc.is_empty());
    }

    #[test]
    fn cloud_and_reparse_entry_classification_handles_all_states() {
        const CLOUD_TAG: u32 = 0x9000_101A;
        const SYMLINK_TAG: u32 = 0xA000_000C;
        // 1. Online-only OneDrive file (recall + alloc=0) -> 0 bytes when alloc known, unknown in fallback
        assert_eq!(
            classify_win_entry(
                FILE_ATTRIBUTE_REPARSE_POINT_BITS | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
                CLOUD_TAG,
                10_000_000,
                Some(0)
            ),
            WinEntryClass::File(0)
        );
        assert_eq!(
            classify_win_entry(
                FILE_ATTRIBUTE_REPARSE_POINT_BITS | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
                CLOUD_TAG,
                10_000_000,
                None
            ),
            WinEntryClass::ReparseOrCloudSkipped
        );
        // 2. Partially hydrated OneDrive file -> min(logical, alloc)
        assert_eq!(
            classify_win_entry(
                FILE_ATTRIBUTE_REPARSE_POINT_BITS | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
                CLOUD_TAG,
                10_000_000,
                Some(4096)
            ),
            WinEntryClass::File(4096)
        );
        // 3. Fully hydrated OneDrive file (reparse tag CLOUD, no recall bit) -> normal logical size
        assert_eq!(
            classify_win_entry(
                FILE_ATTRIBUTE_REPARSE_POINT_BITS,
                CLOUD_TAG,
                8192,
                Some(8192)
            ),
            WinEntryClass::File(8192)
        );
        // 4. Symlink file -> skipped
        assert_eq!(
            classify_win_entry(FILE_ATTRIBUTE_REPARSE_POINT_BITS, SYMLINK_TAG, 100, Some(0)),
            WinEntryClass::ReparseOrCloudSkipped
        );
        // 5. Cloud directory or junction directory -> never descend
        assert_eq!(
            classify_win_entry(
                FILE_ATTRIBUTE_DIRECTORY_BITS | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
                CLOUD_TAG,
                0,
                Some(0)
            ),
            WinEntryClass::ReparseOrCloudSkipped
        );
    }
}

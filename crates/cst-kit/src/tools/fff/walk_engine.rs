#[cfg(windows)]
use std::collections::VecDeque;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::Condvar;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::time::Duration;
use std::time::Instant;

use globset::GlobSet;
use tokio_util::sync::CancellationToken;

use super::super::path_guard::{PathGuardSession, SearchGuardError, VerifiedSearchEntry};
use super::{
    FffindKind, FffindSortBy, FindCandidate, QueryPlan, cmp_find_candidate, dir_is_excluded,
    ext_matches, format_local_rfc3339, normalize_path, path_within_modified_window,
    size_within_bounds, worker_threads,
};

const MAX_DIR_DEPTH: usize = 1024;
#[cfg(windows)]
const MAX_QUEUE_BYTES: usize = 64 * 1024 * 1024;
#[cfg(windows)]
const RETURN_DEADLINE_MARGIN: Duration = Duration::from_millis(120);

/// Global permit pool across concurrent search requests so stuck driver I/O threads
/// continue holding their slot until exit without allowing unbounded thread growth.
#[cfg(windows)]
static GLOBAL_WALK_PERMITS: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
struct GlobalWorkerPermit;
#[cfg(windows)]
impl GlobalWorkerPermit {
    fn try_acquire(max_global: usize) -> Option<Self> {
        let mut cur = GLOBAL_WALK_PERMITS.load(AtomicOrdering::Relaxed);
        loop {
            if cur >= max_global {
                return None;
            }
            match GLOBAL_WALK_PERMITS.compare_exchange_weak(
                cur,
                cur + 1,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Relaxed,
            ) {
                Ok(_) => return Some(Self),
                Err(next) => cur = next,
            }
        }
    }

    fn acquire_bounded(
        max_global: usize,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Option<Self> {
        let wait_until = deadline.min(Instant::now() + Duration::from_millis(250));
        loop {
            if let Some(p) = Self::try_acquire(max_global) {
                return Some(p);
            }
            if cancel.is_cancelled() || Instant::now() >= wait_until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
#[cfg(windows)]
impl Drop for GlobalWorkerPermit {
    fn drop(&mut self) {
        GLOBAL_WALK_PERMITS.fetch_sub(1, AtomicOrdering::AcqRel);
    }
}

#[derive(Clone)]
pub(crate) struct WalkQuerySpec {
    pub kind: FffindKind,
    pub exts: Vec<String>,
    pub excludes: GlobSet,
    pub max_depth: Option<usize>,
    pub modified_window: Option<(i64, i64)>,
    pub size_bounds: Option<(u64, u64)>,
    pub sort_by: FffindSortBy,
    pub query: Arc<QueryPlan>,
    pub limit: usize,
}

#[derive(Clone)]
pub(crate) struct WalkRootTarget {
    pub display_root: String,
    pub verified_root: VerifiedSearchEntry,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub is_volume_root: bool,
}

pub(crate) struct WalkFindOutcome {
    pub candidates: Vec<FindCandidate>,
    pub truncated_by_limit: bool,
    pub timed_out: bool,
    pub queue_overflowed: bool,
    pub partial_roots: Vec<String>,
    pub skipped: usize,
}

pub(crate) struct WalkShared {
    session: PathGuardSession,
    spec: WalkQuerySpec,
    targets: Vec<WalkRootTarget>,
    deadline: Instant,
    cancel: CancellationToken,
    skipped: AtomicUsize,
    truncated_by_limit: AtomicBool,
    timed_out: AtomicBool,
    cancelled: AtomicBool,
    queue_overflowed: AtomicBool,
    stop: AtomicBool,
    partial_flags: Vec<AtomicBool>,
    collected: Mutex<Vec<FindCandidate>>,
}

impl WalkShared {
    fn mark_cancelled(&self) {
        self.cancelled.store(true, AtomicOrdering::Relaxed);
        self.stop.store(true, AtomicOrdering::Relaxed);
    }

    fn mark_timed_out(&self) {
        self.timed_out.store(true, AtomicOrdering::Relaxed);
        self.stop.store(true, AtomicOrdering::Relaxed);
    }

    fn mark_root_partial(&self, root_idx: usize) {
        if let Some(flag) = self.partial_flags.get(root_idx) {
            flag.store(true, AtomicOrdering::Relaxed);
        }
    }
}

struct FindWorker {
    shared: Arc<WalkShared>,
    scratch: String,
    /// Keeps up to `limit + 1` **approved** candidates so `truncated_by_limit`
    /// is only reported when at least `limit + 1` policy-approved items exist.
    local: Vec<FindCandidate>,
    local_skipped: usize,
}

impl FindWorker {
    fn new(shared: Arc<WalkShared>) -> Self {
        let cap = shared.spec.limit.saturating_add(2).min(256);
        Self {
            shared,
            scratch: String::with_capacity(256),
            local: Vec::with_capacity(cap),
            local_skipped: 0,
        }
    }

    fn flush(&mut self) {
        if self.local_skipped > 0 {
            self.shared
                .skipped
                .fetch_add(self.local_skipped, AtomicOrdering::Relaxed);
            self.local_skipped = 0;
        }
        if !self.local.is_empty() {
            let mut guard = self
                .shared
                .collected
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.append(&mut self.local);
        }
    }

    /// Returns true if a candidate with `(score, mtime_secs, size_bytes, path)` could
    /// displace the worst entry in `local` (which holds up to `limit + 1` approved entries).
    fn may_enter_heap(&self, probe: &FindCandidate) -> bool {
        let cap = self.shared.spec.limit.saturating_add(1);
        if self.local.len() < cap {
            return true;
        }
        let sort_by = self.shared.spec.sort_by;
        self.local
            .last()
            .is_none_or(|worst| cmp_find_candidate(probe, worst, sort_by).is_lt())
    }

    #[allow(clippy::too_many_arguments)]
    fn try_verify_and_insert(
        &mut self,
        root_idx: usize,
        full_path: &Path,
        is_dir: bool,
        score: i32,
        est_size: Option<u64>,
        est_mtime: Option<i64>,
        est_display: &str,
        expected_vol: Option<u32>,
        expected_id: Option<u64>,
    ) {
        let probe = FindCandidate {
            score,
            path: est_display.to_owned(),
            is_dir,
            size_bytes: est_size,
            modified_secs: est_mtime,
            modified_iso: None,
        };
        if !self.may_enter_heap(&probe) {
            return;
        }

        let target = &self.shared.targets[root_idx];
        let verified = match self.shared.session.verify_search_entry(
            full_path,
            &target.verified_root,
            expected_vol,
            expected_id,
            is_dir,
        ) {
            Ok(v) => v,
            Err(SearchGuardError::PolicyDenied) => {
                // Policy denials never increment `skipped` or `truncated`.
                return;
            }
            Err(SearchGuardError::IoOrAccess) => {
                self.local_skipped += 1;
                return;
            }
        };

        // Re-check filters against verified handle metadata.
        if verified.is_dir != is_dir {
            return;
        }
        if !path_within_modified_window(verified.modified_secs, self.shared.spec.modified_window) {
            return;
        }
        if !verified.is_dir
            && !size_within_bounds(verified.size_bytes, self.shared.spec.size_bounds)
        {
            return;
        }

        let verified_path = normalize_path(&verified.canonical_path);
        let candidate = FindCandidate {
            score,
            path: verified_path,
            is_dir: verified.is_dir,
            size_bytes: verified.size_bytes,
            modified_secs: verified.modified_secs,
            modified_iso: verified.modified_secs.and_then(format_local_rfc3339),
        };
        self.insert_approved(candidate);
    }

    fn insert_approved(&mut self, candidate: FindCandidate) {
        let cap = self.shared.spec.limit.saturating_add(1);
        let sort_by = self.shared.spec.sort_by;
        // Deduplicate by verified canonical path inside local Top-K before checking limit.
        if let Some(existing_pos) = self.local.iter().position(|c| c.path == candidate.path) {
            if cmp_find_candidate(&candidate, &self.local[existing_pos], sort_by).is_lt() {
                self.local.remove(existing_pos);
            } else {
                return;
            }
        }
        let mut inserted = false;
        if self.local.len() < cap {
            let pos = self
                .local
                .binary_search_by(|existing| cmp_find_candidate(existing, &candidate, sort_by))
                .unwrap_or_else(|i| i);
            self.local.insert(pos, candidate.clone());
            inserted = true;
            if self.local.len() == cap {
                self.shared
                    .truncated_by_limit
                    .store(true, AtomicOrdering::Relaxed);
            }
        } else if let Some(worst) = self.local.last()
            && cmp_find_candidate(&candidate, worst, sort_by).is_lt()
        {
            self.local.pop();
            let pos = self
                .local
                .binary_search_by(|existing| cmp_find_candidate(existing, &candidate, sort_by))
                .unwrap_or_else(|i| i);
            self.local.insert(pos, candidate.clone());
            inserted = true;
        }
        // Incrementally publish verified top candidates so stuck I/O at deadline never loses hits.
        if inserted && let Ok(mut guard) = self.shared.collected.try_lock() {
            guard.push(candidate);
        }
    }
}

impl Drop for FindWorker {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(windows)]
mod win_walk_find {
    use super::*;
    use crate::diagnostics::walk::{is_cloud_reparse_tag, parse_file_id_both_dir_buffer};
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_HANDLE_EOF, ERROR_NO_MORE_FILES,
        HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileIdBothDirectoryInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::CancelSynchronousIo;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};
    use windows::core::PCWSTR;

    const FILE_ATTRIBUTE_CLOUD_RECALL_DIR: u32 = 0x0004_0000 | 0x0040_0000;

    struct OwnedHandle(HANDLE);
    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE && !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    struct CancelRegistry {
        threads: Mutex<Vec<Arc<OwnedHandle>>>,
    }
    impl CancelRegistry {
        fn new() -> Self {
            Self {
                threads: Mutex::new(Vec::new()),
            }
        }

        fn register_current_thread(self: &Arc<Self>) -> ThreadCancelGuard {
            let mut dup = HANDLE::default();
            let handle = unsafe {
                let proc = GetCurrentProcess();
                let cur = GetCurrentThread();
                if DuplicateHandle(proc, cur, proc, &mut dup, 0, false, DUPLICATE_SAME_ACCESS)
                    .is_ok()
                {
                    Some(Arc::new(OwnedHandle(dup)))
                } else {
                    None
                }
            };
            if let Some(ref h) = handle
                && let Ok(mut list) = self.threads.lock()
            {
                list.push(Arc::clone(h));
            }
            ThreadCancelGuard {
                registry: Arc::clone(self),
                handle,
            }
        }

        fn cancel_all(&self) {
            // Clone `Arc<OwnedHandle>` under lock so handles stay alive during CancelSynchronousIo
            // even if the worker thread concurrently exits (prevents handle-reuse races).
            let handles: Vec<Arc<OwnedHandle>> =
                self.threads.lock().map(|g| g.clone()).unwrap_or_default();
            for h in handles {
                unsafe {
                    let _ = CancelSynchronousIo(h.0);
                }
            }
        }
    }

    struct ThreadCancelGuard {
        registry: Arc<CancelRegistry>,
        handle: Option<Arc<OwnedHandle>>,
    }
    impl Drop for ThreadCancelGuard {
        fn drop(&mut self) {
            if let Some(ref h) = self.handle
                && let Ok(mut list) = self.registry.threads.lock()
            {
                list.retain(|entry| !Arc::ptr_eq(entry, h));
            }
        }
    }

    struct FindDirTask {
        root_idx: usize,
        path: PathBuf,
        depth: usize,
        is_low: bool,
        may_contain_protected: bool,
        expected_vol: Option<u32>,
        expected_file_id: Option<u64>,
        est_bytes: usize,
    }

    impl FindDirTask {
        fn new(
            root_idx: usize,
            path: PathBuf,
            depth: usize,
            is_low: bool,
            may_contain_protected: bool,
            expected_vol: Option<u32>,
            expected_file_id: Option<u64>,
        ) -> Self {
            let est_bytes = std::mem::size_of::<Self>() + path.as_os_str().len() * 2 + 32;
            Self {
                root_idx,
                path,
                depth,
                is_low,
                may_contain_protected,
                expected_vol,
                expected_file_id,
                est_bytes,
            }
        }
    }

    struct FindQueueState {
        high: VecDeque<FindDirTask>,
        low: Vec<FindDirTask>,
        active: usize,
        active_by_root: Vec<usize>,
        queued_bytes: usize,
        stop: bool,
        workers_alive: usize,
    }

    struct FindDirQueue {
        state: Mutex<FindQueueState>,
        cv: Condvar,
    }

    impl FindDirQueue {
        fn new(initial: Vec<FindDirTask>, root_count: usize, workers_alive: usize) -> Self {
            let mut high = VecDeque::with_capacity(initial.len().max(16));
            let mut queued_bytes = 0usize;
            for task in initial {
                queued_bytes = queued_bytes.saturating_add(task.est_bytes);
                high.push_back(task);
            }
            Self {
                state: Mutex::new(FindQueueState {
                    high,
                    low: Vec::new(),
                    active: 0,
                    active_by_root: vec![0; root_count],
                    queued_bytes,
                    stop: false,
                    workers_alive,
                }),
                cv: Condvar::new(),
            }
        }

        fn pop(&self) -> Option<FindDirTask> {
            let mut guard = self.state.lock().ok()?;
            loop {
                if guard.stop {
                    return None;
                }
                if let Some(task) = guard.high.pop_front().or_else(|| guard.low.pop()) {
                    guard.queued_bytes = guard.queued_bytes.saturating_sub(task.est_bytes);
                    guard.active += 1;
                    if let Some(slot) = guard.active_by_root.get_mut(task.root_idx) {
                        *slot += 1;
                    }
                    return Some(task);
                }
                if guard.active == 0 {
                    return None;
                }
                guard = self.cv.wait(guard).ok()?;
            }
        }

        /// Atomically push child batches and release the current task's active count in a single
        /// state transition so workers can never exit prematurely while another worker is still
        /// publishing children.
        fn finish_and_push(
            &self,
            shared: &WalkShared,
            root_idx: usize,
            high_batch: Vec<FindDirTask>,
            low_batch: Vec<FindDirTask>,
        ) {
            if let Ok(mut guard) = self.state.lock() {
                if !guard.stop {
                    for task in high_batch {
                        if guard.queued_bytes.saturating_add(task.est_bytes) > MAX_QUEUE_BYTES {
                            shared.queue_overflowed.store(true, AtomicOrdering::Relaxed);
                            shared.mark_root_partial(task.root_idx);
                            continue;
                        }
                        guard.queued_bytes = guard.queued_bytes.saturating_add(task.est_bytes);
                        guard.high.push_back(task);
                    }
                    for task in low_batch {
                        if guard.queued_bytes.saturating_add(task.est_bytes) > MAX_QUEUE_BYTES {
                            shared.queue_overflowed.store(true, AtomicOrdering::Relaxed);
                            shared.mark_root_partial(task.root_idx);
                            continue;
                        }
                        guard.queued_bytes = guard.queued_bytes.saturating_add(task.est_bytes);
                        guard.low.push(task);
                    }
                }
                guard.active = guard.active.saturating_sub(1);
                if let Some(slot) = guard.active_by_root.get_mut(root_idx) {
                    *slot = slot.saturating_sub(1);
                }
                if guard.active == 0 || !guard.high.is_empty() || !guard.low.is_empty() {
                    self.cv.notify_all();
                }
            }
        }

        fn abort_active_task(&self, root_idx: usize) {
            if let Ok(mut guard) = self.state.lock() {
                guard.active = guard.active.saturating_sub(1);
                if let Some(slot) = guard.active_by_root.get_mut(root_idx) {
                    *slot = slot.saturating_sub(1);
                }
                self.cv.notify_all();
            }
        }

        fn mark_inflight_and_queued_partial(guard: &mut FindQueueState, shared: &WalkShared) {
            for (idx, &cnt) in guard.active_by_root.iter().enumerate() {
                if cnt > 0 {
                    shared.mark_root_partial(idx);
                }
            }
            for t in guard.high.drain(..) {
                shared.mark_root_partial(t.root_idx);
            }
            for t in guard.low.drain(..) {
                shared.mark_root_partial(t.root_idx);
            }
            guard.queued_bytes = 0;
        }

        fn stop_all_and_mark_partials(&self, shared: &WalkShared) {
            if let Ok(mut guard) = self.state.lock() {
                guard.stop = true;
                Self::mark_inflight_and_queued_partial(&mut guard, shared);
                self.cv.notify_all();
            }
        }

        fn worker_exited(&self) {
            if let Ok(mut guard) = self.state.lock() {
                guard.workers_alive = guard.workers_alive.saturating_sub(1);
                if guard.workers_alive == 0 {
                    self.cv.notify_all();
                }
            }
        }

        fn wait_until_done_or_deadline(
            &self,
            shared: &WalkShared,
            cancel_reg: &CancelRegistry,
            stop_deadline: Instant,
            return_deadline: Instant,
        ) {
            let mut guard = match self.state.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            let mut cancelling = false;
            while guard.workers_alive > 0 {
                let now = Instant::now();
                if !cancelling {
                    if shared.cancel.is_cancelled() {
                        shared.mark_cancelled();
                        guard.stop = true;
                        Self::mark_inflight_and_queued_partial(&mut guard, shared);
                        self.cv.notify_all();
                        cancelling = true;
                    } else if now >= stop_deadline {
                        shared.mark_timed_out();
                        guard.stop = true;
                        Self::mark_inflight_and_queued_partial(&mut guard, shared);
                        self.cv.notify_all();
                        cancelling = true;
                    } else if guard.stop || shared.stop.load(AtomicOrdering::Relaxed) {
                        guard.stop = true;
                        Self::mark_inflight_and_queued_partial(&mut guard, shared);
                        self.cv.notify_all();
                        cancelling = true;
                    }
                }
                if cancelling {
                    drop(guard);
                    cancel_reg.cancel_all();
                    if Instant::now() >= return_deadline {
                        // Return AT OR BEFORE total request deadline; stuck I/O threads retain
                        // their global permits until they unwind naturally.
                        return;
                    }
                    guard = match self.state.lock() {
                        Ok(g) => g,
                        Err(e) => e.into_inner(),
                    };
                    if guard.workers_alive == 0 {
                        return;
                    }
                }
                let wait = Duration::from_millis(20);
                let (next, _) = self
                    .cv
                    .wait_timeout(guard, wait)
                    .unwrap_or_else(|e| e.into_inner());
                guard = next;
            }
        }
    }

    struct PanicTaskGuard<'a> {
        queue: &'a FindDirQueue,
        shared: &'a WalkShared,
        cancel_reg: &'a CancelRegistry,
        root_idx: usize,
        completed: bool,
    }
    impl Drop for PanicTaskGuard<'_> {
        fn drop(&mut self) {
            if !self.completed {
                self.shared.mark_root_partial(self.root_idx);
                self.queue.abort_active_task(self.root_idx);
                if std::thread::panicking() {
                    self.shared.stop.store(true, AtomicOrdering::Relaxed);
                    self.queue.stop_all_and_mark_partials(self.shared);
                    self.cancel_reg.cancel_all();
                }
            }
        }
    }

    struct WorkerLifetimeGuard {
        queue: Arc<FindDirQueue>,
        shared: Arc<WalkShared>,
        cancel_reg: Arc<CancelRegistry>,
        _permit: GlobalWorkerPermit,
    }
    impl Drop for WorkerLifetimeGuard {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.shared.stop.store(true, AtomicOrdering::Relaxed);
                self.queue.stop_all_and_mark_partials(&self.shared);
                self.cancel_reg.cancel_all();
            }
            self.queue.worker_exited();
        }
    }

    struct WorkerScratch {
        io_buf: Vec<u64>,
        name_buf: String,
        display_buf: String,
        rel_buf: String,
    }
    impl WorkerScratch {
        fn new() -> Self {
            Self {
                io_buf: vec![0u64; (64 * 1024usize) / 8],
                name_buf: String::with_capacity(128),
                display_buf: String::with_capacity(256),
                rel_buf: String::with_capacity(256),
            }
        }
    }

    fn to_wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn is_dot_or_dotdot(name: &[u16]) -> bool {
        (name.len() == 1 && name[0] == b'.' as u16)
            || (name.len() == 2 && name[0] == b'.' as u16 && name[1] == b'.' as u16)
    }

    fn decode_utf16_into(units: &[u16], out: &mut String) -> bool {
        out.clear();
        let mut lossless = true;
        for ch in char::decode_utf16(units.iter().copied()) {
            match ch {
                Ok(c) => out.push(c),
                Err(_) => {
                    lossless = false;
                    out.push(char::REPLACEMENT_CHARACTER);
                }
            }
        }
        lossless
    }

    fn is_enum_eof(err: &windows::core::Error) -> bool {
        let code = err.code().0 as u32 & 0xFFFF;
        code == ERROR_NO_MORE_FILES.0 || code == ERROR_HANDLE_EOF.0
    }

    fn is_low_priority_dir(
        root_is_vol_root: bool,
        depth: usize,
        parent_name_lower: &str,
        child_name: &str,
    ) -> bool {
        let lower = child_name.to_ascii_lowercase();
        if root_is_vol_root && depth == 0 {
            return matches!(
                lower.as_str(),
                "windows"
                    | "programdata"
                    | "$recycle.bin"
                    | "system volume information"
                    | "recovery"
                    | "windows.old"
                    | "msocache"
                    | "perflogs"
            );
        }
        if parent_name_lower == "windows" {
            return matches!(
                lower.as_str(),
                "winsxs" | "servicing" | "installer" | "assembly" | "softwaredistribution"
            );
        }
        matches!(
            lower.as_str(),
            "appdata"
                | "node_modules"
                | ".cache"
                | ".cargo"
                | ".rustup"
                | ".gradle"
                | ".nuget"
                | ".conda"
                | ".m2"
                | "__pycache__"
                | ".venv"
                | ".npm"
                | ".pnpm-store"
                | ".bun"
        )
    }

    fn open_dir_checked(task: &FindDirTask) -> Result<(OwnedHandle, u32), ()> {
        let w = to_wide_path(&task.path);
        let h = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|_| ())?;
        if h == INVALID_HANDLE_VALUE {
            return Err(());
        }
        let handle = OwnedHandle(h);
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle.0, &mut info) }.is_err() {
            return Err(());
        }
        let attrs = info.dwFileAttributes;
        if attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0
            || attrs & (FILE_ATTRIBUTE_REPARSE_POINT.0 | FILE_ATTRIBUTE_CLOUD_RECALL_DIR) != 0
        {
            return Err(());
        }
        if let Some(ev) = task.expected_vol
            && ev != 0
            && (info.dwVolumeSerialNumber == 0 || info.dwVolumeSerialNumber != ev)
        {
            return Err(());
        }
        if let Some(eid) = task.expected_file_id.filter(|&id| id != 0) {
            let opened_id = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
            if opened_id == 0 || opened_id != eid {
                return Err(());
            }
        }
        Ok((handle, info.dwVolumeSerialNumber))
    }

    fn dir_prefix(path: &Path) -> String {
        let mut s = normalize_path(path);
        if !s.is_empty() && !s.ends_with('/') {
            s.push('/');
        }
        s
    }

    #[allow(clippy::too_many_arguments)]
    fn process_entry(
        worker: &mut FindWorker,
        shared: &WalkShared,
        task: &FindDirTask,
        dir_vol: u32,
        parent_name_lower: &str,
        root_is_vol_root: bool,
        display_prefix: &str,
        rel_prefix: &str,
        name_u16: &[u16],
        attrs: u32,
        reparse_tag: u32,
        end_of_file: u64,
        mtime_secs: Option<i64>,
        file_id: Option<u64>,
        high_batch: &mut Vec<FindDirTask>,
        low_batch: &mut Vec<FindDirTask>,
        scratch: &mut WorkerScratch,
    ) -> bool {
        if shared.stop.load(AtomicOrdering::Relaxed) {
            return false;
        }
        let lossless = decode_utf16_into(name_u16, &mut scratch.name_buf);
        if !lossless {
            // Surface unpaired UTF-16 surrogates via `skipped` rather than silent drop or mangling.
            worker.local_skipped += 1;
            return true;
        }
        let name = scratch.name_buf.as_str();
        let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
        let is_reparse = attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
        let child_depth = task.depth + 1;

        if is_dir {
            if is_reparse
                || (attrs & FILE_ATTRIBUTE_CLOUD_RECALL_DIR) != 0
                || name.eq_ignore_ascii_case(".git")
            {
                return true;
            }
            let child_path = task.path.join(name);
            let child_may_contain_protected = if task.may_contain_protected {
                if shared.session.verify_requested(&child_path).is_err() {
                    return true;
                }
                shared.session.dir_may_contain_protected_key(&child_path)
            } else {
                false
            };
            if shared.session.has_state_identity() {
                match (shared.session.state_legacy_identity(), dir_vol, file_id) {
                    (Some((sv, sid)), dv, Some(fid)) if sv != 0 && dv != 0 && fid != 0 => {
                        if sv == dv && sid == fid {
                            return true;
                        }
                    }
                    _ => {
                        if !shared.session.allow_walk_dir(&child_path) {
                            return true;
                        }
                    }
                }
            }
            let root_canon = &shared.targets[task.root_idx].verified_root.canonical_path;
            if !shared.spec.excludes.is_empty()
                && dir_is_excluded(&child_path, root_canon, &shared.spec.excludes)
            {
                return true;
            }

            // Evaluate directory itself as a match candidate if kind allows directories
            // and no file-only filters (`ext`, `size_bounds`) exclude directories.
            let dir_eligible = matches!(shared.spec.kind, FffindKind::Dir | FffindKind::Any)
                && shared.spec.exts.is_empty()
                && shared.spec.size_bounds.is_none()
                && path_within_modified_window(mtime_secs, shared.spec.modified_window);
            if dir_eligible && !shared.spec.query.fast_reject_filename(name) {
                scratch.display_buf.clear();
                scratch.display_buf.push_str(display_prefix);
                scratch.display_buf.push_str(name);
                scratch.rel_buf.clear();
                scratch.rel_buf.push_str(rel_prefix);
                scratch.rel_buf.push_str(name);
                let excluded = !shared.spec.excludes.is_empty()
                    && (shared.spec.excludes.is_match(&scratch.rel_buf)
                        || shared.spec.excludes.is_match(name));
                if !excluded
                    && let Some(score) =
                        shared
                            .spec
                            .query
                            .score_entry(&scratch.rel_buf, name, &mut worker.scratch)
                {
                    let est_display = scratch.display_buf.clone();
                    worker.try_verify_and_insert(
                        task.root_idx,
                        &child_path,
                        true,
                        score,
                        None,
                        mtime_secs,
                        &est_display,
                        (dir_vol != 0).then_some(dir_vol),
                        file_id.filter(|&id| id != 0),
                    );
                }
            }

            // Enqueue child directory if within max_depth and hard depth cap.
            let under_depth = child_depth < MAX_DIR_DEPTH
                && shared.spec.max_depth.is_none_or(|max| child_depth < max);
            if under_depth {
                let child_is_low = task.is_low
                    || is_low_priority_dir(root_is_vol_root, task.depth, parent_name_lower, name);
                let next_task = FindDirTask::new(
                    task.root_idx,
                    child_path,
                    child_depth,
                    child_is_low,
                    child_may_contain_protected,
                    (dir_vol != 0).then_some(dir_vol),
                    file_id.filter(|&id| id != 0),
                );
                if child_is_low {
                    low_batch.push(next_task);
                } else {
                    high_batch.push(next_task);
                }
            }
            return true;
        }

        // File entry
        if matches!(shared.spec.kind, FffindKind::Dir) {
            return true;
        }
        if is_reparse && !is_cloud_reparse_tag(reparse_tag) {
            return true;
        }
        if !shared.spec.exts.is_empty() && !ext_matches(name, &shared.spec.exts) {
            return true;
        }
        if !size_within_bounds(Some(end_of_file), shared.spec.size_bounds) {
            return true;
        }
        if !path_within_modified_window(mtime_secs, shared.spec.modified_window) {
            return true;
        }
        if shared.spec.query.fast_reject_filename(name) {
            return true;
        }

        scratch.display_buf.clear();
        scratch.display_buf.push_str(display_prefix);
        scratch.display_buf.push_str(name);
        scratch.rel_buf.clear();
        scratch.rel_buf.push_str(rel_prefix);
        scratch.rel_buf.push_str(name);
        if !shared.spec.excludes.is_empty()
            && (shared.spec.excludes.is_match(&scratch.rel_buf)
                || shared.spec.excludes.is_match(name))
        {
            return true;
        }
        let Some(score) =
            shared
                .spec
                .query
                .score_entry(&scratch.rel_buf, name, &mut worker.scratch)
        else {
            return true;
        };
        let full_path = task.path.join(name);
        let est_display = scratch.display_buf.clone();
        worker.try_verify_and_insert(
            task.root_idx,
            &full_path,
            false,
            score,
            Some(end_of_file),
            mtime_secs,
            &est_display,
            (dir_vol != 0).then_some(dir_vol),
            file_id.filter(|&id| id != 0),
        );
        true
    }

    fn scan_dir(
        worker: &mut FindWorker,
        shared: &WalkShared,
        task: &FindDirTask,
        high_batch: &mut Vec<FindDirTask>,
        low_batch: &mut Vec<FindDirTask>,
        scratch: &mut WorkerScratch,
    ) {
        let Ok((handle, dir_vol)) = open_dir_checked(task) else {
            worker.local_skipped += 1;
            if task.depth == 0 {
                shared.mark_root_partial(task.root_idx);
            }
            return;
        };
        let target = &shared.targets[task.root_idx];
        let display_prefix = dir_prefix(&task.path);
        let rel_prefix = task
            .path
            .strip_prefix(&target.verified_root.canonical_path)
            .ok()
            .map(dir_prefix)
            .unwrap_or_default();
        let parent_name_lower = task
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        loop {
            if shared.stop.load(AtomicOrdering::Relaxed) {
                break;
            }
            if shared.cancel.is_cancelled() {
                shared.mark_cancelled();
                break;
            }
            if Instant::now() >= shared.deadline {
                shared.mark_timed_out();
                break;
            }
            let buf_bytes = scratch.io_buf.len() * 8;
            let res = unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    FileIdBothDirectoryInfo,
                    scratch.io_buf.as_mut_ptr().cast(),
                    buf_bytes as u32,
                )
            };
            match res {
                Ok(()) => {
                    let raw = unsafe {
                        std::slice::from_raw_parts(scratch.io_buf.as_ptr().cast::<u8>(), buf_bytes)
                    };
                    let (entries, corrupted) = parse_file_id_both_dir_buffer(raw);
                    if corrupted {
                        worker.local_skipped += 1;
                    }
                    for entry in entries {
                        if is_dot_or_dotdot(&entry.name_u16) {
                            continue;
                        }
                        let eof = entry.end_of_file.max(0) as u64;
                        let mtime = crate::tools::path_guard::filetime_to_unix_secs(
                            entry.last_write_time.max(0) as u64,
                        );
                        let keep = process_entry(
                            worker,
                            shared,
                            task,
                            dir_vol,
                            &parent_name_lower,
                            target.is_volume_root,
                            &display_prefix,
                            &rel_prefix,
                            &entry.name_u16,
                            entry.attrs,
                            entry.ea_size,
                            eof,
                            mtime,
                            Some(entry.file_id),
                            high_batch,
                            low_batch,
                            scratch,
                        );
                        if !keep {
                            return;
                        }
                    }
                }
                Err(err) if is_enum_eof(&err) => break,
                Err(_) => {
                    worker.local_skipped += 1;
                    if task.depth == 0 {
                        shared.mark_root_partial(task.root_idx);
                    }
                    break;
                }
            }
        }
    }

    pub(super) fn run_parallel_walk_win(shared: Arc<WalkShared>) {
        let root_count = shared.targets.len();
        let mut initial = Vec::with_capacity(root_count);
        for (idx, target) in shared.targets.iter().enumerate() {
            let may_prot = shared
                .session
                .dir_may_contain_protected_key(&target.verified_root.canonical_path);
            // Pin root volume & object identity even at depth 0 to prevent root rebind races.
            initial.push(FindDirTask::new(
                idx,
                target.verified_root.canonical_path.clone(),
                0,
                false,
                may_prot,
                (target.verified_root.vol_serial != 0).then_some(target.verified_root.vol_serial),
                (target.verified_root.index64 != 0).then_some(target.verified_root.index64),
            ));
        }

        let max_global = worker_threads();
        let mut permits = Vec::with_capacity(max_global);
        if let Some(first) =
            GlobalWorkerPermit::acquire_bounded(max_global, shared.deadline, &shared.cancel)
        {
            permits.push(first);
            for _ in 1..max_global {
                if let Some(p) = GlobalWorkerPermit::try_acquire(max_global) {
                    permits.push(p);
                } else {
                    break;
                }
            }
        }
        if permits.is_empty() {
            // Never bypass the global worker permit pool with unbounded inline blocking I/O.
            for idx in 0..root_count {
                shared.mark_root_partial(idx);
            }
            if shared.cancel.is_cancelled() {
                shared.mark_cancelled();
            } else {
                shared.mark_timed_out();
            }
            return;
        }

        let cancel_reg = Arc::new(CancelRegistry::new());
        let queue = Arc::new(FindDirQueue::new(initial, root_count, permits.len()));
        let mut spawned = 0usize;
        for permit in permits {
            let w_shared = Arc::clone(&shared);
            let w_queue = Arc::clone(&queue);
            let w_cancel = Arc::clone(&cancel_reg);
            let res = std::thread::Builder::new()
                .name("cst-fffind-walk".into())
                .spawn(move || {
                    let _life = WorkerLifetimeGuard {
                        queue: Arc::clone(&w_queue),
                        shared: Arc::clone(&w_shared),
                        cancel_reg: Arc::clone(&w_cancel),
                        _permit: permit,
                    };
                    let _cancel_guard = w_cancel.register_current_thread();
                    let mut worker = FindWorker::new(Arc::clone(&w_shared));
                    let mut scratch = WorkerScratch::new();
                    while let Some(task) = w_queue.pop() {
                        let mut panic_guard = PanicTaskGuard {
                            queue: &w_queue,
                            shared: &w_shared,
                            cancel_reg: &w_cancel,
                            root_idx: task.root_idx,
                            completed: false,
                        };
                        if w_shared.stop.load(AtomicOrdering::Relaxed) {
                            w_shared.mark_root_partial(task.root_idx);
                            w_queue.abort_active_task(task.root_idx);
                            panic_guard.completed = true;
                            break;
                        }
                        let mut high_batch = Vec::new();
                        let mut low_batch = Vec::new();
                        scan_dir(
                            &mut worker,
                            &w_shared,
                            &task,
                            &mut high_batch,
                            &mut low_batch,
                            &mut scratch,
                        );
                        if w_shared.stop.load(AtomicOrdering::Relaxed) {
                            w_shared.mark_root_partial(task.root_idx);
                            w_queue.abort_active_task(task.root_idx);
                            w_queue.stop_all_and_mark_partials(&w_shared);
                            panic_guard.completed = true;
                            break;
                        }
                        w_queue.finish_and_push(&w_shared, task.root_idx, high_batch, low_batch);
                        panic_guard.completed = true;
                    }
                    worker.flush();
                });
            if res.is_ok() {
                spawned += 1;
            } else {
                queue.worker_exited();
            }
        }
        if spawned == 0 {
            for idx in 0..root_count {
                shared.mark_root_partial(idx);
            }
            shared.mark_timed_out();
            return;
        }
        let stop_deadline = shared
            .deadline
            .checked_sub(RETURN_DEADLINE_MARGIN)
            .unwrap_or(shared.deadline);
        queue.wait_until_done_or_deadline(&shared, &cancel_reg, stop_deadline, shared.deadline);
    }
}

#[cfg(not(windows))]
fn run_parallel_walk_unix(shared: Arc<WalkShared>) {
    use ignore::{WalkBuilder, WalkState};

    for (root_idx, target) in shared.targets.iter().enumerate() {
        let root = &target.verified_root.canonical_path;
        let filter_session = shared.session.clone();
        let filter_root = root.clone();
        let filter_excludes = shared.spec.excludes.clone();
        let mut builder = WalkBuilder::new(root);
        builder
            .threads(worker_threads())
            .standard_filters(false)
            .hidden(false)
            .follow_links(false)
            .max_depth(Some(
                shared
                    .spec
                    .max_depth
                    .unwrap_or(MAX_DIR_DEPTH)
                    .min(MAX_DIR_DEPTH),
            ));
        builder.filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if super::is_git_dir_name(entry.file_name()) {
                return false;
            }
            let Some(ft) = entry.file_type() else {
                return false;
            };
            if ft.is_symlink() {
                return false;
            }
            if ft.is_dir() {
                let p = entry.path();
                if !filter_session.allow_walk_dir(p) {
                    return false;
                }
                if !filter_excludes.is_empty() && dir_is_excluded(p, &filter_root, &filter_excludes)
                {
                    return false;
                }
            }
            true
        });

        let root_shared = Arc::clone(&shared);
        builder.build_parallel().run(|| {
            let worker_shared = Arc::clone(&root_shared);
            let mut worker = FindWorker::new(Arc::clone(&worker_shared));
            let root_canon = worker_shared.targets[root_idx]
                .verified_root
                .canonical_path
                .clone();
            Box::new(move |entry| {
                if worker_shared.stop.load(AtomicOrdering::Relaxed) {
                    return WalkState::Quit;
                }
                if worker_shared.cancel.is_cancelled() {
                    worker_shared.mark_cancelled();
                    return WalkState::Quit;
                }
                if Instant::now() >= worker_shared.deadline {
                    worker_shared.mark_timed_out();
                    worker_shared.mark_root_partial(root_idx);
                    return WalkState::Quit;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => {
                        worker.local_skipped += 1;
                        return WalkState::Continue;
                    }
                };
                if entry.depth() == 0 {
                    return WalkState::Continue;
                }
                let Some(ft) = entry.file_type() else {
                    return WalkState::Continue;
                };
                if ft.is_symlink() {
                    return WalkState::Continue;
                }
                let is_dir = ft.is_dir();
                if is_dir && matches!(worker_shared.spec.kind, FffindKind::File) {
                    return WalkState::Continue;
                }
                if !is_dir && matches!(worker_shared.spec.kind, FffindKind::Dir) {
                    return WalkState::Continue;
                }
                if is_dir
                    && (!worker_shared.spec.exts.is_empty()
                        || worker_shared.spec.size_bounds.is_some())
                {
                    return WalkState::Continue;
                }
                let name = entry.file_name().to_string_lossy();
                if !is_dir
                    && !worker_shared.spec.exts.is_empty()
                    && !ext_matches(&name, &worker_shared.spec.exts)
                {
                    return WalkState::Continue;
                }
                let meta = entry.metadata().ok();
                let size = meta.as_ref().and_then(|m| (!is_dir).then_some(m.len()));
                let mtime = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                if !is_dir && !size_within_bounds(size, worker_shared.spec.size_bounds) {
                    return WalkState::Continue;
                }
                if !path_within_modified_window(mtime, worker_shared.spec.modified_window) {
                    return WalkState::Continue;
                }
                let p = entry.path();
                let rel = p
                    .strip_prefix(&root_canon)
                    .ok()
                    .map(normalize_path)
                    .unwrap_or_else(|| normalize_path(p));
                if !worker_shared.spec.excludes.is_empty()
                    && (worker_shared.spec.excludes.is_match(&rel)
                        || worker_shared.spec.excludes.is_match(&*name))
                {
                    return WalkState::Continue;
                }
                let Some(score) =
                    worker_shared
                        .spec
                        .query
                        .score_entry(&rel, &name, &mut worker.scratch)
                else {
                    return WalkState::Continue;
                };
                let est_display = normalize_path(p);
                worker.try_verify_and_insert(
                    root_idx,
                    p,
                    is_dir,
                    score,
                    size,
                    mtime,
                    &est_display,
                    None,
                    None,
                );
                WalkState::Continue
            })
        });
    }
}

pub(crate) fn run_walk_find(
    session: &PathGuardSession,
    targets: Vec<WalkRootTarget>,
    spec: WalkQuerySpec,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<WalkFindOutcome, String> {
    let partial_flags = (0..targets.len()).map(|_| AtomicBool::new(false)).collect();
    let shared = Arc::new(WalkShared {
        session: session.clone(),
        spec,
        targets,
        deadline,
        cancel: cancel.clone(),
        skipped: AtomicUsize::new(0),
        truncated_by_limit: AtomicBool::new(false),
        timed_out: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        queue_overflowed: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        partial_flags,
        collected: Mutex::new(Vec::new()),
    });

    #[cfg(windows)]
    win_walk_find::run_parallel_walk_win(Arc::clone(&shared));
    #[cfg(not(windows))]
    run_parallel_walk_unix(Arc::clone(&shared));

    if shared.cancelled.load(AtomicOrdering::Relaxed) || cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }

    let candidates =
        std::mem::take(&mut *shared.collected.lock().unwrap_or_else(|e| e.into_inner()));
    let mut partial_roots = Vec::new();
    for (idx, flag) in shared.partial_flags.iter().enumerate() {
        if flag.load(AtomicOrdering::Relaxed) {
            partial_roots.push(shared.targets[idx].display_root.clone());
        }
    }

    Ok(WalkFindOutcome {
        candidates,
        truncated_by_limit: shared.truncated_by_limit.load(AtomicOrdering::Relaxed),
        timed_out: shared.timed_out.load(AtomicOrdering::Relaxed),
        queue_overflowed: shared.queue_overflowed.load(AtomicOrdering::Relaxed),
        partial_roots,
        skipped: shared.skipped.load(AtomicOrdering::Relaxed),
    })
}

use super::scan::ScanSnapshot;
use super::win_native::Control;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Cached volume snapshots remain valid for a short window unless invalidated earlier by a
/// volume identity change or an explicit `disk(usage)` refresh.
const VOLUME_CACHE_TTL: Duration = Duration::from_secs(120);
/// Short retry cooldown after a non-cancellation scan failure so transient I/O errors can recover.
const FAILED_RETRY_TTL: Duration = Duration::from_secs(30);
const MAX_CACHED_VOLUMES: usize = 4;
const MAX_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CONCURRENT_MFT_SCANS: usize = 2;

/// Process-wide accounting of bytes currently held by `VolumeIndex` storage snapshots,
/// shared with `fffind`'s search index manager under the unified 384 MiB process ceiling.
static STORAGE_SNAPSHOT_BYTES: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn storage_cache_bytes() -> usize {
    STORAGE_SNAPSHOT_BYTES.load(Ordering::Relaxed)
}

/// Process-level memory reservation ledger shared across storage and search caches.
/// Reservations remain charged until the `MemoryReservation` guard (bound to an `Arc`) drops,
/// ensuring evicted generations still held by active readers count against the 384 MiB ceiling.
pub(crate) struct ProcessMemoryLedger {
    reserved: AtomicUsize,
    max_process_bytes: usize,
    max_search_bytes: usize,
    include_storage_global: bool,
}

pub(crate) struct MemoryReservation {
    ledger: Arc<ProcessMemoryLedger>,
    bytes: usize,
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.ledger.reserved.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

impl std::fmt::Debug for MemoryReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryReservation")
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl ProcessMemoryLedger {
    pub(crate) fn global() -> Arc<Self> {
        static GLOBAL: std::sync::OnceLock<Arc<ProcessMemoryLedger>> = std::sync::OnceLock::new();
        Arc::clone(GLOBAL.get_or_init(|| {
            Arc::new(Self {
                reserved: AtomicUsize::new(0),
                max_process_bytes: 384 * 1024 * 1024,
                max_search_bytes: 256 * 1024 * 1024,
                include_storage_global: true,
            })
        }))
    }

    #[cfg(test)]
    pub(crate) fn new_isolated(max_process_bytes: usize, max_search_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            reserved: AtomicUsize::new(0),
            max_process_bytes,
            max_search_bytes,
            include_storage_global: false,
        })
    }

    pub(crate) fn reserved_bytes(&self) -> usize {
        self.reserved.load(Ordering::Acquire)
    }

    pub(crate) fn search_cap(&self) -> usize {
        let storage = if self.include_storage_global {
            storage_cache_bytes()
        } else {
            0
        };
        self.max_process_bytes
            .saturating_sub(storage)
            .min(self.max_search_bytes)
    }

    pub(crate) fn try_reserve(self: &Arc<Self>, bytes: usize) -> Option<MemoryReservation> {
        if bytes == 0 {
            return Some(MemoryReservation {
                ledger: Arc::clone(self),
                bytes: 0,
            });
        }
        let cap = self.search_cap();
        let mut cur = self.reserved.load(Ordering::Acquire);
        loop {
            let next = cur.checked_add(bytes)?;
            if next > cap {
                return None;
            }
            match self.reserved.compare_exchange_weak(
                cur,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(MemoryReservation {
                        ledger: Arc::clone(self),
                        bytes,
                    });
                }
                Err(actual) => cur = actual,
            }
        }
    }
}

fn update_storage_cache_bytes(snapshots: &HashMap<String, Arc<ScanSnapshot>>) {
    let total: usize = snapshots
        .values()
        .map(|s| s.estimated_bytes())
        .fold(0usize, usize::saturating_add);
    STORAGE_SNAPSHOT_BYTES.store(total, Ordering::Relaxed);
}

struct MftScanSlot {
    counter: Arc<AtomicUsize>,
}
impl MftScanSlot {
    fn try_acquire(counter: &Arc<AtomicUsize>) -> Option<Self> {
        let mut cur = counter.load(Ordering::Relaxed);
        loop {
            if cur >= MAX_CONCURRENT_MFT_SCANS {
                return None;
            }
            match counter.compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => {
                    return Some(Self {
                        counter: Arc::clone(counter),
                    });
                }
                Err(actual) => cur = actual,
            }
        }
    }
}
impl Drop for MftScanSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VolumeIdentity {
    pub serial: u32,
    pub filesystem: String,
    pub root_created: u64,
    pub volume_guid: Option<String>,
}

pub trait ScanSource: Send + Sync {
    fn identity(&self, drive: &str) -> Result<VolumeIdentity, String>;
    fn scan_volume(&self, drive: &str, control: &Control) -> Result<ScanSnapshot, String>;
}

#[derive(Clone)]
enum FlightOutcome {
    Ready(Arc<ScanSnapshot>),
    Failed(String),
    Cancelled,
}

struct Flight {
    scan_cancel: CancellationToken,
    subscribers: AtomicUsize,
    result: Mutex<Option<FlightOutcome>>,
    ready: Condvar,
}
impl Flight {
    fn new() -> Self {
        Self {
            scan_cancel: CancellationToken::new(),
            subscribers: AtomicUsize::new(0),
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
    fn subscribe(
        self: &Arc<Self>,
        inner: Arc<VolumeIndexInner>,
        pending_key: (String, VolumeIdentity),
    ) -> FlightSub {
        self.subscribers.fetch_add(1, Ordering::AcqRel);
        FlightSub {
            inner,
            pending_key,
            flight: Arc::clone(self),
        }
    }
    fn wait(&self, control: &Control) -> Result<FlightOutcome, String> {
        let mut guard = self.result.lock().map_err(|_| "扫描等待状态异常。")?;
        loop {
            control.check()?;
            if let Some(outcome) = guard.as_ref() {
                return Ok(outcome.clone());
            }
            guard = self
                .ready
                .wait_timeout(guard, Duration::from_millis(20))
                .map_err(|_| "扫描等待状态异常。")?
                .0;
        }
    }
    fn finish(&self, outcome: FlightOutcome) {
        if let Ok(mut slot) = self.result.lock() {
            *slot = Some(outcome);
        }
        self.ready.notify_all();
    }
}

/// Keeps a shared physical scan alive while at least one caller is waiting; cancels the physical
/// scan automatically once the last subscriber leaves.
struct FlightSub {
    inner: Arc<VolumeIndexInner>,
    pending_key: (String, VolumeIdentity),
    flight: Arc<Flight>,
}
impl Drop for FlightSub {
    fn drop(&mut self) {
        if self.flight.subscribers.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Re-verify under `inner.state` lock so a concurrent subscriber joining in `scan()`
            // cannot have its flight cancelled out from under it.
            if let Ok(mut state) = self.inner.state.lock()
                && self.flight.subscribers.load(Ordering::Acquire) == 0
            {
                let done = self.flight.result.lock().is_ok_and(|g| g.is_some());
                if !done {
                    self.flight.scan_cancel.cancel();
                    if state
                        .pending
                        .get(&self.pending_key)
                        .is_some_and(|active| Arc::ptr_eq(active, &self.flight))
                    {
                        state.pending.remove(&self.pending_key);
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct State {
    /// Keep the last probe sequence even if the drive could not be identified.
    identities: HashMap<String, (u64, Option<VolumeIdentity>)>,
    snapshots: HashMap<String, Arc<ScanSnapshot>>,
    failed: HashMap<String, (Instant, String)>,
    /// Keyed by `(drive, identity)` so in-flight scans are never shared across volume swaps.
    pending: HashMap<(String, VolumeIdentity), Arc<Flight>>,
}

impl State {
    fn identity(&self, drive: &str) -> Option<&VolumeIdentity> {
        self.identities.get(drive).and_then(|(_, id)| id.as_ref())
    }
}

struct VolumeIndexInner {
    source: Arc<dyn ScanSource>,
    state: Mutex<State>,
    probe_seq: AtomicU64,
    active_scans: Arc<AtomicUsize>,
    production_gate: bool,
    max_cache_bytes: usize,
    failed_ttl: Duration,
}

pub struct VolumeIndex {
    inner: Arc<VolumeIndexInner>,
}

impl VolumeIndex {
    /// Creates an index with explicit cache/scan enabled (used by tests and explicit callers;
    /// still honors `CST_DISABLE_MFT=1`).
    pub fn new(source: Arc<dyn ScanSource>) -> Self {
        Self::with_limits(source, false, MAX_CACHE_BYTES, FAILED_RETRY_TTL)
    }

    /// Creates the production index gated by [`super::mft::mft_enabled`] (default ON for elevated
    /// local NTFS volume roots unless disabled via `CST_DISABLE_MFT=1`).
    pub fn new_production(source: Arc<dyn ScanSource>) -> Self {
        Self::with_limits(source, true, MAX_CACHE_BYTES, FAILED_RETRY_TTL)
    }

    pub fn with_limits(
        source: Arc<dyn ScanSource>,
        production_gate: bool,
        max_cache_bytes: usize,
        failed_ttl: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(VolumeIndexInner {
                source,
                state: Mutex::new(State::default()),
                probe_seq: AtomicU64::new(1),
                active_scans: Arc::new(AtomicUsize::new(0)),
                production_gate,
                max_cache_bytes,
                failed_ttl,
            }),
        }
    }

    /// Whether MFT scanning / cache reads are allowed under current environment gates.
    pub fn allows_mft(&self) -> bool {
        if super::mft::mft_disabled_by_env() {
            return false;
        }
        if self.inner.production_gate {
            super::mft::mft_enabled()
        } else {
            true
        }
    }

    /// Probes native volume identity OUTSIDE the cache mutex, using a monotonic sequence number so
    /// an older probe arriving late can never overwrite a newer observed identity or evict new cache.
    fn refresh(&self, drive: &str) -> Result<VolumeIdentity, String> {
        refresh_inner(&self.inner, drive)
    }

    /// Cached data is never returned without a fresh native identity check, gate check, and TTL check.
    pub fn cached(&self, target: &Path) -> Option<Arc<ScanSnapshot>> {
        if !self.allows_mft() {
            return None;
        }
        let drive = drive_key(target)?;
        let identity = self.refresh(&drive).ok()?;
        let mut state = self.inner.state.lock().ok()?;
        purge_expired(&mut state, self.inner.failed_ttl);
        if state.identity(&drive) != Some(&identity) {
            return None;
        }
        let snapshot = state.snapshots.get(&drive)?;
        if snapshot.identity.as_ref() != Some(&identity) {
            return None;
        }
        snapshot.covers_target(target).then(|| snapshot.clone())
    }

    /// Scans the local volume (or joins an in-flight scan for the same `(drive, identity)`).
    pub fn scan(
        &self,
        root: &Path,
        force: bool,
        control: &Control,
    ) -> Result<Option<Arc<ScanSnapshot>>, String> {
        control.check()?;
        if !self.allows_mft() {
            return Ok(None);
        }
        let Some(drive) = drive_key(root) else {
            return Ok(None);
        };
        let identity = match self.refresh(&drive) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let pending_key = (drive.clone(), identity.clone());
        let (flight, sub) = {
            let mut state = self.inner.state.lock().map_err(|_| "卷缓存状态异常。")?;
            purge_expired(&mut state, self.inner.failed_ttl);
            if state.identity(&drive) != Some(&identity) {
                return Err("扫描期间卷身份变化，请重试。".into());
            }
            if !force {
                if let Some(snapshot) = state.snapshots.get(&drive)
                    && snapshot.identity.as_ref() == Some(&identity)
                    && snapshot.covers_target(root)
                {
                    return Ok(Some(snapshot.clone()));
                }
                if state.failed.contains_key(&drive) {
                    return Ok(None);
                }
            }
            if let Some(existing) = state
                .pending
                .get(&pending_key)
                .filter(|f| !f.scan_cancel.is_cancelled())
            {
                let f = Arc::clone(existing);
                let s = f.subscribe(Arc::clone(&self.inner), pending_key.clone());
                (f, s)
            } else {
                let Some(slot) = MftScanSlot::try_acquire(&self.inner.active_scans) else {
                    return Err("原生卷索引并发扫描已达上限，已回退目录遍历。".into());
                };
                let f = Arc::new(Flight::new());
                let s = f.subscribe(Arc::clone(&self.inner), pending_key.clone());
                state.pending.insert(pending_key.clone(), Arc::clone(&f));
                spawn_flight_worker(
                    Arc::clone(&self.inner),
                    drive.clone(),
                    identity.clone(),
                    Arc::clone(&f),
                    slot,
                );
                (f, s)
            }
        };
        let outcome = {
            let _keep_sub = sub;
            flight.wait(control)?
        };
        if self.refresh(&drive)? != identity {
            return Err("扫描期间卷身份变化，请重试。".into());
        }
        match outcome {
            FlightOutcome::Ready(snapshot) => {
                if snapshot.identity.as_ref() != Some(&identity) {
                    return Err("扫描期间卷身份变化，请重试。".into());
                }
                if snapshot.covers_target(root)
                    || path_key(Path::new(&snapshot.root)) == path_key(root)
                {
                    Ok(Some(snapshot))
                } else {
                    Ok(None)
                }
            }
            FlightOutcome::Failed(err) => Err(err),
            FlightOutcome::Cancelled => Err("诊断已取消。".into()),
        }
    }

    pub fn is_current(&self, snapshot: &ScanSnapshot, target: &Path) -> bool {
        let Some(expected) = snapshot.identity.as_ref() else {
            return false;
        };
        drive_key(target)
            .and_then(|drive| self.refresh(&drive).ok())
            .as_ref()
            == Some(expected)
    }

    pub fn filesystem(&self, target: &Path) -> Option<String> {
        self.refresh(&drive_key(target)?)
            .ok()
            .map(|id| id.filesystem)
    }
}

fn refresh_inner(inner: &VolumeIndexInner, drive: &str) -> Result<VolumeIdentity, String> {
    let seq = inner.probe_seq.fetch_add(1, Ordering::AcqRel);
    let current = inner.source.identity(drive);
    let mut state = inner.state.lock().map_err(|_| "卷缓存状态异常。")?;
    purge_expired(&mut state, inner.failed_ttl);
    // Reject out-of-order stale probe completions so a slow older probe cannot clobber a newer identity.
    let observed = current.as_ref().ok();
    if let Some((existing_seq, latest)) = state.identities.get(drive)
        && *existing_seq > seq
    {
        return if observed == latest.as_ref() {
            current
        } else {
            Err("扫描期间卷身份变化，请重试。".into())
        };
    }
    if state.identity(drive) != observed {
        state.snapshots.remove(drive);
        update_storage_cache_bytes(&state.snapshots);
        state.failed.remove(drive);
        state.pending.retain(|(pending_drive, _), flight| {
            if pending_drive == drive {
                flight.scan_cancel.cancel();
                false
            } else {
                true
            }
        });
    }
    state
        .identities
        .insert(drive.into(), (seq, observed.cloned()));
    current
}

fn spawn_flight_worker(
    inner: Arc<VolumeIndexInner>,
    drive: String,
    identity: VolumeIdentity,
    flight: Arc<Flight>,
    slot: MftScanSlot,
) {
    let pending_key = (drive.clone(), identity.clone());
    std::thread::spawn(move || {
        let scan_control = Control::new(flight.scan_cancel.clone(), Duration::from_secs(180));
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut snapshot = inner.source.scan_volume(&drive, &scan_control)?;
            snapshot.identity = Some(identity.clone());
            snapshot.captured_at = Instant::now();
            scan_control.check()?;
            if refresh_inner(&inner, &drive)? != identity {
                return Err("扫描期间卷身份变化，请重试。".into());
            }
            Ok(Arc::new(snapshot))
        })) {
            Ok(Ok(snap)) => FlightOutcome::Ready(snap),
            Ok(Err(err)) => {
                if flight.scan_cancel.is_cancelled() {
                    FlightOutcome::Cancelled
                } else {
                    FlightOutcome::Failed(err)
                }
            }
            Err(_) => FlightOutcome::Failed("卷索引扫描工作线程异常退出。".into()),
        };

        drop(slot);
        if let Ok(mut state) = inner.state.lock() {
            purge_expired(&mut state, inner.failed_ttl);
            let active_flight = state
                .pending
                .get(&pending_key)
                .is_some_and(|active| Arc::ptr_eq(active, &flight));
            if !active_flight {
                // A detach or cancellation invalidated this flight, even if the same volume
                // identity was observed again before the worker completed.
                flight.finish(FlightOutcome::Cancelled);
                return;
            }
            state.pending.remove(&pending_key);
            if state.identity(&drive) == Some(&identity) {
                match &outcome {
                    FlightOutcome::Ready(snapshot) => {
                        let merged = if let Some(old) = state.snapshots.get(&drive) {
                            Arc::new(old.merge(snapshot))
                        } else {
                            Arc::clone(snapshot)
                        };
                        state.snapshots.insert(drive.clone(), merged);
                        state.failed.remove(&drive);
                        evict_if_needed(&mut state.snapshots, inner.max_cache_bytes);
                    }
                    FlightOutcome::Failed(err) => {
                        state
                            .failed
                            .insert(drive.clone(), (Instant::now(), err.clone()));
                    }
                    FlightOutcome::Cancelled => {}
                }
            }
        }
        flight.finish(outcome);
    });
}

fn purge_expired(state: &mut State, failed_ttl: Duration) {
    state
        .snapshots
        .retain(|_, snap| snap.captured_at.elapsed() < VOLUME_CACHE_TTL);
    state.failed.retain(|_, (at, _)| at.elapsed() < failed_ttl);
    update_storage_cache_bytes(&state.snapshots);
}

fn evict_if_needed(snapshots: &mut HashMap<String, Arc<ScanSnapshot>>, max_bytes: usize) {
    loop {
        let total_bytes: usize = snapshots
            .values()
            .map(|s| s.estimated_bytes())
            .fold(0usize, usize::saturating_add);
        if snapshots.len() <= MAX_CACHED_VOLUMES && total_bytes <= max_bytes {
            STORAGE_SNAPSHOT_BYTES.store(total_bytes, Ordering::Relaxed);
            break;
        }
        let Some(oldest) = snapshots
            .iter()
            .min_by_key(|(_, snap)| snap.captured_at)
            .map(|(k, _)| k.clone())
        else {
            STORAGE_SNAPSHOT_BYTES.store(0, Ordering::Relaxed);
            break;
        };
        snapshots.remove(&oldest);
    }
}

pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(stripped) = text.strip_prefix(r"\\?\") {
        stripped.to_owned()
    } else {
        text.into_owned()
    }
}

pub fn path_key(path: &Path) -> String {
    let text = display_path(path).replace('/', "\\");
    let (prefix, rest) = match text.strip_prefix(r"\\") {
        Some(tail) => (r"\\", tail),
        None => ("", text.as_str()),
    };
    let mut normalized = String::with_capacity(text.len());
    normalized.push_str(prefix);
    let mut prev_slash = false;
    for ch in rest.chars() {
        if ch == '\\' {
            if !prev_slash {
                normalized.push('\\');
                prev_slash = true;
            }
        } else {
            prev_slash = false;
            normalized.push(ch);
        }
    }
    normalized.trim_end_matches('\\').to_uppercase()
}

pub fn drive_key(path: &Path) -> Option<String> {
    let key = path_key(path);
    let bytes = key.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| key[..2].to_owned())
}

pub fn is_volume_root(path: &Path) -> bool {
    drive_key(path).is_some_and(|drive| path_key(path) == drive)
}

pub fn in_tree(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('\\'))
}

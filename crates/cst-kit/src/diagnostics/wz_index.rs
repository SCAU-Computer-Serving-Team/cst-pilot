use super::scan::{ScanSnapshot, parse_export};
use super::win_native::Control;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeIdentity {
    pub serial: u32,
    pub filesystem: String,
    pub root_created: u64,
}

/// Export readers own temporary-file cleanup. Returning a reader means the
/// exporter has exited successfully and its complete output can be consumed.
pub trait ScanSource: Send + Sync {
    fn identity(&self, drive: &str) -> Result<VolumeIdentity, String>;
    fn export(&self, root: &Path, control: &Control) -> Result<Box<dyn Read + Send>, String>;
}

#[derive(Default)]
struct State {
    identities: HashMap<String, VolumeIdentity>,
    snapshots: HashMap<String, Arc<ScanSnapshot>>,
    failed: HashSet<String>,
    pending: HashMap<String, Arc<Flight>>,
}
struct Flight {
    result: Mutex<Option<Option<Arc<ScanSnapshot>>>>,
    ready: Condvar,
}
impl Flight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
    fn wait(&self, control: &Control) -> Result<Option<Arc<ScanSnapshot>>, String> {
        let mut result = self.result.lock().map_err(|_| "扫描等待状态异常。")?;
        loop {
            control.check()?;
            if let Some(result) = result.as_ref() {
                return Ok(result.clone());
            }
            result = self
                .ready
                .wait_timeout(result, Duration::from_millis(20))
                .map_err(|_| "扫描等待状态异常。")?
                .0;
        }
    }
    fn finish(&self, result: Option<Arc<ScanSnapshot>>) {
        if let Ok(mut slot) = self.result.lock() {
            *slot = Some(result);
        }
        self.ready.notify_all();
    }
}

pub struct WzIndex {
    source: Arc<dyn ScanSource>,
    state: Mutex<State>,
}
struct ScanLease<'a> {
    index: &'a WzIndex,
    drive: String,
    flight: Option<Arc<Flight>>,
}
impl ScanLease<'_> {
    fn finish(mut self, result: Option<Arc<ScanSnapshot>>) {
        if let Some(flight) = self.flight.take() {
            self.release(&flight);
            flight.finish(result);
        }
    }
    fn release(&self, flight: &Arc<Flight>) {
        if let Ok(mut state) = self.index.state.lock()
            && state
                .pending
                .get(&self.drive)
                .is_some_and(|active| Arc::ptr_eq(active, flight))
        {
            state.pending.remove(&self.drive);
        }
    }
}
impl Drop for ScanLease<'_> {
    fn drop(&mut self) {
        if let Some(flight) = self.flight.take() {
            self.release(&flight);
            flight.finish(None);
        }
    }
}
impl WzIndex {
    pub fn new(source: Arc<dyn ScanSource>) -> Self {
        Self {
            source,
            state: Mutex::new(State::default()),
        }
    }
    fn refresh(&self, drive: &str) -> Result<VolumeIdentity, String> {
        let mut state = self.state.lock().map_err(|_| "卷缓存状态异常。")?;
        let current = self.source.identity(drive);
        if current.as_ref().ok() != state.identities.get(drive) {
            state.snapshots.remove(drive);
            state.failed.remove(drive);
            state.identities.remove(drive);
            if let Ok(identity) = &current {
                state.identities.insert(drive.into(), identity.clone());
            }
        }
        current
    }
    /// Cached data is never returned without a fresh native identity check.
    pub fn cached(&self, target: &Path) -> Option<Arc<ScanSnapshot>> {
        let drive = drive_key(target)?;
        let identity = self.refresh(&drive).ok()?;
        let state = self.state.lock().ok()?;
        if state.identities.get(&drive) != Some(&identity) {
            return None;
        }
        let snapshot = state.snapshots.get(&drive)?;
        snapshot
            .dirs
            .contains_key(&path_key(target))
            .then(|| snapshot.clone())
    }
    /// ls requests a complete volume snapshot; explicit disk usage refreshes the
    /// requested subtree. All callers on the same volume join an active scan.
    pub fn scan(
        &self,
        root: &Path,
        force: bool,
        control: &Control,
    ) -> Result<Option<Arc<ScanSnapshot>>, String> {
        control.check()?;
        let Some(drive) = drive_key(root) else {
            return Ok(None);
        };
        let identity = match self.refresh(&drive) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let key = path_key(root);
        loop {
            control.check()?;
            let (flight, owner) = {
                let mut state = self.state.lock().map_err(|_| "卷缓存状态异常。")?;
                if state.identities.get(&drive) != Some(&identity) {
                    return Err("扫描期间卷身份变化，请重试。".into());
                }
                if !force {
                    if let Some(snapshot) = state.snapshots.get(&drive)
                        && snapshot.dirs.contains_key(&key)
                    {
                        return Ok(Some(snapshot.clone()));
                    }
                    if state.failed.contains(&drive) {
                        return Ok(None);
                    }
                }
                if let Some(flight) = state.pending.get(&drive) {
                    (flight.clone(), false)
                } else {
                    let flight = Arc::new(Flight::new());
                    state.pending.insert(drive.clone(), flight.clone());
                    (flight, true)
                }
            };
            if !owner {
                let completed = flight.wait(control)?;
                if self.refresh(&drive)? != identity {
                    return Err("扫描期间卷身份变化，请重试。".into());
                }
                if let Some(snapshot) = completed
                    && ((!force && snapshot.dirs.contains_key(&key))
                        || path_key(Path::new(&snapshot.root)) == key)
                {
                    return Ok(Some(snapshot));
                }
                // A cancelled owner never marks the volume failed; another caller
                // may take ownership using its own cancellation/deadline.
                continue;
            }
            let lease = ScanLease {
                index: self,
                drive: drive.clone(),
                flight: Some(flight),
            };
            let target = if force {
                root.to_path_buf()
            } else {
                std::path::PathBuf::from(format!("{drive}\\"))
            };
            let result = (|| {
                let reader = self.source.export(&target, control)?;
                let mut snapshot = parse_export(reader, &target, control)?;
                snapshot.identity = Some(identity.clone());
                control.check()?;
                if self.refresh(&drive)? != identity {
                    return Err("扫描期间卷身份变化，请重试。".into());
                }
                Ok(Arc::new(snapshot))
            })();
            let complete = result.as_ref().ok().cloned();
            {
                let mut state = self.state.lock().map_err(|_| "卷缓存状态异常。")?;
                if state.identities.get(&drive) == Some(&identity) {
                    if let Some(snapshot) = &complete {
                        let merged = if let Some(old) = state.snapshots.get(&drive) {
                            Arc::new(old.merge(snapshot))
                        } else {
                            snapshot.clone()
                        };
                        state.snapshots.insert(drive.clone(), merged);
                        state.failed.remove(&drive);
                    } else if control.check().is_ok() {
                        state.failed.insert(drive.clone());
                    }
                }
            }
            lease.finish(complete);
            return result.map(Some);
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

pub fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches('\\').to_uppercase()
}
pub fn drive_key(path: &Path) -> Option<String> {
    let key = path_key(path);
    let bytes = key.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| key[..2].to_owned())
}
pub fn in_tree(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('\\'))
}

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::owner::Proof;
use crate::runbook::storage::{DirectoryLease, Storage};

/// Cooperative directory lock matching telemetry/file-lock.ts on FAT/exFAT too.
pub struct FileLock {
    storage: Storage,
    path: PathBuf,
    bytes: Vec<u8>,
    _parent: DirectoryLease,
    _serial: MutexGuard<'static, ()>,
}
impl FileLock {
    pub fn acquire(
        root: &Path,
        target: &Path,
        proof: &Proof,
        cancel: &CancellationToken,
    ) -> Result<Self, String> {
        static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
        let mutex = SERIAL.get_or_init(|| Mutex::new(()));
        let serial = loop {
            if cancel.is_cancelled() {
                return Err("遥测文件事务已取消。".into());
            }
            match mutex.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        let storage = Storage::new(root)?;
        let relative = target
            .parent()
            .and_then(|p| p.strip_prefix(&storage.root).ok())
            .ok_or("遥测事务路径超出工具包。")?;
        let parent = storage.pin_directory(relative, true)?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("遥测文件名无效。")?;
        let path = target.with_file_name(format!("{name}.lock"));
        let bytes = serde_json::to_vec(&json!({"pid":std::process::id(),"proof":proof}))
            .map_err(|_| "遥测事务锁无法编码。")?;
        for attempt in 0..=100 {
            if cancel.is_cancelled() {
                return Err("遥测文件事务已取消。".into());
            }
            match fs::create_dir(&path) {
                Ok(()) => {
                    let lock = Self {
                        storage,
                        path,
                        bytes,
                        _parent: parent,
                        _serial: serial,
                    };
                    let relative = lock
                        .path
                        .strip_prefix(&lock.storage.root)
                        .map_err(|_| "遥测锁路径无效。")?;
                    let _lease = lock.storage.pin_directory(relative, false)?;
                    lock.storage
                        .atomic(&lock.path.join("owner.json"), &lock.bytes, false, cancel)
                        .map_err(|_| "遥测事务所有者无法保存。")?;
                    return Ok(lock);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    reclaim(&storage, &path, proof, cancel);
                }
                Err(error)
                    if cfg!(windows)
                        && matches!(error.raw_os_error(), Some(5 | 32))
                        && attempt < 100 => {}
                Err(_) => return Err("遥测事务目录无法创建。".into()),
            }
            if attempt == 100 {
                break;
            }
            let millis = 20 + u64::from(rand::random::<u8>() % 31);
            for _ in 0..millis.div_ceil(5) {
                if cancel.is_cancelled() {
                    return Err("遥测文件事务已取消。".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Err("遥测文件锁等待超时。".into())
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        for _ in 0..100 {
            let Ok(relative) = self.path.strip_prefix(&self.storage.root) else {
                return;
            };
            let Ok(lease) = self.storage.pin_directory(relative, false) else {
                return;
            };
            let owner = self.path.join("owner.json");
            let known = match self.storage.read_limited(&owner, 4096) {
                Ok(Some(bytes)) => bytes == self.bytes,
                Ok(None) => true,
                Err(_) => false,
            };
            if !known {
                return;
            }
            let _ = fs::remove_file(&owner);
            drop(lease);
            match fs::remove_dir(&self.path) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                // Another process's short owner probe may still pin the directory/file.
                Err(error)
                    if cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 145)) =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => return,
            }
        }
    }
}
fn abandoned(storage: &Storage, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(&storage.root) else {
        return false;
    };
    let Ok(_lease) = storage.pin_directory(relative, false) else {
        return false;
    };
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if let Ok(Some(bytes)) = storage.read_limited(&path.join("owner.json"), 4096)
        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
    {
        if let Some(raw) = value.get("proof") {
            return !serde_json::from_value::<Proof>(raw.clone()).is_ok_and(|proof| proof.alive());
        }
        if let Some(pid) = value
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            && pid > 0
            && !process_alive(pid)
        {
            return true;
        }
    }
    metadata
        .modified()
        .ok()
        .and_then(|time| SystemTime::now().duration_since(time).ok())
        .is_some_and(|age| age > Duration::from_secs(120))
}
fn reclaim(storage: &Storage, path: &Path, proof: &Proof, cancel: &CancellationToken) {
    if !abandoned(storage, path) {
        return;
    }
    let recovery = path.with_file_name(format!(
        "{}.recovery",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    if fs::create_dir(&recovery).is_err() {
        // A previous cleaner may have died. Only known owner files and empty dirs are removed.
        if abandoned(storage, &recovery) {
            clean_known(storage, &recovery);
        }
        return;
    }
    let owner = recovery.join("owner.json");
    let bytes = json!({"pid":std::process::id(),"proof":proof}).to_string();
    if let Ok(relative) = recovery.strip_prefix(&storage.root)
        && let Ok(_lease) = storage.pin_directory(relative, false)
        && storage
            .atomic(&owner, bytes.as_bytes(), false, cancel)
            .is_ok()
        && abandoned(storage, path)
    {
        // Identity is rechecked inside the single-cleaner section; the parent stays pinned.
        let stale = path.with_file_name(format!(
            "{}.stale-{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            uuid::Uuid::new_v4()
        ));
        if fs::rename(path, &stale).is_ok() {
            clean_known(storage, &stale);
        }
    }
    clean_known(storage, &recovery);
}
fn clean_known(storage: &Storage, path: &Path) {
    let Ok(relative) = path.strip_prefix(&storage.root) else {
        return;
    };
    let Ok(lease) = storage.pin_directory(relative, false) else {
        return;
    };
    if storage.read_limited(&path.join("owner.json"), 4096).is_ok() {
        let _ = fs::remove_file(path.join("owner.json"));
    }
    drop(lease);
    // Never recurse through unknown contents or links.
    let _ = fs::remove_dir(path);
}
#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            unsafe {
                let _ = CloseHandle(handle);
            }
            true
        }
        Err(error) => error.code().0 as u32 != 0x8007_0057,
    }
}
#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    // Legacy PID-only locks retain the 120-second fallback on platforms without Win32.
    let _ = pid;
    true
}

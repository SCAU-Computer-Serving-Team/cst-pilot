use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use uuid::Uuid;

const RETRY_DELAYS_MS: [u64; 5] = [10, 20, 45, 100, 200];

/// Per-key asynchronous mutex table (`session:{id}`, `model-selection:{id}`, `model-default`,
/// `auth:{provider}`, `auth-config:all`).
#[derive(Clone, Default)]
pub struct KeyedLocks {
    inner: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

pub struct KeyedLockGuard {
    key: String,
    table: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
    _guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for KeyedLockGuard {
    fn drop(&mut self) {
        // Release the async mutex first, then prune expired Weak entries.
        self._guard.take();
        if let Ok(mut map) = self.table.lock()
            && map
                .get(&self.key)
                .is_some_and(|weak| weak.strong_count() == 0)
        {
            map.remove(&self.key);
        }
    }
}

impl KeyedLocks {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn lock(&self, key: impl Into<String>) -> KeyedLockGuard {
        let key = key.into();
        let mutex = {
            let mut map = self.inner.lock().expect("keyed locks");
            if map.len() > 256 {
                map.retain(|_, weak| weak.strong_count() > 0);
            }
            if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
                existing
            } else {
                let created = Arc::new(AsyncMutex::new(()));
                map.insert(key.clone(), Arc::downgrade(&created));
                created
            }
        };
        let guard = mutex.lock_owned().await;
        KeyedLockGuard {
            key,
            table: self.inner.clone(),
            _guard: Some(guard),
        }
    }

    pub async fn serialize<T, F, Fut>(&self, key: impl Into<String>, action: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _guard = self.lock(key).await;
        action().await
    }

    pub fn tracked_key_count(&self) -> usize {
        let mut map = self.inner.lock().expect("keyed locks");
        map.retain(|_, weak| weak.strong_count() > 0);
        map.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageErrorKind {
    StorageFull,
    StorageUnavailable,
}

#[derive(Debug, Clone)]
pub struct StorageError {
    pub kind: StorageErrorKind,
    pub message: String,
}

impl StorageError {
    pub fn code(&self) -> &'static str {
        match self.kind {
            StorageErrorKind::StorageFull => "storage_full",
            StorageErrorKind::StorageUnavailable => "storage_unavailable",
        }
    }

    pub fn status_code(&self) -> u16 {
        match self.kind {
            StorageErrorKind::StorageFull => 507,
            StorageErrorKind::StorageUnavailable => 500,
        }
    }

    pub fn from_io(err: &io::Error, context: &str) -> Self {
        if is_storage_full_io(err) {
            Self {
                kind: StorageErrorKind::StorageFull,
                message: format!("{context}：存储空间不足，输入或配置未被接受。"),
            }
        } else {
            Self {
                kind: StorageErrorKind::StorageUnavailable,
                message: format!("{context}：存储介质不可写或访问受拒。"),
            }
        }
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

pub fn is_storage_full_io(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::StorageFull {
        return true;
    }
    matches!(err.raw_os_error(), Some(28 | 39 | 112))
}

pub fn is_transient_replace_error(err: &io::Error) -> bool {
    if is_storage_full_io(err) {
        return false;
    }
    if matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    matches!(err.raw_os_error(), Some(5 | 32 | 33))
}

/// Synchronously write `bytes` to a sibling temp file, `sync_all()`, and atomically rename
/// over `target`, retrying transient Windows sharing/permission locks up to 5 times (<= 375ms).
pub fn atomic_write_bytes_sync(
    target: &Path,
    bytes: &[u8],
    context: &str,
) -> Result<(), StorageError> {
    let parent = target.parent().ok_or_else(|| StorageError {
        kind: StorageErrorKind::StorageUnavailable,
        message: format!("{context}：目标路径缺少父目录。"),
    })?;
    fs::create_dir_all(parent).map_err(|e| StorageError::from_io(&e, context))?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("state");
    let tmp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let write_res = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(StorageError::from_io(&err, context));
    }

    let mut last_err = None;
    for (attempt, delay_ms) in RETRY_DELAYS_MS.iter().copied().enumerate() {
        match fs::rename(&tmp_path, target) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let retryable = is_transient_replace_error(&err);
                last_err = Some(err);
                if !retryable {
                    break;
                }
                std::thread::sleep(Duration::from_millis(delay_ms));
                if attempt + 1 == RETRY_DELAYS_MS.len() {
                    if let Err(final_err) = fs::rename(&tmp_path, target) {
                        last_err = Some(final_err);
                    } else {
                        return Ok(());
                    }
                }
            }
        }
    }

    let _ = fs::remove_file(&tmp_path);
    let err = last_err.unwrap_or_else(|| io::Error::other("atomic rename failed"));
    Err(StorageError::from_io(&err, context))
}

pub async fn atomic_write_bytes(
    target: impl AsRef<Path>,
    bytes: Vec<u8>,
    context: &'static str,
) -> Result<(), StorageError> {
    let target = target.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || atomic_write_bytes_sync(&target, &bytes, context))
        .await
        .map_err(|_| StorageError {
            kind: StorageErrorKind::StorageUnavailable,
            message: format!("{context}：后台写入线程异常中断。"),
        })?
}

pub async fn atomic_write_json<T: Serialize>(
    target: impl AsRef<Path>,
    value: &T,
    context: &'static str,
) -> Result<(), StorageError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| StorageError {
        kind: StorageErrorKind::StorageUnavailable,
        message: format!("{context}：JSON 序列化失败。"),
    })?;
    bytes.push(b'\n');
    atomic_write_bytes(target, bytes, context).await
}

pub fn atomic_write_json_sync<T: Serialize>(
    target: &Path,
    value: &T,
    context: &str,
) -> Result<(), StorageError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| StorageError {
        kind: StorageErrorKind::StorageUnavailable,
        message: format!("{context}：JSON 序列化失败。"),
    })?;
    bytes.push(b'\n');
    atomic_write_bytes_sync(target, &bytes, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn keyed_locks_serializes_same_key_and_runs_distinct_keys_concurrently() {
        let locks = KeyedLocks::new();
        let active_same = Arc::new(AtomicUsize::new(0));
        let max_same = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let locks = locks.clone();
            let active = active_same.clone();
            let max = max_same.clone();
            handles.push(tokio::spawn(async move {
                locks
                    .serialize("session:a", || async move {
                        let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max.fetch_max(cur, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(15)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(max_same.load(Ordering::SeqCst), 1);
        assert_eq!(locks.tracked_key_count(), 0);
    }

    #[tokio::test]
    async fn atomic_write_json_persists_and_replaces_cleanly() {
        let dir = std::env::temp_dir().join(format!("cst-server-locks-{}", Uuid::new_v4()));
        let file = dir.join("nested").join("state.json");
        atomic_write_json(&file, &serde_json::json!({"step": 1}), "测试写入")
            .await
            .unwrap();
        atomic_write_json(&file, &serde_json::json!({"step": 2}), "测试更新")
            .await
            .unwrap();
        let text = fs::read_to_string(&file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["step"], 2);
        let _ = fs::remove_dir_all(&dir);
    }
}

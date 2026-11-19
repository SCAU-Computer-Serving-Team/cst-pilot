use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

pub use cst_os::fs_safe::{
    StorageError, StorageErrorKind, atomic_write_bytes_sync, atomic_write_json_sync,
    is_storage_full_io, is_transient_replace_error,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use uuid::Uuid;

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

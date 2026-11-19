use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_session::{SessionInfo, list_sessions_from_dir};
use tokio::sync::Mutex as AsyncMutex;

use crate::locks::{StorageError, atomic_write_json, atomic_write_json_sync};
use crate::session::attachments::is_valid_session_id_key;
use crate::session::{SessionPoolError, UnflushedSessionSnapshot};

/// Persists and queries session index metadata (`created.json`, `unflushed/`, `deleted/` tombstones,
/// and project JSONL directories) independently from in-memory `SessionSlot` management.
#[derive(Clone)]
pub struct SessionIndexStore {
    agent_dir: PathBuf,
    inbox_root: PathBuf,
    created_lock: Arc<AsyncMutex<()>>,
    removed: Arc<Mutex<BTreeSet<String>>>,
}

impl SessionIndexStore {
    pub fn new(agent_dir: PathBuf, inbox_root: PathBuf) -> Self {
        Self {
            agent_dir,
            inbox_root,
            created_lock: Arc::new(AsyncMutex::new(())),
            removed: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    pub fn created_file(&self) -> PathBuf {
        self.inbox_root.join("created.json")
    }

    fn legacy_created_times_file(&self) -> PathBuf {
        self.inbox_root.join("created-times.json")
    }

    pub fn unflushed_dir(&self) -> PathBuf {
        self.inbox_root.join("unflushed")
    }

    fn unflushed_file(&self, id: &str) -> PathBuf {
        self.unflushed_dir().join(format!("{id}.json"))
    }

    fn deleted_file(&self, id: &str) -> Result<PathBuf, SessionPoolError> {
        if !is_valid_session_id_key(id) {
            return Err(SessionPoolError::InvalidInput("会话 ID 无效".to_owned()));
        }
        Ok(self.inbox_root.join("deleted").join(id))
    }

    pub fn is_deleted_sync(&self, id: &str) -> Result<bool, SessionPoolError> {
        if self.removed.lock().expect("removed lock").contains(id) {
            return Ok(true);
        }
        let path = self.deleted_file(id)?;
        Ok(path.exists())
    }

    pub fn mark_deleted_sync(&self, id: &str) -> Result<(), SessionPoolError> {
        let tombstone_path = self.deleted_file(id)?;
        if let Some(parent) = tombstone_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                SessionPoolError::Storage(StorageError::from_io(&e, "创建会话删除墓碑目录"))
            })?;
        }
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tombstone_path)
                .map_err(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        SessionPoolError::NotFound("会话不存在".to_owned())
                    } else {
                        SessionPoolError::Storage(StorageError::from_io(&e, "写入会话删除墓碑"))
                    }
                })?;
            let _ = f.write_all(b"deleted");
            let _ = f.sync_all();
        }
        self.removed
            .lock()
            .expect("removed lock")
            .insert(id.to_owned());
        Ok(())
    }

    pub fn read_created_sync(&self) -> Result<BTreeMap<String, Option<String>>, SessionPoolError> {
        let path = self.created_file();
        match fs::read(&path) {
            Ok(bytes) => {
                let map: BTreeMap<String, Option<String>> = serde_json::from_slice(&bytes)
                    .map_err(|_| {
                        SessionPoolError::ConfigurationUnavailable("会话索引损坏".to_owned())
                    })?;
                if !map.iter().all(|(k, v)| {
                    is_valid_session_id_key(k)
                        && v.as_ref().is_none_or(|id| is_valid_session_id_key(id))
                }) {
                    return Err(SessionPoolError::ConfigurationUnavailable(
                        "会话索引损坏".to_owned(),
                    ));
                }
                Ok(map)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(err) => Err(SessionPoolError::Storage(StorageError::from_io(
                &err,
                "读取会话创建索引",
            ))),
        }
    }

    /// Registers a new session ID under `request_key` (writing `unflushed/<id>.json` and `created.json`),
    /// or returns the previously registered session ID if `request_key` already exists.
    pub async fn get_or_create_registration(
        &self,
        request_key: &str,
        build_snapshot: impl FnOnce() -> UnflushedSessionSnapshot,
    ) -> Result<(String, bool), SessionPoolError> {
        let _serial = self.created_lock.lock().await;
        let mut entries = self.read_created_sync()?;
        if let Some(existing) = entries.get(request_key) {
            return match existing {
                Some(id) => Ok((id.clone(), false)),
                None => Err(SessionPoolError::Conflict(
                    "创建请求对应的会话已删除".to_owned(),
                )),
            };
        }
        let unflushed = build_snapshot();
        let id = unflushed.id.clone();
        self.write_unflushed_sync(&unflushed)?;
        entries.insert(request_key.to_owned(), Some(id.clone()));
        atomic_write_json(&self.created_file(), &entries, "保存会话创建索引")
            .await
            .map_err(SessionPoolError::Storage)?;
        Ok((id, true))
    }

    pub async fn clear_created_for_deleted(&self, id: &str) {
        let _serial = self.created_lock.lock().await;
        if let Ok(mut created) = self.read_created_sync() {
            let mut changed = false;
            for val in created.values_mut() {
                if val.as_deref() == Some(id) {
                    *val = None;
                    changed = true;
                }
            }
            if changed {
                let _ = atomic_write_json(&self.created_file(), &created, "更新会话创建索引").await;
            }
        }
    }

    pub fn read_legacy_created_times_sync(&self) -> BTreeMap<String, String> {
        let path = self.legacy_created_times_file();
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn read_unflushed_sync(&self, id: &str) -> Option<UnflushedSessionSnapshot> {
        let path = self.unflushed_file(id);
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn list_all_unflushed_sync(&self) -> Vec<UnflushedSessionSnapshot> {
        let dir = self.unflushed_dir();
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = fs::read(&path)
                && let Ok(snap) = serde_json::from_slice::<UnflushedSessionSnapshot>(&bytes)
            {
                out.push(snap);
            }
        }
        out
    }

    pub fn write_unflushed_sync(
        &self,
        snapshot: &UnflushedSessionSnapshot,
    ) -> Result<(), SessionPoolError> {
        let path = self.unflushed_file(&snapshot.id);
        atomic_write_json_sync(&path, snapshot, "保存未落盘会话快照")
            .map_err(SessionPoolError::Storage)
    }

    pub fn remove_unflushed_sync(&self, id: &str) {
        let _ = fs::remove_file(self.unflushed_file(id));
    }

    /// Scans `agent/home/sessions/*.jsonl` and all `agent/home/sessions/--*--/*.jsonl` workspace subdirectories.
    pub async fn list_all_project_sessions(&self) -> Vec<SessionInfo> {
        let root = self.agent_dir.join("sessions");
        if !root.exists() {
            return Vec::new();
        }
        let mut dirs = vec![root.clone()];
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() && !dirs.contains(&path) {
                    dirs.push(path);
                }
            }
        }
        let mut all = Vec::new();
        for dir in dirs {
            let mut items = list_sessions_from_dir(&dir, None, None).await;
            all.append(&mut items);
        }
        all
    }
}

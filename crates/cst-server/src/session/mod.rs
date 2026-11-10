pub mod attachments;
pub mod decorators;
pub mod events;
pub mod inbox;
pub mod interactions;
pub mod receipts;
pub mod slot;

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cst_pilot::{HostedSessionOptions, KitHost};
use pi_agent::AgentMessage;
use pi_ai::{Model, ModelCost, ModelInputModality, ModelThinkingLevel, uuid_v7};
use pi_runtime::{
    ModelRuntime, SettingsManager, get_default_session_dir, parse_model_thinking_level_cli,
};
use pi_session::{
    ExtensionMode, NewSessionOptions, SessionEntry, SessionInfo, SessionManager, SessionStoreError,
    build_session_info_sync, format_iso_timestamp, list_sessions_from_dir, now_iso_timestamp,
    parse_iso_timestamp_ms,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;

use crate::discovery::{SessionOwnerLock, strip_windows_verbatim_prefix};
use crate::locks::{KeyedLocks, StorageError, atomic_write_json, atomic_write_json_sync};
use crate::mode::CallerIdentity;
use crate::session::attachments::{ImageStore, is_valid_session_id_key};
use crate::session::events::{
    MAX_MULTIPLEX_SUBSCRIPTIONS, MultiplexHub, MultiplexSubscriptionUpdate, SessionEvents,
    SessionEventsResolver,
};
use crate::session::interactions::{BitLockerSessionBridge, ServerUiBridge};
use crate::session::receipts::MutationReceipts;
use crate::session::slot::{SessionLease, SessionSlot};

pub const DEFAULT_MAX_IDLE_SESSIONS: usize = 10;
pub const RELEASE_DEFAULT_PROVIDER: &str = "deepseek";
pub const RELEASE_DEFAULT_MODEL_ID: &str = "deepseek-flash";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnflushedModelRef {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnflushedSessionSnapshot {
    pub id: String,
    pub created_at: String,
    pub cwd: String,
    pub session_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<UnflushedModelRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListRow {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleSessionRow {
    pub id: String,
    pub title: String,
    pub running: bool,
}

#[derive(Debug, Clone)]
pub enum SessionPoolError {
    NotFound(String),
    SessionOwned(String),
    Conflict(String),
    CredentialsRequired(String),
    InvalidInput(String),
    ConfigurationUnavailable(String),
    Stopping,
    Storage(StorageError),
}

impl SessionPoolError {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
            Self::SessionOwned(_)
            | Self::Conflict(_)
            | Self::CredentialsRequired(_)
            | Self::Stopping => 409,
            Self::InvalidInput(_) => 400,
            Self::ConfigurationUnavailable(_) => 503,
            Self::Storage(s) => s.status_code(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::SessionOwned(_) => "session_owned",
            Self::Conflict(_) => "conflict",
            Self::CredentialsRequired(_) => "credentials_required",
            Self::InvalidInput(_) => "invalid_request",
            Self::ConfigurationUnavailable(_) => "configuration_unavailable",
            Self::Stopping => "stopping",
            Self::Storage(s) => s.code(),
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(m)
            | Self::SessionOwned(m)
            | Self::Conflict(m)
            | Self::CredentialsRequired(m)
            | Self::InvalidInput(m)
            | Self::ConfigurationUnavailable(m) => m.as_str(),
            Self::Stopping => "程序正在退出，请勿继续提交操作",
            Self::Storage(s) => &s.message,
        }
    }
}

impl std::fmt::Display for SessionPoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SessionPoolError {}

pub fn explicit_model(runtime: &ModelRuntime, provider: &str, model_id: &str) -> Model {
    runtime
        .get_model(provider, model_id)
        .unwrap_or_else(|| Model {
            id: model_id.to_owned(),
            name: model_id.to_owned(),
            model_type: None,
            api: "openai-completions".to_owned(),
            provider: provider.to_owned(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInputModality::Text],
            input_limits: None,
            cost: ModelCost::default(),
            prompt_cache: None,
            context_window: 1_000_000,
            max_tokens: 16_384,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
            headers: None,
            compat: None,
        })
}

pub fn default_model_reference(settings: &SettingsManager) -> (String, String) {
    let provider = settings
        .get_default_provider()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| RELEASE_DEFAULT_PROVIDER.to_owned());
    let model_id = settings
        .get_default_model()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| RELEASE_DEFAULT_MODEL_ID.to_owned());
    (provider, model_id)
}

pub async fn require_model_credentials(
    runtime: &ModelRuntime,
    invalid_auth: &Mutex<BTreeSet<String>>,
    provider: &str,
    model_id: &str,
) -> Result<Model, SessionPoolError> {
    let Some(model) = runtime.get_model(provider, model_id) else {
        return Err(SessionPoolError::InvalidInput(
            "请求的模型已不可用，请手动选择模型。".to_owned(),
        ));
    };
    let marked_invalid = invalid_auth
        .lock()
        .expect("invalid_auth lock")
        .contains(provider);
    if marked_invalid || !runtime.has_configured_auth(provider) {
        return Err(SessionPoolError::CredentialsRequired(format!(
            "尚未配置 {provider} 的模型服务凭据。请通过 /login 或 Web 模型服务设置完成配置后再发送。"
        )));
    }
    Ok(model)
}

/// Manages concurrent `SessionSlot`s across workspace directories, `unflushed/` snapshots,
/// `deleted/` tombstones, safe `before`-position branching, and 10-idle-session LRU eviction.
#[derive(Clone)]
pub struct SessionPool {
    host: KitHost,
    default_cwd: PathBuf,
    agent_dir: PathBuf,
    default_session_dir: PathBuf,
    inbox_root: PathBuf,
    max_idle_sessions: usize,
    touch_seq: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    slots: Arc<AsyncMutex<BTreeMap<String, Arc<SessionSlot>>>>,
    created_lock: Arc<AsyncMutex<()>>,
    load_locks: KeyedLocks,
    eviction_lock: Arc<AsyncMutex<()>>,
    removed: Arc<Mutex<BTreeSet<String>>>,
    scoped_models: Arc<Mutex<Vec<pi_runtime::ScopedModel>>>,
    pub invalid_auth: Arc<Mutex<BTreeSet<String>>>,
    pub images: ImageStore,
    pub receipts: MutationReceipts,
    pub global_events: SessionEvents,
    pub multiplex_hub: MultiplexHub,
}

impl SessionPool {
    pub fn new(host: KitHost, default_cwd: impl Into<PathBuf>) -> Self {
        Self::with_max_idle(host, default_cwd, DEFAULT_MAX_IDLE_SESSIONS)
    }

    pub fn with_max_idle(
        host: KitHost,
        default_cwd: impl Into<PathBuf>,
        max_idle_sessions: usize,
    ) -> Self {
        let default_cwd = default_cwd.into();
        let canonical_cwd = default_cwd
            .canonicalize()
            .unwrap_or_else(|_| default_cwd.clone());
        let canonical_cwd = strip_windows_verbatim_prefix(&canonical_cwd);
        let agent_dir = strip_windows_verbatim_prefix(&host.kit().agent_dir());
        let default_session_dir = get_default_session_dir(&canonical_cwd, &agent_dir);
        let inbox_root = agent_dir.join("web-inbox");
        let images = ImageStore::new(inbox_root.join("attachments"));
        let receipts = MutationReceipts::new(inbox_root.join("receipts"));
        let global_events = SessionEvents::new();
        let multiplex_hub = MultiplexHub::new(global_events.clone());

        Self {
            host,
            default_cwd: canonical_cwd,
            agent_dir,
            default_session_dir,
            inbox_root,
            max_idle_sessions: max_idle_sessions.max(1),
            touch_seq: Arc::new(AtomicU64::new(1)),
            closed: Arc::new(AtomicBool::new(false)),
            slots: Arc::new(AsyncMutex::new(BTreeMap::new())),
            created_lock: Arc::new(AsyncMutex::new(())),
            load_locks: KeyedLocks::new(),
            eviction_lock: Arc::new(AsyncMutex::new(())),
            removed: Arc::new(Mutex::new(BTreeSet::new())),
            scoped_models: Arc::new(Mutex::new(Vec::new())),
            invalid_auth: Arc::new(Mutex::new(BTreeSet::new())),
            images,
            receipts,
            global_events,
            multiplex_hub,
        }
    }

    pub fn host(&self) -> &KitHost {
        &self.host
    }

    pub fn default_cwd(&self) -> &Path {
        &self.default_cwd
    }

    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    pub fn inbox_root(&self) -> &Path {
        &self.inbox_root
    }

    fn next_touch(&self) -> u64 {
        self.touch_seq.fetch_add(1, Ordering::SeqCst)
    }

    fn created_file(&self) -> PathBuf {
        self.inbox_root.join("created.json")
    }

    fn legacy_created_times_file(&self) -> PathBuf {
        self.inbox_root.join("created-times.json")
    }

    fn unflushed_dir(&self) -> PathBuf {
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

    fn read_created_sync(&self) -> Result<BTreeMap<String, Option<String>>, SessionPoolError> {
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

    fn read_legacy_created_times_sync(&self) -> BTreeMap<String, String> {
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

    pub async fn active_slot_count(&self) -> usize {
        self.slots.lock().await.len()
    }

    pub async fn get_loaded(&self, id: &str) -> Option<SessionLease> {
        let guard = self.slots.lock().await;
        let slot = guard.get(id)?;
        slot.touch(self.next_touch());
        Some(self.lease(slot))
    }

    fn lease(&self, slot: &Arc<SessionSlot>) -> SessionLease {
        let pool = self.clone();
        let runtime = tokio::runtime::Handle::current();
        slot.lease(Arc::new(move || {
            if !pool.closed.load(Ordering::SeqCst) {
                let pool = pool.clone();
                runtime.spawn(async move {
                    pool.evict_excess_idle_slots().await;
                });
            }
        }))
    }

    async fn loaded_leases(&self) -> BTreeMap<String, SessionLease> {
        self.slots
            .lock()
            .await
            .iter()
            .map(|(id, slot)| (id.clone(), self.lease(slot)))
            .collect()
    }

    pub fn settings_manager(&self) -> SettingsManager {
        SettingsManager::create_with_trust(&self.default_cwd, &self.agent_dir, false)
    }

    pub fn set_scoped_models(&self, models: Vec<pi_runtime::ScopedModel>) {
        *self.scoped_models.lock().expect("scoped_models lock") = models;
    }

    pub fn scoped_models(&self) -> Vec<pi_runtime::ScopedModel> {
        self.scoped_models
            .lock()
            .expect("scoped_models lock")
            .clone()
    }

    /// Reloads shared `settings.json` and `models.json`/`models-store.json` without network calls,
    /// validating that shared configuration files are not corrupted.
    pub async fn refresh_services(&self) -> Result<SettingsManager, SessionPoolError> {
        let settings = self.settings_manager();
        if !settings.get_errors().is_empty() {
            return Err(SessionPoolError::ConfigurationUnavailable(
                "settings.json 配置无法解析，原文件已保留，请修正后重试。".to_owned(),
            ));
        }
        self.host
            .auth()
            .runtime()
            .refresh(Some(pi_ai::ModelsRefreshOptions {
                allow_network: Some(false),
                providers: None,
                force: Some(false),
            }))
            .await;
        if let Some(err) = self.host.auth().runtime().get_error() {
            return Err(SessionPoolError::ConfigurationUnavailable(err));
        }
        Ok(settings)
    }

    async fn build_slot(
        &self,
        manager: SessionManager,
        start_reason: &'static str,
        explicit_model_override: Option<Model>,
        thinking_override: Option<ModelThinkingLevel>,
        name_override: Option<String>,
    ) -> Result<Arc<SessionSlot>, SessionPoolError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(SessionPoolError::Stopping);
        }
        let session_id = manager.get_session_id().to_owned();
        let owner_lock = SessionOwnerLock::acquire(&self.agent_dir, &session_id).map_err(|e| {
            if e.starts_with("session_owned:") {
                SessionPoolError::SessionOwned(
                    e.trim_start_matches("session_owned:").trim().to_owned(),
                )
            } else {
                SessionPoolError::Conflict(e)
            }
        })?;

        let raw_cwd = PathBuf::from(manager.get_cwd());
        let effective_cwd = if raw_cwd.exists() {
            raw_cwd
        } else {
            self.default_cwd.clone()
        };

        let settings = self.settings_manager();
        let context_proj = manager.build_session_context();
        let target_model = explicit_model_override.unwrap_or_else(|| {
            let (prov, mid) = context_proj
                .model
                .map(|m| (m.provider, m.model_id))
                .unwrap_or_else(|| default_model_reference(&settings));
            explicit_model(self.host.auth().runtime(), &prov, &mid)
        });
        let target_thinking = thinking_override
            .or_else(|| parse_model_thinking_level_cli(&context_proj.thinking_level));

        let ui = ServerUiBridge::new();
        let bitlocker = BitLockerSessionBridge::new();

        let mut session_opts = pi_runtime::CreateAgentSessionOptions {
            cwd: Some(effective_cwd),
            agent_dir: Some(self.agent_dir.clone()),
            session_manager: Some(manager),
            model: Some(target_model),
            thinking_level: target_thinking,
            ui_context: Some(Arc::new(ui.clone())),
            mode: Some(ExtensionMode::Rpc),
            session_start_event: Some((start_reason.to_owned(), None)),
            ..Default::default()
        };
        let cached_scoped = self.scoped_models();
        if !cached_scoped.is_empty() {
            session_opts.scoped_models = cached_scoped;
        } else if let Some(patterns) = settings.get_enabled_models()
            && !patterns.is_empty()
        {
            let all_models = self.host.auth().runtime().get_all_models();
            let resolved = pi_runtime::resolve_model_scope_from_models(&patterns, &all_models);
            session_opts.scoped_models = resolved.scoped_models;
        }

        let hosted = Box::pin(self.host.open_session(HostedSessionOptions {
            session: session_opts,
            bitlocker_host: Some(Arc::new(bitlocker.clone())),
            telemetry_source: Some("web"),
        }))
        .await
        .map_err(SessionPoolError::ConfigurationUnavailable)?;

        if let Some(ref name) = name_override
            && !name.trim().is_empty()
            && hosted.session().session_name().is_none()
        {
            let _ = hosted.session().set_session_name(name.trim());
        }

        let unflushed_dir = self.unflushed_dir();
        let on_flushed: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |flushed_id: &str| {
            let path = unflushed_dir.join(format!("{flushed_id}.json"));
            if path.exists() {
                let _ = fs::remove_file(path);
            }
        });

        let slot = SessionSlot::assemble(
            session_id,
            hosted,
            owner_lock,
            ui,
            bitlocker,
            &self.inbox_root,
            self.images.clone(),
            self.global_events.clone(),
            self.invalid_auth.clone(),
            on_flushed,
            self.next_touch(),
        )
        .map_err(SessionPoolError::Conflict)?;

        Ok(slot)
    }

    /// Evicts oldest unpinned idle sessions when the number of idle slots exceeds `max_idle_sessions` (10).
    pub async fn evict_excess_idle_slots(&self) {
        let _eviction = self.eviction_lock.lock().await;
        while !self.closed.load(Ordering::SeqCst) {
            let Some((touched, id)) = self.oldest_excess_idle().await else {
                return;
            };
            // Removal, shutdown and owner-lock release share the same lock as cold opens.
            let _loading = self.load_locks.lock(&id).await;
            let slot = {
                let mut slots = self.slots.lock().await;
                let mut idle_count = 0;
                for slot in slots.values() {
                    if !slot.is_pinned().await {
                        idle_count += 1;
                    }
                }
                if idle_count <= self.max_idle_sessions {
                    return;
                }
                if let Some(slot) = slots.get(&id)
                    && slot.last_touched() == touched
                    && !slot.is_pinned().await
                {
                    slots.remove(&id)
                } else {
                    None
                }
            };
            if let Some(slot) = slot {
                slot.close().await;
                drop(slot);
            }
        }
    }

    async fn oldest_excess_idle(&self) -> Option<(u64, String)> {
        let slots = self.slots.lock().await;
        let mut idle = Vec::new();
        for (id, slot) in slots.iter() {
            if !slot.is_pinned().await {
                idle.push((slot.last_touched(), id.clone()));
            }
        }
        if idle.len() <= self.max_idle_sessions {
            return None;
        }
        idle.into_iter().min()
    }

    pub async fn open_saved(&self, id: &str) -> Result<SessionLease, SessionPoolError> {
        let lease = self.open_saved_pinned(id).await?;
        self.evict_excess_idle_slots().await;
        Ok(lease)
    }

    async fn open_saved_pinned(&self, id: &str) -> Result<SessionLease, SessionPoolError> {
        let _loading = self.load_locks.lock(id).await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(SessionPoolError::Stopping);
        }
        if self.is_deleted_sync(id)? {
            return Err(SessionPoolError::NotFound("会话不存在".to_owned()));
        }

        {
            let slots = self.slots.lock().await;
            if let Some(existing) = slots.get(id) {
                existing.touch(self.next_touch());
                return Ok(self.lease(existing));
            }
        }

        // Check if already persisted as a `.jsonl` file across all project directories
        let matches: Vec<SessionInfo> = self
            .list_all_project_sessions()
            .await
            .into_iter()
            .filter(|entry| entry.id == id)
            .collect();
        if matches.len() > 1 {
            return Err(SessionPoolError::NotFound(
                "会话不存在或 ID 不唯一".to_owned(),
            ));
        }

        let slot = if matches.len() == 1 {
            let path = matches[0].path.clone();
            let manager = SessionManager::open(&path, None, None).map_err(|err| match err {
                SessionStoreError::SingleWriterLocked(_) => SessionPoolError::SessionOwned(
                    "该会话正由另一端使用，请先关闭持有该会话的 TUI 或 Web 服务，再重新打开。"
                        .to_owned(),
                ),
                other => SessionPoolError::Conflict(other.to_string()),
            })?;
            // Clean up any stale unflushed snapshot if the session is now on disk
            self.remove_unflushed_sync(id);
            Box::pin(self.build_slot(manager, "resume", None, None, None)).await?
        } else if let Some(unflushed) = self.read_unflushed_sync(id) {
            let manager = SessionManager::create(
                &unflushed.cwd,
                PathBuf::from(&unflushed.session_dir),
                Some(NewSessionOptions {
                    id: Some(unflushed.id.clone()),
                    parent_session: unflushed.parent_session.clone(),
                }),
            )
            .map_err(|e| SessionPoolError::Conflict(e.to_string()))?;

            let model_override = unflushed
                .model
                .as_ref()
                .map(|m| explicit_model(self.host.auth().runtime(), &m.provider, &m.model_id));
            let thinking_override = unflushed
                .thinking_level
                .as_deref()
                .and_then(parse_model_thinking_level_cli);

            Box::pin(self.build_slot(
                manager,
                "new",
                model_override,
                thinking_override,
                unflushed.name.clone(),
            ))
            .await?
        } else {
            let created = self.read_created_sync()?;
            if !created.values().any(|v| v.as_deref() == Some(id)) {
                return Err(SessionPoolError::NotFound(
                    "会话不存在或 ID 不唯一".to_owned(),
                ));
            }
            let manager = SessionManager::create(
                self.default_cwd.to_string_lossy().into_owned(),
                &self.default_session_dir,
                Some(NewSessionOptions {
                    id: Some(id.to_owned()),
                    parent_session: None,
                }),
            )
            .map_err(|e| SessionPoolError::Conflict(e.to_string()))?;
            Box::pin(self.build_slot(manager, "new", None, None, None)).await?
        };

        let lease = self.lease(&slot);
        self.slots.lock().await.insert(id.to_owned(), slot);
        Ok(lease)
    }

    /// Creates or recovers an empty session identified by `request_key` (`created.json` + `unflushed/<id>.json`).
    pub async fn create_with_key(
        &self,
        request_key: &str,
        initial_model: Option<(String, String)>,
        initial_thinking: Option<ModelThinkingLevel>,
    ) -> Result<SessionLease, SessionPoolError> {
        if !is_valid_session_id_key(request_key) || self.closed.load(Ordering::SeqCst) {
            return Err(SessionPoolError::InvalidInput("创建请求无效".to_owned()));
        }

        let (session_id, is_new) = {
            let _serial = self.created_lock.lock().await;
            let mut entries = self.read_created_sync()?;
            if let Some(existing) = entries.get(request_key) {
                match existing {
                    Some(id) => (id.clone(), false),
                    None => {
                        return Err(SessionPoolError::Conflict(
                            "创建请求对应的会话已删除".to_owned(),
                        ));
                    }
                }
            } else {
                let id = uuid_v7();
                let created_at = now_iso_timestamp();
                let settings = self.settings_manager();
                let (def_prov, def_mid) = initial_model
                    .clone()
                    .unwrap_or_else(|| default_model_reference(&settings));
                let unflushed = UnflushedSessionSnapshot {
                    id: id.clone(),
                    created_at,
                    cwd: self.default_cwd.to_string_lossy().into_owned(),
                    session_dir: self.default_session_dir.to_string_lossy().into_owned(),
                    parent_session: None,
                    name: None,
                    model: Some(UnflushedModelRef {
                        provider: def_prov,
                        model_id: def_mid,
                    }),
                    thinking_level: initial_thinking.map(|t| t.as_str().to_owned()),
                };
                self.write_unflushed_sync(&unflushed)?;
                entries.insert(request_key.to_owned(), Some(id.clone()));
                atomic_write_json(&self.created_file(), &entries, "保存会话创建索引")
                    .await
                    .map_err(SessionPoolError::Storage)?;
                (id, true)
            }
        };

        let slot = self.open_saved(&session_id).await?;
        if is_new {
            if let Some((ref provider, ref model_id)) = initial_model
                && let Some(model) = self.host.auth().runtime().get_model(provider, model_id)
            {
                self.set_session_model(&session_id, model).await?;
            }
            if let Some(level) = initial_thinking {
                self.set_session_thinking_level(&session_id, level).await?;
            }
        }
        Ok(slot)
    }

    pub fn sync_unflushed_metadata(&self, slot: &SessionSlot) -> Result<(), SessionPoolError> {
        if slot.is_flushed() {
            self.remove_unflushed_sync(&slot.id);
            return Ok(());
        }
        let mut snap = self.read_unflushed_sync(&slot.id).unwrap_or_else(|| {
            let (cwd, session_dir, parent_session, created_at) =
                slot.session().with_session_manager(|sm| {
                    let header = sm.get_header();
                    (
                        sm.get_cwd().to_owned(),
                        sm.get_session_dir().to_string_lossy().into_owned(),
                        header.and_then(|h| h.parent_session.clone()),
                        header
                            .map(|h| h.timestamp.clone())
                            .unwrap_or_else(now_iso_timestamp),
                    )
                });
            UnflushedSessionSnapshot {
                id: slot.id.clone(),
                created_at,
                cwd,
                session_dir,
                parent_session,
                name: None,
                model: None,
                thinking_level: None,
            }
        });

        let model = slot.session().model();
        snap.name = slot.session().session_name();
        snap.model = Some(UnflushedModelRef {
            provider: model.provider,
            model_id: model.id,
        });
        snap.thinking_level = Some(slot.session().thinking_level().as_str().to_owned());
        self.write_unflushed_sync(&snap)
    }

    pub fn rename_session(
        &self,
        slot: &SessionSlot,
        name: &str,
    ) -> Result<Option<String>, SessionPoolError> {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 160 {
            return Err(SessionPoolError::InvalidInput(
                "请求的会话名称无效".to_owned(),
            ));
        }
        slot.session()
            .set_session_name(trimmed)
            .map_err(|e| SessionPoolError::Conflict(e.to_string()))?;
        self.sync_unflushed_metadata(slot)?;
        self.global_events
            .publish("state", &json!({"type": "sessions_changed"}));
        Ok(slot.session().session_name())
    }

    pub async fn set_session_model(
        &self,
        session_id: &str,
        model: Model,
    ) -> Result<(), SessionPoolError> {
        let slot = self.open_saved(session_id).await?;
        slot.session()
            .set_model(model)
            .await
            .map_err(SessionPoolError::InvalidInput)?;
        self.sync_unflushed_metadata(&slot)?;
        Ok(())
    }

    pub async fn set_session_thinking_level(
        &self,
        session_id: &str,
        level: ModelThinkingLevel,
    ) -> Result<ModelThinkingLevel, SessionPoolError> {
        let slot = self.open_saved(session_id).await?;
        slot.session()
            .set_thinking_level(level)
            .map_err(SessionPoolError::InvalidInput)?;
        self.sync_unflushed_metadata(&slot)?;
        Ok(slot.session().thinking_level())
    }

    /// Safely forks a new session from `source_id` without reopening the source file or releasing its lock.
    ///
    /// - Passing a `user` message `entry_id` branches strictly BEFORE that user message at `entry.parent_id()`.
    /// - Passing an explicit parent node `entry_id` (such as an assistant or metadata node from `ForkPicker`) branches at `entry_id`.
    ///
    /// If the branched manager has no flushed conversation on disk, persists `unflushed/<new_id>.json`.
    pub async fn fork(
        &self,
        source_id: &str,
        entry_id: &str,
    ) -> Result<SessionLease, SessionPoolError> {
        let source = self.open_saved(source_id).await?;
        if source.session().is_streaming()
            || source
                .inbox
                .has_delivering()
                .await
                .map_err(|e| SessionPoolError::Conflict(e.to_string()))?
        {
            return Err(SessionPoolError::Conflict("会话正在执行".to_owned()));
        }
        if !source.is_flushed() {
            return Err(SessionPoolError::Conflict("会话尚未保存".to_owned()));
        }

        let current_model = source.session().model();
        let current_thinking = source.session().thinking_level();

        let (
            new_manager,
            branch_model,
            branch_thinking,
            branch_name,
            source_cwd,
            source_dir,
            source_file,
        ) = source
            .session()
            .with_session_manager(|sm| -> Result<_, SessionPoolError> {
                let Some(target_entry) = sm.get_entry(entry_id) else {
                    return Err(SessionPoolError::InvalidInput("分支节点不存在".to_owned()));
                };
                let Some(src_file) = sm
                    .get_session_file()
                    .map(|p| p.to_string_lossy().into_owned())
                else {
                    return Err(SessionPoolError::Conflict("会话尚未保存".to_owned()));
                };
                let src_cwd = sm.get_cwd().to_owned();
                let src_dir = sm.get_session_dir().to_path_buf();

                // Determine the branch leaf ID:
                // If `entry_id` is a `user` message, `fork` branches BEFORE `entry_id` (`target_entry.parent_id()`).
                // Otherwise (e.g. `ForkPicker` already passed `target.parentId`), branch at `Some(entry_id)`.
                let is_user_msg = matches!(
                    target_entry,
                    SessionEntry::Message(m) if m.message.role() == "user"
                );
                let leaf_id_opt: Option<String> = if is_user_msg {
                    target_entry.parent_id().map(str::to_owned)
                } else {
                    Some(entry_id.to_owned())
                };

                // Compute effective model, thinking level, and session title up to `leaf_id_opt`
                let prefix_entries = match &leaf_id_opt {
                    Some(lid) => sm.get_branch(Some(lid)),
                    None => Vec::new(),
                };
                let mut eff_provider = current_model.provider.clone();
                let mut eff_model_id = current_model.id.clone();
                let mut eff_thinking = current_thinking.as_str().to_owned();
                let mut eff_name: Option<String> = None;

                for e in &prefix_entries {
                    match e {
                        SessionEntry::ModelChange(mc) => {
                            eff_provider.clone_from(&mc.provider);
                            eff_model_id.clone_from(&mc.model_id);
                        }
                        SessionEntry::ThinkingLevelChange(tlc) => {
                            eff_thinking.clone_from(&tlc.thinking_level);
                        }
                        SessionEntry::SessionInfo(info) => {
                            eff_name = info
                                .name
                                .as_deref()
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned);
                        }
                        SessionEntry::Message(m) => {
                            if let AgentMessage::Assistant(a) = &m.message {
                                eff_provider.clone_from(&a.provider);
                                eff_model_id.clone_from(&a.model);
                            }
                        }
                        _ => {}
                    }
                }
                if eff_name.is_none() {
                    eff_name = sm.get_session_name();
                }

                let new_sm = match leaf_id_opt {
                    Some(ref lid) => sm
                        .fork_branch_to_new_manager(lid)
                        .map_err(|e| SessionPoolError::Conflict(e.to_string()))?,
                    None => SessionManager::create(
                        src_cwd.clone(),
                        src_dir.clone(),
                        Some(NewSessionOptions {
                            id: None,
                            parent_session: Some(src_file.clone()),
                        }),
                    )
                    .map_err(|e| SessionPoolError::Conflict(e.to_string()))?,
                };

                Ok((
                    new_sm,
                    (eff_provider, eff_model_id),
                    eff_thinking,
                    eff_name,
                    src_cwd,
                    src_dir,
                    src_file,
                ))
            })?;

        let new_id = new_manager.get_session_id().to_owned();
        let is_flushed = new_manager.is_flushed()
            && new_manager
                .get_session_file()
                .is_some_and(|path| path.exists());

        // Step 4: If the forked session has not flushed a `.jsonl` file (e.g. forked before first user message),
        // atomically persist `unflushed/<new_id>.json` so LRU eviction or restart restores it losslessly.
        if !is_flushed {
            let created_at = new_manager
                .get_header()
                .map(|h| h.timestamp.clone())
                .unwrap_or_else(now_iso_timestamp);
            let snap = UnflushedSessionSnapshot {
                id: new_id.clone(),
                created_at,
                cwd: source_cwd,
                session_dir: source_dir.to_string_lossy().into_owned(),
                parent_session: Some(source_file),
                name: branch_name.clone(),
                model: Some(UnflushedModelRef {
                    provider: branch_model.0.clone(),
                    model_id: branch_model.1.clone(),
                }),
                thinking_level: Some(branch_thinking.clone()),
            };
            self.write_unflushed_sync(&snap)?;
        }

        let model_override = Some(explicit_model(
            self.host.auth().runtime(),
            &branch_model.0,
            &branch_model.1,
        ));
        let thinking_override = parse_model_thinking_level_cli(&branch_thinking);

        let slot = Box::pin(self.build_slot(
            new_manager,
            "fork",
            model_override,
            thinking_override,
            branch_name,
        ))
        .await?;

        let lease = self.lease(&slot);
        self.slots.lock().await.insert(new_id, slot);
        self.evict_excess_idle_slots().await;
        Ok(lease)
    }

    pub async fn delete_saved(&self, id: &str) -> Result<(), SessionPoolError> {
        let slot = self.open_saved(id).await?;
        if slot.session().is_streaming()
            || slot
                .inbox
                .has_delivering()
                .await
                .map_err(|e| SessionPoolError::Conflict(e.to_string()))?
        {
            return Err(SessionPoolError::Conflict("会话正在执行".to_owned()));
        }

        let matches: Vec<SessionInfo> = self
            .list_all_project_sessions()
            .await
            .into_iter()
            .filter(|entry| entry.id == id)
            .collect();
        if matches.len() > 1 {
            return Err(SessionPoolError::Conflict("会话 ID 不唯一".to_owned()));
        }

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

        slot.hosted.finalize_telemetry("delete").await;
        {
            let mut guard = self.slots.lock().await;
            guard.remove(id);
        }
        slot.close().await;

        if matches.len() == 1 {
            let _ = fs::remove_file(&matches[0].path);
        }
        self.remove_unflushed_sync(id);
        let _ = fs::remove_file(self.inbox_root.join(format!("{id}.json")));
        self.images.delete_session_attachments(id).await;

        {
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
                    let _ =
                        atomic_write_json(&self.created_file(), &created, "更新会话创建索引").await;
                }
            }
        }

        self.global_events
            .publish("state", &json!({"type": "sessions_changed"}));
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<SessionListRow>, SessionPoolError> {
        let saved = self.list_all_project_sessions().await;
        let active_slots = self.loaded_leases().await;

        let mut rows: Vec<SessionListRow> = Vec::new();
        for entry in saved {
            if self.is_deleted_sync(&entry.id)? {
                continue;
            }
            let live_slot = active_slots.get(&entry.id);
            let title = entry
                .name
                .clone()
                .or_else(|| {
                    (entry.first_message != "(no messages)"
                        && !entry.first_message.trim().is_empty())
                    .then(|| entry.first_message.chars().take(160).collect())
                })
                .unwrap_or_else(|| "新对话".to_owned());
            let mtime_iso = fs::metadata(&entry.path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format_iso_timestamp(d.as_millis() as u64))
                .unwrap_or_else(|| format_iso_timestamp(entry.modified_ms));

            rows.push(SessionListRow {
                id: entry.id,
                title,
                updated_at: Some(mtime_iso),
                running: live_slot.is_some_and(|s| s.session().is_streaming()),
            });
        }

        let created_map = self.read_created_sync()?;
        let legacy_times = self.read_legacy_created_times_sync();
        let unflushed_list = self.list_all_unflushed_sync();
        let mut unflushed_by_id: BTreeMap<String, UnflushedSessionSnapshot> = unflushed_list
            .into_iter()
            .map(|u| (u.id.clone(), u))
            .collect();

        let mut candidate_ids: BTreeSet<String> = BTreeSet::new();
        for id in created_map.values().flatten() {
            candidate_ids.insert(id.clone());
        }
        for id in unflushed_by_id.keys() {
            candidate_ids.insert(id.clone());
        }
        for id in active_slots.keys() {
            candidate_ids.insert(id.clone());
        }

        for id in candidate_ids {
            if self.is_deleted_sync(&id)? {
                continue;
            }
            let live = active_slots.get(&id);
            let unflushed = unflushed_by_id.remove(&id);

            let (live_name, first_user_text, last_msg_ts, header_ts, running) = match live {
                Some(slot) => {
                    let s = slot.session();
                    let state = s.state();
                    let first_text = state.messages.iter().find_map(|m| match m {
                        AgentMessage::User(u) => {
                            let t = match &u.content {
                                pi_ai::UserContent::Text(s) => s.trim().to_owned(),
                                pi_ai::UserContent::Blocks(blocks) => blocks
                                    .iter()
                                    .filter_map(|b| match b {
                                        pi_ai::UserContentBlock::Text(tc) => Some(tc.text.as_str()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ")
                                    .trim()
                                    .to_owned(),
                            };
                            (!t.is_empty()).then(|| t.chars().take(160).collect::<String>())
                        }
                        _ => None,
                    });
                    let last_ts = state
                        .messages
                        .last()
                        .map(AgentMessage::timestamp)
                        .filter(|&t| t > 0);
                    let h_ts =
                        s.with_session_manager(|sm| sm.get_header().map(|h| h.timestamp.clone()));
                    (
                        s.session_name(),
                        first_text,
                        last_ts,
                        h_ts,
                        s.is_streaming(),
                    )
                }
                None => (None, None, None, None, false),
            };

            let updated_at = last_msg_ts
                .map(format_iso_timestamp)
                .or_else(|| unflushed.as_ref().map(|u| u.created_at.clone()))
                .or_else(|| legacy_times.get(&id).cloned())
                .or(header_ts);

            let computed_title = live_name
                .or_else(|| unflushed.as_ref().and_then(|u| u.name.clone()))
                .or(first_user_text);

            if let Some(existing_row) = rows.iter_mut().find(|r| r.id == id) {
                existing_row.running = running;
                if last_msg_ts.is_some()
                    && let Some(ref ts) = updated_at
                    && ts.as_str() > existing_row.updated_at.as_deref().unwrap_or("")
                {
                    existing_row.updated_at = Some(ts.clone());
                }
                if let Some(t) = computed_title {
                    existing_row.title = t;
                }
            } else {
                rows.push(SessionListRow {
                    id,
                    title: computed_title.unwrap_or_else(|| "新对话".to_owned()),
                    updated_at,
                    running,
                });
            }
        }

        rows.sort_by(|a, b| match (&a.updated_at, &b.updated_at) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(ta), Some(tb)) => tb.cmp(ta),
        });
        Ok(rows)
    }

    pub async fn lifecycle_snapshot(&self, stopping: bool) -> Vec<LifecycleSessionRow> {
        let rows = if stopping {
            Vec::new()
        } else {
            self.list().await.unwrap_or_default()
        };
        let active = self.loaded_leases().await;
        active
            .into_values()
            .map(|slot| {
                let title = rows
                    .iter()
                    .find(|r| r.id == slot.id)
                    .map(|r| r.title.clone())
                    .or_else(|| slot.session().session_name())
                    .unwrap_or_else(|| "会话".to_owned());
                LifecycleSessionRow {
                    id: slot.id.clone(),
                    title,
                    running: !slot.session().is_idle(),
                }
            })
            .collect()
    }

    pub async fn has_running_session_for_provider(&self, provider_id: &str) -> bool {
        let active = self.loaded_leases().await;
        active
            .into_values()
            .any(|slot| !slot.session().is_idle() && slot.session().model().provider == provider_id)
    }

    pub(crate) async fn update_multiplex_subscriptions(
        &self,
        caller: CallerIdentity,
        client_id: &str,
        update: MultiplexSubscriptionUpdate,
    ) -> Result<Vec<String>, (Option<String>, SessionPoolError)> {
        if update.sessions.len() > MAX_MULTIPLEX_SUBSCRIPTIONS
            || update
                .sessions
                .iter()
                .any(|id| !is_valid_session_id_key(id))
        {
            return Err((
                None,
                SessionPoolError::InvalidInput(format!(
                    "聚合流订阅列表无效或超过 {MAX_MULTIPLEX_SUBSCRIPTIONS} 个会话"
                )),
            ));
        }

        let mut leases = Vec::new();
        let result = async {
            let mut sources = BTreeMap::new();
            for id in &update.sessions {
                if let Entry::Vacant(entry) = sources.entry(id.clone()) {
                    let lease = self
                        .open_saved_pinned(id)
                        .await
                        .map_err(|err| (Some(id.clone()), err))?;
                    entry.insert(lease.events.clone());
                    leases.push(lease);
                }
            }
            let resolver: SessionEventsResolver = Arc::new(move |id| sources.get(id).cloned());
            self.multiplex_hub
                .update_subscriptions(caller, client_id, update, &resolver)
                .map_err(|msg| (None, SessionPoolError::InvalidInput(msg)))
        }
        .await;
        drop(leases);
        self.evict_excess_idle_slots().await;
        result
    }

    pub async fn revoke_web_connections(&self) {
        self.global_events.revoke_web_subscribers();
        self.multiplex_hub.revoke_web_slots();
        let active = self.loaded_leases().await;
        for slot in active.into_values() {
            slot.events.revoke_web_subscribers();
        }
    }

    /// Gracefully pauses all session inboxes, aborts running generations, finalizes telemetry,
    /// and closes all slots on server shutdown.
    pub async fn shutdown(&self) -> Result<(), String> {
        self.closed.store(true, Ordering::SeqCst);
        let _eviction = self.eviction_lock.lock().await;
        let active: Vec<Arc<SessionSlot>> = self.slots.lock().await.values().cloned().collect();

        for slot in &active {
            let _ = slot.inbox.pause().await;
        }
        for slot in &active {
            slot.session().abort().await;
        }
        if active.iter().any(|slot| !slot.session().is_idle()) {
            self.closed.store(false, Ordering::SeqCst);
            return Err("会话尚未停止，未关闭程序。请稍后重试。".to_owned());
        }

        self.host.shutdown_with_reason("quit").await;

        let drained: Vec<Arc<SessionSlot>> = {
            let mut guard = self.slots.lock().await;
            std::mem::take(&mut *guard).into_values().collect()
        };
        for slot in drained {
            slot.close().await;
        }
        self.multiplex_hub.close_all();
        self.global_events.close_all();
        Ok(())
    }
}

pub fn read_session_summary_from_file(path: &Path) -> Option<SessionInfo> {
    build_session_info_sync(path)
}

pub fn parse_timestamp_or_zero(iso: &str) -> u64 {
    parse_iso_timestamp_ms(iso).unwrap_or(0)
}

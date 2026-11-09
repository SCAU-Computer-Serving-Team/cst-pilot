use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use cst_pilot::KitHost;
use http::HeaderMap;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::discovery::{
    cleanup_discovery_file, generate_random_hex_token, strip_windows_verbatim_prefix,
};
use crate::error::{MutationError, ServiceError, ServiceErrorKind};
use crate::exports::ExportManager;
use crate::locks::KeyedLocks;
use crate::mode::{CallerIdentity, ModeController, ModeControllerConfig};
use crate::oauth::OAuthFlowManager;
use crate::project_files::ProjectFileCache;
use crate::runbook::{RunbookOpenerFn, default_runbook_opener};
use crate::session::receipts::{ReceiptError, compute_request_signature};
use crate::session::{DEFAULT_MAX_IDLE_SESSIONS, SessionPool};
use crate::settings::{DesktopSettingsStore, WebSettingsStore};

pub const DEFAULT_SERVER_PORT: u16 = 52831;

#[derive(Clone)]
pub struct ServerConfig {
    pub kit_root: PathBuf,
    pub cwd: Option<PathBuf>,
    pub static_dir: Option<PathBuf>,
    pub artifacts_root: Option<PathBuf>,
    pub port: Option<u16>,
    pub app_token: Option<String>,
    pub mode_config: ModeControllerConfig,
    pub max_idle_sessions: usize,
    pub acquire_instance_lock: bool,
    pub open_runbook: Option<RunbookOpenerFn>,
}

impl ServerConfig {
    pub fn new(kit_root: impl Into<PathBuf>) -> Self {
        Self {
            kit_root: kit_root.into(),
            cwd: None,
            static_dir: None,
            artifacts_root: None,
            port: None,
            app_token: None,
            mode_config: ModeControllerConfig::default(),
            max_idle_sessions: DEFAULT_MAX_IDLE_SESSIONS,
            acquire_instance_lock: true,
            open_runbook: None,
        }
    }
}

pub struct ServerState {
    pub host: KitHost,
    kit_root: PathBuf,
    agent_dir: PathBuf,
    static_dir: PathBuf,
    artifacts_root: PathBuf,
    kit_version: String,
    port: AtomicU16,
    app_token: String,
    pub mode: ModeController,
    pub locks: KeyedLocks,
    pub pool: SessionPool,
    pub web_settings: WebSettingsStore,
    pub desktop_settings: DesktopSettingsStore,
    pub oauth_flows: OAuthFlowManager,
    pub file_cache: ProjectFileCache,
    pub exports: ExportManager,
    pub open_runbook: RunbookOpenerFn,
    pub server_stop: CancellationToken,
    shutdown_mutex: AsyncMutex<bool>,
}

impl ServerState {
    pub fn new(host: KitHost, config: &ServerConfig) -> Arc<Self> {
        let kit_root = strip_windows_verbatim_prefix(host.kit().root());
        let agent_dir = strip_windows_verbatim_prefix(&host.kit().agent_dir());
        let cwd = config.cwd.clone().unwrap_or_else(|| kit_root.clone());
        let static_dir = config
            .static_dir
            .clone()
            .unwrap_or_else(|| agent_dir.join("extensions/web/static"));
        let artifacts_root = config
            .artifacts_root
            .clone()
            .unwrap_or_else(|| kit_root.join("artifacts"));
        let kit_version = read_kit_version(&kit_root);
        let initial_port = config.port.unwrap_or(DEFAULT_SERVER_PORT);
        let app_token = config
            .app_token
            .clone()
            .unwrap_or_else(generate_random_hex_token);

        let mode = ModeController::new(config.mode_config.clone());
        let locks = KeyedLocks::new();
        let pool = SessionPool::with_max_idle(host.clone(), cwd, config.max_idle_sessions);
        let web_settings = WebSettingsStore::new(&agent_dir);
        let desktop_settings = DesktopSettingsStore::new(&agent_dir);

        let global_for_oauth = pool.global_events.clone();
        let invalid_for_oauth = pool.invalid_auth.clone();
        let host_for_oauth = host.clone();
        let oauth_flows = OAuthFlowManager::new(move |provider_id, view| {
            let mut state_evt = serde_json::to_value(view).unwrap_or_else(|_| json!({}));
            if let Some(obj) = state_evt.as_object_mut() {
                obj.insert("type".to_owned(), json!("oauth_state"));
                obj.insert("providerId".to_owned(), json!(provider_id));
            }
            global_for_oauth.publish("state", &state_evt);
            if let Some(ref dc) = view.device_code
                && let Ok(mut dc_val) = serde_json::to_value(dc)
                && let Some(obj) = dc_val.as_object_mut()
            {
                obj.insert("type".to_owned(), json!("oauth_device_code"));
                obj.insert("providerId".to_owned(), json!(provider_id));
                global_for_oauth.publish("state", &dc_val);
            }
            if view.state == "succeeded" {
                invalid_for_oauth
                    .lock()
                    .expect("invalid_auth lock")
                    .remove(provider_id);
                if provider_id == "cstoa" {
                    host_for_oauth.auth().invalidate_account();
                }
                global_for_oauth.publish(
                    "state",
                    &json!({
                        "type": "oauth_result",
                        "providerId": provider_id,
                        "ok": true,
                    }),
                );
                global_for_oauth.publish(
                    "state",
                    &json!({
                        "type": "auth_changed",
                        "providerId": provider_id,
                    }),
                );
            }
        });

        Arc::new(Self {
            host,
            kit_root,
            agent_dir,
            static_dir,
            artifacts_root,
            kit_version,
            port: AtomicU16::new(initial_port),
            app_token,
            mode,
            locks,
            pool,
            web_settings,
            desktop_settings,
            oauth_flows,
            file_cache: ProjectFileCache::new(),
            exports: ExportManager::new(),
            open_runbook: config
                .open_runbook
                .clone()
                .unwrap_or_else(default_runbook_opener),
            server_stop: CancellationToken::new(),
            shutdown_mutex: AsyncMutex::new(false),
        })
    }

    pub fn kit_root(&self) -> &Path {
        &self.kit_root
    }

    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    pub fn static_dir(&self) -> &Path {
        &self.static_dir
    }

    pub fn artifacts_root(&self) -> &Path {
        &self.artifacts_root
    }

    pub fn kit_version(&self) -> &str {
        &self.kit_version
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    pub fn set_bound_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    pub fn app_token(&self) -> &str {
        &self.app_token
    }

    /// Executes an idempotent write mutation guarded by `ModeController::admit_write`
    /// and `MutationReceipts::once_value`.
    pub async fn write_once<E, F, Fut>(
        &self,
        caller: CallerIdentity,
        method: &str,
        pathname: &str,
        body: &Value,
        headers: Option<&HeaderMap>,
        action: F,
    ) -> Result<Value, MutationError<E>>
    where
        E: std::fmt::Display,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, E>>,
    {
        let key = headers
            .and_then(|h| h.get("idempotency-key"))
            .and_then(|v| v.to_str().ok())
            .ok_or(MutationError::InvalidKey)?;

        // Channel admission ends at durable acceptance. Keep the activity lease
        // until the accepted mutation settles, so update handoff cannot overtake it.
        let (acceptance, _activity) = self
            .mode
            .admit_write(caller, Some(self.host.kit().activity_gate()))
            .await
            .map_err(MutationError::Admission)?
            .into_parts();

        let signature = compute_request_signature(method, pathname, body);
        let err_slot: Arc<std::sync::Mutex<Option<E>>> = Arc::new(std::sync::Mutex::new(None));
        let err_slot_inner = err_slot.clone();

        let res = self
            .pool
            .receipts
            .once_value_with_acceptance(key, &signature, acceptance, || async move {
                match Box::pin(action()).await {
                    Ok(v) => Ok(v),
                    Err(action_err) => {
                        let msg = action_err.to_string();
                        *err_slot_inner.lock().expect("err_slot") = Some(action_err);
                        Err(ReceiptError::Action(msg))
                    }
                }
            })
            .await;

        match res {
            Ok(val) => Ok(val),
            Err(ReceiptError::Action(_)) => match err_slot.lock().expect("err_slot").take() {
                Some(captured) => Err(MutationError::Action(captured)),
                None => Err(MutationError::Receipt(ReceiptError::Action(
                    "操作执行失败".to_owned(),
                ))),
            },
            Err(other) => Err(MutationError::Receipt(other)),
        }
    }

    /// Acquires `session:{session_id}` before admitting and persisting an idempotent mutation.
    /// A request waiting for its session never retains channel admission.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_once_for_session<E, F, Fut>(
        &self,
        caller: CallerIdentity,
        method: &str,
        pathname: &str,
        body: &Value,
        headers: &HeaderMap,
        session_id: &str,
        action: F,
    ) -> Result<Value, MutationError<E>>
    where
        E: std::fmt::Display,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, E>>,
    {
        let _session_guard = self.locks.lock(format!("session:{session_id}")).await;
        self.write_once(caller, method, pathname, body, Some(headers), action)
            .await
    }

    /// Pauses all session inboxes, stops running generations, finalizes telemetry (5s budget),
    /// removes `.state/server.json`, and signals server stop.
    pub async fn execute_shutdown(&self) -> Result<(), ServiceError> {
        let mut guard = self.shutdown_mutex.lock().await;
        if *guard {
            return Ok(());
        }
        self.mode.begin_shutdown();
        self.oauth_flows.close_all().await;
        self.pool
            .shutdown()
            .await
            .map_err(|msg| ServiceError::new(ServiceErrorKind::Conflict, "conflict", msg))?;
        cleanup_discovery_file(&self.kit_root, std::process::id(), self.port());
        *guard = true;
        let stop = self.server_stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            stop.cancel();
        });
        Ok(())
    }
}

fn read_kit_version(kit_root: &Path) -> String {
    let pkg_path = kit_root.join("package.json");
    if let Ok(bytes) = fs::read(pkg_path)
        && let Ok(val) = serde_json::from_slice::<Value>(&bytes)
        && let Some(ver) = val.get("version").and_then(Value::as_str)
    {
        return ver.to_owned();
    }
    env!("CARGO_PKG_VERSION").to_owned()
}

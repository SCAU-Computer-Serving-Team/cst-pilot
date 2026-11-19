use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_os::{ProcessIdentity, UpdateReport};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bootstrap::{
    BootstrapOptions, DesktopInstanceGuard, DesktopInstanceOutcome, VerifiedServerEndpoint,
    acquire_or_activate_desktop_instance, ensure_server_running, recover_pending_update_transaction,
};
use crate::error::ClientError;
use crate::http::AppHttpClient;
use crate::outbox::{
    ExitCheckReport, OutboxItem, SessionOutbox, WebSwitchCheck,
};
use crate::session_store::{SessionStore, SessionViewSnapshot};
use crate::sse::{
    KeepaliveSignal, MultiplexSignal, run_keepalive_sse_loop, run_multiplex_sse_loop,
};
use crate::state::{
    AccountStatusDto, ChannelModeDto, ClientSnapshot, DesktopSettingsDto, DesktopSettingsPatchDto,
    MissingKeyClaimDto, ModelsSnapshot, ProviderStatusDto, SessionSummary, UiHintsDto,
    UpdateStateDto, UpdateStatusDto, WebOpenDto,
};
use crate::tool_presenter::ToolPresenterCache;
use crate::workspace::{ComposerDraft, DraftKey, WorkspaceState};

pub const MAX_MULTIPLEX_SUBSCRIPTIONS: usize = 32;
const SUBSCRIPTION_DEBOUNCE_MS: u64 = 40;
const STATE_REFRESH_COALESCE_MS: u64 = 25;

pub enum ClientBootstrapResult {
    Started(CstClient),
    ActivatedExisting { owner_pid: u32 },
}

enum WorkerCommand {
    ActivateSession {
        session_id: Option<String>,
        reply: oneshot::Sender<Result<Option<Arc<SessionViewSnapshot>>, ClientError>>,
    },
    RefreshGlobals {
        reply: oneshot::Sender<Result<(), ClientError>>,
    },
    RefreshModels {
        session_id: Option<String>,
        reply: oneshot::Sender<Result<Arc<ModelsSnapshot>, ClientError>>,
    },
    SendDraft {
        target: DraftKey,
        draft: ComposerDraft,
        reply: oneshot::Sender<Result<SendSubmissionOutcome, ClientError>>,
    },
    ReconcileUncertain {
        reply: oneshot::Sender<Result<usize, ClientError>>,
    },
    PatchDesktopSettings {
        patch: DesktopSettingsPatchDto,
        reply: oneshot::Sender<Result<DesktopSettingsDto, ClientError>>,
    },
    OpenWeb {
        reply: oneshot::Sender<Result<WebOpenDto, ClientError>>,
    },
    CloseWeb {
        reply: oneshot::Sender<Result<(), ClientError>>,
    },
    ShutdownAndExit {
        reply: oneshot::Sender<Result<(), ClientError>>,
    },
}

#[derive(Debug, Clone)]
pub struct SendSubmissionOutcome {
    pub session_id: String,
    pub client_message_id: String,
}

struct SharedClientCore {
    kit_root: PathBuf,
    app_identity: ProcessIdentity,
    http: AppHttpClient,
    snapshot: Mutex<Arc<ClientSnapshot>>,
    workspace: Mutex<WorkspaceState>,
    outbox: Mutex<SessionOutbox>,
    presenter_cache: Mutex<ToolPresenterCache>,
    revision_rx: watch::Receiver<u64>,
    wakeup_rx: watch::Receiver<u64>,
    cmd_tx: mpsc::UnboundedSender<WorkerCommand>,
    stop: CancellationToken,
}

/// Cloneable handle shared between `cst-desktop` views and Phase 10 `cst-tui`.
#[derive(Clone)]
pub struct ClientHandle {
    core: Arc<SharedClientCore>,
}

/// Owns the desktop single-instance lock and the dedicated 2-worker Tokio runtime.
pub struct CstClient {
    handle: ClientHandle,
    _instance_guard: Option<DesktopInstanceGuard>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for CstClient {
    fn drop(&mut self) {
        self.handle.core.stop.cancel();
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_millis(500));
        }
    }
}

impl CstClient {
    /// Full desktop startup flow:
    /// 1. Acquire per-kit desktop single-instance lock (or wake existing window & return `ActivatedExisting`)
    /// 2. Recover any interrupted `.state/update/transaction.json` via `cst-updater.exe --recover`
    /// 3. Verify or spawn `cst-server.exe` (`CREATE_NO_WINDOW`) with bidirectional token/identity checks
    /// 4. Start dedicated 2-worker Tokio runtime with isolated `/api/app/events` keepalive + on-demand skeleton load
    pub fn bootstrap(
        kit_root: &Path,
        options: BootstrapOptions,
    ) -> Result<ClientBootstrapResult, ClientError> {
        let instance_guard = match acquire_or_activate_desktop_instance(kit_root, &options)? {
            DesktopInstanceOutcome::ActivatedExisting { owner_pid } => {
                return Ok(ClientBootstrapResult::ActivatedExisting { owner_pid });
            }
            DesktopInstanceOutcome::Acquired(guard) => guard,
        };

        let _recovered = recover_pending_update_transaction(kit_root, &options)?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("cst-client-worker")
            .build()
            .map_err(|e| ClientError::BootstrapFailed(format!("无法创建客户端异步运行时：{e}")))?;

        let endpoint = runtime.block_on(ensure_server_running(kit_root, &options))?;
        let wakeup_rx = instance_guard.subscribe_wakeup();
        let handle = spawn_client_workers(runtime.handle(), endpoint, wakeup_rx)?;

        Ok(ClientBootstrapResult::Started(Self {
            handle,
            _instance_guard: Some(instance_guard),
            runtime: Some(runtime),
        }))
    }

    /// Connects to an already-verified `VerifiedServerEndpoint` on an existing Tokio runtime handle (useful for tests).
    pub fn from_verified_endpoint(
        runtime_handle: &tokio::runtime::Handle,
        endpoint: VerifiedServerEndpoint,
        wakeup_rx: Option<watch::Receiver<u64>>,
    ) -> Result<ClientHandle, ClientError> {
        let rx = wakeup_rx.unwrap_or_else(|| {
            let (_tx, rx) = watch::channel(0u64);
            rx
        });
        spawn_client_workers(runtime_handle, endpoint, rx)
    }

    pub fn handle(&self) -> &ClientHandle {
        &self.handle
    }
}

impl ClientHandle {
    pub fn kit_root(&self) -> &Path {
        &self.core.kit_root
    }

    pub fn app_identity(&self) -> &ProcessIdentity {
        &self.core.app_identity
    }

    pub fn http(&self) -> &AppHttpClient {
        &self.core.http
    }

    /// Lock-free (nanosecond `Arc::clone`) read of the latest immutable `ClientSnapshot`.
    pub fn snapshot(&self) -> Arc<ClientSnapshot> {
        Arc::clone(&self.core.snapshot.lock().expect("snapshot lock"))
    }

    /// Subscribes to monotonically increasing snapshot revision numbers.
    pub fn subscribe_revisions(&self) -> watch::Receiver<u64> {
        self.core.revision_rx.clone()
    }

    /// Subscribes to single-instance window activation wakeup signals.
    pub fn subscribe_wakeup(&self) -> watch::Receiver<u64> {
        self.core.wakeup_rx.clone()
    }

    pub fn with_workspace<R>(&self, f: impl FnOnce(&WorkspaceState) -> R) -> R {
        let guard = self.core.workspace.lock().expect("workspace lock");
        f(&guard)
    }

    pub fn with_workspace_mut<R>(&self, f: impl FnOnce(&mut WorkspaceState) -> R) -> R {
        let mut guard = self.core.workspace.lock().expect("workspace lock");
        f(&mut guard)
    }

    pub fn with_outbox<R>(&self, f: impl FnOnce(&SessionOutbox) -> R) -> R {
        let guard = self.core.outbox.lock().expect("outbox lock");
        f(&guard)
    }

    pub fn with_presenter_cache<R>(&self, f: impl FnOnce(&mut ToolPresenterCache) -> R) -> R {
        let mut guard = self
            .core
            .presenter_cache
            .lock()
            .expect("presenter cache lock");
        f(&mut guard)
    }

    pub fn check_web_switch(&self) -> WebSwitchCheck {
        let unresolved_local_outbox = self.with_outbox(SessionOutbox::total_unresolved_count);
        let unsent_drafts = self.with_workspace(WorkspaceState::unsent_draft_count);
        WebSwitchCheck {
            unresolved_local_outbox,
            unsent_drafts,
        }
    }

    pub async fn check_exit(&self) -> Result<ExitCheckReport, ClientError> {
        let lifecycle = self.core.http.lifecycle().await?;
        let unresolved_local_outbox = self.with_outbox(SessionOutbox::total_unresolved_count);
        let unsent_drafts = self.with_workspace(WorkspaceState::unsent_draft_count);
        Ok(ExitCheckReport::from_sources(
            &lifecycle,
            unresolved_local_outbox,
            unsent_drafts,
        ))
    }

    pub async fn activate_session(
        &self,
        session_id: Option<String>,
    ) -> Result<Option<Arc<SessionViewSnapshot>>, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::ActivateSession {
                session_id,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn refresh_globals(&self) -> Result<(), ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::RefreshGlobals { reply: tx })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn refresh_models(
        &self,
        session_id: Option<String>,
    ) -> Result<Arc<ModelsSnapshot>, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::RefreshModels {
                session_id,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    /// Takes the draft from `WorkspaceState` (or restores it on home-page / missing-key failure)
    /// and executes optimistic two-phase submission through `SessionOutbox`.
    pub async fn submit_draft(
        &self,
        target: DraftKey,
    ) -> Result<SendSubmissionOutcome, ClientError> {
        let draft = self.with_workspace_mut(|ws| ws.take_draft(&target));
        if draft.is_empty() {
            return Err(ClientError::BadRequest {
                code: "empty_message".into(),
                message: "请输入消息内容或添加图片。".into(),
            });
        }
        let (tx, rx) = oneshot::channel();
        if self
            .core
            .cmd_tx
            .send(WorkerCommand::SendDraft {
                target: target.clone(),
                draft: draft.clone(),
                reply: tx,
            })
            .is_err()
        {
            self.with_workspace_mut(|ws| ws.restore_draft(target, draft));
            return Err(ClientError::Disconnected("客户端后台线程已退出。".into()));
        }
        match rx.await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(err)) => {
                // Home-page creation failure or `409 credentials_required` preserves the user's draft in the composer!
                if matches!(target, DraftKey::NewSession) || err.is_credentials_required() {
                    self.with_workspace_mut(|ws| ws.restore_draft(target, draft));
                }
                Err(err)
            }
            Err(_) => Err(ClientError::Disconnected("客户端后台响应中断。".into())),
        }
    }

    pub async fn reconcile_uncertain_outbox(&self) -> Result<usize, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::ReconcileUncertain { reply: tx })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn patch_desktop_settings(
        &self,
        patch: DesktopSettingsPatchDto,
    ) -> Result<DesktopSettingsDto, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::PatchDesktopSettings { patch, reply: tx })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn open_web(&self) -> Result<WebOpenDto, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::OpenWeb { reply: tx })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn close_web(&self) -> Result<(), ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::CloseWeb { reply: tx })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn claim_missing_key_prompt(&self) -> Result<MissingKeyClaimDto, ClientError> {
        self.core.http.claim_missing_key_prompt(None).await
    }

    pub async fn get_ui_hints(&self) -> Result<UiHintsDto, ClientError> {
        self.core.http.get_ui_hints().await
    }

    pub async fn get_update_status(&self) -> Result<UpdateStateDto, ClientError> {
        self.core.http.get_update_status().await
    }

    pub async fn check_update(&self) -> Result<UpdateStatusDto, ClientError> {
        self.core.http.check_update().await
    }

    pub async fn prepare_update(&self) -> Result<UpdateStatusDto, ClientError> {
        self.core.http.prepare_update().await
    }

    pub async fn cancel_update(
        &self,
        operation_id: Option<&str>,
    ) -> Result<UpdateStatusDto, ClientError> {
        self.core.http.cancel_update(operation_id).await
    }

    pub async fn launch_update(&self, ui: &str) -> Result<Value, ClientError> {
        self.core
            .http
            .launch_update(self.core.app_identity.pid, self.core.app_identity.created, ui)
            .await
    }

    pub async fn last_update_report(&self) -> Option<UpdateReport> {
        self.snapshot()
            .update_state
            .as_ref()
            .and_then(|u| u.last_report.clone())
    }

    pub async fn shutdown_and_exit(&self) -> Result<(), ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::ShutdownAndExit { reply: tx })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub fn stop(&self) {
        self.core.stop.cancel();
    }
}

fn spawn_client_workers(
    rt_handle: &tokio::runtime::Handle,
    endpoint: VerifiedServerEndpoint,
    wakeup_rx: watch::Receiver<u64>,
) -> Result<ClientHandle, ClientError> {
    let app_identity = ProcessIdentity::current().map_err(ClientError::BootstrapFailed)?;
    let initial_snapshot = Arc::new(ClientSnapshot {
        revision: 1,
        connected: false,
        reconnecting: false,
        channel_mode: ChannelModeDto::AppActive,
        version: endpoint.version.clone(),
        ..ClientSnapshot::default()
    });
    let (revision_tx, revision_rx) = watch::channel(1u64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let stop = CancellationToken::new();

    let core = Arc::new(SharedClientCore {
        kit_root: endpoint.kit_root.clone(),
        app_identity,
        http: endpoint.http.clone(),
        snapshot: Mutex::new(initial_snapshot),
        workspace: Mutex::new(WorkspaceState::default()),
        outbox: Mutex::new(SessionOutbox::default()),
        presenter_cache: Mutex::new(ToolPresenterCache::default()),
        revision_rx,
        wakeup_rx,
        cmd_tx,
        stop: stop.clone(),
    });

    let (keepalive_tx, keepalive_rx) = mpsc::unbounded_channel();
    rt_handle.spawn(run_keepalive_sse_loop(
        endpoint.http.clone(),
        keepalive_tx,
        stop.clone(),
    ));

    let reducer_core = Arc::clone(&core);
    rt_handle.spawn(async move {
        run_state_reducer_loop(reducer_core, revision_tx, keepalive_rx, cmd_rx).await;
    });

    Ok(ClientHandle { core })
}

#[derive(Default)]
struct DirtyRefreshFlags {
    sessions_and_state: bool,
    models: bool,
    auth: bool,
    desktop_settings: bool,
    update_status: bool,
}

impl DirtyRefreshFlags {
    fn any(&self) -> bool {
        self.sessions_and_state
            || self.models
            || self.auth
            || self.desktop_settings
            || self.update_status
    }

    fn mark_all(&mut self) {
        self.sessions_and_state = true;
        self.models = true;
        self.auth = true;
        self.desktop_settings = true;
        self.update_status = true;
    }
}

type SessionLoadReply = oneshot::Sender<Result<Option<Arc<SessionViewSnapshot>>, ClientError>>;

struct GlobalsFetchedPayload {
    state_val: Option<Value>,
    models: Option<ModelsSnapshot>,
    auth: Option<Vec<ProviderStatusDto>>,
    account: Option<AccountStatusDto>,
    desktop_settings: Result<DesktopSettingsDto, ClientError>,
    update_state: Option<UpdateStateDto>,
}

enum InternalAsyncCompletion {
    GlobalsFetched(Box<GlobalsFetchedPayload>),
    SessionSnapshotLoaded {
        session_id: String,
        result: Result<Value, ClientError>,
        waiters: Vec<SessionLoadReply>,
    },
}

async fn run_state_reducer_loop(
    core: Arc<SharedClientCore>,
    revision_tx: watch::Sender<u64>,
    mut keepalive_rx: mpsc::UnboundedReceiver<KeepaliveSignal>,
    mut cmd_rx: mpsc::UnboundedReceiver<WorkerCommand>,
) {
    let client_id = Uuid::new_v4().to_string();
    let (mux_tx, mut mux_rx) = mpsc::unbounded_channel::<MultiplexSignal>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<InternalAsyncCompletion>();

    let mut session_store = SessionStore::default();
    let mut pending_session_waiters: BTreeMap<String, Vec<SessionLoadReply>> = BTreeMap::new();
    let mut desired_subs: Vec<String> = Vec::new();
    let mut sub_debounce_pending = false;
    let mut dirty = DirtyRefreshFlags::default();
    dirty.mark_all();
    let mut refresh_in_flight = false;

    let mut mux_cancel: Option<CancellationToken> = None;

    let publish = |core: &Arc<SharedClientCore>,
                   revision_tx: &watch::Sender<u64>,
                   mutator: &mut dyn FnMut(&mut ClientSnapshot)| {
        let next_rev = {
            let mut guard = core.snapshot.lock().expect("snapshot lock");
            let mut next = (**guard).clone();
            mutator(&mut next);
            next.revision = next.revision.wrapping_add(1);
            let rev = next.revision;
            *guard = Arc::new(next);
            rev
        };
        revision_tx.send_replace(next_rev);
    };

    let ensure_multiplex_running =
        |mode: ChannelModeDto,
         mux_cancel: &mut Option<CancellationToken>,
         http: &AppHttpClient,
         cid: &str,
         tx: &mpsc::UnboundedSender<MultiplexSignal>,
         root_stop: &CancellationToken| {
            if mode == ChannelModeDto::WebActive {
                if let Some(tok) = mux_cancel.take() {
                    tok.cancel();
                }
            } else if mux_cancel.is_none() {
                let tok = root_stop.child_token();
                *mux_cancel = Some(tok.clone());
                tokio::spawn(run_multiplex_sse_loop(
                    http.clone(),
                    cid.to_owned(),
                    None,
                    tx.clone(),
                    tok,
                ));
            }
        };

    // Start multiplex stream initially in AppActive
    ensure_multiplex_running(
        ChannelModeDto::AppActive,
        &mut mux_cancel,
        &core.http,
        &client_id,
        &mux_tx,
        &core.stop,
    );

    let sub_sleep = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(sub_sleep);
    let refresh_sleep = tokio::time::sleep(Duration::from_millis(1));
    tokio::pin!(refresh_sleep);
    let mut refresh_timer_armed = true;

    loop {
        tokio::select! {
            biased;
            _ = core.stop.cancelled() => {
                if let Some(tok) = mux_cancel.take() {
                    tok.cancel();
                }
                break;
            }
            Some(sig) = keepalive_rx.recv() => {
                match sig {
                    KeepaliveSignal::Connected => {
                        let was_reconnecting = core.snapshot.lock().expect("snapshot lock").reconnecting;
                        publish(&core, &revision_tx, &mut |snap| {
                            snap.connected = true;
                            snap.reconnecting = false;
                        });
                        dirty.mark_all();
                        if !refresh_timer_armed {
                            refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(5));
                            refresh_timer_armed = true;
                        }
                        if was_reconnecting {
                            let c_clone = Arc::clone(&core);
                            tokio::spawn(async move {
                                let _ = reconcile_uncertain_items(&c_clone).await;
                            });
                        }
                    }
                    KeepaliveSignal::Disconnected => {
                        publish(&core, &revision_tx, &mut |snap| {
                            snap.connected = false;
                            snap.reconnecting = true;
                        });
                    }
                    KeepaliveSignal::Reset => {
                        dirty.mark_all();
                        if !refresh_timer_armed {
                            refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(STATE_REFRESH_COALESCE_MS));
                            refresh_timer_armed = true;
                        }
                    }
                    KeepaliveSignal::StateEvent { payload, .. } => {
                        handle_global_state_payload(
                            &core,
                            &revision_tx,
                            &payload,
                            &mut dirty,
                            &mut mux_cancel,
                            &client_id,
                            &mux_tx,
                        );
                        if dirty.any() && !refresh_timer_armed {
                            refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(STATE_REFRESH_COALESCE_MS));
                            refresh_timer_armed = true;
                        }
                    }
                }
            }
            Some(msig) = mux_rx.recv() => {
                match msig {
                    MultiplexSignal::StreamReady { subscribed_sessions } => {
                        let current_desired = compute_desired_subscriptions(&core);
                        if subscribed_sessions != current_desired {
                            desired_subs = current_desired;
                            sub_debounce_pending = true;
                            sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(5));
                        }
                    }
                    MultiplexSignal::StateEvent { payload, .. } => {
                        handle_global_state_payload(
                            &core,
                            &revision_tx,
                            &payload,
                            &mut dirty,
                            &mut mux_cancel,
                            &client_id,
                            &mux_tx,
                        );
                        if dirty.any() && !refresh_timer_armed {
                            refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(STATE_REFRESH_COALESCE_MS));
                            refresh_timer_armed = true;
                        }
                    }
                    MultiplexSignal::SessionEvent { session_id, event, .. } => {
                        // Observe clientMessageId in incoming user message to clear optimistic outbox
                        if let Some(cmid) = event
                            .get("message")
                            .and_then(|m| m.get("clientMessageId"))
                            .and_then(Value::as_str)
                        {
                            core.outbox
                                .lock()
                                .expect("outbox lock")
                                .mark_accepted(&session_id, cmid);
                        }
                        if session_store.apply_session_event(&session_id, event) {
                            let map = Arc::new(session_store.export_ready_map());
                            publish(&core, &revision_tx, &mut |snap| {
                                snap.loaded_sessions = Arc::clone(&map);
                            });
                        }
                    }
                    MultiplexSignal::QueueEvent { session_id, queue, .. } => {
                        let ids: Vec<String> = queue
                            .get("items")
                            .and_then(Value::as_array)
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|i| i.get("id").and_then(Value::as_str).map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !ids.is_empty() {
                            core.outbox
                                .lock()
                                .expect("outbox lock")
                                .reconcile_server_inbox_ids(&session_id, &ids);
                        }
                        if session_store.apply_queue_snapshot(&session_id, queue) {
                            let map = Arc::new(session_store.export_ready_map());
                            publish(&core, &revision_tx, &mut |snap| {
                                snap.loaded_sessions = Arc::clone(&map);
                            });
                        }
                    }
                    MultiplexSignal::ResetGlobal => {
                        dirty.mark_all();
                        if !refresh_timer_armed {
                            refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(STATE_REFRESH_COALESCE_MS));
                            refresh_timer_armed = true;
                        }
                    }
                    MultiplexSignal::ResetSession { session_id } => {
                        session_store.reset_to_loading(&session_id);
                        spawn_session_snapshot_fetch(
                            core.http.clone(),
                            done_tx.clone(),
                            session_id,
                            Vec::new(),
                        );
                    }
                    MultiplexSignal::Disconnected => {}
                }
            }
            Some(done) = done_rx.recv() => {
                match done {
                    InternalAsyncCompletion::GlobalsFetched(fetched) => {
                        let GlobalsFetchedPayload {
                            state_val,
                            models,
                            auth,
                            account,
                            desktop_settings,
                            update_state,
                        } = *fetched;
                        refresh_in_flight = false;
                        publish(&core, &revision_tx, &mut |snap| {
                            if let Some(ref st) = state_val {
                                if let Some(v) = st.get("version").and_then(Value::as_str) {
                                    snap.version = v.to_owned();
                                }
                                if let Some(cm) = st.get("channelMode").and_then(Value::as_str) {
                                    snap.channel_mode = if cm == "web_active" {
                                        ChannelModeDto::WebActive
                                    } else {
                                        ChannelModeDto::AppActive
                                    };
                                }
                                if let Some(cmds) = st.get("commands").and_then(Value::as_array) {
                                    snap.commands = Arc::new(
                                        cmds.iter()
                                            .filter_map(Value::as_str)
                                            .map(str::to_owned)
                                            .collect(),
                                    );
                                }
                                if let Some(sess_arr) = st.get("sessions").and_then(Value::as_array)
                                    && let Ok(parsed) = serde_json::from_value::<Vec<SessionSummary>>(Value::Array(sess_arr.clone()))
                                {
                                    snap.sessions = Arc::new(parsed);
                                }
                            }
                            if let Some(m) = models.clone() {
                                snap.models = Some(Arc::new(m));
                            }
                            if let Some(a) = auth.clone() {
                                snap.auth_providers = Some(Arc::new(a));
                            }
                            if let Some(acc) = account.clone() {
                                snap.account = Some(Arc::new(acc));
                            }
                            match &desktop_settings {
                                Ok(ds) => {
                                    snap.desktop_settings = ds.clone();
                                    snap.desktop_settings_corrupt = false;
                                }
                                Err(ClientError::ConfigurationUnavailable(_)) => {
                                    snap.desktop_settings_corrupt = true;
                                }
                                Err(_) => {}
                            }
                            if let Some(us) = update_state.clone() {
                                snap.update_state = Some(Arc::new(us));
                            }
                        });

                        let cur_mode = core.snapshot.lock().expect("snapshot lock").channel_mode;
                        ensure_multiplex_running(
                            cur_mode,
                            &mut mux_cancel,
                            &core.http,
                            &client_id,
                            &mux_tx,
                            &core.stop,
                        );
                        let next_subs = compute_desired_subscriptions(&core);
                        if cur_mode == ChannelModeDto::AppActive && next_subs != desired_subs {
                            desired_subs = next_subs;
                            sub_debounce_pending = true;
                            sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(SUBSCRIPTION_DEBOUNCE_MS));
                        }
                        if dirty.any() && !refresh_timer_armed {
                            refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(STATE_REFRESH_COALESCE_MS));
                            refresh_timer_armed = true;
                        }
                    }
                    InternalAsyncCompletion::SessionSnapshotLoaded {
                        session_id,
                        result,
                        mut waiters,
                    } => {
                        if let Some(extra) = pending_session_waiters.remove(&session_id) {
                            waiters.extend(extra);
                        }
                        match result {
                            Ok(payload) => {
                                let loaded = session_store.finish_loading_snapshot(&session_id, payload);
                                if let Some(ref snap_arc) = loaded {
                                    let inbox_ids = snap_arc.server_inbox_ids();
                                    if !inbox_ids.is_empty() {
                                        core.outbox
                                            .lock()
                                            .expect("outbox lock")
                                            .reconcile_server_inbox_ids(&session_id, &inbox_ids);
                                    }
                                }
                                let pinned = collect_pinned_sessions(&core);
                                session_store.evict_lru(&pinned);
                                let map = Arc::new(session_store.export_ready_map());
                                publish(&core, &revision_tx, &mut |snap| {
                                    snap.loaded_sessions = Arc::clone(&map);
                                });
                                for waiter in waiters {
                                    let _ = waiter.send(Ok(loaded.clone()));
                                }
                            }
                            Err(err) => {
                                session_store.cancel_loading(&session_id);
                                for waiter in waiters {
                                    let _ = waiter.send(Err(err.clone()));
                                }
                            }
                        }
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    WorkerCommand::ActivateSession { session_id, reply } => {
                        core.workspace
                            .lock()
                            .expect("workspace lock")
                            .set_active_session(session_id.clone());

                        let Some(sid) = session_id else {
                            desired_subs = compute_desired_subscriptions(&core);
                            sub_debounce_pending = true;
                            sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(SUBSCRIPTION_DEBOUNCE_MS));
                            let _ = reply.send(Ok(None));
                            continue;
                        };

                        if session_store.has_ready(&sid) {
                            session_store.touch(&sid);
                            let existing = session_store.export_ready_map().get(&sid).cloned();
                            desired_subs = compute_desired_subscriptions(&core);
                            sub_debounce_pending = true;
                            sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(SUBSCRIPTION_DEBOUNCE_MS));
                            let _ = reply.send(Ok(existing));
                            continue;
                        }

                        if session_store.is_loading(&sid) {
                            pending_session_waiters.entry(sid).or_default().push(reply);
                            continue;
                        }

                        // Zero-gap initial subscription:
                        // 1. Mark session as `Loading` to buffer incoming SSE frames
                        // 2. Submit `PUT /subscriptions` FIRST (in AppActive)
                        // 3. Fetch `GET /api/sessions/<sid>` and merge buffered frames
                        session_store.begin_loading_if_absent(&sid);
                        desired_subs = compute_desired_subscriptions(&core);
                        sub_debounce_pending = false;
                        let is_app_active = core.snapshot.lock().expect("snapshot lock").channel_mode == ChannelModeDto::AppActive;
                        let http = core.http.clone();
                        let cid = client_id.clone();
                        let subs_to_put = desired_subs.clone();
                        let done_tx_clone = done_tx.clone();
                        tokio::spawn(async move {
                            if is_app_active && !subs_to_put.is_empty() {
                                let _ = http.put_multiplex_subscriptions(&cid, &subs_to_put, None).await;
                            }
                            let res = http.get_session_snapshot(&sid).await;
                            let _ = done_tx_clone.send(InternalAsyncCompletion::SessionSnapshotLoaded {
                                session_id: sid,
                                result: res,
                                waiters: vec![reply],
                            });
                        });
                    }
                    WorkerCommand::RefreshGlobals { reply } => {
                        let http = core.http.clone();
                        let done_tx_clone = done_tx.clone();
                        tokio::spawn(async move {
                            let (state_res, models_res, auth_res, acc_res, ds_res, upd_res) = tokio::join!(
                                http.state(),
                                http.get_models(None),
                                http.get_auth_providers(),
                                http.get_account(false),
                                http.get_desktop_settings(),
                                http.get_update_status(),
                            );
                            let first_err = state_res.as_ref().err().cloned();
                            let _ = done_tx_clone.send(InternalAsyncCompletion::GlobalsFetched(
                                Box::new(GlobalsFetchedPayload {
                                    state_val: state_res.ok(),
                                    models: models_res.ok(),
                                    auth: auth_res.ok(),
                                    account: acc_res.ok(),
                                    desktop_settings: ds_res,
                                    update_state: upd_res.ok(),
                                }),
                            ));
                            let _ = reply.send(match first_err {
                                Some(e) => Err(e),
                                None => Ok(()),
                            });
                        });
                    }
                    WorkerCommand::RefreshModels { session_id, reply } => {
                        let http = core.http.clone();
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            match http.get_models(session_id.as_deref()).await {
                                Ok(models) => {
                                    let arc = Arc::new(models);
                                    let arc_for_snap = Arc::clone(&arc);
                                    let next_rev = {
                                        let mut guard = core_clone.snapshot.lock().expect("snapshot lock");
                                        let mut next = (**guard).clone();
                                        next.models = Some(arc_for_snap);
                                        next.revision = next.revision.wrapping_add(1);
                                        let r = next.revision;
                                        *guard = Arc::new(next);
                                        r
                                    };
                                    rev_tx.send_replace(next_rev);
                                    let _ = reply.send(Ok(arc));
                                }
                                Err(e) => {
                                    let _ = reply.send(Err(e));
                                }
                            }
                        });
                    }
                    WorkerCommand::SendDraft { target, draft, reply } => {
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            let res = execute_send_draft(&core_clone, &rev_tx, target, &draft).await;
                            let _ = reply.send(res);
                        });
                    }
                    WorkerCommand::ReconcileUncertain { reply } => {
                        let core_clone = Arc::clone(&core);
                        tokio::spawn(async move {
                            let res = reconcile_uncertain_items(&core_clone).await;
                            let _ = reply.send(res);
                        });
                    }
                    WorkerCommand::PatchDesktopSettings { patch, reply } => {
                        let http = core.http.clone();
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            let res = http.patch_desktop_settings(&patch).await;
                            if let Ok(ref saved) = res {
                                let saved_clone = saved.clone();
                                let next_rev = {
                                    let mut guard = core_clone.snapshot.lock().expect("snapshot lock");
                                    let mut next = (**guard).clone();
                                    next.desktop_settings = saved_clone;
                                    next.desktop_settings_corrupt = false;
                                    next.revision = next.revision.wrapping_add(1);
                                    let r = next.revision;
                                    *guard = Arc::new(next);
                                    r
                                };
                                rev_tx.send_replace(next_rev);
                            }
                            let _ = reply.send(res);
                        });
                    }
                    WorkerCommand::OpenWeb { reply } => {
                        let res = core.http.open_web().await;
                        if let Ok(ref opened) = res {
                            publish(&core, &revision_tx, &mut |snap| {
                                snap.channel_mode = opened.mode;
                            });
                            ensure_multiplex_running(
                                opened.mode,
                                &mut mux_cancel,
                                &core.http,
                                &client_id,
                                &mux_tx,
                                &core.stop,
                            );
                        }
                        let _ = reply.send(res);
                    }
                    WorkerCommand::CloseWeb { reply } => {
                        let res = core.http.close_web().await;
                        if res.is_ok() {
                            publish(&core, &revision_tx, &mut |snap| {
                                snap.channel_mode = ChannelModeDto::AppActive;
                            });
                            ensure_multiplex_running(
                                ChannelModeDto::AppActive,
                                &mut mux_cancel,
                                &core.http,
                                &client_id,
                                &mux_tx,
                                &core.stop,
                            );
                            dirty.sessions_and_state = true;
                            if let Some(active_sid) = core.workspace.lock().expect("workspace lock").active_session_id().map(str::to_owned) {
                                session_store.reset_to_loading(&active_sid);
                                spawn_session_snapshot_fetch(
                                    core.http.clone(),
                                    done_tx.clone(),
                                    active_sid,
                                    Vec::new(),
                                );
                            }
                            if !refresh_timer_armed {
                                refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(10));
                                refresh_timer_armed = true;
                            }
                        }
                        let _ = reply.send(res.map(|_| ()));
                    }
                    WorkerCommand::ShutdownAndExit { reply } => {
                        let res = core.http.app_exit(Duration::from_millis(5_500)).await;
                        core.stop.cancel();
                        let _ = reply.send(res.map(|_| ()));
                        break;
                    }
                }
            }
            () = &mut sub_sleep, if sub_debounce_pending => {
                sub_debounce_pending = false;
                let cur_mode = core.snapshot.lock().expect("snapshot lock").channel_mode;
                if cur_mode == ChannelModeDto::AppActive {
                    let http = core.http.clone();
                    let cid = client_id.clone();
                    let subs = desired_subs.clone();
                    tokio::spawn(async move {
                        let _ = http.put_multiplex_subscriptions(&cid, &subs, None).await;
                    });
                }
            }
            () = &mut refresh_sleep, if refresh_timer_armed => {
                refresh_timer_armed = false;
                if !dirty.any() || refresh_in_flight {
                    continue;
                }
                refresh_in_flight = true;
                let flags = std::mem::take(&mut dirty);
                let http = core.http.clone();
                let done_tx_clone = done_tx.clone();
                let is_web_active = core.snapshot.lock().expect("snapshot lock").channel_mode == ChannelModeDto::WebActive;
                tokio::spawn(async move {
                    let state_fut = async {
                        if flags.sessions_and_state {
                            if is_web_active {
                                // In WebActive, also refresh `/api/sessions` explicitly with `X-CST-Client: app`
                                let (st, sess) = tokio::join!(http.state(), http.list_sessions());
                                match (st, sess) {
                                    (Ok(mut st_val), Ok(rows)) => {
                                        if let Some(obj) = st_val.as_object_mut() {
                                            obj.insert(
                                                "sessions".into(),
                                                serde_json::to_value(rows).unwrap_or(json!([])),
                                            );
                                        }
                                        Some(st_val)
                                    }
                                    (Ok(st_val), Err(_)) => Some(st_val),
                                    _ => None,
                                }
                            } else {
                                http.state().await.ok()
                            }
                        } else {
                            None
                        }
                    };
                    let models_fut = async {
                        if flags.models {
                            http.get_models(None).await.ok()
                        } else {
                            None
                        }
                    };
                    let auth_fut = async {
                        if flags.auth {
                            http.get_auth_providers().await.ok()
                        } else {
                            None
                        }
                    };
                    let acc_fut = async {
                        if flags.auth {
                            http.get_account(false).await.ok()
                        } else {
                            None
                        }
                    };
                    let ds_fut = async {
                        if flags.desktop_settings {
                            http.get_desktop_settings().await
                        } else {
                            Err(ClientError::NotFound {
                                code: "skip".into(),
                                message: String::new(),
                            })
                        }
                    };
                    let upd_fut = async {
                        if flags.update_status {
                            http.get_update_status().await.ok()
                        } else {
                            None
                        }
                    };
                    let (state_val, models, auth, account, desktop_settings, update_state) =
                        tokio::join!(state_fut, models_fut, auth_fut, acc_fut, ds_fut, upd_fut);
                    let _ = done_tx_clone.send(InternalAsyncCompletion::GlobalsFetched(
                        Box::new(GlobalsFetchedPayload {
                            state_val,
                            models,
                            auth,
                            account,
                            desktop_settings,
                            update_state,
                        }),
                    ));
                });
            }
        }
    }
}

fn handle_global_state_payload(
    core: &Arc<SharedClientCore>,
    revision_tx: &watch::Sender<u64>,
    payload: &Value,
    dirty: &mut DirtyRefreshFlags,
    mux_cancel: &mut Option<CancellationToken>,
    client_id: &str,
    mux_tx: &mpsc::UnboundedSender<MultiplexSignal>,
) {
    let Some(ev_type) = payload.get("type").and_then(Value::as_str) else {
        return;
    };
    match ev_type {
        "sessions_changed" => {
            dirty.sessions_and_state = true;
        }
        "models_changed" => {
            dirty.models = true;
        }
        "auth_changed" => {
            dirty.auth = true;
            dirty.models = true;
        }
        "settings_changed" => match payload.get("scope").and_then(Value::as_str) {
            Some("desktop") => dirty.desktop_settings = true,
            Some("shared") => dirty.models = true,
            Some("web") => {}
            _ => {
                dirty.desktop_settings = true;
                dirty.models = true;
            }
        },
        "channel_mode_changed" => {
            let new_mode = if payload.get("mode").and_then(Value::as_str) == Some("web_active") {
                ChannelModeDto::WebActive
            } else {
                ChannelModeDto::AppActive
            };
            let changed = {
                let mut guard = core.snapshot.lock().expect("snapshot lock");
                if guard.channel_mode != new_mode {
                    let mut next = (**guard).clone();
                    next.channel_mode = new_mode;
                    next.revision = next.revision.wrapping_add(1);
                    let r = next.revision;
                    *guard = Arc::new(next);
                    revision_tx.send_replace(r);
                    true
                } else {
                    false
                }
            };
            if changed {
                if new_mode == ChannelModeDto::WebActive {
                    if let Some(tok) = mux_cancel.take() {
                        tok.cancel();
                    }
                } else if mux_cancel.is_none() {
                    let tok = core.stop.child_token();
                    *mux_cancel = Some(tok.clone());
                    tokio::spawn(run_multiplex_sse_loop(
                        core.http.clone(),
                        client_id.to_owned(),
                        None,
                        mux_tx.clone(),
                        tok,
                    ));
                }
                dirty.sessions_and_state = true;
            }
        }
        "update_changed" => {
            if let Some(status_val) = payload.get("status")
                && let Ok(status_dto) =
                    serde_json::from_value::<UpdateStatusDto>(status_val.clone())
            {
                let mut guard = core.snapshot.lock().expect("snapshot lock");
                let mut next = (**guard).clone();
                let current_ver = next.version.clone();
                let last_report = next
                    .update_state
                    .as_ref()
                    .and_then(|u| u.last_report.clone());
                next.update_state = Some(Arc::new(UpdateStateDto {
                    current_version: current_ver,
                    status: status_dto,
                    last_report,
                }));
                next.revision = next.revision.wrapping_add(1);
                let r = next.revision;
                *guard = Arc::new(next);
                revision_tx.send_replace(r);
            } else {
                dirty.update_status = true;
            }
        }
        "oauth_state" | "oauth_device_code" | "oauth_result" => {
            let mut guard = core.snapshot.lock().expect("snapshot lock");
            let mut next = (**guard).clone();
            next.last_oauth_event = Some(Arc::new(payload.clone()));
            next.revision = next.revision.wrapping_add(1);
            let r = next.revision;
            *guard = Arc::new(next);
            revision_tx.send_replace(r);
            if ev_type == "oauth_result" {
                dirty.auth = true;
                dirty.models = true;
            }
        }
        _ => {}
    }
}

fn compute_desired_subscriptions(core: &Arc<SharedClientCore>) -> Vec<String> {
    let mut ordered: Vec<String> = Vec::new();
    if let Some(active) = core
        .workspace
        .lock()
        .expect("workspace lock")
        .active_session_id()
    {
        ordered.push(active.to_owned());
    }
    let snap = Arc::clone(&core.snapshot.lock().expect("snapshot lock"));
    for s in snap.sessions.iter() {
        if s.running && !ordered.contains(&s.id) && ordered.len() < MAX_MULTIPLEX_SUBSCRIPTIONS {
            ordered.push(s.id.clone());
        }
    }
    ordered
}

fn collect_pinned_sessions(core: &Arc<SharedClientCore>) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    {
        let ws = core.workspace.lock().expect("workspace lock");
        if let Some(active) = ws.active_session_id() {
            set.insert(active.to_owned());
        }
        let snap = Arc::clone(&core.snapshot.lock().expect("snapshot lock"));
        for s in snap.sessions.iter() {
            if ws.has_non_empty_draft(&s.id) {
                set.insert(s.id.clone());
            }
        }
    }
    {
        let ob = core.outbox.lock().expect("outbox lock");
        let snap = Arc::clone(&core.snapshot.lock().expect("snapshot lock"));
        for s in snap.sessions.iter() {
            if ob.has_unresolved_for_session(&s.id) {
                set.insert(s.id.clone());
            }
        }
    }
    set
}

fn spawn_session_snapshot_fetch(
    http: AppHttpClient,
    done_tx: mpsc::UnboundedSender<InternalAsyncCompletion>,
    session_id: String,
    waiters: Vec<SessionLoadReply>,
) {
    tokio::spawn(async move {
        let result = http.get_session_snapshot(&session_id).await;
        let _ = done_tx.send(InternalAsyncCompletion::SessionSnapshotLoaded {
            session_id,
            result,
            waiters,
        });
    });
}

async fn execute_send_draft(
    core: &Arc<SharedClientCore>,
    revision_tx: &watch::Sender<u64>,
    target: DraftKey,
    draft: &ComposerDraft,
) -> Result<SendSubmissionOutcome, ClientError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let initial_session_id = match &target {
        DraftKey::Session(sid) => Some(sid.clone()),
        DraftKey::NewSession => None,
    };
    let item = OutboxItem::from_draft(initial_session_id.clone(), draft, now_ms)?;
    let client_message_id = item.client_message_id.clone();

    let session_id = match initial_session_id {
        Some(sid) => {
            if core.outbox.lock().expect("outbox lock").is_session_paused(&sid) {
                core.outbox
                    .lock()
                    .expect("outbox lock")
                    .enqueue_session(&sid, item);
                bump_revision(core, revision_tx);
                return Err(ClientError::StorageFull(
                    "本机磁盘空间已满，当前会话后续消息已暂停发送。".into(),
                ));
            }
            core.outbox
                .lock()
                .expect("outbox lock")
                .enqueue_session(&sid, item);
            bump_revision(core, revision_tx);
            sid
        }
        None => {
            let req_key = item
                .creation_request_key
                .clone()
                .expect("home item has creation_request_key");
            core.outbox.lock().expect("outbox lock").enqueue_home(item)?;
            bump_revision(core, revision_tx);

            match core
                .http
                .create_session(
                    &req_key,
                    draft.selected_provider.as_deref(),
                    draft.selected_model_id.as_deref(),
                    draft.selected_thinking_level.as_deref(),
                )
                .await
            {
                Ok(created_sid) => {
                    core.outbox
                        .lock()
                        .expect("outbox lock")
                        .bind_home_to_session(&created_sid);
                    core.workspace
                        .lock()
                        .expect("workspace lock")
                        .set_active_session(Some(created_sid.clone()));
                    bump_revision(core, revision_tx);
                    created_sid
                }
                Err(err) if err.is_disconnected() => {
                    core.outbox
                        .lock()
                        .expect("outbox lock")
                        .mark_home_uncertain(err.to_string());
                    bump_revision(core, revision_tx);
                    return Err(err);
                }
                Err(err) => {
                    core.outbox.lock().expect("outbox lock").clear_home();
                    bump_revision(core, revision_tx);
                    return Err(err);
                }
            }
        }
    };

    let mut msg_body = serde_json::Map::new();
    msg_body.insert("id".into(), Value::String(client_message_id.clone()));
    msg_body.insert("text".into(), Value::String(draft.text.clone()));
    msg_body.insert(
        "delivery".into(),
        Value::String(draft.send_mode.as_api_delivery().to_owned()),
    );
    if let Some(skill) = draft.skill_name() {
        msg_body.insert("skill".into(), Value::String(skill.to_owned()));
    }
    if !draft.images.is_empty() {
        let imgs: Vec<Value> = draft.images.iter().map(|h| h.to_base64_payload()).collect();
        msg_body.insert("images".into(), Value::Array(imgs));
    }

    match core
        .http
        .post_session_message(&session_id, &client_message_id, &Value::Object(msg_body))
        .await
    {
        Ok(_) => {
            // Accepted (`202`): server durable inbox now owns delivery -> release local outbox & image handles!
            core.outbox
                .lock()
                .expect("outbox lock")
                .mark_accepted(&session_id, &client_message_id);
            bump_revision(core, revision_tx);
            Ok(SendSubmissionOutcome {
                session_id,
                client_message_id,
            })
        }
        Err(err) => {
            if err.is_credentials_required() {
                core.outbox
                    .lock()
                    .expect("outbox lock")
                    .remove_item(&session_id, &client_message_id);
            } else {
                core.outbox.lock().expect("outbox lock").mark_session_error(
                    &session_id,
                    &client_message_id,
                    &err,
                );
            }
            bump_revision(core, revision_tx);
            Err(err)
        }
    }
}

/// Read-only receipt reconciliation after a disconnect (`GET /api/sessions/creation/<key>`
/// and `GET /api/sessions/<id>/messages/<msgId>`). Never auto-resends unaccepted payloads!
async fn reconcile_uncertain_items(core: &Arc<SharedClientCore>) -> Result<usize, ClientError> {
    let items = core.outbox.lock().expect("outbox lock").uncertain_items();
    let mut resolved = 0usize;

    for (maybe_sid, item) in items {
        match maybe_sid {
            None => {
                if let Some(req_key) = &item.creation_request_key {
                    let (status, created_sid) =
                        core.http.check_session_creation(req_key).await?;
                    if status == "done"
                        && let Some(sid) = created_sid
                    {
                        core.outbox
                            .lock()
                            .expect("outbox lock")
                            .bind_home_to_session(&sid);
                        core.outbox.lock().expect("outbox lock").mark_session_error(
                            &sid,
                            &item.client_message_id,
                            &ClientError::NotFound {
                                code: "message_not_delivered".into(),
                                message: "会话已创建，首条消息未送达，请点击重试。".into(),
                            },
                        );
                        resolved += 1;
                    } else if status == "missing" {
                        core.outbox.lock().expect("outbox lock").clear_home();
                        resolved += 1;
                    }
                }
            }
            Some(sid) => {
                if let Some(_server_item) = core
                    .http
                    .check_message_receipt(&sid, &item.client_message_id)
                    .await?
                {
                    core.outbox
                        .lock()
                        .expect("outbox lock")
                        .mark_accepted(&sid, &item.client_message_id);
                    resolved += 1;
                } else {
                    core.outbox.lock().expect("outbox lock").mark_session_error(
                        &sid,
                        &item.client_message_id,
                        &ClientError::NotFound {
                            code: "message_not_received".into(),
                            message: "网络中断前服务端未收到该消息，可手动重试或编辑。".into(),
                        },
                    );
                    resolved += 1;
                }
            }
        }
    }

    Ok(resolved)
}

fn bump_revision(core: &Arc<SharedClientCore>, revision_tx: &watch::Sender<u64>) {
    let next_rev = {
        let mut guard = core.snapshot.lock().expect("snapshot lock");
        let mut next = (**guard).clone();
        next.revision = next.revision.wrapping_add(1);
        let r = next.revision;
        *guard = Arc::new(next);
        r
    };
    revision_tx.send_replace(next_rev);
}

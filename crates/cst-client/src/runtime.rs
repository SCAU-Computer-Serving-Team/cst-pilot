mod delivery;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_os::{ProcessIdentity, UpdateReport};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bootstrap::{
    BootstrapOptions, DesktopInstanceGuard, DesktopInstanceOutcome, VerifiedServerEndpoint,
    acquire_or_activate_desktop_instance, ensure_server_running,
    recover_pending_update_transaction,
};
use crate::error::ClientError;
use crate::http::AppHttpClient;
use crate::outbox::{ExitCheckReport, OutboxItem, SessionOutbox, WebSwitchCheck};
use crate::session_store::{
    DEFAULT_MAX_CACHED_BYTES, DEFAULT_MAX_CACHED_SESSIONS, DEFAULT_MAX_LOADING_BUFFER_BYTES,
    FinishLoadingOutcome, SessionStore, SessionViewSnapshot,
};
use crate::sse::{
    BoundedMultiplexSender, DEFAULT_MAX_MULTIPLEX_QUEUE_BYTES, KeepaliveSignal, MultiplexSignal,
    SessionCursor, bounded_multiplex_channel, format_single_cursor, run_keepalive_sse_loop,
    run_multiplex_sse_loop,
};
use crate::state::{
    AccountStatusDto, ChannelModeDto, ClientSnapshot, DesktopSettingsDto, DesktopSettingsPatchDto,
    MissingKeyClaimDto, ModelsSnapshot, ProviderStatusDto, SessionSummary, UiHintsDto,
    UpdateStateDto, UpdateStatusDto, WebOpenDto,
};
use crate::tool_presenter::ToolPresenterCache;
use crate::workspace::{ComposerDraft, DraftKey, WorkspaceState};

use self::delivery::{execute_retry_outbox, execute_send_draft, reconcile_uncertain_items};

pub const MAX_MULTIPLEX_SUBSCRIPTIONS: usize = 32;
const SUBSCRIPTION_DEBOUNCE_MS: u64 = 40;
const STATE_REFRESH_COALESCE_MS: u64 = 25;
const STARTUP_DESKTOP_SETTINGS_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct ClientRuntimeOptions {
    pub max_multiplex_queue_bytes: usize,
    pub max_loading_buffer_bytes: usize,
    pub max_cached_sessions: usize,
    pub max_cached_bytes: usize,
}

impl Default for ClientRuntimeOptions {
    fn default() -> Self {
        Self {
            max_multiplex_queue_bytes: DEFAULT_MAX_MULTIPLEX_QUEUE_BYTES,
            max_loading_buffer_bytes: DEFAULT_MAX_LOADING_BUFFER_BYTES,
            max_cached_sessions: DEFAULT_MAX_CACHED_SESSIONS,
            max_cached_bytes: DEFAULT_MAX_CACHED_BYTES,
        }
    }
}

/// Monotonically ordered operation receipt returned by ordered async queries and settings mutations.
#[derive(Debug, Clone)]
pub struct OpReceipt<T> {
    pub op_revision: u64,
    pub snapshot_revision: u64,
    pub applied_to_snapshot: bool,
    pub value: T,
}

impl<T> Deref for OpReceipt<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub resolved_count: usize,
    pub recovered_home_session_id: Option<String>,
    pub restored_home_draft: bool,
}

pub enum ClientBootstrapResult {
    Started(CstClient),
    ActivatedExisting { owner_pid: u32 },
}

enum WorkerCommand {
    ActivateSession {
        session_id: Option<String>,
        reply: oneshot::Sender<Result<Option<Arc<SessionViewSnapshot>>, ClientError>>,
    },
    ReloadSession {
        session_id: String,
        reply: oneshot::Sender<Result<Option<Arc<SessionViewSnapshot>>, ClientError>>,
    },
    RefreshGlobals {
        op_revision: u64,
        reply: oneshot::Sender<Result<OpReceipt<()>, ClientError>>,
    },
    RefreshModels {
        op_revision: u64,
        session_id: Option<String>,
        reply: oneshot::Sender<Result<OpReceipt<Arc<ModelsSnapshot>>, ClientError>>,
    },
    QueryDesktopSettings {
        op_revision: u64,
        reply: oneshot::Sender<Result<OpReceipt<DesktopSettingsDto>, ClientError>>,
    },
    SendDraft {
        target: DraftKey,
        draft: ComposerDraft,
        reply: oneshot::Sender<Result<SendSubmissionOutcome, ClientError>>,
    },
    SendDirect {
        session_id: String,
        draft: ComposerDraft,
        reply: oneshot::Sender<Result<SendSubmissionOutcome, ClientError>>,
    },
    RetryOutbox {
        session_id: Option<String>,
        client_message_id: String,
        reply: oneshot::Sender<Result<SendSubmissionOutcome, ClientError>>,
    },
    ReconcileUncertain {
        reply: oneshot::Sender<Result<ReconcileReport, ClientError>>,
    },
    PatchDesktopSettings {
        op_revision: u64,
        patch: DesktopSettingsPatchDto,
        reply: oneshot::Sender<Result<OpReceipt<DesktopSettingsDto>, ClientError>>,
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

#[derive(Default)]
struct OpWatermarks {
    state_op_rev: u64,
    auth_op_rev: u64,
    account_op_rev: u64,
    update_op_rev: u64,
    models_op_rev: u64,
    desktop_settings_op_rev: u64,
}

struct SharedClientCore {
    kit_root: PathBuf,
    app_identity: ProcessIdentity,
    rt_handle: tokio::runtime::Handle,
    http: AppHttpClient,
    next_op_revision: AtomicU64,
    op_watermarks: Mutex<OpWatermarks>,
    snapshot: Mutex<Arc<ClientSnapshot>>,
    workspace: Mutex<WorkspaceState>,
    outbox: Mutex<SessionOutbox>,
    presenter_cache: Mutex<ToolPresenterCache>,
    revision_tx: watch::Sender<u64>,
    revision_rx: watch::Receiver<u64>,
    wakeup_rx: watch::Receiver<u64>,
    cmd_tx: mpsc::UnboundedSender<WorkerCommand>,
    stop: CancellationToken,
}

impl SharedClientCore {
    fn alloc_op_revision(&self) -> u64 {
        self.next_op_revision.fetch_add(1, Ordering::SeqCst)
    }

    fn publish(&self, mutator: impl FnOnce(&mut ClientSnapshot)) -> u64 {
        let next_rev = {
            let mut guard = self.snapshot.lock().expect("snapshot lock");
            let mut next = (**guard).clone();
            mutator(&mut next);
            next.revision = next.revision.wrapping_add(1);
            let rev = next.revision;
            *guard = Arc::new(next);
            rev
        };
        notify_revision(&self.revision_tx, next_rev);
        next_rev
    }
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
        // Give saved bounds a chance to reach the first window without inheriting the
        // normal settings request's timeout. The reducer refreshes settings on connect.
        let initial_snapshot = runtime.block_on(load_startup_desktop_settings(&endpoint.http));
        let handle = spawn_client_workers(
            runtime.handle(),
            endpoint,
            instance_guard.subscribe_wakeup(),
            ClientRuntimeOptions::default(),
            initial_snapshot,
        )?;

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
        Self::from_verified_endpoint_with_options(
            runtime_handle,
            endpoint,
            wakeup_rx,
            ClientRuntimeOptions::default(),
        )
    }

    pub fn from_verified_endpoint_with_options(
        runtime_handle: &tokio::runtime::Handle,
        endpoint: VerifiedServerEndpoint,
        wakeup_rx: Option<watch::Receiver<u64>>,
        options: ClientRuntimeOptions,
    ) -> Result<ClientHandle, ClientError> {
        let rx = wakeup_rx.unwrap_or_else(|| {
            let (_tx, rx) = watch::channel(0u64);
            rx
        });
        spawn_client_workers(
            runtime_handle,
            endpoint,
            rx,
            options,
            ClientSnapshot::default(),
        )
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

    pub fn runtime_handle(&self) -> &tokio::runtime::Handle {
        &self.core.rt_handle
    }

    /// Runs a Tokio/reqwest future on the client's dedicated Tokio runtime so non-Tokio UI executors
    /// (such as GPUI's foreground/background executors) never panic on missing Tokio reactor context.
    pub async fn spawn_tokio<F, T>(&self, fut: F) -> Result<T, ClientError>
    where
        F: std::future::Future<Output = Result<T, ClientError>> + Send + 'static,
        T: Send + 'static,
    {
        self.core
            .rt_handle
            .spawn(fut)
            .await
            .map_err(|e| ClientError::Disconnected(format!("客户端异步任务异常中止：{e}")))?
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

    /// Retains recovered content in the local outbox when the destination draft changed.
    pub fn retain_recovered_payload(
        &self,
        session_id: &str,
        draft: &ComposerDraft,
    ) -> Result<(), ClientError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let item = OutboxItem::from_draft(Some(session_id.to_owned()), draft, now)?;
        let id = item.client_message_id.clone();
        let mut outbox = self.core.outbox.lock().expect("outbox lock");
        outbox.enqueue_session(session_id, item);
        // enqueue_session normalizes non-sending states.
        outbox.mark_session_error(
            session_id,
            &id,
            &ClientError::Conflict {
                code: "draft_changed".into(),
                message: "草稿已更新，请从此处恢复编辑。".into(),
            },
        );
        Ok(())
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
        let http = self.core.http.clone();
        let lifecycle = self
            .spawn_tokio(async move { http.lifecycle().await })
            .await?;
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
        self.begin_activate_session(session_id).await
    }

    /// Forces an unconditional snapshot reload (`GET /api/sessions/<id>`) while keeping the previous
    /// snapshot visible in `SessionStore` until the fresh snapshot installs.
    pub async fn reload_session(
        &self,
        session_id: String,
    ) -> Result<Option<Arc<SessionViewSnapshot>>, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::ReloadSession {
                session_id,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    /// Changes the selected session immediately and returns its asynchronous load.
    pub fn begin_activate_session(
        &self,
        session_id: Option<String>,
    ) -> impl Future<Output = Result<Option<Arc<SessionViewSnapshot>>, ClientError>> + use<> {
        let (tx, rx) = oneshot::channel();
        let sent = {
            let mut workspace = self.core.workspace.lock().expect("workspace lock");
            let sent = self.core.cmd_tx.send(WorkerCommand::ActivateSession {
                session_id: session_id.clone(),
                reply: tx,
            });
            if sent.is_ok() {
                workspace.set_active_session(session_id);
            }
            sent
        };
        if sent.is_ok() {
            self.core.publish(|_| {});
        }
        async move {
            sent.map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
            rx.await
                .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
        }
    }

    pub async fn refresh_globals(&self) -> Result<OpReceipt<()>, ClientError> {
        let op_revision = self.core.alloc_op_revision();
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::RefreshGlobals {
                op_revision,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn refresh_models(
        &self,
        session_id: Option<String>,
    ) -> Result<OpReceipt<Arc<ModelsSnapshot>>, ClientError> {
        let op_revision = self.core.alloc_op_revision();
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::RefreshModels {
                op_revision,
                session_id,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    pub async fn query_desktop_settings(
        &self,
    ) -> Result<OpReceipt<DesktopSettingsDto>, ClientError> {
        let op_revision = self.core.alloc_op_revision();
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::QueryDesktopSettings {
                op_revision,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    /// Takes the draft from `WorkspaceState` and executes optimistic two-phase submission through `SessionOutbox`.
    /// On home-page creation failure or `409 credentials_required`, restores the draft into `WorkspaceState`
    /// ONLY if the user has not started typing a new draft in the meantime (otherwise retains the item in `SessionOutbox`).
    pub async fn submit_draft(
        &self,
        target: DraftKey,
    ) -> Result<SendSubmissionOutcome, ClientError> {
        self.begin_submit_draft(target).await
    }

    /// Takes the draft and enqueues the send synchronously so an interactive
    /// caller can clear its editor before accepting any further input.
    pub fn begin_submit_draft(
        &self,
        target: DraftKey,
    ) -> impl Future<Output = Result<SendSubmissionOutcome, ClientError>> + use<> {
        let reply = self.enqueue_draft(target);
        async move {
            reply?
                .await
                .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
        }
    }

    /// Submits an explicit payload directly into `SessionOutbox` for `session_id` without reading or
    /// mutating `WorkspaceState` composer drafts (used by in-place turn regeneration).
    pub async fn submit_direct_payload(
        &self,
        session_id: String,
        draft: ComposerDraft,
    ) -> Result<SendSubmissionOutcome, ClientError> {
        if draft.is_empty() {
            return Err(ClientError::BadRequest {
                code: "empty_message".into(),
                message: "请输入消息内容或添加图片。".into(),
            });
        }
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::SendDirect {
                session_id,
                draft,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    fn enqueue_draft(
        &self,
        target: DraftKey,
    ) -> Result<oneshot::Receiver<Result<SendSubmissionOutcome, ClientError>>, ClientError> {
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
            self.with_workspace_mut(|ws| {
                let _ = ws.restore_draft_if_empty(target, draft);
            });
            return Err(ClientError::Disconnected("客户端后台线程已退出。".into()));
        }
        Ok(rx)
    }

    /// Retries a failed, storage-paused, or uncertain outbox item (`None` = Home item, `Some(sid)` = Session item).
    /// Uncertain items check read-only server receipts first so already-accepted items never duplicate.
    pub async fn retry_outbox_item(
        &self,
        session_id: Option<String>,
        client_message_id: String,
    ) -> Result<SendSubmissionOutcome, ClientError> {
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::RetryOutbox {
                session_id,
                client_message_id,
                reply: tx,
            })
            .map_err(|_| ClientError::Disconnected("客户端后台线程已退出。".into()))?;
        rx.await
            .map_err(|_| ClientError::Disconnected("客户端后台响应中断。".into()))?
    }

    /// Moves a settled (failed/paused/uncertain) outbox item back into the target `WorkspaceState` composer draft.
    /// Returns `409 draft_not_empty` unless `force_replace_draft` is `true` when the composer already holds unsent text/images.
    pub fn edit_outbox_item(
        &self,
        session_id: Option<&str>,
        client_message_id: &str,
        force_replace_draft: bool,
    ) -> Result<ComposerDraft, ClientError> {
        let target_key = match session_id {
            Some(sid) => DraftKey::Session(sid.to_owned()),
            None => DraftKey::NewSession,
        };
        let draft = {
            let mut ws = self.core.workspace.lock().expect("workspace lock");
            if !force_replace_draft
                && ws
                    .draft(&target_key)
                    .is_some_and(ComposerDraft::has_unsent_content)
            {
                return Err(ClientError::Conflict {
                    code: "draft_not_empty".into(),
                    message: "当前输入框已有未发送草稿，请先发送或清空后再编辑。".into(),
                });
            }
            let item = self
                .core
                .outbox
                .lock()
                .expect("outbox lock")
                .take_settled_item(session_id, client_message_id)?;
            let restored = item.to_composer_draft();
            ws.restore_draft(target_key, restored.clone());
            restored
        };
        bump_revision(&self.core, &self.core.revision_tx);
        Ok(draft)
    }

    /// Removes a settled (failed/paused/uncertain) item from `SessionOutbox`, releasing any attached image handles.
    pub fn remove_outbox_item(
        &self,
        session_id: Option<&str>,
        client_message_id: &str,
    ) -> Result<OutboxItem, ClientError> {
        let removed = self
            .core
            .outbox
            .lock()
            .expect("outbox lock")
            .take_settled_item(session_id, client_message_id)?;
        bump_revision(&self.core, &self.core.revision_tx);
        Ok(removed)
    }

    pub async fn reconcile_uncertain_outbox(&self) -> Result<usize, ClientError> {
        self.reconcile_uncertain_outbox_detailed()
            .await
            .map(|r| r.resolved_count)
    }

    pub async fn reconcile_uncertain_outbox_detailed(
        &self,
    ) -> Result<ReconcileReport, ClientError> {
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
    ) -> Result<OpReceipt<DesktopSettingsDto>, ClientError> {
        let op_revision = self.core.alloc_op_revision();
        let (tx, rx) = oneshot::channel();
        self.core
            .cmd_tx
            .send(WorkerCommand::PatchDesktopSettings {
                op_revision,
                patch,
                reply: tx,
            })
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

    pub async fn rename_session(
        &self,
        session_id: String,
        name: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.rename_session(&session_id, &name).await })
            .await
    }

    pub async fn delete_session(&self, session_id: String) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.delete_session(&session_id).await })
            .await
    }

    pub async fn abort_session(&self, session_id: String) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.abort_session(&session_id).await })
            .await
    }

    pub async fn compact_session(
        &self,
        session_id: String,
        instructions: Option<String>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.compact_session(&session_id, instructions.as_deref())
                .await
        })
        .await
    }

    pub async fn fork_session(
        &self,
        session_id: String,
        entry_id: String,
    ) -> Result<String, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.fork_session(&session_id, &entry_id).await })
            .await
    }

    pub async fn get_session_tree(&self, session_id: String) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.get_session_tree(&session_id).await })
            .await
    }

    pub async fn navigate_session_tree(
        &self,
        session_id: String,
        entry_id: String,
        summarize: bool,
        custom_instructions: Option<String>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.navigate_session_tree(
                &session_id,
                &entry_id,
                summarize,
                custom_instructions.as_deref(),
            )
            .await
        })
        .await
    }

    pub async fn edit_queue_item(
        &self,
        session_id: String,
        item_id: String,
        version: u64,
        text: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.edit_queue_item(&session_id, &item_id, version, &text)
                .await
        })
        .await
    }

    pub async fn delete_queue_item(
        &self,
        session_id: String,
        item_id: String,
        version: u64,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(
            async move { http.delete_queue_item(&session_id, &item_id, version).await },
        )
        .await
    }

    pub async fn reorder_queue(
        &self,
        session_id: String,
        version: u64,
        ids: Vec<String>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.reorder_queue(&session_id, version, &ids).await })
            .await
    }

    pub async fn steer_queue_item(
        &self,
        session_id: String,
        item_id: String,
        version: u64,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.steer_queue_item(&session_id, &item_id, version).await })
            .await
    }

    pub async fn retry_queue_item(
        &self,
        session_id: String,
        item_id: String,
        version: u64,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.retry_queue_item(&session_id, &item_id, version).await })
            .await
    }

    pub async fn select_model(
        &self,
        session_id: Option<String>,
        provider: String,
        model_id: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.select_model(session_id.as_deref(), &provider, &model_id)
                .await
        })
        .await
    }

    pub async fn set_session_thinking_level(
        &self,
        session_id: String,
        level: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.set_session_thinking_level(&session_id, &level).await })
            .await
    }

    pub async fn get_provider_quota(
        &self,
        provider: String,
        refresh: bool,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.get_provider_quota(&provider, refresh).await })
            .await
    }

    pub async fn search_files(
        &self,
        session_id: Option<String>,
        query: String,
    ) -> Result<Vec<String>, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.search_files(session_id.as_deref(), &query).await })
            .await
    }

    pub async fn get_session_attachment_bytes(
        &self,
        session_id: String,
        attachment_id: String,
    ) -> Result<Vec<u8>, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.get_session_attachment_bytes(&session_id, &attachment_id)
                .await
        })
        .await
    }

    pub async fn open_session_runbook(
        &self,
        session_id: String,
        entry_id: String,
        tool_call_id: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.open_session_runbook(&session_id, &entry_id, &tool_call_id)
                .await
        })
        .await
    }

    pub async fn respond_bitlocker_password(
        &self,
        session_id: String,
        request_id: String,
        password_buf: zeroize::Zeroizing<[u8; 55]>,
        password_len: usize,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.respond_bitlocker_password(&session_id, &request_id, password_buf, password_len)
                .await
        })
        .await
    }

    pub async fn respond_bitlocker_confirm(
        &self,
        session_id: String,
        request_id: String,
        confirm: bool,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.respond_bitlocker_confirm(&session_id, &request_id, confirm)
                .await
        })
        .await
    }

    pub async fn cancel_bitlocker_request(
        &self,
        session_id: String,
        request_id: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.cancel_bitlocker_request(&session_id, &request_id)
                .await
        })
        .await
    }

    pub async fn preview_session_export(
        &self,
        session_id: String,
        format: String,
        theme: String,
        include_thinking: bool,
        include_tools: bool,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.preview_session_export(
                &session_id,
                &format,
                &theme,
                include_thinking,
                include_tools,
            )
            .await
        })
        .await
    }

    pub async fn create_session_export(
        &self,
        session_id: String,
        format: String,
        theme: String,
        include_thinking: bool,
        include_tools: bool,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.create_session_export(
                &session_id,
                &format,
                &theme,
                include_thinking,
                include_tools,
            )
            .await
        })
        .await
    }

    pub async fn get_export_job(&self, export_id: String) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.get_export_job(&export_id).await })
            .await
    }

    pub async fn open_export_directory(&self, export_id: String) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.open_export_directory(&export_id).await })
            .await
    }

    pub async fn refresh_models_catalog(&self) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.refresh_models_catalog().await })
            .await
    }

    pub async fn set_default_model(
        &self,
        provider: String,
        model_id: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.set_default_model(&provider, &model_id).await })
            .await
    }

    pub async fn set_scoped_models(
        &self,
        patterns: Vec<String>,
        empty: bool,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.set_scoped_models(&patterns, empty).await })
            .await
    }

    /// Saves an explicit scope even when it currently matches the implicit default order.
    pub async fn set_explicit_model_scope(
        &self,
        patterns: Vec<String>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.save_model_scope(&patterns, patterns.is_empty(), true)
                .await
        })
        .await
    }

    pub async fn get_custom_provider(&self, provider_id: String) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.get_custom_provider(&provider_id).await })
            .await
    }

    pub async fn save_custom_provider(
        &self,
        payload: serde_json::Map<String, Value>,
        key: Option<zeroize::Zeroizing<String>>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.save_custom_provider(&payload, key).await })
            .await
    }

    pub async fn delete_custom_provider(
        &self,
        provider_id: String,
        revision: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.delete_custom_provider(&provider_id, &revision).await })
            .await
    }

    pub async fn start_auth_flow(
        &self,
        provider_id: String,
        channel: String,
        base_url: Option<String>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.start_auth_flow(&provider_id, &channel, base_url.as_deref())
                .await
        })
        .await
    }

    pub async fn get_auth_flow_status(
        &self,
        provider_id: String,
        channel: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.get_auth_flow_status(&provider_id, &channel).await })
            .await
    }

    pub async fn cancel_auth_flow(
        &self,
        provider_id: String,
        channel: String,
        flow_id: Option<String>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.cancel_auth_flow(&provider_id, &channel, flow_id.as_deref())
                .await
        })
        .await
    }

    pub async fn respond_auth_flow(
        &self,
        provider_id: String,
        channel: String,
        flow_id: String,
        prompt_id: String,
        value: String,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.respond_auth_flow(&provider_id, &channel, &flow_id, &prompt_id, &value)
                .await
        })
        .await
    }

    pub async fn save_provider_api_key(
        &self,
        provider_id: String,
        api_key: zeroize::Zeroizing<String>,
        base_url: Option<String>,
    ) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move {
            http.save_provider_api_key(&provider_id, api_key, base_url.as_deref())
                .await
        })
        .await
    }

    pub async fn logout_provider(&self, provider_id: String) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.logout_provider(&provider_id).await })
            .await
    }

    pub async fn refresh_account(&self) -> Result<AccountStatusDto, ClientError> {
        let op_rev = self.core.alloc_op_revision();
        let http = self.core.http.clone();
        let account = self
            .spawn_tokio(async move { http.get_account(true).await })
            .await?;
        let mut wm = self.core.op_watermarks.lock().expect("op_watermarks lock");
        if op_rev >= wm.account_op_rev {
            wm.account_op_rev = op_rev;
            self.core.publish(|snap| {
                snap.account = Some(Arc::new(account.clone()));
            });
        }
        Ok(account)
    }

    pub async fn claim_missing_key_prompt(&self) -> Result<MissingKeyClaimDto, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.claim_missing_key_prompt(None).await })
            .await
    }

    pub async fn get_ui_hints(&self) -> Result<UiHintsDto, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.get_ui_hints().await })
            .await
    }

    pub async fn get_update_status(&self) -> Result<UpdateStateDto, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.get_update_status().await })
            .await
    }

    pub async fn check_update(&self) -> Result<UpdateStatusDto, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.check_update().await })
            .await
    }

    pub async fn prepare_update(&self) -> Result<UpdateStatusDto, ClientError> {
        let http = self.core.http.clone();
        self.spawn_tokio(async move { http.prepare_update().await })
            .await
    }

    pub async fn cancel_update(
        &self,
        operation_id: Option<&str>,
    ) -> Result<UpdateStatusDto, ClientError> {
        let http = self.core.http.clone();
        let op = operation_id.map(str::to_owned);
        self.spawn_tokio(async move { http.cancel_update(op.as_deref()).await })
            .await
    }

    pub async fn launch_update(&self, ui: &str) -> Result<Value, ClientError> {
        let http = self.core.http.clone();
        let pid = self.core.app_identity.pid;
        let created = self.core.app_identity.created;
        let ui_owned = ui.to_owned();
        self.spawn_tokio(async move { http.launch_update(pid, created, &ui_owned).await })
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

async fn load_startup_desktop_settings(http: &AppHttpClient) -> ClientSnapshot {
    let mut snapshot = ClientSnapshot::default();
    match tokio::time::timeout(
        STARTUP_DESKTOP_SETTINGS_TIMEOUT,
        http.get_desktop_settings(),
    )
    .await
    {
        Ok(Ok(settings)) => snapshot.desktop_settings = settings,
        Ok(Err(ClientError::ConfigurationUnavailable(_))) => {
            snapshot.desktop_settings_corrupt = true;
        }
        _ => {}
    }
    snapshot
}

fn spawn_client_workers(
    rt_handle: &tokio::runtime::Handle,
    endpoint: VerifiedServerEndpoint,
    wakeup_rx: watch::Receiver<u64>,
    options: ClientRuntimeOptions,
    startup_snapshot: ClientSnapshot,
) -> Result<ClientHandle, ClientError> {
    let app_identity = ProcessIdentity::current().map_err(ClientError::BootstrapFailed)?;
    let initial_snapshot = Arc::new(ClientSnapshot {
        revision: 1,
        connected: false,
        reconnecting: false,
        channel_mode: ChannelModeDto::AppActive,
        version: endpoint.version.clone(),
        ..startup_snapshot
    });
    let (revision_tx, revision_rx) = watch::channel(1u64);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let stop = CancellationToken::new();

    let core = Arc::new(SharedClientCore {
        kit_root: endpoint.kit_root.clone(),
        app_identity,
        rt_handle: rt_handle.clone(),
        http: endpoint.http.clone(),
        next_op_revision: AtomicU64::new(1),
        op_watermarks: Mutex::new(OpWatermarks::default()),
        snapshot: Mutex::new(initial_snapshot),
        workspace: Mutex::new(WorkspaceState::default()),
        outbox: Mutex::new(SessionOutbox::default()),
        presenter_cache: Mutex::new(ToolPresenterCache::default()),
        revision_tx: revision_tx.clone(),
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
        run_state_reducer_loop(reducer_core, revision_tx, keepalive_rx, cmd_rx, options).await;
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
type GlobalsRefreshReply = (
    oneshot::Sender<Result<OpReceipt<()>, ClientError>>,
    Option<ClientError>,
);

struct GlobalsFetchedPayload {
    op_revision: u64,
    from_explicit_command: bool,
    state_val: Option<Value>,
    models: Option<ModelsSnapshot>,
    auth: Option<Vec<ProviderStatusDto>>,
    account: Option<AccountStatusDto>,
    desktop_settings: Option<Result<DesktopSettingsDto, ClientError>>,
    update_state: Option<UpdateStateDto>,
    reply: Option<GlobalsRefreshReply>,
}

enum InternalAsyncCompletion {
    GlobalsFetched(Box<GlobalsFetchedPayload>),
    SessionSnapshotLoaded {
        session_id: String,
        generation: u64,
        result: Result<Value, ClientError>,
        waiters: Vec<SessionLoadReply>,
    },
    RetrySessionSnapshot {
        session_id: String,
        generation: u64,
    },
    ReconciledUncertain {
        result: Result<ReconcileReport, ClientError>,
        reply: Option<oneshot::Sender<Result<ReconcileReport, ClientError>>>,
    },
}

fn update_monotonic_session_cursor(
    map: &mut BTreeMap<String, SessionCursor>,
    session_id: &str,
    cursor: &SessionCursor,
) {
    match map.get_mut(session_id) {
        Some((cur_ep, cur_seq)) if cur_ep == &cursor.0 => {
            if cursor.1 > *cur_seq {
                *cur_seq = cursor.1;
            }
        }
        _ => {
            map.insert(session_id.to_owned(), cursor.clone());
        }
    }
}

fn build_subscription_last_event_ids(
    desired_subs: &[String],
    confirmed_subs: &BTreeSet<String>,
    session_cursors: &BTreeMap<String, SessionCursor>,
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for sid in desired_subs {
        if !confirmed_subs.contains(sid)
            && let Some(cur) = session_cursors.get(sid)
        {
            out.insert(sid.clone(), Value::String(format_single_cursor(cur)));
        }
    }
    out
}

fn parse_subscribed_set(resp: &Value, fallback: &[String]) -> BTreeSet<String> {
    resp.get("subscribedSessions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| fallback.iter().cloned().collect())
}

async fn run_state_reducer_loop(
    core: Arc<SharedClientCore>,
    revision_tx: watch::Sender<u64>,
    mut keepalive_rx: mpsc::UnboundedReceiver<KeepaliveSignal>,
    mut cmd_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    options: ClientRuntimeOptions,
) {
    let client_id = Uuid::new_v4().to_string();
    let (mux_tx, mut mux_rx) = bounded_multiplex_channel(options.max_multiplex_queue_bytes);
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<InternalAsyncCompletion>();

    let mut session_store = SessionStore::with_limits(
        options.max_cached_sessions,
        options.max_cached_bytes,
        options.max_loading_buffer_bytes,
    );
    let mut session_cursors: BTreeMap<String, SessionCursor> = BTreeMap::new();
    let mut confirmed_subs: BTreeSet<String> = BTreeSet::new();
    let mut pending_session_waiters: BTreeMap<String, Vec<SessionLoadReply>> = BTreeMap::new();
    let mut desired_subs: Vec<String> = Vec::new();
    let mut sub_debounce_pending = false;
    let mut dirty = DirtyRefreshFlags::default();
    dirty.mark_all();
    let mut refresh_in_flight = false;

    let mut mux_cancel: Option<CancellationToken> = None;
    let mut active_mux_generation = 0u64;
    let mut send_tails: BTreeMap<String, watch::Receiver<()>> = BTreeMap::new();

    let publish = |core: &Arc<SharedClientCore>,
                   revision_tx: &watch::Sender<u64>,
                   mutator: &mut dyn FnMut(&mut ClientSnapshot)|
     -> u64 {
        let next_rev = {
            let mut guard = core.snapshot.lock().expect("snapshot lock");
            let mut next = (**guard).clone();
            mutator(&mut next);
            next.revision = next.revision.wrapping_add(1);
            let rev = next.revision;
            *guard = Arc::new(next);
            rev
        };
        notify_revision(revision_tx, next_rev);
        next_rev
    };

    let ensure_multiplex_running =
        |mode: ChannelModeDto,
         mux_cancel: &mut Option<CancellationToken>,
         active_gen: &mut u64,
         confirmed: &mut BTreeSet<String>,
         http: &AppHttpClient,
         cid: &str,
         tx: &BoundedMultiplexSender,
         root_stop: &CancellationToken| {
            if mode == ChannelModeDto::WebActive {
                if let Some(tok) = mux_cancel.take() {
                    tok.cancel();
                }
                confirmed.clear();
            } else if mux_cancel.is_none() {
                *active_gen = active_gen.wrapping_add(1);
                confirmed.clear();
                let tok = root_stop.child_token();
                *mux_cancel = Some(tok.clone());
                tokio::spawn(run_multiplex_sse_loop(
                    http.clone(),
                    cid.to_owned(),
                    *active_gen,
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
        &mut active_mux_generation,
        &mut confirmed_subs,
        &core.http,
        &client_id,
        &mux_tx,
        &core.stop,
    );

    const STREAM_PUBLISH_COALESCE_MS: u64 = 16;
    let sub_sleep = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(sub_sleep);
    let refresh_sleep = tokio::time::sleep(Duration::from_millis(1));
    tokio::pin!(refresh_sleep);
    let mut refresh_timer_armed = true;
    let stream_publish_sleep = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(stream_publish_sleep);
    let mut stream_publish_dirty = false;
    let mut last_stream_publish = tokio::time::Instant::now()
        .checked_sub(Duration::from_millis(STREAM_PUBLISH_COALESCE_MS))
        .unwrap_or_else(tokio::time::Instant::now);

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
                            let done_tx_clone = done_tx.clone();
                            tokio::spawn(async move {
                                let result = reconcile_uncertain_items(&c_clone).await;
                                let _ = done_tx_clone.send(InternalAsyncCompletion::ReconciledUncertain {
                                    result,
                                    reply: None,
                                });
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
                            &mut active_mux_generation,
                            &mut confirmed_subs,
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
            Some(first_envelope) = mux_rx.recv() => {
                let mut batch = Vec::with_capacity(8);
                batch.push(first_envelope);
                for _ in 0..64 {
                    let Some(next) = mux_rx.try_recv() else {
                        break;
                    };
                    batch.push(next);
                }

                let mut session_dirty = false;
                let mut needs_immediate_publish = false;
                let mut reload_sessions = BTreeSet::new();
                for envelope in batch {
                    if envelope.mux_generation != active_mux_generation {
                        continue;
                    }
                    for (source, cur) in &envelope.vector_cursors {
                        if source != "global" {
                            update_monotonic_session_cursor(&mut session_cursors, source, cur);
                        }
                    }
                    match envelope.signal {
                        MultiplexSignal::StreamReady { subscribed_sessions } => {
                            confirmed_subs = subscribed_sessions.into_iter().collect();
                            let current_desired = compute_desired_subscriptions(&core);
                            if confirmed_subs.len() != current_desired.len()
                                || current_desired.iter().any(|id| !confirmed_subs.contains(id))
                            {
                                desired_subs = current_desired;
                                sub_debounce_pending = true;
                                sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(5));
                            }
                        }
                        // The independent app stream is the sole source of global state.
                        MultiplexSignal::StateEvent { .. } => {}
                        MultiplexSignal::SessionEvent {
                            session_cursor,
                            session_id,
                            event,
                            approx_bytes,
                        } => {
                            if let Some(ref cur) = session_cursor {
                                update_monotonic_session_cursor(&mut session_cursors, &session_id, cur);
                            }
                            let ev_type = event.get("type").and_then(Value::as_str);
                            let committed_message = match ev_type {
                                Some("message_end") => event.get("message"),
                                Some("entry_appended") => event.get("entry").and_then(|e| e.get("message")),
                                _ => None,
                            };
                            if let Some(cmid) = committed_message
                                .and_then(|m| m.get("clientMessageId"))
                                .and_then(Value::as_str)
                                && core.outbox.lock().expect("outbox lock").mark_accepted(&session_id, cmid)
                            {
                                session_dirty = true;
                                needs_immediate_publish = true;
                            }
                            let is_high_freq_delta = matches!(
                                ev_type,
                                Some("message_update" | "tool_execution_update")
                            );
                            let needs_snapshot_reload = matches!(
                                ev_type,
                                Some("session_tree" | "compaction_end")
                            ) || (ev_type == Some("entry_appended")
                                && matches!(
                                    event
                                        .get("entry")
                                        .and_then(|e| e.get("type"))
                                        .and_then(Value::as_str),
                                    Some("compaction" | "branch_summary")
                                ));
                            if session_store.apply_session_event(&session_id, session_cursor, event, approx_bytes) {
                                session_dirty = true;
                                if !is_high_freq_delta {
                                    needs_immediate_publish = true;
                                }
                            }
                            if needs_snapshot_reload
                                && (session_store.has_ready(&session_id)
                                    || session_store.is_loading(&session_id))
                            {
                                reload_sessions.insert(session_id);
                            }
                        }
                        MultiplexSignal::QueueEvent {
                            session_cursor,
                            session_id,
                            queue,
                            approx_bytes,
                        } => {
                            if let Some(ref cur) = session_cursor {
                                update_monotonic_session_cursor(&mut session_cursors, &session_id, cur);
                            }
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
                            if session_store.apply_queue_snapshot(&session_id, session_cursor, queue, approx_bytes) {
                                session_dirty = true;
                                needs_immediate_publish = true;
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
                            if !session_store.has_ready(&session_id) && !session_store.is_loading(&session_id) {
                                continue;
                            }
                            let generation = session_store.reset_to_loading(&session_id);
                            spawn_session_snapshot_fetch(
                                core.http.clone(),
                                done_tx.clone(),
                                session_id,
                                generation,
                                Vec::new(),
                            );
                        }
                        MultiplexSignal::QueueOverflow => {
                            // Bounded multiplex channel hit byte cap: perform targeted reload of tracked subscribed sessions + globals.
                            dirty.mark_all();
                            if !refresh_timer_armed {
                                refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(5));
                                refresh_timer_armed = true;
                            }
                            let tracked = session_store.tracked_session_ids();
                            for sid in tracked {
                                if desired_subs.contains(&sid) || confirmed_subs.contains(&sid) {
                                    let generation = session_store.reset_to_loading(&sid);
                                    spawn_session_snapshot_fetch(
                                        core.http.clone(),
                                        done_tx.clone(),
                                        sid,
                                        generation,
                                        Vec::new(),
                                    );
                                }
                            }
                        }
                        MultiplexSignal::Disconnected => {
                            confirmed_subs.clear();
                        }
                    }
                }

                for session_id in reload_sessions {
                    let generation = session_store.reset_to_loading(&session_id);
                    spawn_session_snapshot_fetch(core.http.clone(), done_tx.clone(), session_id, generation, Vec::new());
                }
                if session_dirty {
                    let now = tokio::time::Instant::now();
                    let min_gap = Duration::from_millis(STREAM_PUBLISH_COALESCE_MS);
                    if needs_immediate_publish || now.saturating_duration_since(last_stream_publish) >= min_gap {
                        stream_publish_dirty = false;
                        last_stream_publish = now;
                        let map = Arc::new(session_store.export_ready_map());
                        publish(&core, &revision_tx, &mut |snap| {
                            snap.loaded_sessions = Arc::clone(&map);
                        });
                    } else if !stream_publish_dirty {
                        stream_publish_dirty = true;
                        stream_publish_sleep.as_mut().reset(last_stream_publish + min_gap);
                    }
                }
            }
            Some(done) = done_rx.recv() => {
                match done {
                    InternalAsyncCompletion::GlobalsFetched(fetched) => {
                        let GlobalsFetchedPayload {
                            op_revision,
                            from_explicit_command,
                            state_val,
                            models,
                            auth,
                            account,
                            desktop_settings,
                            update_state,
                            reply,
                        } = *fetched;
                        if !from_explicit_command {
                            refresh_in_flight = false;
                        }

                        // Keep validation and publication atomic with refresh_account.
                        let mut wm = core.op_watermarks.lock().expect("op_watermarks lock");
                        let (
                            apply_state,
                            apply_auth,
                            apply_account,
                            apply_update,
                            apply_models,
                            apply_ds,
                        ) = {
                            let a_state = if state_val.is_some() && op_revision >= wm.state_op_rev {
                                wm.state_op_rev = op_revision;
                                true
                            } else {
                                false
                            };
                            let a_auth = if auth.is_some() && op_revision >= wm.auth_op_rev {
                                wm.auth_op_rev = op_revision;
                                true
                            } else {
                                false
                            };
                            let a_account =
                                if account.is_some() && op_revision >= wm.account_op_rev {
                                    wm.account_op_rev = op_revision;
                                    true
                                } else {
                                    false
                                };
                            let a_update =
                                if update_state.is_some() && op_revision >= wm.update_op_rev {
                                    wm.update_op_rev = op_revision;
                                    true
                                } else {
                                    false
                                };
                            let am = if models.is_some() && op_revision >= wm.models_op_rev {
                                wm.models_op_rev = op_revision;
                                true
                            } else {
                                false
                            };
                            let ads = if desktop_settings.is_some()
                                && op_revision >= wm.desktop_settings_op_rev
                            {
                                wm.desktop_settings_op_rev = op_revision;
                                true
                            } else {
                                false
                            };
                            (a_state, a_auth, a_account, a_update, am, ads)
                        };

                        let applied_any = apply_state
                            || apply_auth
                            || apply_account
                            || apply_update
                            || apply_models
                            || apply_ds;
                        let snap_rev = if applied_any {
                            publish(&core, &revision_tx, &mut |snap| {
                                if apply_state && let Some(ref st) = state_val {
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
                                if apply_auth && let Some(a) = auth.clone() {
                                    snap.auth_providers = Some(Arc::new(a));
                                }
                                if apply_account && let Some(acc) = account.clone() {
                                    snap.account = Some(Arc::new(acc));
                                }
                                if apply_update && let Some(us) = update_state.clone() {
                                    snap.update_state = Some(Arc::new(us));
                                }
                                if apply_models && let Some(m) = models.clone() {
                                    snap.models = Some(Arc::new(m));
                                }
                                if apply_ds && let Some(ref ds_res) = desktop_settings {
                                    match ds_res {
                                        Ok(ds) => {
                                            snap.desktop_settings = ds.clone();
                                            snap.desktop_settings_corrupt = false;
                                        }
                                        Err(ClientError::ConfigurationUnavailable(_)) => {
                                            snap.desktop_settings_corrupt = true;
                                        }
                                        Err(_) => {}
                                    }
                                }
                            })
                        } else {
                            core.snapshot.lock().expect("snapshot lock").revision
                        };

                        drop(wm);
                        if let Some((reply_tx, maybe_err)) = reply {
                            let _ = reply_tx.send(match maybe_err {
                                Some(e) => Err(e),
                                None => Ok(OpReceipt {
                                    op_revision,
                                    snapshot_revision: snap_rev,
                                    applied_to_snapshot: applied_any,
                                    value: (),
                                }),
                            });
                        }

                        let cur_mode = core.snapshot.lock().expect("snapshot lock").channel_mode;
                        ensure_multiplex_running(
                            cur_mode,
                            &mut mux_cancel,
                            &mut active_mux_generation,
                            &mut confirmed_subs,
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
                        generation,
                        result,
                        mut waiters,
                    } => {
                        let result = result.and_then(|payload| {
                            match session_store.finish_loading_snapshot(&session_id, generation, payload) {
                                FinishLoadingOutcome::InvalidPayload => Err(ClientError::Server {
                                    status: 200,
                                    code: "invalid_response".into(),
                                    message: "后台返回的会话快照不匹配。".into(),
                                }),
                                outcome => Ok(outcome),
                            }
                        });
                        match result {
                            Ok(FinishLoadingOutcome::StaleGeneration)
                            | Err(_) if session_store.loading_generation(&session_id) != Some(generation) => {
                                if session_store.is_loading(&session_id) {
                                    pending_session_waiters.entry(session_id).or_default().extend(waiters);
                                } else {
                                    let existing = session_store.export_ready_map().get(&session_id).cloned();
                                    for waiter in waiters {
                                        let _ = waiter.send(Ok(existing.clone()));
                                    }
                                }
                            }
                            Ok(FinishLoadingOutcome::BufferOverflowNeedsReload) => {
                                if let Some(extra) = pending_session_waiters.remove(&session_id) {
                                    waiters.extend(extra);
                                }
                                let next_gen = session_store.reset_to_loading(&session_id);
                                spawn_session_snapshot_fetch(core.http.clone(), done_tx.clone(), session_id, next_gen, waiters);
                            }
                            Ok(FinishLoadingOutcome::Installed(loaded)) => {
                                if let Some(extra) = pending_session_waiters.remove(&session_id) {
                                    waiters.extend(extra);
                                }
                                if let Some(ref cur) = loaded.event_cursor {
                                    update_monotonic_session_cursor(&mut session_cursors, &session_id, cur);
                                }
                                let inbox_ids = loaded.server_inbox_ids();
                                if !inbox_ids.is_empty() {
                                    core.outbox.lock().expect("outbox lock").reconcile_server_inbox_ids(&session_id, &inbox_ids);
                                }
                                let pinned = collect_pinned_sessions(&core);
                                session_store.evict_lru(&pinned);
                                let map = Arc::new(session_store.export_ready_map());
                                publish(&core, &revision_tx, &mut |snap| {
                                    snap.loaded_sessions = Arc::clone(&map);
                                });
                                for waiter in waiters {
                                    let _ = waiter.send(Ok(Some(loaded.clone())));
                                }
                            }
                            Err(err) => {
                                if let Some(extra) = pending_session_waiters.remove(&session_id) {
                                    waiters.extend(extra);
                                }
                                for waiter in waiters {
                                    let _ = waiter.send(Err(err.clone()));
                                }
                                if session_store.has_reload_fallback(&session_id)
                                    && !matches!(err, ClientError::NotFound { .. })
                                {
                                    let done_tx = done_tx.clone();
                                    let stop = core.stop.clone();
                                    tokio::spawn(async move {
                                        tokio::select! {
                                            () = stop.cancelled() => {}
                                            () = tokio::time::sleep(Duration::from_secs(1)) => {
                                                let _ = done_tx.send(InternalAsyncCompletion::RetrySessionSnapshot { session_id, generation });
                                            }
                                        }
                                    });
                                } else {
                                    session_store.cancel_loading_if_generation(&session_id, generation);
                                }
                            }
                            Ok(FinishLoadingOutcome::StaleGeneration | FinishLoadingOutcome::InvalidPayload) => {
                                unreachable!("completion was classified above");
                            }
                        }
                    }
                    InternalAsyncCompletion::RetrySessionSnapshot { session_id, generation } => {
                        if session_store.loading_generation(&session_id) != Some(generation) {
                            continue;
                        }
                        if compute_desired_subscriptions(&core).contains(&session_id) {
                            spawn_session_snapshot_fetch(core.http.clone(), done_tx.clone(), session_id, generation, Vec::new());
                        } else {
                            session_store.cancel_loading_if_generation(&session_id, generation);
                            if let Some(waiters) = pending_session_waiters.remove(&session_id) {
                                let existing = session_store.export_ready_map().get(&session_id).cloned();
                                for waiter in waiters {
                                    let _ = waiter.send(Ok(existing.clone()));
                                }
                            }
                        }
                    }
                    InternalAsyncCompletion::ReconciledUncertain { result, reply } => {
                        if let Ok(ref report) = result
                            && report.resolved_count > 0
                        {
                            if report.recovered_home_session_id.is_some() {
                                dirty.sessions_and_state = true;
                                if !refresh_timer_armed {
                                    refresh_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(5));
                                    refresh_timer_armed = true;
                                }
                            }
                            bump_revision(&core, &revision_tx);
                        }
                        if let Some(tx) = reply {
                            let _ = tx.send(result);
                        }
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    WorkerCommand::ActivateSession { session_id, reply } => {
                        let Some(sid) = session_id else {
                            desired_subs = compute_desired_subscriptions(&core);
                            sub_debounce_pending = true;
                            sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(SUBSCRIPTION_DEBOUNCE_MS));
                            let _ = reply.send(Ok(None));
                            continue;
                        };

                        if session_store.has_ready(&sid) {
                            let is_app_active = core.snapshot.lock().expect("snapshot lock").channel_mode == ChannelModeDto::AppActive;
                            let already_subscribed = confirmed_subs.contains(&sid);
                            let has_known_cursor = session_cursors.contains_key(&sid)
                                || session_store.ready_cursor(&sid).is_some();

                            if !is_app_active || already_subscribed || has_known_cursor {
                                session_store.touch(&sid);
                                let existing = session_store.export_ready_map().get(&sid).cloned();
                                desired_subs = compute_desired_subscriptions(&core);
                                if is_app_active && !already_subscribed {
                                    let last_ids = build_subscription_last_event_ids(
                                        &desired_subs,
                                        &confirmed_subs,
                                        &session_cursors,
                                    );
                                    sub_debounce_pending = false;
                                    if let Ok(subscribed) = core
                                        .http
                                        .put_multiplex_subscriptions(&client_id, &desired_subs, Some(&last_ids))
                                        .await
                                    {
                                        confirmed_subs = parse_subscribed_set(&subscribed, &desired_subs);
                                    } else {
                                        sub_debounce_pending = true;
                                        sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(SUBSCRIPTION_DEBOUNCE_MS));
                                    }
                                } else {
                                    sub_debounce_pending = true;
                                    sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(SUBSCRIPTION_DEBOUNCE_MS));
                                }
                                let _ = reply.send(Ok(existing));
                                continue;
                            }
                            // Ready snapshot lacked an eventCursor while unsubscribed: refresh cleanly.
                            session_store.remove(&sid);
                        }

                        if session_store.is_loading(&sid) {
                            pending_session_waiters.entry(sid).or_default().push(reply);
                            continue;
                        }

                        // Zero-gap initial subscription:
                        // 1. Mark session as `Loading(generation)` to buffer incoming SSE frames
                        // 2. Submit `PUT /subscriptions` FIRST (in AppActive)
                        // 3. Fetch `GET /api/sessions/<sid>` and merge buffered frames past `eventCursor`
                        let generation = session_store
                            .begin_loading_if_absent(&sid)
                            .unwrap_or_else(|| session_store.reset_to_loading(&sid));
                        desired_subs = compute_desired_subscriptions(&core);
                        sub_debounce_pending = false;
                        let is_app_active = core.snapshot.lock().expect("snapshot lock").channel_mode == ChannelModeDto::AppActive;
                        if is_app_active {
                            let last_ids = build_subscription_last_event_ids(
                                &desired_subs,
                                &confirmed_subs,
                                &session_cursors,
                            );
                            match core
                                .http
                                .put_multiplex_subscriptions(&client_id, &desired_subs, Some(&last_ids))
                                .await
                            {
                                Ok(subscribed) => {
                                    confirmed_subs = parse_subscribed_set(&subscribed, &desired_subs);
                                }
                                Err(error) => {
                                    session_store.cancel_loading_if_generation(&sid, generation);
                                    let _ = reply.send(Err(error));
                                    continue;
                                }
                            }
                        }
                        spawn_session_snapshot_fetch(
                            core.http.clone(),
                            done_tx.clone(),
                            sid,
                            generation,
                            vec![reply],
                        );
                    }
                    WorkerCommand::ReloadSession { session_id, reply } => {
                        let generation = session_store.reset_to_loading(&session_id);
                        pending_session_waiters.entry(session_id.clone()).or_default().push(reply);
                        spawn_session_snapshot_fetch(
                            core.http.clone(),
                            done_tx.clone(),
                            session_id,
                            generation,
                            Vec::new(),
                        );
                    }
                    WorkerCommand::RefreshGlobals { op_revision, reply } => {
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
                                    op_revision,
                                    from_explicit_command: true,
                                    state_val: state_res.ok(),
                                    models: models_res.ok(),
                                    auth: auth_res.ok(),
                                    account: acc_res.ok(),
                                    desktop_settings: Some(ds_res),
                                    update_state: upd_res.ok(),
                                    reply: Some((reply, first_err)),
                                }),
                            ));
                        });
                    }
                    WorkerCommand::RefreshModels {
                        op_revision,
                        session_id,
                        reply,
                    } => {
                        let http = core.http.clone();
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            match http.get_models(session_id.as_deref()).await {
                                Ok(models) => {
                                    let arc = Arc::new(models);
                                    let (applied, snap_rev) = {
                                        let mut wm = core_clone.op_watermarks.lock().expect("op_watermarks lock");
                                        let mut guard = core_clone.snapshot.lock().expect("snapshot lock");
                                        if op_revision >= wm.models_op_rev {
                                            wm.models_op_rev = op_revision;
                                            let mut next = (**guard).clone();
                                            next.models = Some(Arc::clone(&arc));
                                            next.revision = next.revision.wrapping_add(1);
                                            let r = next.revision;
                                            *guard = Arc::new(next);
                                            (true, r)
                                        } else {
                                            (false, guard.revision)
                                        }
                                    };
                                    if applied {
                                        notify_revision(&rev_tx, snap_rev);
                                    }
                                    let _ = reply.send(Ok(OpReceipt {
                                        op_revision,
                                        snapshot_revision: snap_rev,
                                        applied_to_snapshot: applied,
                                        value: arc,
                                    }));
                                }
                                Err(e) => {
                                    let _ = reply.send(Err(e));
                                }
                            }
                        });
                    }
                    WorkerCommand::QueryDesktopSettings { op_revision, reply } => {
                        let http = core.http.clone();
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            let res = http.get_desktop_settings().await;
                            match res {
                                Ok(ds) => {
                                    let (applied, snap_rev) = {
                                        let mut wm = core_clone.op_watermarks.lock().expect("op_watermarks lock");
                                        let mut guard = core_clone.snapshot.lock().expect("snapshot lock");
                                        if op_revision >= wm.desktop_settings_op_rev {
                                            wm.desktop_settings_op_rev = op_revision;
                                            let mut next = (**guard).clone();
                                            next.desktop_settings = ds.clone();
                                            next.desktop_settings_corrupt = false;
                                            next.revision = next.revision.wrapping_add(1);
                                            let r = next.revision;
                                            *guard = Arc::new(next);
                                            (true, r)
                                        } else {
                                            (false, guard.revision)
                                        }
                                    };
                                    if applied {
                                        notify_revision(&rev_tx, snap_rev);
                                    }
                                    let _ = reply.send(Ok(OpReceipt {
                                        op_revision,
                                        snapshot_revision: snap_rev,
                                        applied_to_snapshot: applied,
                                        value: ds,
                                    }));
                                }
                                Err(err) => {
                                    if matches!(err, ClientError::ConfigurationUnavailable(_)) {
                                        let (applied, snap_rev) = {
                                            let mut wm = core_clone.op_watermarks.lock().expect("op_watermarks lock");
                                            let mut guard = core_clone.snapshot.lock().expect("snapshot lock");
                                            if op_revision >= wm.desktop_settings_op_rev {
                                                wm.desktop_settings_op_rev = op_revision;
                                                let mut next = (**guard).clone();
                                                next.desktop_settings_corrupt = true;
                                                next.revision = next.revision.wrapping_add(1);
                                                let r = next.revision;
                                                *guard = Arc::new(next);
                                                (true, r)
                                            } else {
                                                (false, guard.revision)
                                            }
                                        };
                                        if applied {
                                            notify_revision(&rev_tx, snap_rev);
                                        }
                                    }
                                    let _ = reply.send(Err(err));
                                }
                            }
                        });
                    }
                    WorkerCommand::SendDraft { target, draft, reply } => {
                        send_tails.retain(|_, tail| tail.has_changed().is_ok());
                        let (tail, previous) = if let DraftKey::Session(sid) = &target {
                            let (tx, rx) = watch::channel(());
                            (Some(tx), send_tails.insert(sid.clone(), rx))
                        } else {
                            (None, None)
                        };
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            let res = execute_send_draft(&core_clone, &rev_tx, target, &draft, previous, false).await;
                            let _ = reply.send(res);
                            drop(tail);
                        });
                    }
                    WorkerCommand::SendDirect {
                        session_id,
                        draft,
                        reply,
                    } => {
                        send_tails.retain(|_, tail| tail.has_changed().is_ok());
                        let (tx, rx) = watch::channel(());
                        let previous = send_tails.insert(session_id.clone(), rx);
                        let tail = Some(tx);
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            let res = execute_send_draft(
                                &core_clone,
                                &rev_tx,
                                DraftKey::Session(session_id),
                                &draft,
                                previous,
                                true,
                            )
                            .await;
                            let _ = reply.send(res);
                            drop(tail);
                        });
                    }
                    WorkerCommand::RetryOutbox {
                        session_id,
                        client_message_id,
                        reply,
                    } => {
                        send_tails.retain(|_, tail| tail.has_changed().is_ok());
                        let (tail, previous) = if let Some(ref sid) = session_id {
                            let (tx, rx) = watch::channel(());
                            (Some(tx), send_tails.insert(sid.clone(), rx))
                        } else {
                            (None, None)
                        };
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            let res = execute_retry_outbox(
                                &core_clone,
                                &rev_tx,
                                session_id,
                                &client_message_id,
                                previous,
                            )
                            .await;
                            let _ = reply.send(res);
                            drop(tail);
                        });
                    }
                    WorkerCommand::ReconcileUncertain { reply } => {
                        let core_clone = Arc::clone(&core);
                        let done_tx_clone = done_tx.clone();
                        tokio::spawn(async move {
                            let result = reconcile_uncertain_items(&core_clone).await;
                            let _ = done_tx_clone.send(InternalAsyncCompletion::ReconciledUncertain {
                                result,
                                reply: Some(reply),
                            });
                        });
                    }
                    WorkerCommand::PatchDesktopSettings {
                        op_revision,
                        patch,
                        reply,
                    } => {
                        let http = core.http.clone();
                        let core_clone = Arc::clone(&core);
                        let rev_tx = revision_tx.clone();
                        tokio::spawn(async move {
                            match http.patch_desktop_settings(&patch).await {
                                Ok(saved) => {
                                    let (applied, snap_rev) = {
                                        let mut wm = core_clone.op_watermarks.lock().expect("op_watermarks lock");
                                        let mut guard = core_clone.snapshot.lock().expect("snapshot lock");
                                        if op_revision >= wm.desktop_settings_op_rev {
                                            wm.desktop_settings_op_rev = op_revision;
                                            let mut next = (**guard).clone();
                                            next.desktop_settings = saved.clone();
                                            next.desktop_settings_corrupt = false;
                                            next.revision = next.revision.wrapping_add(1);
                                            let r = next.revision;
                                            *guard = Arc::new(next);
                                            (true, r)
                                        } else {
                                            (false, guard.revision)
                                        }
                                    };
                                    if applied {
                                        notify_revision(&rev_tx, snap_rev);
                                    }
                                    let _ = reply.send(Ok(OpReceipt {
                                        op_revision,
                                        snapshot_revision: snap_rev,
                                        applied_to_snapshot: applied,
                                        value: saved,
                                    }));
                                }
                                Err(err) => {
                                    let _ = reply.send(Err(err));
                                }
                            }
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
                                &mut active_mux_generation,
                                &mut confirmed_subs,
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
                                &mut active_mux_generation,
                                &mut confirmed_subs,
                                &core.http,
                                &client_id,
                                &mux_tx,
                                &core.stop,
                            );
                            dirty.sessions_and_state = true;
                            dirty.auth = true;
                            dirty.models = true;
                            let active = core.workspace.lock().expect("workspace lock").active_session_id().map(str::to_owned);
                            if let Some(active_sid) = active {
                                let generation = session_store.reset_to_loading(&active_sid);
                                let subs = compute_desired_subscriptions(&core);
                                let last_ids = build_subscription_last_event_ids(
                                    &subs,
                                    &confirmed_subs,
                                    &session_cursors,
                                );
                                if let Ok(subscribed) = core
                                    .http
                                    .put_multiplex_subscriptions(&client_id, &subs, Some(&last_ids))
                                    .await
                                {
                                    confirmed_subs = parse_subscribed_set(&subscribed, &subs);
                                    spawn_session_snapshot_fetch(
                                        core.http.clone(),
                                        done_tx.clone(),
                                        active_sid,
                                        generation,
                                        Vec::new(),
                                    );
                                } else {
                                    session_store.cancel_loading_if_generation(&active_sid, generation);
                                }
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
                        let exited = res.is_ok();
                        let _ = reply.send(res.map(|_| ()));
                        if exited {
                            core.stop.cancel();
                            break;
                        }
                    }
                }
            }
            () = &mut stream_publish_sleep, if stream_publish_dirty => {
                stream_publish_dirty = false;
                last_stream_publish = tokio::time::Instant::now();
                let map = Arc::new(session_store.export_ready_map());
                publish(&core, &revision_tx, &mut |snap| {
                    snap.loaded_sessions = Arc::clone(&map);
                });
            }
            () = &mut sub_sleep, if sub_debounce_pending => {
                sub_debounce_pending = false;
                let cur_mode = core.snapshot.lock().expect("snapshot lock").channel_mode;
                if cur_mode == ChannelModeDto::AppActive {
                    let last_ids = build_subscription_last_event_ids(
                        &desired_subs,
                        &confirmed_subs,
                        &session_cursors,
                    );
                    match core
                        .http
                        .put_multiplex_subscriptions(&client_id, &desired_subs, Some(&last_ids))
                        .await
                    {
                        Ok(subscribed) => {
                            confirmed_subs = parse_subscribed_set(&subscribed, &desired_subs);
                        }
                        Err(error) if error.is_disconnected() => {
                            sub_debounce_pending = true;
                            sub_sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(500));
                        }
                        Err(_) => {}
                    }
                }
            }
            () = &mut refresh_sleep, if refresh_timer_armed => {
                refresh_timer_armed = false;
                if !dirty.any() || refresh_in_flight {
                    continue;
                }
                refresh_in_flight = true;
                let flags = std::mem::take(&mut dirty);
                let op_revision = core.alloc_op_revision();
                let http = core.http.clone();
                let done_tx_clone = done_tx.clone();
                let is_web_active = core.snapshot.lock().expect("snapshot lock").channel_mode == ChannelModeDto::WebActive;
                tokio::spawn(async move {
                    let state_fut = async {
                        if flags.sessions_and_state {
                            if is_web_active {
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
                            Some(http.get_desktop_settings().await)
                        } else {
                            None
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
                            op_revision,
                            from_explicit_command: false,
                            state_val,
                            models,
                            auth,
                            account,
                            desktop_settings,
                            update_state,
                            reply: None,
                        }),
                    ));
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_global_state_payload(
    core: &Arc<SharedClientCore>,
    revision_tx: &watch::Sender<u64>,
    payload: &Value,
    dirty: &mut DirtyRefreshFlags,
    mux_cancel: &mut Option<CancellationToken>,
    active_mux_generation: &mut u64,
    confirmed_subs: &mut BTreeSet<String>,
    client_id: &str,
    mux_tx: &BoundedMultiplexSender,
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
        "quota_changed" => dirty.auth = true,
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
                    drop(guard);
                    notify_revision(revision_tx, r);
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
                    confirmed_subs.clear();
                } else if mux_cancel.is_none() {
                    *active_mux_generation = active_mux_generation.wrapping_add(1);
                    confirmed_subs.clear();
                    let tok = core.stop.child_token();
                    *mux_cancel = Some(tok.clone());
                    tokio::spawn(run_multiplex_sse_loop(
                        core.http.clone(),
                        client_id.to_owned(),
                        *active_mux_generation,
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
                drop(guard);
                notify_revision(revision_tx, r);
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
            drop(guard);
            notify_revision(revision_tx, r);
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
    generation: u64,
    waiters: Vec<SessionLoadReply>,
) {
    tokio::spawn(async move {
        let result = http.get_session_snapshot(&session_id).await;
        let _ = done_tx.send(InternalAsyncCompletion::SessionSnapshotLoaded {
            session_id,
            generation,
            result,
            waiters,
        });
    });
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
    notify_revision(revision_tx, next_rev);
}

fn notify_revision(tx: &watch::Sender<u64>, revision: u64) {
    // Concurrent HTTP completions may publish out of order after releasing the snapshot lock.
    tx.send_if_modified(|current| {
        if revision <= *current {
            return false;
        }
        *current = revision;
        true
    });
}

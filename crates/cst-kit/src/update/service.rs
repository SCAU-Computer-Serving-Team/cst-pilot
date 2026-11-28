//! Shared update coordination. The independent updater performs replacement after host exit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use cst_os::{Storage, parse_strict_json};
use pi_session::{AgentSession, ExtensionRegistrar, ExtensionUiContext, UiNotifyLevel};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::telemetry::TelemetryService;
use crate::update::{
    UpdateClient, UpdateError,
    activity::{ActivityGate, UpgradeLease},
    handoff::{Handoff, LaunchReceipt},
    layout,
    manifest::VerifiedRelease,
    process::{ProcessIdentity, RestartUi},
    verify::VerifiedPackage,
};

/// A trusted test host may replace process creation, but never signature verification.
pub trait HandoffLauncher: Send + Sync {
    fn launch(&self, handoff: Handoff) -> Result<LaunchReceipt, String>;
}

pub struct NativeLauncher;
impl HandoffLauncher for NativeLauncher {
    fn launch(&self, handoff: Handoff) -> Result<LaunchReceipt, String> {
        handoff.launch()
    }
}

#[derive(Default)]
pub enum UpdateOptions {
    #[default]
    Compiled,
    Disabled,
    Trusted {
        client: Arc<UpdateClient>,
        launcher: Arc<dyn HandoffLauncher>,
    },
}

impl UpdateOptions {
    pub fn with_client(client: UpdateClient) -> Self {
        Self::Trusted {
            client: Arc::new(client),
            launcher: Arc::new(NativeLauncher),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Unavailable,
    Idle,
    Checking,
    Current,
    Available,
    Preparing,
    Ready,
    Launching,
    ExitRequired,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub phase: Phase,
    pub version: Option<String>,
    pub archive_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    pub release_notes: Option<String>,
    pub operation_id: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    pub launch: Option<LaunchReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateControlError {
    Busy(String),
    NotAvailable(String),
    NotReady(String),
    Conflict(String),
    Failed(String),
}

struct ActivePrepare {
    operation_id: String,
    cancel: CancellationToken,
}

type StatusListener = Arc<dyn Fn(&UpdateStatus) + Send + Sync>;

struct State {
    status: UpdateStatus,
    release: Option<VerifiedRelease>,
    package: Option<VerifiedPackage>,
    active_prepare: Option<ActivePrepare>,
    handoff_gate: Option<UpgradeLease>,
}

/// Owned by KitSession / HostedSession, weakly registered in the shared service to avoid a session/runner cycle.
pub struct SessionRegistration {
    session: AgentSession,
    telemetry: Option<Arc<TelemetryService>>,
    stop: CancellationToken,
    ui: Arc<dyn ExtensionUiContext>,
}

pub struct UpdateService {
    root: PathBuf,
    pub gate: Arc<ActivityGate>,
    client: Option<Arc<UpdateClient>>,
    launcher: Arc<dyn HandoffLauncher>,
    state: Mutex<State>,
    listeners: Mutex<Vec<StatusListener>>,
    operation: tokio::sync::Mutex<()>,
    sessions: Arc<Mutex<Vec<Weak<SessionRegistration>>>>,
    stop: CancellationToken,
}

impl UpdateService {
    /// Construct an `UpdateService` bound to an explicitly injected `ActivityGate`.
    /// Session registrations are shared across all hosts targeting the same canonical kit root.
    pub fn new(
        root: &Path,
        _oa_host: &str,
        gate: Arc<ActivityGate>,
        options: UpdateOptions,
    ) -> Result<Arc<Self>, String> {
        let root_state = crate::kit::root_state_for(root)?;
        let root = root.canonicalize().map_err(|_| "工具包目录无法读取。")?;
        let is_compiled_mode = matches!(options, UpdateOptions::Compiled);
        let (client, launcher, message): (_, Arc<dyn HandoffLauncher>, _) = match options {
            UpdateOptions::Disabled => (None, Arc::new(NativeLauncher), "此宿主已关闭升级检查。"),
            UpdateOptions::Trusted { client, launcher } => {
                (Some(client), launcher, "尚未检查更新。")
            }
            UpdateOptions::Compiled => {
                if cst_updater::manifest::compiled_manifest_url().is_err()
                    || super::manifest::ReleaseKey::compiled().is_err()
                {
                    (None, Arc::new(NativeLauncher), "当前构建未启用在线更新。")
                } else if cst_os::KitLayout::new(&root).is_ok_and(|k| k.is_dev_workspace()) {
                    (None, Arc::new(NativeLauncher), "仅发行版支持")
                } else {
                    match compiled_client(&root) {
                        Ok(client) => (
                            Some(Arc::new(client)),
                            Arc::new(NativeLauncher),
                            "尚未检查更新。",
                        ),
                        Err(_) => (None, Arc::new(NativeLauncher), "本地配置异常"),
                    }
                }
            }
        };
        let service = Arc::new(Self {
            gate,
            root,
            launcher,
            state: Mutex::new(State {
                status: UpdateStatus {
                    phase: if client.is_some() {
                        Phase::Idle
                    } else {
                        Phase::Unavailable
                    },
                    version: None,
                    archive_bytes: None,
                    downloaded_bytes: None,
                    release_notes: None,
                    operation_id: None,
                    message: message.into(),
                    error_kind: None,
                    launch: None,
                },
                release: None,
                package: None,
                active_prepare: None,
                handoff_gate: None,
            }),
            listeners: Mutex::new(Vec::new()),
            client,
            operation: tokio::sync::Mutex::new(()),
            sessions: root_state.update_sessions.clone(),
            stop: CancellationToken::new(),
        });
        if should_spawn_startup_check(
            service.client.is_some(),
            is_compiled_mode,
            cst_os::is_offline_env_enabled(),
        ) {
            let service = service.clone();
            tokio::spawn(async move {
                let _ = service.check_inner(service.stop.child_token(), true).await;
            });
        }
        Ok(service)
    }

    pub fn status(&self) -> UpdateStatus {
        self.state.lock().expect("update state").status.clone()
    }

    pub fn last_report(&self) -> Result<Option<cst_os::UpdateReport>, String> {
        cst_updater::applier::read_last_report(&self.root)
    }

    pub fn subscribe_status(&self, listener: StatusListener) {
        self.listeners
            .lock()
            .expect("update status listeners")
            .push(listener);
    }

    fn notify_listeners(&self, status: &UpdateStatus) {
        let listeners = self
            .listeners
            .lock()
            .expect("update status listeners")
            .clone();
        for listener in listeners {
            listener(status);
        }
    }

    fn publish(&self, phase: Phase, message: &str) {
        {
            let mut state = self.state.lock().expect("update state");
            state.status.phase = phase;
            state.status.message = message.into();
        }
        let status = self.status();
        for session in self.participants() {
            session.ui.set_status(
                "cst-update",
                match status.phase {
                    Phase::Available | Phase::Ready => Some(status.message.as_str()),
                    _ => None,
                },
            );
        }
        self.notify_listeners(&status);
    }

    fn publish_download_progress(&self, downloaded: u64) {
        let status = {
            let mut state = self.state.lock().expect("update state");
            if state.status.phase != Phase::Preparing {
                return;
            }
            state.status.downloaded_bytes = Some(downloaded);
            state.status.clone()
        };
        self.notify_listeners(&status);
    }

    fn participants(&self) -> Vec<Arc<SessionRegistration>> {
        let mut entries = self.sessions.lock().expect("update sessions");
        let result = entries.iter().filter_map(Weak::upgrade).collect();
        entries.retain(|s| s.strong_count() > 0);
        result
    }

    pub(crate) fn attach(
        &self,
        session: AgentSession,
        telemetry: Option<Arc<TelemetryService>>,
        stop: CancellationToken,
        ui: Arc<dyn ExtensionUiContext>,
    ) -> Arc<SessionRegistration> {
        let entry = Arc::new(SessionRegistration {
            session,
            telemetry,
            stop,
            ui,
        });
        self.sessions
            .lock()
            .expect("update sessions")
            .push(Arc::downgrade(&entry));
        let status = self.status();
        if matches!(status.phase, Phase::Available | Phase::Ready) {
            entry.ui.set_status("cst-update", Some(&status.message));
        }
        entry
    }

    pub async fn check(&self, cancel: CancellationToken) -> Result<UpdateStatus, String> {
        self.check_inner(cancel, false).await
    }

    /// Control-plane `check` used by `POST /api/app/update/check`:
    /// returns `200 UpdateStatus` when `Phase::Unavailable` or after completing check,
    /// and rejects concurrent check/download/handoff with `UpdateControlError::Busy`.
    pub async fn check_for_app(
        &self,
        cancel: CancellationToken,
    ) -> Result<UpdateStatus, UpdateControlError> {
        if self.client.is_none() {
            return Ok(self.status());
        }
        let _serial = self
            .operation
            .try_lock()
            .map_err(|_| UpdateControlError::Busy("升级检查、下载或交接正在进行中。".into()))?;
        if self.gate.check().is_err() {
            return Err(UpdateControlError::Busy("升级交接正在进行中。".into()));
        }
        if matches!(
            self.status().phase,
            Phase::Preparing | Phase::Launching | Phase::ExitRequired
        ) {
            return Err(UpdateControlError::Busy(
                "升级下载或交接正在进行中。".into(),
            ));
        }
        if self.state.lock().expect("update state").package.is_some()
            && self.revalidate_local_version_for_staged().is_ok()
        {
            return Ok(self.status());
        }
        if self.status().phase == Phase::Failed
            && self.status().error_kind.as_deref() == Some("manual_upgrade_required")
            && layout::read_version(&self.root).is_err()
        {
            return Ok(self.status());
        }
        let _ = self.run_check_locked(cancel).await;
        Ok(self.status())
    }

    async fn check_inner(
        &self,
        cancel: CancellationToken,
        only_initial: bool,
    ) -> Result<UpdateStatus, String> {
        let _serial = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("升级检查已取消。".into()),
            guard = self.operation.lock() => guard
        };
        if only_initial && self.status().phase != Phase::Idle {
            return Ok(self.status());
        }
        self.gate.check()?;
        if self.client.is_none() {
            return Err(self.status().message);
        }
        if self.state.lock().expect("update state").package.is_some() {
            self.revalidate_local_version_for_staged()?;
            return Ok(self.status());
        }
        self.run_check_locked(cancel).await
    }

    fn revalidate_local_version_for_staged(&self) -> Result<(), String> {
        if layout::read_version(&self.root).is_ok() {
            return Ok(());
        }
        {
            let mut state = self.state.lock().expect("update state");
            state.package = None;
            state.release = None;
            state.status.version = None;
            state.status.archive_bytes = None;
            state.status.downloaded_bytes = None;
            state.status.release_notes = None;
            state.status.operation_id = None;
            state.status.error_kind = Some("manual_upgrade_required".into());
        }
        let message = UpdateError::InvalidLocalVersion.ui_message(false);
        self.publish(Phase::Failed, &message);
        Err(message)
    }

    async fn run_check_locked(&self, cancel: CancellationToken) -> Result<UpdateStatus, String> {
        let client = self.client.as_ref().ok_or_else(|| self.status().message)?;
        {
            let mut state = self.state.lock().expect("update state");
            state.status.downloaded_bytes = None;
            state.status.error_kind = None;
        }
        self.publish(Phase::Checking, "正在检查更新。");
        let result = async {
            let current =
                layout::read_version(&self.root).map_err(|_| UpdateError::InvalidLocalVersion)?;
            client.check_detailed(&current, cancel).await
        }
        .await;
        match result {
            Ok(release) => {
                let available = release.is_some();
                {
                    let mut state = self.state.lock().expect("update state");
                    state.status.version = release.as_ref().map(|r| r.info().version.clone());
                    state.status.archive_bytes = release.as_ref().map(|r| r.info().archive.size);
                    state.status.downloaded_bytes = None;
                    state.status.release_notes =
                        release.as_ref().map(|r| r.info().release_notes.clone());
                    state.status.operation_id = None;
                    state.status.error_kind = None;
                    state.release = release;
                }
                self.publish(
                    if available {
                        Phase::Available
                    } else {
                        Phase::Current
                    },
                    if available {
                        "发现新版本，可运行 /update install。"
                    } else {
                        "当前已是最新版本。"
                    },
                );
                Ok(self.status())
            }
            Err(UpdateError::ChannelEmpty) => {
                {
                    let mut state = self.state.lock().expect("update state");
                    state.status.version = None;
                    state.status.archive_bytes = None;
                    state.status.downloaded_bytes = None;
                    state.status.release_notes = None;
                    state.status.operation_id = None;
                    state.status.error_kind = None;
                    state.release = None;
                }
                self.publish(Phase::Current, "暂未发布在线更新");
                Ok(self.status())
            }
            Err(err) => {
                let manual = err.is_manual_upgrade_required();
                let kind = if manual {
                    "manual_upgrade_required"
                } else {
                    "check_failed"
                };
                {
                    let mut state = self.state.lock().expect("update state");
                    state.status.downloaded_bytes = None;
                    state.status.operation_id = None;
                    state.status.error_kind = Some(kind.to_owned());
                    if manual {
                        state.release = None;
                        state.package = None;
                        state.status.version = None;
                        state.status.archive_bytes = None;
                        state.status.release_notes = None;
                    }
                }
                let message = err.ui_message(false);
                self.publish(Phase::Failed, &message);
                Err(self.status().message)
            }
        }
    }

    pub async fn prepare(&self, cancel: CancellationToken) -> Result<UpdateStatus, String> {
        let _serial = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("升级准备已取消。".into()),
            guard = self.operation.lock() => guard
        };
        self.prepare_locked(None, cancel).await
    }

    /// Control-plane `prepare` used by `POST /api/app/update/prepare`:
    /// assigns a fresh `operationId` and cancellable token, rejecting concurrent operations
    /// with `UpdateControlError::Busy` and missing available releases with `NotAvailable`.
    pub async fn prepare_for_app(self: &Arc<Self>) -> Result<UpdateStatus, UpdateControlError> {
        // Once admitted, only the explicit cancel endpoint should interrupt preparation.
        let service = Arc::clone(self);
        tokio::spawn(async move { service.prepare_for_app_inner().await })
            .await
            .map_err(|_| UpdateControlError::Failed("升级准备任务异常结束。".into()))?
    }

    async fn prepare_for_app_inner(&self) -> Result<UpdateStatus, UpdateControlError> {
        let _serial = self
            .operation
            .try_lock()
            .map_err(|_| UpdateControlError::Busy("升级下载或交接正在进行中。".into()))?;
        if self.client.is_none() {
            return Err(UpdateControlError::NotAvailable(self.status().message));
        }
        let staged = {
            let state = self.state.lock().expect("update state");
            if matches!(
                state.status.phase,
                Phase::Preparing | Phase::Launching | Phase::ExitRequired
            ) {
                return Err(UpdateControlError::Busy(
                    "升级包已就绪或正在准备中。".into(),
                ));
            }
            state.package.is_some()
        };
        if staged {
            self.revalidate_local_version_for_staged()
                .map_err(UpdateControlError::Failed)?;
            return Err(UpdateControlError::Busy(
                "升级包已就绪或正在准备中。".into(),
            ));
        }
        if self.state.lock().expect("update state").release.is_none() {
            return Err(UpdateControlError::NotAvailable(
                "请先检查并确认存在新版本。".into(),
            ));
        }
        let op_id = uuid::Uuid::new_v4().to_string();
        let op_cancel = CancellationToken::new();
        {
            let mut state = self.state.lock().expect("update state");
            state.active_prepare = Some(ActivePrepare {
                operation_id: op_id.clone(),
                cancel: op_cancel.clone(),
            });
        }
        self.prepare_locked(Some(op_id), op_cancel.clone())
            .await
            .map_err(|msg| {
                if op_cancel.is_cancelled() {
                    UpdateControlError::Conflict("升级准备已取消。".into())
                } else {
                    UpdateControlError::Failed(msg)
                }
            })
    }

    async fn prepare_locked(
        &self,
        operation_id: Option<String>,
        cancel: CancellationToken,
    ) -> Result<UpdateStatus, String> {
        let _activity = self.gate.enter()?;
        let client = self.client.as_ref().ok_or_else(|| self.status().message)?;
        if self.state.lock().expect("update state").package.is_some() {
            self.revalidate_local_version_for_staged()?;
            return Ok(self.status());
        }
        let release = self
            .state
            .lock()
            .expect("update state")
            .release
            .clone()
            .ok_or("请先检查并确认存在新版本。")?;
        let total_size = release.info().archive.size;
        {
            let mut state = self.state.lock().expect("update state");
            state.status.operation_id = operation_id.clone();
            state.status.downloaded_bytes = Some(0);
            state.status.error_kind = None;
        }
        self.publish(Phase::Preparing, "正在下载并验证升级包。");
        let mut last_emitted_bytes = 0u64;
        let mut last_emitted_at = Instant::now();
        const PROGRESS_STEP_BYTES: u64 = 256 * 1024;
        const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
        let res = client
            .prepare_detailed(&self.root, &release, cancel.clone(), |downloaded| {
                let now = Instant::now();
                if downloaded >= total_size
                    || downloaded.saturating_sub(last_emitted_bytes) >= PROGRESS_STEP_BYTES
                    || now.duration_since(last_emitted_at) >= PROGRESS_INTERVAL
                {
                    last_emitted_bytes = downloaded;
                    last_emitted_at = now;
                    self.publish_download_progress(downloaded);
                }
            })
            .await;
        match res {
            Ok(package) => {
                {
                    let mut state = self.state.lock().expect("update state");
                    state.active_prepare = None;
                    state.status.downloaded_bytes = Some(total_size);
                    state.status.error_kind = None;
                    state.package = Some(package);
                }
                self.publish(Phase::Ready, "升级包已验证，可开始交接。");
                Ok(self.status())
            }
            Err(_) if cancel.is_cancelled() => {
                let has_release = {
                    let mut state = self.state.lock().expect("update state");
                    state.active_prepare = None;
                    state.status.operation_id = None;
                    state.status.downloaded_bytes = None;
                    state.status.error_kind = None;
                    state.release.is_some()
                };
                if has_release && self.revalidate_local_version_for_staged().is_ok() {
                    self.publish(Phase::Available, "已取消下载，可重新升级。");
                }
                Err("升级准备已取消。".into())
            }
            Err(err) => {
                let manual = err.is_manual_upgrade_required();
                let kind = if manual {
                    "manual_upgrade_required"
                } else {
                    "download_failed"
                };
                {
                    let mut state = self.state.lock().expect("update state");
                    state.active_prepare = None;
                    state.status.operation_id = None;
                    state.status.downloaded_bytes = None;
                    state.status.error_kind = Some(kind.to_owned());
                    if manual {
                        state.release = None;
                        state.package = None;
                        state.status.version = None;
                        state.status.archive_bytes = None;
                        state.status.release_notes = None;
                    }
                }
                let message = err.ui_message(true);
                self.publish(Phase::Failed, &message);
                Err(self.status().message)
            }
        }
    }

    /// Cancels an active or staged `prepare`, waits for download/extraction cleanup,
    /// and restores `Phase::Available` when a verified release is present.
    pub async fn cancel_prepare(
        &self,
        expected_operation_id: Option<&str>,
    ) -> Result<UpdateStatus, UpdateControlError> {
        let operation_id = {
            let state = self.state.lock().expect("update state");
            if matches!(state.status.phase, Phase::Launching | Phase::ExitRequired) {
                return Err(UpdateControlError::Busy(
                    "升级交接已开始，无法取消。".into(),
                ));
            }
            let current_id = state
                .active_prepare
                .as_ref()
                .map(|active| active.operation_id.as_str())
                .or(state.status.operation_id.as_deref());
            if expected_operation_id.is_some() && expected_operation_id != current_id {
                return Err(UpdateControlError::Conflict("取消操作标识不匹配。".into()));
            }
            if let Some(ref active) = state.active_prepare {
                active.cancel.cancel();
            }
            current_id.map(str::to_owned)
        };
        let _serial = self.operation.lock().await;
        let (has_release, was_preparing_or_staged) = {
            let mut state = self.state.lock().expect("update state");
            if matches!(state.status.phase, Phase::Launching | Phase::ExitRequired) {
                return Err(UpdateControlError::Busy(
                    "升级交接已开始，无法取消。".into(),
                ));
            }
            if let Some(current) = state.status.operation_id.as_ref()
                && Some(current) != operation_id.as_ref()
            {
                return Err(UpdateControlError::Conflict("取消操作标识不匹配。".into()));
            }
            let had_stage = state.package.take().is_some();
            let had_active = state.active_prepare.take().is_some();
            state.status.operation_id = None;
            state.status.downloaded_bytes = None;
            if state.status.phase != Phase::Failed {
                state.status.error_kind = None;
            }
            (
                state.release.is_some(),
                had_stage || had_active || state.status.phase == Phase::Available,
            )
        };
        if has_release
            && was_preparing_or_staged
            && self.revalidate_local_version_for_staged().is_ok()
        {
            self.publish(Phase::Available, "已取消下载，可重新升级。");
        }
        Ok(self.status())
    }

    pub fn is_ready(&self) -> bool {
        let state = self.state.lock().expect("update state");
        state.status.phase == Phase::Ready && state.package.is_some()
    }

    /// Capture every relevant desktop/host identity in the trusted launcher. The library never exits them.
    /// Once started, this task finishes independently of a dropped caller; status retains the receipt.
    pub async fn launch(
        self: &Arc<Self>,
        parents: Vec<ProcessIdentity>,
        ui: RestartUi,
        cancel: CancellationToken,
    ) -> Result<LaunchReceipt, String> {
        self.launch_after_validation(parents, ui, cancel, || async { Ok(()) })
            .await
    }

    /// Check host-specific busy state after the shared write gate has closed.
    pub async fn launch_after_validation<F, Fut>(
        self: &Arc<Self>,
        parents: Vec<ProcessIdentity>,
        ui: RestartUi,
        cancel: CancellationToken,
        validate: F,
    ) -> Result<LaunchReceipt, String>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send,
    {
        let service = self.clone();
        tokio::spawn(async move { service.launch_inner(parents, ui, cancel, validate).await })
            .await
            .map_err(|_| "升级交接任务异常结束。".to_owned())?
    }

    async fn launch_inner<F, Fut>(
        self: &Arc<Self>,
        parents: Vec<ProcessIdentity>,
        ui: RestartUi,
        cancel: CancellationToken,
        validate: F,
    ) -> Result<LaunchReceipt, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        let _serial = self.operation.lock().await;
        if cancel.is_cancelled() {
            return Err("升级交接已取消。".into());
        }
        let gate = self.gate.close()?;
        validate().await?;
        let participants = self.participants();
        if participants
            .iter()
            .any(|p| !p.session.is_idle() || p.session.pending_message_count() > 0)
        {
            return Err("仍有会话运行、输入排队或资源重载，请结束后重试升级。".into());
        }
        let key = self
            .client
            .as_ref()
            .ok_or_else(|| self.status().message)?
            .key()
            .clone();
        let package = self
            .state
            .lock()
            .expect("update state")
            .package
            .take()
            .ok_or("升级包尚未准备完成。")?;
        self.publish(Phase::Launching, "正在启动独立更新器。");
        let root = self.root.clone();
        let launcher = self.launcher.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let handoff = Handoff::prepare(&root, package, &key, parents, ui, &cancel)?;
            if cancel.is_cancelled() {
                return Err("升级交接已取消。".to_owned());
            }
            let receipt = launcher
                .launch(handoff)
                .map_err(|_| "独立更新器启动失败，当前程序未替换。".to_owned())?;
            Ok::<_, String>((receipt, gate))
        })
        .await
        .map_err(|_| "升级交接工作线程异常退出。".to_owned())?;
        let (receipt, gate) = match prepared {
            Ok(result) => result,
            Err(_) => {
                self.publish(Phase::Failed, "升级交接失败，当前程序与用户数据保持不变。");
                return Err(self.status().message);
            }
        };
        {
            let mut state = self.state.lock().expect("update state");
            state.handoff_gate = Some(gate);
            state.status.launch = Some(receipt.clone());
        }
        let mut telemetries: Vec<Arc<TelemetryService>> = Vec::new();
        for participant in &participants {
            participant.stop.cancel();
            if let Some(ref telemetry) = participant.telemetry
                && !telemetries
                    .iter()
                    .any(|existing| Arc::ptr_eq(existing, telemetry))
            {
                telemetries.push(telemetry.clone());
            }
        }
        for telemetry in telemetries {
            telemetry.close_process("quit").await;
        }
        self.publish(
            Phase::ExitRequired,
            "更新器已就绪，请由宿主退出全部交接进程；更新器随后安装并重启原界面。",
        );
        Ok(receipt)
    }

    pub(crate) fn register(self: &Arc<Self>, api: &mut ExtensionRegistrar) {
        let service = self.clone();
        api.register_simple_command(
            "update",
            "检查更新；/update install 验证并交接升级",
            move |args, ctx| {
                let service = service.clone();
                async move {
                    let cancel = ctx.cancel_token().unwrap_or_default();
                    match args.trim() {
                        "" | "check" => {
                            let status = service.check(cancel).await?;
                            ctx.ui().notify(&status.message, UiNotifyLevel::Info);
                        }
                        "install" => {
                            if !ctx.has_ui() {
                                return Err(
                                    "升级需要本地确认，请使用交互界面或可信宿主接口。".into()
                                );
                            }
                            let status = service.check(cancel.clone()).await?;
                            if !matches!(status.phase, Phase::Available | Phase::Ready) {
                                ctx.ui().notify(&status.message, UiNotifyLevel::Info);
                                return Ok(());
                            }
                            let ui = ctx.ui();
                            let question = format!(
                                "升级至 {}？将校验安装包并退出当前界面。",
                                status.version.as_deref().unwrap_or("新版本")
                            );
                            let confirmed = ui
                                .confirm(
                                    "确认软件升级",
                                    &question,
                                    Some(pi_session::UiDialogOptions {
                                        timeout_ms: Some(180_000),
                                        ..Default::default()
                                    }),
                                    cancel.clone(),
                                )
                                .await;
                            if !confirmed || cancel.is_cancelled() {
                                return Ok(());
                            }
                            service.prepare(cancel.clone()).await?;
                            service
                                .launch(vec![ProcessIdentity::current()?], RestartUi::Tui, cancel)
                                .await?;
                            ui.notify(&service.status().message, UiNotifyLevel::Info);
                            ctx.shutdown();
                        }
                        _ => return Err("用法：/update [check|install]。".into()),
                    }
                    Ok(())
                }
            },
        );
    }
}

impl Drop for UpdateService {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Call before starting auth, sessions or loading program resources. Recovery runs from an independent updater.
pub fn startup_check(root: &Path) -> Result<(), String> {
    let storage = Storage::new(root)?;
    let relative = Path::new(".state/update");
    if !storage.root.join(relative).exists() {
        return Ok(());
    }
    let _pin = storage.pin_directory(relative, false)?;
    if storage
        .open_read(&storage.root.join(relative).join("transaction.json"))?
        .is_some()
    {
        return Err("检测到未完成的升级，请退出工具包并由独立更新器执行恢复后再启动。".into());
    }
    Ok(())
}

/// Validates compiled-in update manifest URL and public key, returning the 16-hex key fingerprint.
pub fn compiled_update_key_fingerprint() -> Result<String, String> {
    let _ = cst_updater::manifest::compiled_manifest_url()?;
    let key = super::manifest::ReleaseKey::compiled()?;
    Ok(key.fingerprint())
}

fn resolve_update_allow_tun(kit_layout: &cst_os::KitLayout) -> Result<bool, String> {
    use std::io::Read as _;
    use std::path::Component;

    let clean_root = cst_os::strip_windows_verbatim_prefix(kit_layout.root());
    let clean_data = cst_os::strip_windows_verbatim_prefix(kit_layout.data_dir());
    let (anchor, rel_target) = if let Ok(rel) = clean_data.strip_prefix(&clean_root) {
        (clean_root, rel.join("config"))
    } else {
        let full = clean_data.join("config");
        let mut anchor = PathBuf::new();
        let mut rel = PathBuf::new();
        for comp in full.components() {
            match comp {
                Component::Prefix(p) => anchor.push(p.as_os_str()),
                Component::RootDir => anchor.push(comp.as_os_str()),
                Component::Normal(part) => rel.push(part),
                Component::CurDir | Component::ParentDir => {
                    return Err("配置目录路径无效。".into());
                }
            }
        }
        if anchor.as_os_str().is_empty() {
            return Err("数据目录必须是绝对路径。".into());
        }
        (anchor, rel)
    };

    let mut dir_handles = Vec::new();
    let (root_handle, _) = cst_os::pin_directory_handle(&anchor)?;
    dir_handles.push(root_handle);

    let mut cur_path = anchor;
    for comp in rel_target.components() {
        let Component::Normal(part) = comp else {
            return Err("配置目录路径无效。".into());
        };
        cur_path.push(part);
        #[cfg(target_os = "linux")]
        let probe_path = {
            use std::os::unix::io::AsRawFd;
            let parent_h = dir_handles.last().expect("anchor handle present");
            PathBuf::from(format!("/proc/self/fd/{}", parent_h.as_raw_fd())).join(part)
        };
        #[cfg(not(target_os = "linux"))]
        let probe_path = cur_path.clone();

        match std::fs::symlink_metadata(&probe_path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(_) => return Err("配置目录状态无法读取。".into()),
            Ok(meta) if cst_os::is_reparse_or_symlink(&meta) || !meta.is_dir() => {
                return Err("配置目录不能为符号链接或文件。".into());
            }
            Ok(_) => {
                let parent_h = dir_handles.last().expect("anchor handle present");
                let (child_h, _) =
                    cst_os::pin_child_directory_handle(parent_h, &cur_path, part, false)?;
                dir_handles.push(child_h);
            }
        }
    }

    let cfg_file_path = cur_path.join("web-search.json");
    #[cfg(target_os = "linux")]
    let cfg_probe_path = {
        use std::os::unix::io::AsRawFd;
        let cfg_dir_h = dir_handles.last().expect("config dir handle present");
        PathBuf::from(format!("/proc/self/fd/{}", cfg_dir_h.as_raw_fd())).join("web-search.json")
    };
    #[cfg(not(target_os = "linux"))]
    let cfg_probe_path = cfg_file_path.clone();

    match std::fs::symlink_metadata(&cfg_probe_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(_) => return Err("联网检索配置状态无法读取。".into()),
        Ok(meta) if cst_os::is_reparse_or_symlink(&meta) || !meta.is_file() => {
            return Err("联网检索配置不能为符号链接或非普通文件。".into());
        }
        Ok(_) => {}
    }

    let cfg_dir_h = dir_handles.last().expect("config dir handle present");
    let (opened, _, len) = cst_os::open_read_in_pinned_dir(
        cfg_dir_h,
        &cfg_file_path,
        std::ffi::OsStr::new("web-search.json"),
    )?;
    if cst_os::link_count(&opened)? != 1 {
        return Err("联网检索配置不能是硬链接文件。".into());
    }
    let max_bytes = crate::tools::web_access::MAX_CONFIG_BYTES;
    if len > max_bytes as u64 {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    if opened
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > max_bytes
    {
        return Ok(false);
    }
    drop(dir_handles);

    let trimmed = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let Ok(value) = parse_strict_json(trimmed) else {
        return Ok(false);
    };
    let Some(obj) = value.as_object() else {
        return Ok(false);
    };
    for section in ["ssrf", "defaults", "providers"] {
        if obj.get(section).is_some_and(|v| !v.is_object()) {
            return Ok(false);
        }
    }
    let Ok(cfg) = crate::tools::web_access::WebSearchConfig::from_json_value(value) else {
        return Ok(false);
    };
    Ok(cfg.ssrf.allow_ranges.iter().any(|r| r == "198.18.0.0/15"))
}

fn compiled_client(root: &Path) -> Result<UpdateClient, String> {
    let source = cst_updater::manifest::compiled_manifest_url()?;
    let key = super::manifest::ReleaseKey::compiled()?;
    let kit_layout = cst_os::KitLayout::new(root)?;
    let allow_tun = resolve_update_allow_tun(&kit_layout)?;
    UpdateClient::with_fetcher(
        source.as_str(),
        key,
        Arc::new(super::network::NativeFetcher::new(
            crate::tools::ssrf::NetworkPolicy { allow_tun },
        )),
    )
}

fn should_spawn_startup_check(has_client: bool, is_compiled_mode: bool, offline_env: bool) -> bool {
    has_client && !(is_compiled_mode && offline_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_check_gate_skips_compiled_mode_only_when_offline() {
        assert!(!should_spawn_startup_check(false, true, false));
        assert!(!should_spawn_startup_check(false, false, false));
        assert!(should_spawn_startup_check(true, true, false));
        assert!(!should_spawn_startup_check(true, true, true));
        assert!(should_spawn_startup_check(true, false, false));
        assert!(should_spawn_startup_check(true, false, true));
    }

    #[test]
    fn default_compiled_fingerprint_resolves_when_not_disabled() {
        if option_env!("CST_UPDATE_MANIFEST_URL")
            .is_some_and(cst_updater::manifest::is_disabled_update_sentinel)
            || option_env!("CST_UPDATE_PUBLIC_KEY")
                .is_some_and(cst_updater::manifest::is_disabled_update_sentinel)
        {
            assert!(compiled_update_key_fingerprint().is_err());
        } else {
            let fp = compiled_update_key_fingerprint().expect("compiled update root must be valid");
            assert_eq!(fp.len(), 16);
        }
    }

    fn temp_test_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "cst-update-svc-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn compiled_mode_marks_dev_workspace_unavailable_without_startup_check() {
        let dir = temp_test_dir("dev-ws");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"dev\"\n").unwrap();
        let gate = ActivityGate::new();
        let svc = UpdateService::new(&dir, "https://www.cstoa.top", gate, UpdateOptions::Compiled)
            .unwrap();
        let st = svc.status();
        assert_eq!(st.phase, Phase::Unavailable);
        if option_env!("CST_UPDATE_MANIFEST_URL")
            .is_some_and(cst_updater::manifest::is_disabled_update_sentinel)
            || option_env!("CST_UPDATE_PUBLIC_KEY")
                .is_some_and(cst_updater::manifest::is_disabled_update_sentinel)
        {
            assert_eq!(st.message, "当前构建未启用在线更新。");
        } else {
            assert_eq!(st.message, "仅发行版支持");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_update_allow_tun_defaults_true_and_respects_explicit_override() {
        let dir = temp_test_dir("allow-tun");
        let data_dir = dir.join("data");
        let layout = cst_os::KitLayout::with_data_dir(&dir, &data_dir).unwrap();
        assert!(resolve_update_allow_tun(&layout).unwrap());

        // Non-directory `data` path must reject instead of silently defaulting to true
        std::fs::write(&data_dir, b"not-a-dir").unwrap();
        assert!(resolve_update_allow_tun(&layout).is_err());
        std::fs::remove_file(&data_dir).unwrap();

        let cfg_dir = layout.config_dir();
        std::fs::create_dir_all(&cfg_dir).unwrap();
        assert!(resolve_update_allow_tun(&layout).unwrap());

        // UTF-8 BOM + omitted ssrf defaults to true
        let mut bom_json = vec![0xEF, 0xBB, 0xBF];
        bom_json.extend_from_slice(br#"{"version":2}"#);
        std::fs::write(cfg_dir.join("web-search.json"), bom_json).unwrap();
        assert!(resolve_update_allow_tun(&layout).unwrap());

        // Explicit empty allowRanges disables TUN
        std::fs::write(
            cfg_dir.join("web-search.json"),
            br#"{"version":2,"ssrf":{"allowRanges":[]}}"#,
        )
        .unwrap();
        assert!(!resolve_update_allow_tun(&layout).unwrap());

        // Non-object ssrf, invalid sibling fields, legacy empty ssrf, or extra CIDRs all fail closed
        for invalid in [
            r#"{"version":2,"ssrf":null}"#,
            r#"{"version":2,"ssrf":[]}"#,
            r#"{"version":2,"ssrf":{"allowRanges":null}}"#,
            r#"{"version":2,"ssrf":{"allowRanges":[123]}}"#,
            r#"{"version":2,"ssrf":{"trustEnvProxy":"bad"}}"#,
            r#"{"ssrf":{}}"#,
            r#"{"version":2,"ssrf":{"allowRanges":["198.18.0.0/15"],"allowRanges":[]}}"#,
            r#"{"version":2,"ssrf":{"allowRanges":["198.18.0.0/15","10.0.0.0/8"]}}"#,
            "{bad",
        ] {
            std::fs::write(cfg_dir.join("web-search.json"), invalid.as_bytes()).unwrap();
            assert!(!resolve_update_allow_tun(&layout).unwrap(), "{invalid}");
        }

        // External data_dir outside kit root also walks every ancestor and rejects non-directory ancestors
        let ext_base = temp_test_dir("allow-tun-ext");
        let ext_missing = ext_base.join("nested").join("data");
        let ext_layout = cst_os::KitLayout::with_data_dir(&dir, &ext_missing).unwrap();
        assert!(resolve_update_allow_tun(&ext_layout).unwrap());

        std::fs::write(ext_base.join("nested"), b"file-ancestor").unwrap();
        assert!(resolve_update_allow_tun(&ext_layout).is_err());

        let _ = std::fs::remove_dir_all(&ext_base);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Shared update coordination. The independent updater performs replacement after host exit.
use super::{
    UpdateClient,
    activity::{ActivityGate, UpgradeLease},
    handoff::{Handoff, LaunchReceipt},
    layout,
    manifest::VerifiedRelease,
    process::{ProcessIdentity, RestartUi},
    verify::VerifiedPackage,
};
use crate::{runbook::storage::Storage, telemetry::TelemetryService};
use pi_session::{AgentSession, ExtensionRegistrar, ExtensionUiContext, UiNotifyLevel};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio_util::sync::CancellationToken;

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
    pub release_notes: Option<String>,
    pub message: String,
    pub launch: Option<LaunchReceipt>,
}
struct State {
    status: UpdateStatus,
    release: Option<VerifiedRelease>,
    package: Option<VerifiedPackage>,
    handoff_gate: Option<UpgradeLease>,
}
/// Owned by KitSession, weakly registered in the shared service to avoid a session/runner cycle.
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
    operation: tokio::sync::Mutex<()>,
    sessions: Mutex<Vec<Weak<SessionRegistration>>>,
    stop: CancellationToken,
}
impl UpdateService {
    pub fn load(root: &Path, oa_host: &str, options: UpdateOptions) -> Result<Arc<Self>, String> {
        let root = root.canonicalize().map_err(|_| "工具包目录无法读取。")?;
        let key = if cfg!(windows) {
            PathBuf::from(root.to_string_lossy().to_lowercase())
        } else {
            root.clone()
        };
        static SERVICES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<UpdateService>>>> = OnceLock::new();
        let mut services = SERVICES
            .get_or_init(Default::default)
            .lock()
            .expect("update services");
        if let Some(service) = services.get(&key).and_then(Weak::upgrade) {
            return Ok(service);
        }
        let (client, launcher, message): (_, Arc<dyn HandoffLauncher>, _) = match options {
            UpdateOptions::Disabled => (None, Arc::new(NativeLauncher), "此宿主已关闭升级检查。"),
            UpdateOptions::Trusted { client, launcher } => {
                (Some(client), launcher, "尚未检查更新。")
            }
            UpdateOptions::Compiled => match compiled_client(&root, oa_host) {
                Ok(client) => (
                    Some(Arc::new(client)),
                    Arc::new(NativeLauncher),
                    "尚未检查更新。",
                ),
                Err(_) => (
                    None,
                    Arc::new(NativeLauncher),
                    "此构建未配置可用的官方升级公钥或发布源。",
                ),
            },
        };
        let service = Arc::new(Self {
            gate: ActivityGate::for_root(&root)?,
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
                    release_notes: None,
                    message: message.into(),
                    launch: None,
                },
                release: None,
                package: None,
                handoff_gate: None,
            }),
            client,
            operation: tokio::sync::Mutex::new(()),
            sessions: Mutex::default(),
            stop: CancellationToken::new(),
        });
        services.retain(|_, v| v.strong_count() > 0);
        services.insert(key, Arc::downgrade(&service));
        // No UI dialog and no blocking wait on startup. One check for each shared home.
        if service.client.is_some() {
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
    async fn check_inner(
        &self,
        cancel: CancellationToken,
        only_initial: bool,
    ) -> Result<UpdateStatus, String> {
        let _serial = tokio::select! { biased; _ = cancel.cancelled() => return Err("升级检查已取消。".into()), guard = self.operation.lock() => guard };
        if only_initial && self.status().phase != Phase::Idle {
            return Ok(self.status());
        }
        self.gate.check()?;
        let client = self.client.as_ref().ok_or_else(|| self.status().message)?;
        if self.state.lock().expect("update state").package.is_some() {
            return Ok(self.status());
        }
        self.publish(Phase::Checking, "正在检查更新。");
        let result = async {
            let current = layout::read_version(&self.root)?;
            client.check(&current, cancel).await
        }
        .await;
        match result {
            Ok(release) => {
                let available = release.is_some();
                {
                    let mut state = self.state.lock().expect("update state");
                    state.status.version = release.as_ref().map(|r| r.info().version.clone());
                    state.status.release_notes =
                        release.as_ref().map(|r| r.info().release_notes.clone());
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
            Err(_) => {
                self.publish(
                    Phase::Failed,
                    "升级检查未完成，请检查网络、发行配置或版本后重试。",
                );
                Err(self.status().message)
            }
        }
    }
    pub async fn prepare(&self, cancel: CancellationToken) -> Result<UpdateStatus, String> {
        let _serial = tokio::select! { biased; _ = cancel.cancelled() => return Err("升级准备已取消。".into()), guard = self.operation.lock() => guard };
        let _activity = self.gate.enter()?;
        let client = self.client.as_ref().ok_or_else(|| self.status().message)?;
        if self.state.lock().expect("update state").package.is_some() {
            return Ok(self.status());
        }
        let release = self
            .state
            .lock()
            .expect("update state")
            .release
            .clone()
            .ok_or("请先检查并确认存在新版本。")?;
        self.publish(Phase::Preparing, "正在下载并验证升级包。");
        match client.prepare(&self.root, &release, cancel).await {
            Ok(package) => {
                self.state.lock().expect("update state").package = Some(package);
                self.publish(Phase::Ready, "升级包已验证，可开始交接。");
                Ok(self.status())
            }
            Err(_) => {
                self.publish(
                    Phase::Failed,
                    "升级包准备失败，当前程序与用户数据保持不变。",
                );
                Err(self.status().message)
            }
        }
    }
    /// Capture every relevant desktop/host identity in the trusted launcher. The library never exits them.
    /// Once started, this task finishes independently of a dropped caller; status retains the receipt.
    pub async fn launch(
        self: &Arc<Self>,
        parents: Vec<ProcessIdentity>,
        ui: RestartUi,
        cancel: CancellationToken,
    ) -> Result<LaunchReceipt, String> {
        let service = self.clone();
        tokio::spawn(async move { service.launch_inner(parents, ui, cancel).await })
            .await
            .map_err(|_| "升级交接任务异常结束。".to_owned())?
    }
    async fn launch_inner(
        self: &Arc<Self>,
        parents: Vec<ProcessIdentity>,
        ui: RestartUi,
        cancel: CancellationToken,
    ) -> Result<LaunchReceipt, String> {
        let _serial = self.operation.lock().await;
        if cancel.is_cancelled() {
            return Err("升级交接已取消。".into());
        }
        let gate = self.gate.close()?;
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
            .key
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
        // The closed gate is owned by the actual blocking worker, including caller/runtime cancellation.
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
        // Child is only waiting for parent handles. Replacement cannot begin until after this local close.
        // Start finalization only after successful process creation, so launch failure leaves telemetry usable.
        for participant in &participants {
            participant.stop.cancel();
        }
        if let Some(telemetry) = participants.iter().find_map(|p| p.telemetry.clone()) {
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
                            // Request orderly session shutdown; process exit remains a trusted launcher responsibility.
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
fn compiled_client(root: &Path, oa_host: &str) -> Result<UpdateClient, String> {
    let key = super::manifest::ReleaseKey::compiled()?;
    let source = super::network::release_url(oa_host)?
        .join("/api/agent/releases/latest")
        .map_err(|_| "升级源无效。")?;
    let storage = Storage::new(root)?;
    let _home = storage.pin_directory(Path::new("agent/home"), false)?;
    let allow_tun = storage
        .read_limited(&storage.root.join("agent/home/web-search.json"), 64 * 1024)?
        .and_then(|bytes| super::json::parse(&bytes).ok())
        .and_then(|value| {
            value
                .get("ssrf")?
                .get("allowRanges")?
                .as_array()
                .map(|ranges| ranges.iter().any(|v| v == "198.18.0.0/15"))
        })
        .unwrap_or(false);
    UpdateClient::with_fetcher(
        source.as_str(),
        key,
        Arc::new(super::network::NativeFetcher::new(
            crate::tools::ssrf::NetworkPolicy { allow_tun },
        )),
    )
}

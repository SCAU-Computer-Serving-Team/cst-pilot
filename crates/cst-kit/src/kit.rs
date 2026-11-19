use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use cst_os::{KitLayout, strip_windows_verbatim_prefix};
use pi_runtime::{
    CreateAgentSessionOptions, CreateAgentSessionResult, DefaultResourceLoader,
    DefaultResourceLoaderOptions, FileSettingsStorage, InitialModelPolicy, ReadToolOptions,
    SettingsManager, SettingsScope, SettingsStorage, SettingsUpdate, create_agent_session,
    create_read_tool_definition, strip_json_comments,
};
use pi_session::{
    Extension, ExtensionFactory, ExtensionInitError, ExtensionRegistrar, SessionBeforeEventOutput,
    SessionDataPolicy, ToolDefinition,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::auth::{AuthRuntime, Channel, oa::OaClient};
use crate::telemetry::{TelemetryBinding, TelemetryOptions, TelemetryService, billing::OaBilling};
use crate::tools::{
    fff,
    path_guard::{GuardedReadOperations, PathGuard},
    web_access::WebAccess,
};
use crate::update::activity::ActivityGate;
use crate::update::service::{SessionRegistration, UpdateOptions, UpdateService};

pub const OPEN_TOOL_NAMES: &[&str] = &[
    "read",
    "ls",
    "disk",
    "sys",
    "startup",
    "eventlog",
    "driver",
    "command_plan",
    "runbook_deliver",
    "runbook",
    "bitlocker",
    "fffind",
    "ffgrep",
    "web_search",
    "source_check",
    "fetch_content",
    "get_search_content",
];

pub const SKILL_NAMES: &[&str] = cst_updater::layout::REQUIRED_SKILL_NAMES;

/// Per-normalized-`kit_root` coordination primitives owned at the composition root.
/// Guarantees that every handle targeting the same canonical kit directory shares one
/// `ActivityGate` and per-root write serialization locks, while distinct kit roots stay isolated.
pub(crate) struct KitRootState {
    pub(crate) gate: Arc<ActivityGate>,
    pub(crate) auth_writes: Arc<tokio::sync::Mutex<()>>,
    pub(crate) runbook_writes: Arc<tokio::sync::Mutex<()>>,
    pub(crate) telemetry_file_lock: Arc<tokio::sync::Mutex<()>>,
}

struct WeakRootState {
    gate: Weak<ActivityGate>,
    auth_writes: Weak<tokio::sync::Mutex<()>>,
    runbook_writes: Weak<tokio::sync::Mutex<()>>,
    telemetry_file_lock: Weak<tokio::sync::Mutex<()>>,
}

impl WeakRootState {
    fn is_alive(&self) -> bool {
        self.gate.strong_count() > 0
            || self.auth_writes.strong_count() > 0
            || self.runbook_writes.strong_count() > 0
            || self.telemetry_file_lock.strong_count() > 0
    }
}

pub(crate) fn root_state_for(root: &Path) -> Result<Arc<KitRootState>, String> {
    let layout = KitLayout::new(root)?;
    let key = layout.normalized_key();
    static ROOTS: OnceLock<Mutex<BTreeMap<PathBuf, WeakRootState>>> = OnceLock::new();
    let mut roots = ROOTS
        .get_or_init(Default::default)
        .lock()
        .expect("kit root states");
    roots.retain(|_, weak| weak.is_alive());
    let entry = roots.entry(key).or_insert_with(|| WeakRootState {
        gate: Weak::new(),
        auth_writes: Weak::new(),
        runbook_writes: Weak::new(),
        telemetry_file_lock: Weak::new(),
    });
    let gate = entry.gate.upgrade().unwrap_or_else(|| {
        let created = ActivityGate::new();
        entry.gate = Arc::downgrade(&created);
        created
    });
    let auth_writes = entry.auth_writes.upgrade().unwrap_or_else(|| {
        let created = Arc::new(tokio::sync::Mutex::new(()));
        entry.auth_writes = Arc::downgrade(&created);
        created
    });
    let runbook_writes = entry.runbook_writes.upgrade().unwrap_or_else(|| {
        let created = Arc::new(tokio::sync::Mutex::new(()));
        entry.runbook_writes = Arc::downgrade(&created);
        created
    });
    let telemetry_file_lock = entry.telemetry_file_lock.upgrade().unwrap_or_else(|| {
        let created = Arc::new(tokio::sync::Mutex::new(()));
        entry.telemetry_file_lock = Arc::downgrade(&created);
        created
    });
    Ok(Arc::new(KitRootState {
        gate,
        auth_writes,
        runbook_writes,
        telemetry_file_lock,
    }))
}

#[derive(Clone)]
struct AuthHooks {
    provider: pi_ai::Provider,
    accounts: Arc<crate::auth::account::AccountService>,
    stop: CancellationToken,
    wake: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
pub struct Kit {
    root: PathBuf,
    guard: PathGuard,
    root_state: Arc<KitRootState>,
}

impl Kit {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let layout = KitLayout::new(root.as_ref())?;
        let root = layout.root().to_path_buf();
        let guard = PathGuard::new(&root)?;
        let root_state = root_state_for(&root)?;
        Ok(Self {
            root,
            guard,
            root_state,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn agent_dir(&self) -> PathBuf {
        self.root.join("agent/home")
    }

    pub fn path_guard(&self) -> &PathGuard {
        &self.guard
    }

    pub fn activity_gate(&self) -> &Arc<ActivityGate> {
        &self.root_state.gate
    }

    /// Keep model/UI/session options; replace all resource and tool policy overrides.
    /// Only implemented business tools are activated. No built-in ls fallback is exposed.
    pub fn session_options(
        &self,
        options: CreateAgentSessionOptions,
    ) -> Result<CreateAgentSessionOptions, String> {
        crate::update::service::startup_check(self.root())?;
        self.session_options_with_auth(options, None, None, None, None, None)
    }

    /// Validate local assembly during an updater-owned transaction without constructing all tools.
    pub(crate) fn check_resources(&self) -> Result<(), String> {
        self.activity_gate().check()?;
        for name in SKILL_NAMES {
            self.resource(&format!("skills/{name}/SKILL.md"))?;
        }
        self.resource("APPEND_SYSTEM.md")?;
        WebAccess::from_config(&self.agent_dir().join("web-search.json"))?;
        Ok(())
    }

    fn session_options_with_auth(
        &self,
        mut options: CreateAgentSessionOptions,
        auth: Option<AuthHooks>,
        telemetry: Option<Arc<TelemetryBinding>>,
        updates: Option<Arc<UpdateService>>,
        bitlocker_executor: Option<crate::bitlocker::core::Executor>,
        bitlocker_host: Option<Arc<dyn crate::bitlocker::BitLockerHostChannel>>,
    ) -> Result<CreateAgentSessionOptions, String> {
        self.activity_gate().check()?;
        let cwd_canonical = options
            .cwd
            .clone()
            .unwrap_or_else(|| self.root.clone())
            .canonicalize()
            .map_err(|_| "工作目录无法读取。")?;
        let cwd = strip_windows_verbatim_prefix(&cwd_canonical);
        let agent_dir = strip_windows_verbatim_prefix(&self.agent_dir());
        let settings = SettingsManager::from_storage(
            Arc::new(KitSettingsStorage(
                FileSettingsStorage::new(&cwd, &agent_dir),
                self.activity_gate().clone(),
            )),
            false,
        );
        let mut extension =
            CstPilotExtension::new_with_bitlocker(self, bitlocker_executor, bitlocker_host)?;
        extension.auth = auth;
        extension.telemetry = telemetry;
        let update_command = updates.is_some();
        extension.updates = updates;
        let active = extension.tool_names();
        let factory: ExtensionFactory =
            Arc::new(move || Ok(Box::new(extension.clone()) as Box<dyn Extension>));
        let mut skill_paths = Vec::new();
        for name in SKILL_NAMES {
            skill_paths.push(self.resource(&format!("skills/{name}/SKILL.md"))?);
        }
        let prompt = self.resource("APPEND_SYSTEM.md")?;
        let resources = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: Some(settings.clone()),
            extension_factories: vec![factory],
            no_context_files: true,
            no_skills: true,
            no_prompt_templates: true,
            no_system_prompt_file: true,
            additional_skill_paths: skill_paths,
            append_system_prompt: vec![prompt.to_string_lossy().into_owned()],
            ..DefaultResourceLoaderOptions::default()
        });
        options.data_policy = Some(Arc::new(CompositeDataPolicy {
            gate: self.activity_gate().clone(),
            update_command,
            privacy: crate::bitlocker::credentials::PrivacyPolicy,
        }));
        options.cwd = Some(cwd);
        options.agent_dir = Some(agent_dir);
        options.project_trusted = Some(false);
        options.settings_manager = Some(settings);
        options.resource_loader = Some(Arc::new(resources));
        options.extension_factories.clear();
        options.custom_tools.clear();
        options.tools_options = None;
        options.tools = Some(active);
        options.exclude_tools = None;
        options.no_tools = None;
        Ok(options)
    }

    pub async fn create_session(
        &self,
        options: CreateAgentSessionOptions,
        channel: Channel,
    ) -> Result<KitSession, String> {
        crate::update::service::startup_check(self.root())?;
        let _activity = self.activity_gate().enter()?;
        let auth = AuthRuntime::create(self.root(), channel, OaClient::official()?).await?;
        self.create_session_with_auth(options, auth).await
    }

    /// A trusted host may share AuthRuntime across sessions or inject a local OA for tests.
    pub async fn create_session_with_auth(
        &self,
        options: CreateAgentSessionOptions,
        auth: AuthRuntime,
    ) -> Result<KitSession, String> {
        self.create_session_with_services(options, auth, TelemetryOptions::default())
            .await
    }

    /// Test/local hosts may inject a transport and an environment probe without touching production.
    pub async fn create_session_with_services(
        &self,
        options: CreateAgentSessionOptions,
        auth: AuthRuntime,
        telemetry_options: TelemetryOptions,
    ) -> Result<KitSession, String> {
        self.create_session_with_update(options, auth, telemetry_options, Default::default())
            .await
    }

    /// Release key, update transport and launcher injection are trusted host configuration.
    pub async fn create_session_with_update(
        &self,
        options: CreateAgentSessionOptions,
        auth: AuthRuntime,
        telemetry_options: TelemetryOptions,
        update_options: UpdateOptions,
    ) -> Result<KitSession, String> {
        crate::update::service::startup_check(self.root())?;
        let _activity = self.activity_gate().enter()?;
        if auth.files.path("") != self.agent_dir() {
            return Err("会话与认证目录不一致。".into());
        }
        let updates = UpdateService::new(
            self.root(),
            auth.catalog.client.host(),
            self.activity_gate().clone(),
            update_options,
        )?;
        let telemetry_service = TelemetryService::load(
            self.root(),
            Arc::new(auth.runtime().credentials().clone()),
            Arc::new(OaBilling::from_auth(&auth)),
            telemetry_options,
        )
        .await?;
        let stop = CancellationToken::new();
        let wake = Arc::new(tokio::sync::Notify::new());
        let bitlocker_executor = crate::bitlocker::core::Executor::new(Arc::new(
            crate::bitlocker::wmi_encryptable::WindowsBitLocker,
        ));
        let opened = open_session_inner(
            self,
            &auth,
            telemetry_service.as_ref(),
            &updates,
            &bitlocker_executor,
            HostedSessionOptions::from(options),
            stop.clone(),
            wake.clone(),
        )
        .await?;
        let maintenance_auth = opened.auth.clone();
        let cancel = stop.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = maintenance_auth.refresh(false) => ()
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = wake.notified() => (),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => ()
                }
            }
        });
        Ok(KitSession {
            updates: opened.updates,
            _update_registration: opened.update_registration,
            created: opened.created,
            auth: opened.auth,
            stop: opened.stop,
            maintenance: task,
            telemetry: opened.telemetry,
            unsubscribe: opened.unsubscribe,
            bitlocker_verification: opened.bitlocker_verification,
        })
    }

    fn resource(&self, relative: &str) -> Result<PathBuf, String> {
        let home = self
            .agent_dir()
            .canonicalize()
            .map_err(|_| "工具包资源目录无法读取。")?;
        let path = home
            .join(relative)
            .canonicalize()
            .map_err(|_| format!("工具包缺少资源：{relative}"))?;
        if !path.starts_with(&home) || !path.is_file() {
            return Err(format!("工具包资源越界或不是文件：{relative}"));
        }
        Ok(path)
    }
}

struct OpenedSessionParts {
    updates: Arc<UpdateService>,
    update_registration: Arc<SessionRegistration>,
    created: CreateAgentSessionResult,
    auth: AuthRuntime,
    bitlocker_verification: Option<Value>,
    stop: CancellationToken,
    telemetry: Option<Arc<TelemetryBinding>>,
    unsubscribe: Option<Box<dyn FnOnce() + Send + Sync>>,
}

/// Shared session assembly kernel used by both `Kit::create_session*` and `KitHost::open_session`.
#[allow(clippy::too_many_arguments)]
async fn open_session_inner(
    kit: &Kit,
    auth: &AuthRuntime,
    telemetry_service: Option<&Arc<TelemetryService>>,
    updates: &Arc<UpdateService>,
    bitlocker_executor: &crate::bitlocker::core::Executor,
    mut hosted: HostedSessionOptions,
    session_stop: CancellationToken,
    wake: Arc<tokio::sync::Notify>,
) -> Result<OpenedSessionParts, String> {
    let host_ui = hosted
        .session
        .ui_context
        .clone()
        .unwrap_or_else(|| Arc::new(pi_session::NoOpUiContext::new()));
    hosted.session.model_runtime = Some(auth.runtime().clone());
    hosted.session.initial_model_policy = InitialModelPolicy::FixedIdentity {
        default_provider: "deepseek".into(),
        default_model_id: "deepseek-flash".into(),
        preserve_unauthenticated: true,
    };
    let source = hosted
        .telemetry_source
        .unwrap_or(match hosted.session.mode {
            Some(pi_session::ExtensionMode::Interactive) => "tui",
            _ => "web",
        });
    let telemetry = telemetry_service.map(|service| TelemetryBinding::new(service.clone(), source));
    let session_options = kit.session_options_with_auth(
        hosted.session,
        Some(AuthHooks {
            provider: auth.provider()?,
            accounts: auth.accounts.clone(),
            stop: session_stop.clone(),
            wake,
        }),
        telemetry.clone(),
        Some(updates.clone()),
        Some(bitlocker_executor.clone()),
        hosted.bitlocker_host,
    )?;
    let created = Box::pin(create_agent_session(session_options)).await?;
    let bitlocker_verification = bitlocker_executor
        .reconcile(
            &created.session.startup_reconciliation_report(),
            session_stop.child_token(),
        )
        .await;
    if let Some(result) = &bitlocker_verification {
        created
            .session
            .with_session_manager_mut(|manager| {
                manager.append_custom_entry(
                    crate::bitlocker::VERIFICATION_CUSTOM_TYPE,
                    Some(result.clone()),
                )
            })
            .map_err(|_| "BitLocker 中断复查记录无法保存。")?;
    }
    let unsubscribe = telemetry.as_ref().map(|binding| {
        let binding = Arc::downgrade(binding);
        created.session.subscribe(Arc::new(move |event| {
            if let Some(binding) = binding.upgrade() {
                binding.on_session_event(event);
            }
        }))
    });
    let session_auth = auth.clone().with_runtime(created.model_runtime.clone());
    let registration = updates.attach(
        created.session.clone(),
        telemetry.as_ref().map(|binding| binding.service.clone()),
        session_stop.clone(),
        host_ui,
    );
    Ok(OpenedSessionParts {
        updates: updates.clone(),
        update_registration: registration,
        created,
        auth: session_auth,
        bitlocker_verification,
        stop: session_stop,
        telemetry,
        unsubscribe,
    })
}

/// Owns the background catalog task; dropping this wrapper cancels it.
/// Call shutdown to also close the underlying session through its lifecycle hooks.
pub struct KitSession {
    pub updates: Arc<UpdateService>,
    _update_registration: Arc<SessionRegistration>,
    pub created: CreateAgentSessionResult,
    pub auth: AuthRuntime,
    /// Present only after unresolved BitLocker calls were queried during startup.
    pub bitlocker_verification: Option<Value>,
    stop: CancellationToken,
    maintenance: tokio::task::JoinHandle<()>,
    telemetry: Option<Arc<TelemetryBinding>>,
    unsubscribe: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl KitSession {
    pub fn telemetry(&self) -> Option<Arc<TelemetryService>> {
        self.telemetry.as_ref().map(|b| b.service.clone())
    }

    pub async fn finalize_telemetry(&self, reason: &str) {
        if let Some(binding) = &self.telemetry {
            binding.finalize(reason, None).await;
        }
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        self.stop.cancel();
        self.created.session.shutdown().await
    }
}

impl Drop for KitSession {
    fn drop(&mut self) {
        self.stop.cancel();
        self.maintenance.abort();
        if let Some(unsubscribe) = self.unsubscribe.take() {
            unsubscribe();
        }
        if let Some(binding) = &self.telemetry {
            binding.abandon();
        }
    }
}

#[derive(Default)]
pub struct HostedSessionOptions {
    pub session: CreateAgentSessionOptions,
    pub bitlocker_host: Option<Arc<dyn crate::bitlocker::BitLockerHostChannel>>,
    pub telemetry_source: Option<&'static str>,
}

impl From<CreateAgentSessionOptions> for HostedSessionOptions {
    fn from(session: CreateAgentSessionOptions) -> Self {
        Self {
            session,
            bitlocker_host: None,
            telemetry_source: None,
        }
    }
}

struct KitHostMaintenance {
    stop: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for KitHostMaintenance {
    fn drop(&mut self) {
        self.stop.cancel();
        self.handle.abort();
    }
}

/// Shared multi-session host for `cst-server`, owning one catalog maintenance loop
/// and shared `AuthRuntime`, `TelemetryService`, and `UpdateService`.
#[derive(Clone)]
pub struct KitHost {
    kit: Kit,
    auth: AuthRuntime,
    telemetry: Option<Arc<TelemetryService>>,
    updates: Arc<UpdateService>,
    bitlocker_executor: crate::bitlocker::core::Executor,
    stop: CancellationToken,
    wake: Arc<tokio::sync::Notify>,
    _maintenance: Arc<KitHostMaintenance>,
}

impl KitHost {
    pub async fn new(
        kit: Kit,
        auth: AuthRuntime,
        telemetry_options: TelemetryOptions,
        update_options: UpdateOptions,
    ) -> Result<Self, String> {
        crate::update::service::startup_check(kit.root())?;
        if auth.files.path("") != kit.agent_dir() {
            return Err("会话与认证目录不一致。".into());
        }
        let updates = UpdateService::new(
            kit.root(),
            auth.catalog.client.host(),
            kit.activity_gate().clone(),
            update_options,
        )?;
        let telemetry = TelemetryService::load(
            kit.root(),
            Arc::new(auth.runtime().credentials().clone()),
            Arc::new(OaBilling::from_auth(&auth)),
            telemetry_options,
        )
        .await?;
        let stop = CancellationToken::new();
        let wake = Arc::new(tokio::sync::Notify::new());
        let maintenance_auth = auth.clone();
        let cancel = stop.clone();
        let wake_task = wake.clone();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = maintenance_auth.refresh(false) => ()
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = wake_task.notified() => (),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => ()
                }
            }
        });
        Ok(Self {
            kit,
            auth,
            telemetry,
            updates,
            bitlocker_executor: crate::bitlocker::core::Executor::new(Arc::new(
                crate::bitlocker::wmi_encryptable::WindowsBitLocker,
            )),
            stop: stop.clone(),
            wake,
            _maintenance: Arc::new(KitHostMaintenance { stop, handle }),
        })
    }

    pub fn with_bitlocker_executor(mut self, executor: crate::bitlocker::core::Executor) -> Self {
        self.bitlocker_executor = executor;
        self
    }

    pub fn with_telemetry(mut self, telemetry: Option<Arc<TelemetryService>>) -> Self {
        self.telemetry = telemetry;
        self
    }

    pub fn kit(&self) -> &Kit {
        &self.kit
    }

    pub fn auth(&self) -> &AuthRuntime {
        &self.auth
    }

    pub fn telemetry(&self) -> Option<&Arc<TelemetryService>> {
        self.telemetry.as_ref()
    }

    pub fn updates(&self) -> &Arc<UpdateService> {
        &self.updates
    }

    pub fn wake_catalog(&self) {
        self.wake.notify_one();
    }

    pub async fn open_session(
        &self,
        options: impl Into<HostedSessionOptions>,
    ) -> Result<HostedSession, String> {
        let _activity = self.kit.activity_gate().enter()?;
        let session_stop = self.stop.child_token();
        let opened = open_session_inner(
            &self.kit,
            &self.auth,
            self.telemetry.as_ref(),
            &self.updates,
            &self.bitlocker_executor,
            options.into(),
            session_stop,
            self.wake.clone(),
        )
        .await?;
        Ok(HostedSession {
            updates: opened.updates,
            _update_registration: opened.update_registration,
            created: opened.created,
            auth: opened.auth,
            bitlocker_verification: opened.bitlocker_verification,
            stop: opened.stop,
            telemetry: opened.telemetry,
            unsubscribe: opened.unsubscribe,
        })
    }

    pub async fn shutdown(&self) {
        self.shutdown_with_reason("quit").await;
    }

    pub async fn shutdown_with_reason(&self, reason: &str) {
        self.stop.cancel();
        if let Some(telemetry) = &self.telemetry {
            telemetry.close_process(reason).await;
        }
    }
}

pub struct HostedSession {
    pub updates: Arc<UpdateService>,
    _update_registration: Arc<SessionRegistration>,
    pub created: CreateAgentSessionResult,
    pub auth: AuthRuntime,
    /// Present only after unresolved BitLocker calls were queried during startup.
    pub bitlocker_verification: Option<Value>,
    stop: CancellationToken,
    telemetry: Option<Arc<TelemetryBinding>>,
    unsubscribe: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl HostedSession {
    pub fn session(&self) -> &pi_session::AgentSession {
        &self.created.session
    }

    pub fn telemetry(&self) -> Option<Arc<TelemetryService>> {
        self.telemetry.as_ref().map(|b| b.service.clone())
    }

    pub async fn finalize_telemetry(&self, reason: &str) {
        if let Some(binding) = &self.telemetry {
            binding.finalize(reason, None).await;
        }
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        self.stop.cancel();
        self.created.session.shutdown().await
    }
}

impl Drop for HostedSession {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(unsubscribe) = self.unsubscribe.take() {
            unsubscribe();
        }
        if let Some(binding) = &self.telemetry {
            binding.abandon();
        }
    }
}

/// Composes `ActivityGate` admission with BitLocker privacy redaction explicitly in `kit.rs`.
struct CompositeDataPolicy {
    gate: Arc<ActivityGate>,
    update_command: bool,
    privacy: crate::bitlocker::credentials::PrivacyPolicy,
}

impl SessionDataPolicy for CompositeDataPolicy {
    fn admit_input(
        &self,
        text: &str,
        commands_enabled: bool,
    ) -> Result<Option<Box<dyn Send>>, String> {
        if self.update_command
            && commands_enabled
            && text.starts_with("/update")
            && text.split_whitespace().next() == Some("/update")
        {
            return Ok(None);
        }
        Ok(Some(Box::new(self.gate.enter()?)))
    }

    fn persisted_input(&self, text: &str) -> Result<String, String> {
        self.privacy.persisted_input(text)
    }

    fn persisted_message(
        &self,
        message: pi_agent::AgentMessage,
    ) -> Result<pi_agent::AgentMessage, String> {
        self.privacy.persisted_message(message)
    }

    fn project_event(
        &self,
        event: pi_session::AgentSessionEvent,
    ) -> Result<pi_session::AgentSessionEvent, String> {
        self.privacy.project_event(event)
    }
}

#[derive(Clone)]
pub struct CstPilotExtension {
    tools: Vec<ToolDefinition>,
    credentials: crate::bitlocker::credentials::Vault,
    auth: Option<AuthHooks>,
    telemetry: Option<Arc<TelemetryBinding>>,
    updates: Option<Arc<UpdateService>>,
    gate: Arc<ActivityGate>,
}

impl CstPilotExtension {
    pub fn new(kit: &Kit) -> Result<Self, String> {
        Self::new_with_bitlocker(kit, None, None)
    }

    pub fn new_with_bitlocker(
        kit: &Kit,
        bitlocker_executor: Option<crate::bitlocker::core::Executor>,
        bitlocker_host: Option<Arc<dyn crate::bitlocker::BitLockerHostChannel>>,
    ) -> Result<Self, String> {
        let web = WebAccess::from_config(&kit.agent_dir().join("web-search.json"))?;
        let mut read = create_read_tool_definition(
            kit.root.to_string_lossy(),
            Some(ReadToolOptions {
                operations: Some(Arc::new(GuardedReadOperations(kit.guard.clone()))),
                ..Default::default()
            }),
        );
        read.prompt_snippet = Some("读取文本或图片，禁止读取凭据文件和运行态目录。".into());
        let mut tools = vec![read];
        tools.extend(fff::definitions(kit.guard.clone()));
        tools.extend(web.definitions());
        tools.extend(crate::diagnostics::definitions(kit.root()));
        tools.extend(crate::runbook::definitions(kit.root())?);
        let credentials = crate::bitlocker::credentials::Vault::default();
        let executor = bitlocker_executor.unwrap_or_else(|| {
            crate::bitlocker::core::Executor::new(Arc::new(
                crate::bitlocker::wmi_encryptable::WindowsBitLocker,
            ))
        });
        tools.push(crate::bitlocker::definition_with_host(
            credentials.clone(),
            executor,
            bitlocker_host,
        ));
        debug_assert_eq!(
            tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<BTreeSet<_>>(),
            OPEN_TOOL_NAMES.iter().copied().collect::<BTreeSet<_>>(),
            "CstPilotExtension tool list must match OPEN_TOOL_NAMES"
        );
        kit.activity_gate().wrap(&mut tools);
        Ok(Self {
            tools,
            credentials,
            auth: None,
            telemetry: None,
            updates: None,
            gate: kit.activity_gate().clone(),
        })
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name.clone()).collect()
    }
}

impl Extension for CstPilotExtension {
    fn path(&self) -> &str {
        "cst-pilot"
    }

    fn register(&self, api: &mut ExtensionRegistrar) -> Result<(), ExtensionInitError> {
        self.gate.register(api);
        if let Some(updates) = &self.updates {
            updates.register(api);
        }
        self.credentials.register(api);
        if let Some(hooks) = &self.auth {
            api.register_provider(hooks.provider.clone());
            let wake = hooks.wake.clone();
            api.on_lifecycle("agent_start", move |_, _| {
                wake.notify_one();
                async { Ok(SessionBeforeEventOutput::None) }
            });
            let accounts = hooks.accounts.clone();
            api.on_lifecycle("agent_end", move |_, _| {
                accounts.invalidate();
                async { Ok(SessionBeforeEventOutput::None) }
            });
            let stop = hooks.stop.clone();
            api.on_lifecycle("session_shutdown", move |_, _| {
                stop.cancel();
                async { Ok(SessionBeforeEventOutput::None) }
            });
        }
        if let Some(binding) = &self.telemetry {
            binding.register(api);
        }
        for definition in &self.tools {
            api.register_tool(definition.clone());
        }
        Ok(())
    }
}

/// Never read the machine owner's project settings. Global data stays in the kit.
struct KitSettingsStorage(FileSettingsStorage, Arc<ActivityGate>);

impl SettingsStorage for KitSettingsStorage {
    fn path_for(&self, scope: SettingsScope) -> Option<PathBuf> {
        if scope == SettingsScope::Global {
            self.0.path_for(scope)
        } else {
            None
        }
    }

    fn with_lock(&self, scope: SettingsScope, f: &mut SettingsUpdate<'_>) -> Result<(), String> {
        let _activity = self.1.enter()?;
        if scope == SettingsScope::Project {
            return f(None).map(|_| ());
        }
        self.0.with_lock(scope, &mut |raw| {
            let mut value: Value = serde_json::from_str(&strip_json_comments(raw.unwrap_or("{}")))
                .map_err(|_| "设置文件无法读取，请检查 settings.json 后重试。")?;
            let object = value
                .as_object_mut()
                .ok_or("设置文件必须包含 JSON 对象。")?;
            object.insert("defaultProjectTrust".into(), Value::String("never".into()));
            let text = serde_json::to_string(&value).map_err(|_| "设置文件无法编码。")?;
            f(Some(&text))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_tool_names_and_skill_names_match_contracts() {
        let root = std::env::temp_dir().join(format!("cst-kit-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("agent/home")).unwrap();
        let kit = Kit::new(&root).unwrap();
        let ext = CstPilotExtension::new(&kit).unwrap();
        let actual: BTreeSet<String> = ext.tool_names().into_iter().collect();
        let expected: BTreeSet<String> = OPEN_TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(actual, expected);
        assert_eq!(SKILL_NAMES, cst_updater::layout::REQUIRED_SKILL_NAMES);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn same_root_shares_single_activity_gate_and_distinct_roots_are_isolated() {
        let root_a = std::env::temp_dir().join(format!("cst-kit-gate-a-{}", uuid::Uuid::new_v4()));
        let root_b = std::env::temp_dir().join(format!("cst-kit-gate-b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root_a.join("agent/home")).unwrap();
        std::fs::create_dir_all(root_b.join("agent/home")).unwrap();

        let kit_a1 = Kit::new(&root_a).unwrap();
        let kit_a2 = Kit::new(root_a.join("agent/..")).unwrap();
        let kit_b = Kit::new(&root_b).unwrap();

        assert!(Arc::ptr_eq(kit_a1.activity_gate(), kit_a2.activity_gate()));
        assert!(!Arc::ptr_eq(kit_a1.activity_gate(), kit_b.activity_gate()));

        let upgrade = kit_a1.activity_gate().close().unwrap();
        assert!(kit_a2.activity_gate().enter().is_err());
        assert!(kit_b.activity_gate().enter().is_ok());
        drop(upgrade);
        assert!(kit_a2.activity_gate().enter().is_ok());

        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(root_b);
    }
}

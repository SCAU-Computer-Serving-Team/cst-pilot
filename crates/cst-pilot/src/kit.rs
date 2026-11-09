use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_runtime::{
    CreateAgentSessionOptions, CreateAgentSessionResult, DefaultResourceLoader,
    DefaultResourceLoaderOptions, FileSettingsStorage, InitialModelPolicy, ReadToolOptions,
    SettingsManager, SettingsScope, SettingsStorage, SettingsUpdate, create_agent_session,
    create_read_tool_definition, strip_json_comments,
};
use pi_session::{
    Extension, ExtensionFactory, ExtensionInitError, ExtensionRegistrar, SessionBeforeEventOutput,
    ToolDefinition,
};
use tokio_util::sync::CancellationToken;

use crate::auth::{AuthRuntime, Channel, oa::OaClient};
use crate::telemetry::{TelemetryBinding, TelemetryOptions, TelemetryService, billing::OaBilling};
use serde_json::Value;

use crate::tools::{
    fff,
    path_guard::{GuardedReadOperations, PathGuard},
    web_access::WebAccess,
};

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
pub const SKILL_NAMES: &[&str] = &[
    "disk",
    "ls",
    "sys",
    "startup",
    "eventlog",
    "driver",
    "runbook",
    "bitlocker",
];

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
    gate: Arc<crate::update::activity::ActivityGate>,
}

impl Kit {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| "工具包目录无法读取。")?;
        let guard = PathGuard::new(&root)?;
        let gate = crate::update::activity::ActivityGate::for_root(&root)?;
        Ok(Self { root, guard, gate })
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

    /// Keep model/UI/session options; replace all resource and tool policy overrides.
    /// Only implemented business tools are activated. No built-in ls fallback is exposed.
    pub fn session_options(
        &self,
        options: CreateAgentSessionOptions,
    ) -> Result<CreateAgentSessionOptions, String> {
        crate::update::service::startup_check(self.root())?;
        self.session_options_with_auth(options, None, None, None)
    }

    /// Validate local assembly during an updater-owned transaction, without starting a session.
    pub(crate) fn check_resources(&self) -> Result<(), String> {
        self.session_options_with_auth(CreateAgentSessionOptions::default(), None, None, None)
            .map(|_| ())
    }

    fn session_options_with_auth(
        &self,
        mut options: CreateAgentSessionOptions,
        auth: Option<AuthHooks>,
        telemetry: Option<Arc<TelemetryBinding>>,
        updates: Option<Arc<crate::update::service::UpdateService>>,
    ) -> Result<CreateAgentSessionOptions, String> {
        self.gate.check()?;
        let cwd = options
            .cwd
            .clone()
            .unwrap_or_else(|| self.root.clone())
            .canonicalize()
            .map_err(|_| "工作目录无法读取。")?;
        let settings = SettingsManager::from_storage(
            Arc::new(KitSettingsStorage(
                FileSettingsStorage::new(&cwd, self.agent_dir()),
                self.gate.clone(),
            )),
            false,
        );
        let extension = CstPilotExtension::new(self)?;
        let active = extension
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let kit = self.clone();
        let update_command = updates.is_some();
        let factory: ExtensionFactory = Arc::new(move || {
            CstPilotExtension::new(&kit)
                .map(|mut extension| {
                    extension.auth = auth.clone();
                    extension.telemetry = telemetry.clone();
                    extension.updates = updates.clone();
                    extension
                })
                .map(|extension| Box::new(extension) as Box<dyn Extension>)
                .map_err(|message| ExtensionInitError {
                    extension_path: "cst-pilot".into(),
                    message,
                })
        });
        let mut skill_paths = Vec::new();
        for name in SKILL_NAMES {
            skill_paths.push(self.resource(&format!("skills/{name}/SKILL.md"))?);
        }
        let prompt = self.resource("APPEND_SYSTEM.md")?;
        let resources = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: self.agent_dir(),
            settings_manager: Some(settings.clone()),
            extension_factories: vec![factory],
            no_context_files: true,
            no_skills: true,
            no_prompt_templates: true,
            additional_skill_paths: skill_paths,
            // An explicit empty input prevents SYSTEM.md discovery in the host's .pi.
            // None after override retains the standard Pi prompt and its tool declaration list.
            system_prompt: Some(String::new()),
            system_prompt_override: Some(Arc::new(|_| None)),
            append_system_prompt: vec![prompt.to_string_lossy().into_owned()],
            ..DefaultResourceLoaderOptions::default()
        });
        options.data_policy = Some(Arc::new(crate::update::activity::InputPolicy(
            self.gate.clone(),
            update_command,
        )));
        options.cwd = Some(cwd);
        options.agent_dir = Some(self.agent_dir());
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
        let _activity = self.gate.enter()?;
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
        mut options: CreateAgentSessionOptions,
        auth: AuthRuntime,
        telemetry_options: TelemetryOptions,
        update_options: crate::update::service::UpdateOptions,
    ) -> Result<KitSession, String> {
        crate::update::service::startup_check(self.root())?;
        let _activity = self.gate.enter()?;
        if auth.files.path("") != self.agent_dir() {
            return Err("会话与认证目录不一致。".into());
        }
        let updates = crate::update::service::UpdateService::load(
            self.root(),
            auth.catalog.client.host(),
            update_options,
        )?;
        let host_ui = options
            .ui_context
            .clone()
            .unwrap_or_else(|| Arc::new(pi_session::NoOpUiContext::new()));
        let stop = CancellationToken::new();
        let wake = Arc::new(tokio::sync::Notify::new());
        options.model_runtime = Some(auth.runtime().clone());
        options.initial_model_policy = InitialModelPolicy::FixedIdentity {
            default_provider: "deepseek".into(),
            default_model_id: "deepseek-flash".into(),
            preserve_unauthenticated: true,
        };
        let service = TelemetryService::load(
            self.root(),
            Arc::new(auth.runtime().credentials().clone()),
            Arc::new(OaBilling::from_auth(&auth)),
            telemetry_options,
        )
        .await?;
        let telemetry = service.map(|s| {
            TelemetryBinding::new(
                s,
                match options.mode {
                    Some(pi_session::ExtensionMode::Interactive) => "tui",
                    _ => "web",
                },
            )
        });
        let options = self.session_options_with_auth(
            options,
            Some(AuthHooks {
                provider: auth.provider()?,
                accounts: auth.accounts.clone(),
                stop: stop.clone(),
                wake: wake.clone(),
            }),
            telemetry.clone(),
            Some(updates.clone()),
        )?;
        let created = create_agent_session(options).await?;
        let bitlocker_verification = crate::bitlocker::core::Executor::new(Arc::new(
            crate::bitlocker::wmi_encryptable::WindowsBitLocker,
        ))
        .reconcile(
            &created.session.startup_reconciliation_report(),
            stop.child_token(),
        )
        .await;
        if let Some(result) = &bitlocker_verification {
            created
                .session
                .with_session_manager_mut(|manager| {
                    manager.append_custom_entry(
                        "cst.bitlocker-state-verification",
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
        let auth = auth.with_runtime(created.model_runtime.clone());
        let maintenance_auth = auth.clone();
        let cancel = stop.clone();
        // Check once without delaying session creation, then honor Catalog's 5 min TTL / 30 s retry.
        let task = tokio::spawn(async move {
            loop {
                tokio::select! { biased; _ = cancel.cancelled() => break, _ = maintenance_auth.refresh(false) => () }
                tokio::select! { biased; _ = cancel.cancelled() => break, _ = wake.notified() => (), _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => () }
            }
        });
        let registration = updates.attach(
            created.session.clone(),
            telemetry.as_ref().map(|binding| binding.service.clone()),
            stop.clone(),
            host_ui,
        );
        Ok(KitSession {
            updates,
            _update_registration: registration,
            created,
            auth,
            stop,
            maintenance: task,
            telemetry,
            unsubscribe,
            bitlocker_verification,
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

/// Owns the background catalog task; dropping this wrapper cancels it.
/// Call shutdown to also close the underlying session through its lifecycle hooks.
pub struct KitSession {
    pub updates: Arc<crate::update::service::UpdateService>,
    _update_registration: Arc<crate::update::service::SessionRegistration>,
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

#[derive(Clone)]
pub struct CstPilotExtension {
    tools: Vec<ToolDefinition>,
    credentials: crate::bitlocker::credentials::Vault,
    auth: Option<AuthHooks>,
    telemetry: Option<Arc<TelemetryBinding>>,
    updates: Option<Arc<crate::update::service::UpdateService>>,
    gate: Arc<crate::update::activity::ActivityGate>,
}

impl CstPilotExtension {
    pub fn new(kit: &Kit) -> Result<Self, String> {
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
        tools.push(crate::bitlocker::definition(credentials.clone()));
        kit.gate.wrap(&mut tools);
        Ok(Self {
            tools,
            credentials,
            auth: None,
            telemetry: None,
            updates: None,
            gate: kit.gate.clone(),
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
struct KitSettingsStorage(
    FileSettingsStorage,
    Arc<crate::update::activity::ActivityGate>,
);
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

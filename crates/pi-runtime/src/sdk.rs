use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent::{AgentMessage, StreamFn, ToolExecutionMode};
use pi_ai::ModelThinkingLevel;
use pi_ai::{Model, SimpleStreamOptions, TranscriptContext};
use pi_session::{
    AgentSession, AgentSessionOptions, BuildSystemPromptOptions, CacheWarmingDecisionEvent,
    ExecutionLogController, ExecutionLogFactory, ExtensionFactory, ExtensionFlagType,
    ExtensionFlagValue, ExtensionMode, ExtensionRunner, ExtensionUiContext,
    ResolvedPhysicalRequest, SessionEntry, SessionLifecycleEvent, SessionManager,
    SessionReloadOutcome, SkillPromptEntry, ToolDefinition, format_no_models_available_message,
    get_branch_selection, is_unconfigured_model,
};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::cache_warmer::{CacheWarmRequest, CacheWarmer, RuntimeClock};
use crate::config_value::ConfigValueResolver;
use crate::image_process::{NormalizeToolResultImagesOptions, normalize_tool_result_images};
use crate::model_resolver::{
    DEFAULT_THINKING_LEVEL, FindInitialModelOptions, InitialModelPolicy, ScopedModel,
    find_initial_model,
};
use crate::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use crate::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, LoadExtensionsResult, ResourceLoader,
};
use crate::settings_manager::{
    DEFAULT_TOOL_NAMES, SettingsManager, apply_tool_modifiers, get_tool_list_error,
    is_tool_modifier,
};
use crate::skills::glob_fragment_to_regex;
use crate::tools::{
    ALL_BUILTIN_TOOL_NAMES, ToolsOptions, create_all_tool_definitions, create_bash_executor,
    create_local_bash_operations, create_local_powershell_operations,
};

pub fn get_default_agent_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PI_CODING_AGENT_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir.trim());
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
        && !home.trim().is_empty()
    {
        return PathBuf::from(home).join(".pi").join("agent");
    }
    PathBuf::from(".pi").join("agent")
}

pub fn get_default_session_dir(cwd: &Path, agent_dir: &Path) -> PathBuf {
    let resolved = resolve_path(cwd, None);
    let encoded: String = resolved
        .to_string_lossy()
        .trim_start_matches(['/', '\\'])
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    agent_dir.join("sessions").join(format!("--{encoded}--"))
}

pub fn resolve_path(raw: impl AsRef<Path>, base_cwd: Option<&Path>) -> PathBuf {
    let p = raw.as_ref();
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(base) = base_cwd {
        base.join(p)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoToolsMode {
    All,
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntimeDiagnostic {
    #[serde(rename = "type")]
    pub kind: String, // "info" | "warning" | "error"
    pub message: String,
}

#[derive(Clone, Default)]
pub struct CreateAgentSessionServicesOptions {
    pub cwd: PathBuf,
    pub agent_dir: Option<PathBuf>,
    pub project_trusted: Option<bool>,
    pub settings_manager: Option<SettingsManager>,
    pub model_runtime: Option<ModelRuntime>,
    pub extension_flag_values: Option<BTreeMap<String, ExtensionFlagValue>>,
    pub resource_loader_options: Option<DefaultResourceLoaderOptions>,
}

#[derive(Clone)]
pub struct AgentSessionServices {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub model_runtime: ModelRuntime,
    pub settings_manager: SettingsManager,
    pub resource_loader: Arc<dyn ResourceLoader>,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

pub(crate) fn apply_extension_flag_values(
    runner: &ExtensionRunner,
    flag_values: Option<&BTreeMap<String, ExtensionFlagValue>>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let Some(values) = flag_values else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    let registered = runner.get_flags();
    let mut unknown = Vec::new();

    for (name, val) in values {
        let Some(flag) = registered.get(name) else {
            unknown.push(name.clone());
            continue;
        };
        match (flag.flag_type, val) {
            (ExtensionFlagType::Boolean, ExtensionFlagValue::Bool(value)) => {
                runner.set_flag_value(name.clone(), ExtensionFlagValue::Bool(*value));
            }
            (ExtensionFlagType::Boolean, ExtensionFlagValue::String(_)) => {
                diagnostics.push(AgentSessionRuntimeDiagnostic {
                    kind: "error".into(),
                    message: format!("Extension flag \"--{name}\" requires a boolean"),
                });
            }
            (ExtensionFlagType::String, ExtensionFlagValue::String(s)) => {
                runner.set_flag_value(name.clone(), ExtensionFlagValue::String(s.clone()));
            }
            (ExtensionFlagType::String, ExtensionFlagValue::Bool(_)) => {
                diagnostics.push(AgentSessionRuntimeDiagnostic {
                    kind: "error".to_owned(),
                    message: format!("Extension flag \"--{name}\" requires a value"),
                });
            }
        }
    }

    if !unknown.is_empty() {
        let plural = if unknown.len() == 1 { "" } else { "s" };
        let formatted: Vec<String> = unknown.into_iter().map(|n| format!("--{n}")).collect();
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: "error".to_owned(),
            message: format!("Unknown option{plural}: {}", formatted.join(", ")),
        });
    }
    diagnostics
}

/// 创建绑定到目标 `cwd` 的运行时服务集（对照 `createAgentSessionServices`）。
pub async fn create_agent_session_services(
    options: CreateAgentSessionServicesOptions,
) -> AgentSessionServices {
    let cwd = resolve_path(&options.cwd, None);
    let agent_dir = options
        .agent_dir
        .map(|d| resolve_path(d, Some(&cwd)))
        .unwrap_or_else(get_default_agent_dir);
    let project_trusted = options.project_trusted.unwrap_or(true);

    let settings_manager = options
        .settings_manager
        .unwrap_or_else(|| SettingsManager::create_with_trust(&cwd, &agent_dir, project_trusted));
    let resolver =
        ConfigValueResolver::new(&cwd, &agent_dir, settings_manager.is_project_trusted());

    let model_runtime = match options.model_runtime {
        Some(rt) => rt,
        None => {
            ModelRuntime::create(CreateModelRuntimeOptions {
                auth_path: Some(agent_dir.join("auth.json")),
                models_path: Some(agent_dir.join("models.json")),
                resolver: Some(resolver),
                refresh_on_create: Some(true),
                ..CreateModelRuntimeOptions::default()
            })
            .await
        }
    };

    let mut rl_opts = options.resource_loader_options.unwrap_or_default();
    rl_opts.cwd = cwd.clone();
    rl_opts.agent_dir = agent_dir.clone();
    rl_opts.settings_manager = Some(settings_manager.clone());
    rl_opts.extension_flag_values = options.extension_flag_values.unwrap_or_default();
    if rl_opts.builtin_extensions.is_empty() {
        rl_opts.builtin_extensions = crate::extensions::built_in_extensions_for_session(
            settings_manager.clone(),
            &cwd,
            &agent_dir,
        );
    }
    let resource_loader = Arc::new(DefaultResourceLoader::new(rl_opts));
    let preload_error = resource_loader.reload().await.err();
    let mut diagnostics = Vec::new();
    if let Some(message) = preload_error {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: "error".to_owned(),
            message,
        });
    }
    for err in settings_manager.get_errors() {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: "warning".to_owned(),
            message: format!("Settings ({:?}): {}", err.scope, err.message),
        });
    }
    if let Some(m_err) = model_runtime.get_error() {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: "warning".to_owned(),
            message: m_err,
        });
    }

    let ext_res = resource_loader.get_extensions();
    for err in &ext_res.errors {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: "error".to_owned(),
            message: format!(
                "Extension \"{}\" error: {}",
                err.extension_path, err.message
            ),
        });
    }
    for warn in &ext_res.warnings {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: warn.kind.clone(),
            message: warn.message.clone(),
        });
    }
    for diag in resource_loader.get_skills().diagnostics {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: if diag.kind == "error" {
                "error".to_owned()
            } else {
                "warning".to_owned()
            },
            message: diag.message,
        });
    }
    for diag in resource_loader.get_prompts().diagnostics {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            kind: if diag.kind == "error" {
                "error".to_owned()
            } else {
                "warning".to_owned()
            },
            message: diag.message,
        });
    }

    AgentSessionServices {
        cwd,
        agent_dir,
        model_runtime,
        settings_manager,
        resource_loader,
        diagnostics,
    }
}

pub fn create_tool_name_matcher(patterns: &[String]) -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
    let mut compiled = Vec::new();
    let mut exact = Vec::new();
    for pat in patterns {
        let trimmed = pat.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains('*') || trimmed.contains('?') {
            let rx = format!("^{}$", glob_fragment_to_regex(trimmed));
            if let Ok(re) = Regex::new(&rx) {
                compiled.push(re);
            }
        } else {
            exact.push(trimmed.to_owned());
        }
    }
    Arc::new(move |name: &str| {
        exact.iter().any(|e| e == name) || compiled.iter().any(|re| re.is_match(name))
    })
}

#[derive(Default)]
pub struct CreateAgentSessionOptions {
    pub cwd: Option<PathBuf>,
    pub agent_dir: Option<PathBuf>,
    pub project_trusted: Option<bool>,
    pub model_runtime: Option<ModelRuntime>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub scoped_models: Vec<ScopedModel>,
    pub initial_model_policy: InitialModelPolicy,
    pub no_tools: Option<NoToolsMode>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub custom_tools: Vec<ToolDefinition>,
    pub tools_options: Option<ToolsOptions>,
    pub resource_loader: Option<Arc<dyn ResourceLoader>>,
    pub extension_factories: Vec<ExtensionFactory>,
    pub session_manager: Option<SessionManager>,
    pub execution_log: Option<ExecutionLogController>,
    pub execution_log_factory: Option<ExecutionLogFactory>,
    pub data_policy: Option<Arc<dyn pi_session::SessionDataPolicy>>,
    pub settings_manager: Option<SettingsManager>,
    pub ui_context: Option<Arc<dyn ExtensionUiContext>>,
    pub mode: Option<ExtensionMode>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub session_start_event: Option<(String, Option<PathBuf>)>,
    pub clock: Option<Arc<dyn RuntimeClock>>,
}

pub struct CreateAgentSessionFromServicesOptions {
    pub services: AgentSessionServices,
    pub session_manager: SessionManager,
    pub execution_log: Option<ExecutionLogController>,
    pub execution_log_factory: Option<ExecutionLogFactory>,
    pub data_policy: Option<Arc<dyn pi_session::SessionDataPolicy>>,
    pub session_start_event: Option<(String, Option<PathBuf>)>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub scoped_models: Vec<ScopedModel>,
    pub initial_model_policy: InitialModelPolicy,
    pub no_tools: Option<NoToolsMode>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub custom_tools: Vec<ToolDefinition>,
    pub tools_options: Option<ToolsOptions>,
    pub ui_context: Option<Arc<dyn ExtensionUiContext>>,
    pub mode: Option<ExtensionMode>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub clock: Option<Arc<dyn RuntimeClock>>,
}

#[derive(Clone)]
pub struct CreateAgentSessionResult {
    pub session: AgentSession,
    pub extensions_result: LoadExtensionsResult,
    pub model_fallback_message: Option<String>,
    pub cache_warmer: CacheWarmer,
    pub model_runtime: ModelRuntime,
    pub settings_manager: SettingsManager,
    pub resource_loader: Arc<dyn ResourceLoader>,
}

struct SessionConstructionGuard(Option<AgentSession>);
impl Drop for SessionConstructionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.dispose();
        }
    }
}

pub async fn create_agent_session_from_services(
    options: CreateAgentSessionFromServicesOptions,
) -> Result<CreateAgentSessionResult, String> {
    create_agent_session(CreateAgentSessionOptions {
        cwd: Some(options.services.cwd),
        agent_dir: Some(options.services.agent_dir),
        project_trusted: Some(options.services.settings_manager.is_project_trusted()),
        model_runtime: Some(options.services.model_runtime),
        model: options.model,
        thinking_level: options.thinking_level,
        scoped_models: options.scoped_models,
        initial_model_policy: options.initial_model_policy,
        no_tools: options.no_tools,
        tools: options.tools,
        exclude_tools: options.exclude_tools,
        custom_tools: options.custom_tools,
        tools_options: options.tools_options,
        resource_loader: Some(options.services.resource_loader),
        extension_factories: Vec::new(),
        session_manager: Some(options.session_manager),
        execution_log: options.execution_log,
        execution_log_factory: options.execution_log_factory,
        data_policy: options.data_policy,
        settings_manager: Some(options.services.settings_manager),
        ui_context: options.ui_context,
        mode: options.mode,
        tool_execution: options.tool_execution,
        session_start_event: options.session_start_event,
        clock: options.clock,
    })
    .await
}

/// 组装完整 `AgentSession`（对照 `createAgentSession`）。
pub async fn create_agent_session(
    options: CreateAgentSessionOptions,
) -> Result<CreateAgentSessionResult, String> {
    let inferred_cwd = options
        .cwd
        .clone()
        .or_else(|| {
            options
                .session_manager
                .as_ref()
                .map(|sm| PathBuf::from(sm.get_cwd()))
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cwd = resolve_path(inferred_cwd, None);
    let cwd_str = cwd.to_string_lossy().into_owned();
    let agent_dir = options
        .agent_dir
        .map(|d| resolve_path(d, Some(&cwd)))
        .unwrap_or_else(get_default_agent_dir);
    let project_trusted = options.project_trusted.unwrap_or(true);

    let settings_manager = options
        .settings_manager
        .unwrap_or_else(|| SettingsManager::create_with_trust(&cwd, &agent_dir, project_trusted));
    let resolver =
        ConfigValueResolver::new(&cwd, &agent_dir, settings_manager.is_project_trusted());

    let base_model_runtime = match options.model_runtime {
        Some(rt) => rt,
        None => {
            ModelRuntime::create(CreateModelRuntimeOptions {
                auth_path: Some(agent_dir.join("auth.json")),
                models_path: Some(agent_dir.join("models.json")),
                resolver: Some(resolver),
                refresh_on_create: Some(true),
                ..CreateModelRuntimeOptions::default()
            })
            .await
        }
    };

    // 获取本会话隔离的目录视图（当多个会话复用同一 ModelRuntime 时互不撤销扩展注册项）
    let model_runtime = base_model_runtime.acquire_session_view();

    let session_manager = match options.session_manager {
        Some(sm) => sm,
        None => {
            let session_dir = get_default_session_dir(&cwd, &agent_dir);
            SessionManager::create(&cwd_str, session_dir, None).map_err(|e| e.to_string())?
        }
    };

    let resource_loader: Arc<dyn ResourceLoader> = match options.resource_loader {
        Some(rl) => rl,
        None => Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: options.extension_factories.clone(),
            builtin_extensions: crate::extensions::built_in_extensions_for_session(
                settings_manager.clone(),
                &cwd,
                &agent_dir,
            ),
            ..DefaultResourceLoaderOptions::default()
        })),
    };

    let session_resources = resource_loader.load_session_resources(false).await?;
    let session_runner = session_resources.extensions.runner.clone();
    model_runtime.bind_extension_runner(&session_runner);

    // Check if session has existing data to restore
    let existing_context = session_manager.build_session_context();
    let branch_entries = session_manager.get_branch(None);
    let has_existing_session = !existing_context.messages.is_empty();
    let has_thinking_entry = branch_entries
        .iter()
        .any(|e| matches!(e, SessionEntry::ThinkingLevelChange(_)));

    let mut model = options.model;
    let mut model_fallback_message: Option<String> = None;

    let session_model_sel = get_branch_selection(&branch_entries, |prov, mid| {
        model_runtime.get_model(prov, mid)
    });

    if model.is_none()
        && has_existing_session
        && let Some(sel) = &session_model_sel
    {
        if let Some(restored) = model_runtime.get_model(&sel.provider, &sel.model_id) {
            let preserve_unauth = matches!(
                options.initial_model_policy,
                InitialModelPolicy::FixedIdentity {
                    preserve_unauthenticated: true,
                    ..
                }
            );
            if preserve_unauth || model_runtime.has_configured_auth(&restored.provider) {
                model = Some(restored);
            }
        }
        if model.is_none() {
            model_fallback_message = Some(format!(
                "Could not restore model {}/{}",
                sel.provider, sel.model_id
            ));
        }
    }

    if model.is_none() {
        let def_prov = settings_manager.get_default_provider();
        let def_mid = settings_manager.get_default_model();
        let def_think = settings_manager.get_default_thinking_level();
        let all_think = settings_manager.get_all_model_thinking_levels();
        let initial_res = find_initial_model(FindInitialModelOptions {
            scoped_models: &options.scoped_models,
            is_continuing: has_existing_session,
            default_provider: def_prov.as_deref(),
            default_model_id: def_mid.as_deref(),
            default_thinking_level: def_think,
            model_thinking_levels: &all_think,
            model_runtime: &model_runtime,
            policy: &options.initial_model_policy,
        });
        model = initial_res.model;
        if let Some(ref m) = model {
            if let Some(ref mut fb) = model_fallback_message {
                fb.push_str(&format!(". Using {}/{}", m.provider, m.id));
            }
        } else {
            model_fallback_message = Some(format_no_models_available_message());
        }
    }

    let mut thinking_level = options.thinking_level;
    if thinking_level.is_none() && has_existing_session {
        thinking_level = if has_thinking_entry {
            pi_session::parse_model_thinking_level(&existing_context.thinking_level)
        } else {
            Some(
                settings_manager
                    .get_default_thinking_level()
                    .unwrap_or(DEFAULT_THINKING_LEVEL),
            )
        };
    }
    if thinking_level.is_none()
        && let Some(ref m) = model
    {
        thinking_level = settings_manager.get_model_thinking_level(&m.provider, &m.id);
    }
    let resolved_thinking = match &model {
        Some(m) => {
            let raw = thinking_level
                .or_else(|| settings_manager.get_default_thinking_level())
                .unwrap_or(DEFAULT_THINKING_LEVEL);
            pi_ai::clamp_thinking_level(m, raw)
        }
        None => ModelThinkingLevel::Off,
    };

    // Tool selection & modifier validation
    if let Some(ref tool_list) = options.tools
        && let Some(err) = get_tool_list_error(tool_list)
    {
        return Err(format!("Invalid tools option: {err}"));
    }

    let default_tool_names: Vec<String> = if options.no_tools.is_some() {
        Vec::new()
    } else {
        settings_manager
            .get_default_tools()
            .unwrap_or_else(|| DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect())
    };

    let tool_modifiers: Vec<String> = options
        .tools
        .as_ref()
        .filter(|list| list.iter().any(|e| is_tool_modifier(e)))
        .cloned()
        .unwrap_or_default();

    let mut tools_options = options.tools_options.unwrap_or_default();
    let read_options = tools_options.read.get_or_insert_with(Default::default);
    if read_options.auto_resize_images.is_none() {
        read_options.auto_resize_images = Some(settings_manager.get_image_auto_resize());
    }
    let expose_session_env = settings_manager.get_expose_session_environment();
    let shell_command_prefix = settings_manager.get_shell_command_prefix();

    let bash_options = tools_options.bash.get_or_insert_with(Default::default);
    if bash_options.shell_path.is_none() {
        bash_options.shell_path = settings_manager.get_shell_path();
    }
    if bash_options.command_prefix.is_none() {
        bash_options.command_prefix = shell_command_prefix.clone();
    }
    if bash_options.expose_session_environment.is_none() {
        bash_options.expose_session_environment = Some(expose_session_env);
    }
    if bash_options.operations.is_none() {
        bash_options.operations = Some(create_local_bash_operations(
            bash_options.shell_path.clone(),
        ));
    }
    let bash_executor = create_bash_executor(bash_options.clone());

    let powershell_options = tools_options
        .powershell
        .get_or_insert_with(Default::default);
    if powershell_options.command_prefix.is_none() {
        powershell_options.command_prefix = shell_command_prefix;
    }
    if powershell_options.expose_session_environment.is_none() {
        powershell_options.expose_session_environment = Some(expose_session_env);
    }
    if powershell_options.operations.is_none() {
        powershell_options.operations = Some(create_local_powershell_operations());
    }
    let builtin_defs_map = create_all_tool_definitions(&cwd_str, Some(&tools_options));
    let mut all_known_tool_names: Vec<String> = ALL_BUILTIN_TOOL_NAMES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    for reg in session_runner.get_all_registered_tools() {
        if !all_known_tool_names.contains(&reg.definition.name) {
            all_known_tool_names.push(reg.definition.name);
        }
    }
    for custom in &options.custom_tools {
        if !all_known_tool_names.contains(&custom.name) {
            all_known_tool_names.push(custom.name.clone());
        }
    }

    let selected_tool_names: Option<Vec<String>> = if !tool_modifiers.is_empty() {
        Some(apply_tool_modifiers(&default_tool_names, &tool_modifiers))
    } else if let Some(ref explicit_patterns) = options.tools {
        let matcher = create_tool_name_matcher(explicit_patterns);
        let matched: Vec<String> = all_known_tool_names
            .iter()
            .filter(|name| matcher(name))
            .cloned()
            .collect();
        Some(matched)
    } else {
        None
    };

    let allowed_tool_names: Option<Vec<String>> = if !tool_modifiers.is_empty() {
        if options.no_tools == Some(NoToolsMode::All) {
            selected_tool_names.clone()
        } else {
            None
        }
    } else if options.tools.is_some() {
        selected_tool_names.clone()
    } else if options.no_tools == Some(NoToolsMode::All) {
        Some(Vec::new())
    } else {
        None
    };

    let excluded_tool_names: Option<Vec<String>> = options.exclude_tools.as_ref().map(|patterns| {
        let matcher = create_tool_name_matcher(patterns);
        all_known_tool_names
            .iter()
            .filter(|name| matcher(name))
            .cloned()
            .collect()
    });

    let excluded_set: std::collections::BTreeSet<String> = excluded_tool_names
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Base active tools + default-active extension/custom tools (unless no_tools == All or explicit allowlist)
    let mut initial_active_tool_names: Vec<String> = selected_tool_names
        .unwrap_or_else(|| {
            let mut list = default_tool_names.clone();
            if options.no_tools != Some(NoToolsMode::All) {
                for reg in session_runner.get_all_registered_tools() {
                    if reg.definition.is_default_active() && !list.contains(&reg.definition.name) {
                        list.push(reg.definition.name);
                    }
                }
                for custom in &options.custom_tools {
                    if custom.is_default_active() && !list.contains(&custom.name) {
                        list.push(custom.name.clone());
                    }
                }
            }
            list
        })
        .into_iter()
        .filter(|name| !excluded_set.contains(name))
        .collect();

    if let Some(ref allowed) = allowed_tool_names {
        let allowed_set: std::collections::BTreeSet<&String> = allowed.iter().collect();
        initial_active_tool_names.retain(|n| allowed_set.contains(n));
    }

    // Build initial system prompt options from ResourceLoader
    let skills_entries: Vec<SkillPromptEntry> = session_resources
        .skills
        .clone()
        .skills
        .iter()
        .map(|s| s.to_prompt_entry())
        .collect();
    let prompt_template_entries = session_resources
        .prompts
        .clone()
        .templates
        .iter()
        .map(|t| t.to_prompt_entry())
        .collect();
    let context_files = session_resources.agents_files.clone();
    let custom_prompt = session_resources.system_prompt.clone();
    let append_prompts = session_resources.append_system_prompt.clone();
    let append_str = (!append_prompts.is_empty()).then(|| append_prompts.join("\n\n"));

    let visible_skills = session_resources
        .skills
        .clone()
        .skills
        .into_iter()
        .filter(|s| !s.disable_model_invocation)
        .map(|s| pi_session::PromptSkill {
            name: s.name,
            description: s.description,
            file_path: s.file_path,
            base_dir: s.base_dir,
            disable_model_invocation: s.disable_model_invocation,
        })
        .collect();

    let system_prompt_options = BuildSystemPromptOptions {
        custom_prompt,
        selected_tools: Some(initial_active_tool_names.clone()),
        append_system_prompt: append_str,
        cwd: cwd_str.clone(),
        context_files: Some(context_files),
        skills: Some(visible_skills),
        ..BuildSystemPromptOptions::default()
    };

    let rt_for_stream = model_runtime.clone();
    let settings_for_stream = settings_manager.clone();
    let stream_fn: StreamFn = Arc::new(
        move |req_model: Model, transcript: TranscriptContext, mut opts: SimpleStreamOptions| {
            let rt = rt_for_stream.clone();
            let sm = settings_for_stream.clone();
            Box::pin(async move {
                let prov_retry = sm.get_provider_retry_settings();
                let idle_timeout = sm.get_http_idle_timeout_ms();
                let effective_timeout = if idle_timeout == 0 {
                    u64::from(i32::MAX as u32)
                } else {
                    idle_timeout
                };
                if opts.base.max_retries.is_none() {
                    opts.base.max_retries = prov_retry.max_retries;
                }
                if opts.base.max_retry_delay_ms.is_none() {
                    opts.base.max_retry_delay_ms = prov_retry.max_retry_delay_ms;
                }
                let _ = effective_timeout;
                rt.stream_simple(req_model, transcript, opts).await
            })
        },
    );

    let settings_for_block = settings_manager.clone();
    let block_images_fn: Arc<dyn Fn() -> bool + Send + Sync> =
        Arc::new(move || settings_for_block.get_block_images());
    let settings_for_norm = settings_manager.clone();
    let normalize_tool_result_fn: pi_session::NormalizeToolResultContentFn =
        Arc::new(move |content, model| {
            let auto_resize = settings_for_norm.get_image_auto_resize();
            let resize_options = model
                .input_limits
                .and_then(|limits| limits.images)
                .and_then(|images| images.resize)
                .map(|resize| crate::image_process::ImageResizeOptions {
                    max_width: resize.max_width,
                    max_height: resize.max_height,
                    max_bytes: resize.max_bytes,
                    jpeg_quality: resize.jpeg_quality,
                });
            Box::pin(async move {
                normalize_tool_result_images(
                    content,
                    NormalizeToolResultImagesOptions {
                        auto_resize_images: Some(auto_resize),
                        resize_options,
                    },
                )
                .await
            })
        });

    let mut session_opts = AgentSessionOptions::with_optional_model(
        &cwd_str,
        model.clone(),
        model_runtime.models().clone(),
        session_manager,
    );
    session_opts.thinking_level = resolved_thinking;
    session_opts.stream_fn = Some(stream_fn);
    session_opts.execution_log = options.execution_log;
    session_opts.execution_log_factory = options.execution_log_factory;
    session_opts.data_policy = options.data_policy;
    session_opts.base_tools = builtin_defs_map.into_values().collect();
    session_opts.custom_tools = options.custom_tools;
    session_opts.extension_runner = Some(session_runner.clone());
    session_opts.initial_active_tool_names = Some(initial_active_tool_names);
    session_opts.default_tool_modifiers = tool_modifiers;
    session_opts.allowed_tool_names = allowed_tool_names;
    session_opts.excluded_tool_names = excluded_tool_names;
    session_opts.system_prompt_options = Some(system_prompt_options);
    session_opts.compaction_settings = settings_manager.get_compaction_settings();
    session_opts.retry_policy = settings_manager.get_retry_policy();
    session_opts.tool_execution = options
        .tool_execution
        .unwrap_or(ToolExecutionMode::Parallel);
    session_opts.ui_context = options.ui_context;
    session_opts.mode = options.mode.unwrap_or(ExtensionMode::Print);
    session_opts.default_bash_executor = Some(bash_executor);
    session_opts.block_images = Some(block_images_fn);
    session_opts.normalize_tool_result_content = Some(normalize_tool_result_fn);
    session_opts.skills = skills_entries;
    session_opts.prompt_templates = prompt_template_entries;
    session_opts.enable_skill_commands = settings_manager.get_enable_skill_commands();

    let session = AgentSession::new(session_opts).map_err(|e| e.to_string())?;
    let mut construction = SessionConstructionGuard(Some(session.clone()));
    session
        .agent()
        .set_steering_mode(settings_manager.get_steering_mode());
    session
        .agent()
        .set_follow_up_mode(settings_manager.get_follow_up_mode());

    // Initialization writes are part of construction; a failed durable append cannot be ignored.
    let initialized = (|| -> Result<(), pi_session::SessionStoreError> {
        if has_existing_session {
            if !has_thinking_entry {
                session.with_session_manager_mut(|sm| {
                    sm.append_thinking_level_change(resolved_thinking.as_str())
                })?;
            }
        } else {
            if let Some(ref m) = model
                && !is_unconfigured_model(m)
            {
                session
                    .with_session_manager_mut(|sm| sm.append_model_change(&m.provider, &m.id))?;
            }
            session.with_session_manager_mut(|sm| {
                sm.append_thinking_level_change(resolved_thinking.as_str())
            })?;
        }
        Ok(())
    })();
    if let Err(error) = initialized {
        session.set_fatal_runtime_error(format!(
            "Failed to persist initial session selection: {error}"
        ));
        let _ = session.shutdown().await;
        return Err(error.to_string());
    }

    // Assemble CacheWarmer and wire into AgentSession
    let s_for_branch = session.clone();
    let s_for_usage = session.clone();
    let s_for_err = session.clone();
    let sm_for_mode = settings_manager.clone();
    let s_for_decide = session.clone();

    let cache_warmer = CacheWarmer::new(
        model_runtime.clone(),
        Arc::new(move || s_for_branch.with_session_manager(|sm| sm.get_branch(None))),
        Arc::new(move |source, provider, model, usage, note, is_current| {
            s_for_usage
                .with_session_manager_mut_if(
                    || is_current(),
                    |sm| sm.append_usage(source, provider, model, usage, note),
                )
                .map_err(|e| e.to_string())
        }),
        Arc::new(move |err_msg| {
            s_for_err
                .set_fatal_runtime_error(format!("Failed to persist cache warm usage: {err_msg}"));
        }),
        Arc::new(move || sm_for_mode.get_cache_warming_mode()),
        Arc::new(move |ev: CacheWarmingDecisionEvent| {
            let runner = s_for_decide.extension_runner();
            Box::pin(async move { runner.emit_cache_warming_decision(ev).await })
        }),
        options.clock,
    );

    let warmer_for_req = cache_warmer.clone();
    let s_for_req = session.clone();
    session.set_on_resolved_session_request(Some(Arc::new(move |req: ResolvedPhysicalRequest| {
        let s_check = s_for_req.clone();
        let snapshot_msgs: Vec<AgentMessage> = s_check.state().messages;
        let expected_session_id = req.session_id.clone();
        let expected_session_gen = req.session_generation;
        let expected_sel_gen = req.selection_generation;
        let expected_sel_prov = req.selected_model.provider.clone();
        let expected_sel_id = req.selected_model.id.clone();

        let is_current: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            if s_check.is_closing_or_closed()
                || s_check.fatal_runtime_error().is_some()
                || s_check.with_session_manager(SessionManager::is_poisoned)
            {
                return false;
            }
            if s_check.session_id() != expected_session_id
                || s_check.session_generation() != expected_session_gen
                || s_check.selection_generation() != expected_sel_gen
            {
                return false;
            }
            let current_model = s_check.model();
            if current_model.provider != expected_sel_prov || current_model.id != expected_sel_id {
                return false;
            }
            let current_msgs = s_check.state().messages;
            current_msgs.len() >= snapshot_msgs.len()
                && snapshot_msgs
                    .iter()
                    .enumerate()
                    .all(|(idx, msg)| current_msgs.get(idx) == Some(msg))
        });

        warmer_for_req.start(
            CacheWarmRequest {
                session_id: req.session_id,
                session_generation: req.session_generation,
                selection_generation: req.selection_generation,
                request_seq: req.request_seq,
                selected_model: req.selected_model,
                physical_model: req.physical_model,
                transcript: req.transcript,
                options: req.options,
            },
            is_current,
        );
    })));

    let warmer_for_settled = cache_warmer.clone();
    session.set_on_agent_settled(Some(Arc::new(move || {
        warmer_for_settled.on_agent_settled();
    })));

    let warmer_for_closing = cache_warmer.clone();
    session.set_on_session_closing(Some(Arc::new(move || {
        warmer_for_closing.cancel();
    })));

    // Wire zero-arg `reload_resources` hook
    let rl_for_reload = resource_loader.clone();
    let sm_for_reload = settings_manager.clone();
    let rt_for_reload = model_runtime.clone();
    let cwd_for_reload = cwd_str.clone();
    session.set_on_reload(Some(Arc::new(move || {
        let rl = rl_for_reload.clone();
        let sm = sm_for_reload.clone();
        let rt = rt_for_reload.clone();
        let cwd_s = cwd_for_reload.clone();
        Box::pin(async move {
            sm.reload();
            let resources = rl.load_session_resources(true).await?;
            let new_runner = resources.extensions.runner;

            let skills_entries: Vec<SkillPromptEntry> = resources
                .skills
                .skills
                .iter()
                .map(|s| s.to_prompt_entry())
                .collect();
            let tpl_entries = resources
                .prompts
                .templates
                .iter()
                .map(|t| t.to_prompt_entry())
                .collect();
            let visible_skills = resources
                .skills
                .skills
                .into_iter()
                .filter(|s| !s.disable_model_invocation)
                .map(|s| pi_session::PromptSkill {
                    name: s.name,
                    description: s.description,
                    file_path: s.file_path,
                    base_dir: s.base_dir,
                    disable_model_invocation: s.disable_model_invocation,
                })
                .collect();
            let append_prompts = resources.append_system_prompt;
            let append_str = (!append_prompts.is_empty()).then(|| append_prompts.join("\n\n"));

            let prompt_opts = BuildSystemPromptOptions {
                custom_prompt: resources.system_prompt,
                selected_tools: None,
                append_system_prompt: append_str,
                cwd: cwd_s,
                context_files: Some(resources.agents_files),
                skills: Some(visible_skills),
                ..BuildSystemPromptOptions::default()
            };

            Ok(SessionReloadOutcome {
                extension_runner: new_runner,
                on_committed: Some(Arc::new(move |runner| rt.bind_extension_runner(runner))),
                skills: skills_entries,
                prompt_templates: tpl_entries,
                enable_skill_commands: Some(sm.get_enable_skill_commands()),
                system_prompt_options: Some(prompt_opts),
            })
        })
    })));

    let (start_reason, prev_file) = options
        .session_start_event
        .unwrap_or_else(|| ("startup".to_owned(), None));
    let _ = session
        .extension_runner()
        .emit_lifecycle(SessionLifecycleEvent::SessionStart {
            reason: start_reason,
            previous_session_file: prev_file,
        })
        .await;

    let extensions_result = session_resources.extensions;
    construction.0.take();

    Ok(CreateAgentSessionResult {
        session,
        extensions_result,
        model_fallback_message,
        cache_warmer,
        model_runtime,
        settings_manager,
        resource_loader,
    })
}

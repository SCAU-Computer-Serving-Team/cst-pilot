use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use pi_agent::{
    AfterToolCallContext, AfterToolCallResult, Agent, AgentContext, AgentEvent, AgentInitialState,
    AgentLoopTurnUpdate, AgentMessage, AgentOptions, AgentRequestUpdate, AgentState, AgentTool,
    AgentToolCallOutcome, AgentTurnDecision, BeforeToolCallContext, BeforeToolCallResult,
    BoxFuture, PrepareNextTurnContext, PrepareRequestContext, RunToolCallOptions, StreamFn,
    ToolExecutionMode, ToolReplayPolicy, get_agent_current_system_message, get_agent_current_tools,
    models_stream_fn, run_tool_call,
};
use pi_ai::{
    AssistantMessage, ContentBlock, ImageContent, JsonValue, Model, ModelCost, ModelInputModality,
    ModelThinkingLevel, Models, RetryPolicy, SimpleStreamOptions, StopReason, SystemContent,
    SystemMessage, TextContent, ToolCall, ToolResultMessage, TranscriptContext, Usage, UserContent,
    UserContentBlock, UserMessage, clamp_thinking_level, is_context_overflow,
    is_recoverable_length, is_retryable_assistant_error, is_thinking_level_supported, now_ms,
    retry_delay_ms,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;

tokio::task_local! {
    static SHUTDOWN_SESSION: usize;
    static RUN_SESSION: usize;
}
thread_local! {
    static STORE_CALLBACKS: RefCell<Vec<(usize, u8)>> = const { RefCell::new(Vec::new()) };
}
struct StoreCallbackGuard;
impl StoreCallbackGuard {
    fn enter(id: usize, store: u8) -> Self {
        STORE_CALLBACKS.with(|callbacks| callbacks.borrow_mut().push((id, store)));
        Self
    }
    fn contains(id: usize, store: Option<u8>) -> bool {
        STORE_CALLBACKS.with(|callbacks| {
            callbacks
                .borrow()
                .iter()
                .any(|(session, kind)| *session == id && store.is_none_or(|store| store == *kind))
        })
    }
}
impl Drop for StoreCallbackGuard {
    fn drop(&mut self) {
        STORE_CALLBACKS.with(|callbacks| {
            callbacks.borrow_mut().pop();
        });
    }
}

use tokio_util::sync::CancellationToken;

use crate::bash::{BashExecutor, BashRequest, BashResult, ExecuteBashOptions};
use crate::branch_summary::{
    BranchSummaryDetails, BranchSummaryResult, GenerateBranchSummaryOptions,
    collect_entries_for_branch_summary, generate_branch_summary,
};
use crate::compaction::{
    CompactionPreparation, CompactionResult, CompactionSettings, DEFAULT_COMPACTION_SETTINGS,
    SummarizationCallOptions, calculate_context_tokens, compact, estimate_context_tokens,
    estimate_projected_context_tokens, estimate_tokens, prepare_compaction,
    prepare_compaction_with_adaptive_budget, should_compact,
};
use crate::execution_log::{
    ExecutionLogController, InFlightToolRecord, InputDeliveryStatus, JsonFileExecutionStateStore,
    PromptDisposition, StartupReconciliationReport, WORK_DURATION_CUSTOM_TYPE,
    reconcile_on_startup,
};
use crate::extensions::{
    AfterProviderResponseEvent, BeforeAgentStartCustomMessageDraft, BeforeAgentStartTrigger,
    BoundaryContextPreview, BoundaryEventKind, CompactionReason, ContextUsage, Extension,
    ExtensionActions, ExtensionCommandContextActions, ExtensionContextActions, ExtensionError,
    ExtensionInitError, ExtensionRunner, ForkSessionOptions, InputEventResult, InputSource,
    NavigateTreeOptions, ReplacedSessionContext, SendMessageOptions, SessionBeforeEventOutput,
    SessionLifecycleEvent, StreamingInputBehavior, ToolCallEvent, ToolDefinition, ToolExposure,
    ToolInfo, ToolLoadout, ToolResultEvent, TreePreparation, UserBashEvent, UserBashEventResult,
};
use crate::messages::{
    BashExecutionMessage, CustomMessage, convert_to_llm, convert_to_llm_with_options,
    filter_blocked_images,
};
use crate::nested_calls::{NestedToolCallHost, NestedToolCallOptions, NestedToolCallRunner};
use crate::session_export::export_session_to_jsonl;
use crate::session_manager::{
    BranchSummaryEntry, NewSessionOptions, SessionBoundaryDraft, SessionEntry, SessionManager,
    get_latest_compaction_entry, parse_iso_timestamp_ms,
};
use crate::session_store::SessionStoreError;
use crate::slash_commands::{
    BUILTIN_SLASH_COMMANDS, SlashCommandInfo, SlashCommandSource, SourceInfo,
};
use crate::system_prompt::{
    BuildSystemPromptOptions, NormalizedBuildSystemPromptOptions, build_system_prompt,
    build_system_prompt_sections, diff_system_prompt_sections,
    normalize_build_system_prompt_options,
};
use crate::ui::{ExtensionMode, ExtensionUiContext};
use crate::usage_totals::{add_usage_to_totals, combine_usage, create_usage_totals};
use crate::virtual_models::{
    FailedModelRoute, ModelRouteReason, ModelRouteRequest, PreviousModelRoute,
    VIRTUAL_MODEL_STATE_ENTRY, VirtualModelStateData, find_latest_response, get_branch_selection,
    get_virtual_model_state, is_virtual_model,
};

const TOOL_SELECTION_ENTRY: &str = "pi-tool-selection";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolSelectionState {
    selected: Vec<String>,
    known: BTreeSet<String>,
}

// Re-alias ToolUpdateSink from pi_agent
type AgentToolUpdateSink = pi_agent::ToolUpdateSink;

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentSessionEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
        will_retry: bool,
    },
    AgentSettled {
        aborted: bool,
    },
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: AgentMessage,
    },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: pi_ai::AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        partial_result: pi_agent::AgentToolResult,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: pi_agent::AgentToolResult,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    QueueUpdate {
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    CompactionStart {
        reason: CompactionReason,
    },
    CompactionEnd {
        reason: CompactionReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<CompactionResult>,
        aborted: bool,
        will_retry: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_error: Option<String>,
    },
    EntryAppended {
        entry: SessionEntry,
    },
    SessionInfoChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    ThinkingLevelChanged {
        level: ModelThinkingLevel,
    },
}

pub type AgentSessionListener = Arc<dyn Fn(AgentSessionEvent) + Send + Sync>;
pub type AsyncAgentSessionListener =
    Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<'static, ()> + Send + Sync>;
pub type PromptAdmittedCallback = Arc<dyn Fn(Result<PromptDisposition, String>) + Send + Sync>;

pub const UNCONFIGURED_MODEL_PROVIDER: &str = "pi-unconfigured";
pub const UNCONFIGURED_MODEL_ID: &str = "unconfigured";
pub const UNCONFIGURED_MODEL_API: &str = "pi-unconfigured";
pub const SESSION_CLOSED_ERROR: &str = "AgentSession is closed";

/// 构造不可联网的内部未配置模型占位实例（满足底层必填 `Model` 签名，且禁止任何网络请求）。
pub fn unconfigured_model() -> Model {
    Model {
        id: UNCONFIGURED_MODEL_ID.to_owned(),
        name: "Unconfigured".to_owned(),
        model_type: None,
        api: UNCONFIGURED_MODEL_API.to_owned(),
        provider: UNCONFIGURED_MODEL_PROVIDER.to_owned(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        prompt_cache: None,
        context_window: 0,
        max_tokens: 0,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        headers: None,
        compat: None,
    }
}

pub fn is_unconfigured_model(model: &Model) -> bool {
    model.provider == UNCONFIGURED_MODEL_PROVIDER || model.api == UNCONFIGURED_MODEL_API
}

pub fn format_no_models_available_message() -> String {
    "No models available.\n\nSet an API key environment variable (e.g. DEEPSEEK_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY), run /login, or configure models.json.".to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionLifecycleState {
    Open = 0,
    Closing = 1,
    Closed = 2,
}

impl SessionLifecycleState {
    fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Closing,
            2 => Self::Closed,
            _ => Self::Open,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedSkillBlock {
    pub name: String,
    pub location: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message: Option<String>,
}

/// 解析 `<skill name="..." location="...">...</skill>` 消息块（对照 `parseSkillBlock`）。
pub fn parse_skill_block(text: &str) -> Option<ParsedSkillBlock> {
    let rest = text.strip_prefix("<skill name=\"")?;
    let name_end = rest.find("\" location=\"")?;
    let name = &rest[..name_end];
    let after_name = &rest[name_end + "\" location=\"".len()..];
    let loc_end = after_name.find("\">\n")?;
    let location = &after_name[..loc_end];
    let after_open = &after_name[loc_end + "\">\n".len()..];
    let close_idx = after_open.find("\n</skill>")?;
    let content = &after_open[..close_idx];
    let tail = &after_open[close_idx + "\n</skill>".len()..];
    let user_message = if tail.is_empty() {
        None
    } else {
        let msg = tail.strip_prefix("\n\n")?;
        let trimmed = msg.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    };
    Some(ParsedSkillBlock {
        name: name.to_owned(),
        location: location.to_owned(),
        content: content.to_owned(),
        user_message,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillPromptEntry {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateEntry {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub content: String,
    pub source_info: SourceInfo,
    pub file_path: String,
}

/// 解析斜杠命令或提示词模板的 bash 风格引号参数（保留 `""` 与 `''` 空字符串参数）。
pub fn parse_template_command_args(args_str: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut in_quote: Option<char> = None;

    for ch in args_str.chars() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
            has_token = true;
        } else if ch.is_whitespace() {
            if has_token {
                args.push(std::mem::take(&mut current));
                has_token = false;
            }
        } else {
            current.push(ch);
            has_token = true;
        }
    }
    if has_token {
        args.push(current);
    }
    args
}

/// 单遍替换提示词模板中的 `$1..$N`、`$@`、`$ARGUMENTS`、`${N:-default}`、`${@:-default}`、`${@:N}`、`${@:N:L}` 占位符（实参内容绝不二次递归替换）。
pub fn substitute_template_args(content: &str, args: &[String]) -> String {
    static PLACEHOLDER_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = PLACEHOLDER_RE.get_or_init(|| {
        regex::Regex::new(
            r"\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)",
        )
        .expect("template placeholder regex")
    });
    let all_args = args.join(" ");
    re.replace_all(content, |caps: &regex::Captures<'_>| {
        if let Some(def_target) = caps.get(1) {
            let def_val = caps.get(2).map_or("", |m| m.as_str());
            let target = def_target.as_str();
            let val = if target == "@" || target == "ARGUMENTS" {
                all_args.as_str()
            } else {
                target
                    .parse::<usize>()
                    .ok()
                    .and_then(|idx| idx.checked_sub(1))
                    .and_then(|i| args.get(i))
                    .map_or("", String::as_str)
            };
            if val.is_empty() {
                def_val.to_owned()
            } else {
                val.to_owned()
            }
        } else if let Some(slice_start) = caps.get(3) {
            let raw_start = slice_start.as_str().parse::<usize>().unwrap_or(1);
            let start = raw_start.saturating_sub(1);
            let slice = if start >= args.len() {
                &[][..]
            } else if let Some(len_m) = caps.get(4) {
                let len = len_m.as_str().parse::<usize>().unwrap_or(0);
                let end = start.saturating_add(len).min(args.len());
                &args[start..end]
            } else {
                &args[start..]
            };
            slice.join(" ")
        } else if let Some(simple) = caps.get(5) {
            let s = simple.as_str();
            if s == "ARGUMENTS" || s == "@" {
                all_args.clone()
            } else {
                s.parse::<usize>()
                    .ok()
                    .and_then(|idx| idx.checked_sub(1))
                    .and_then(|i| args.get(i))
                    .cloned()
                    .unwrap_or_default()
            }
        } else {
            String::new()
        }
    })
    .into_owned()
}

pub fn expand_prompt_template_text(text: &str, templates: &[PromptTemplateEntry]) -> String {
    let Some(without_slash) = text.strip_prefix('/') else {
        return text.to_owned();
    };
    let (name, args_str) = match without_slash.find(' ') {
        Some(idx) => (&without_slash[..idx], &without_slash[idx + 1..]),
        None => (without_slash, ""),
    };
    let Some(tpl) = templates.iter().find(|t| t.name == name) else {
        return text.to_owned();
    };
    let args = parse_template_command_args(args_str);
    substitute_template_args(&tpl.content, &args)
}

#[derive(Clone)]
pub struct ResolvedPhysicalRequest {
    pub session_id: String,
    pub session_generation: u64,
    pub selection_generation: u64,
    pub request_seq: u64,
    pub selected_model: Model,
    pub physical_model: Model,
    pub transcript: TranscriptContext,
    pub options: SimpleStreamOptions,
}

pub type ResolvedRequestObserver = Arc<dyn Fn(ResolvedPhysicalRequest) + Send + Sync>;

pub type SessionRunnerCommitted = Arc<dyn Fn(&ExtensionRunner) + Send + Sync>;

pub struct SessionReloadOutcome {
    pub extension_runner: ExtensionRunner,
    /// Called after the previous catalog is detached and the new runner is installed.
    pub on_committed: Option<SessionRunnerCommitted>,
    pub skills: Vec<SkillPromptEntry>,
    pub prompt_templates: Vec<PromptTemplateEntry>,
    pub enable_skill_commands: Option<bool>,
    pub system_prompt_options: Option<BuildSystemPromptOptions>,
}

pub type SessionReloadHook =
    Arc<dyn Fn() -> BoxFuture<'static, Result<SessionReloadOutcome, String>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct PromptOptions {
    pub client_message_id: Option<String>,
    pub queue_item_id: Option<String>,
    pub expand_prompt_templates: Option<bool>,
    pub images: Option<Vec<ImageContent>>,
    pub streaming_behavior: Option<StreamingInputBehavior>,
    pub source: Option<InputSource>,
    pub on_admitted: Option<PromptAdmittedCallback>,
}

impl std::fmt::Debug for PromptOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptOptions")
            .field("client_message_id", &self.client_message_id)
            .field("queue_item_id", &self.queue_item_id)
            .field("expand_prompt_templates", &self.expand_prompt_templates)
            .field(
                "image_count",
                &self.images.as_ref().map_or(0, std::vec::Vec::len),
            )
            .field("streaming_behavior", &self.streaming_behavior)
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct NavigateTreeResult {
    pub editor_text: Option<String>,
    pub cancelled: bool,
    pub aborted: bool,
    pub summary_entry: Option<BranchSummaryEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub session_file: Option<PathBuf>,
    pub session_id: String,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_results: usize,
    pub total_messages: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,
    pub context_usage: Option<ContextUsage>,
}

/// Create a session-specific execution log for initial setup and replacements.
pub type ExecutionLogFactory =
    Arc<dyn Fn(&SessionManager) -> Result<ExecutionLogController, SessionStoreError> + Send + Sync>;

pub type NormalizeToolResultContentFn = Arc<
    dyn Fn(Vec<UserContentBlock>, Model) -> BoxFuture<'static, Vec<UserContentBlock>> + Send + Sync,
>;

/// 构造 `AgentSession` 的配置参数。
pub struct AgentSessionOptions {
    pub cwd: String,
    pub model: Option<Model>,
    pub no_models_error_fn: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub thinking_level: ModelThinkingLevel,
    pub models: Arc<Models>,
    pub stream_fn: Option<StreamFn>,
    pub session_manager: SessionManager,
    /// Initial log override; replacements use the factory or the target's default log.
    pub execution_log: Option<ExecutionLogController>,
    pub execution_log_factory: Option<ExecutionLogFactory>,
    pub data_policy: Option<Arc<dyn crate::SessionDataPolicy>>,
    pub base_tools: Vec<ToolDefinition>,
    pub custom_tools: Vec<ToolDefinition>,
    pub extensions: Vec<Box<dyn Extension>>,
    pub extension_runner: Option<ExtensionRunner>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub default_tool_modifiers: Vec<String>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub excluded_tool_names: Option<Vec<String>>,
    pub system_prompt_options: Option<BuildSystemPromptOptions>,
    pub compaction_settings: CompactionSettings,
    pub retry_policy: RetryPolicy,
    pub tool_execution: ToolExecutionMode,
    pub ui_context: Option<Arc<dyn ExtensionUiContext>>,
    pub mode: ExtensionMode,
    pub default_bash_executor: Option<BashExecutor>,
    pub block_images: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub normalize_tool_result_content: Option<NormalizeToolResultContentFn>,
    pub on_resolved_session_request: Option<ResolvedRequestObserver>,
    pub on_agent_settled: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_session_closing: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_reload: Option<SessionReloadHook>,
    pub skills: Vec<SkillPromptEntry>,
    pub prompt_templates: Vec<PromptTemplateEntry>,
    pub enable_skill_commands: bool,
}

impl AgentSessionOptions {
    pub fn new(
        cwd: impl Into<String>,
        model: Model,
        models: Arc<Models>,
        session_manager: SessionManager,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            model: (!is_unconfigured_model(&model)).then_some(model),
            no_models_error_fn: None,
            thinking_level: ModelThinkingLevel::Off,
            models,
            stream_fn: None,
            session_manager,
            execution_log: None,
            execution_log_factory: None,
            data_policy: None,
            base_tools: Vec::new(),
            custom_tools: Vec::new(),
            extensions: Vec::new(),
            extension_runner: None,
            initial_active_tool_names: None,
            default_tool_modifiers: Vec::new(),
            allowed_tool_names: None,
            excluded_tool_names: None,
            system_prompt_options: None,
            compaction_settings: DEFAULT_COMPACTION_SETTINGS,
            retry_policy: RetryPolicy::default(),
            tool_execution: ToolExecutionMode::Parallel,
            ui_context: None,
            mode: ExtensionMode::Print,
            default_bash_executor: None,
            block_images: None,
            normalize_tool_result_content: None,
            on_resolved_session_request: None,
            on_agent_settled: None,
            on_session_closing: None,
            on_reload: None,
            skills: Vec::new(),
            prompt_templates: Vec::new(),
            enable_skill_commands: true,
        }
    }

    pub fn new_unconfigured(
        cwd: impl Into<String>,
        models: Arc<Models>,
        session_manager: SessionManager,
    ) -> Self {
        Self::new(cwd, unconfigured_model(), models, session_manager)
    }

    pub fn with_optional_model(
        cwd: impl Into<String>,
        model: Option<Model>,
        models: Arc<Models>,
        session_manager: SessionManager,
    ) -> Self {
        match model {
            Some(m) => Self::new(cwd, m, models, session_manager),
            None => Self::new_unconfigured(cwd, models, session_manager),
        }
    }
}

pub fn parse_model_thinking_level(s: &str) -> Option<ModelThinkingLevel> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::XHigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

pub fn apply_tool_modifiers(base: &[String], modifiers: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for name in base {
        if !result.contains(name) {
            result.push(name.clone());
        }
    }
    for modifier in modifiers {
        let trimmed = modifier.trim();
        if let Some(added) = trimmed.strip_prefix('+') {
            let name = added.trim();
            if !name.is_empty() && !result.contains(&name.to_owned()) {
                result.push(name.to_owned());
            }
        } else if let Some(removed) = trimmed.strip_prefix('-') {
            let name = removed.trim();
            result.retain(|x| x != name);
        }
    }
    result
}

type DeferredSettledAction = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

type ShutdownResultReceiver = tokio::sync::watch::Receiver<Option<Result<(), String>>>;

fn spawn_detached(future: impl std::future::Future<Output = ()> + Send + 'static) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    } else {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("background runtime");
            runtime.block_on(future);
        });
    }
}

struct AgentSessionInner {
    cwd: String,
    models: Arc<Models>,
    agent: Agent,
    has_configured_model: AtomicBool,
    no_models_error_fn: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    session_manager: Mutex<SessionManager>,
    execution_log: Mutex<ExecutionLogController>,
    execution_log_factory: Option<ExecutionLogFactory>,
    data_policy: Option<Arc<dyn crate::SessionDataPolicy>>,
    extension_runner: RwLock<ExtensionRunner>,
    base_tools: RwLock<BTreeMap<String, ToolDefinition>>,
    custom_tools: RwLock<Vec<ToolDefinition>>,
    tool_definitions: RwLock<BTreeMap<String, (ToolDefinition, SourceInfo)>>,
    tool_registry: RwLock<BTreeMap<String, AgentTool>>,
    active_tool_names: RwLock<Vec<String>>,
    requested_tool_names: RwLock<Vec<String>>,
    initial_active_tool_names: Option<Vec<String>>,
    pending_tool_names: RwLock<BTreeSet<String>>,
    hidden_declarations: RwLock<BTreeSet<String>>,
    allowed_tool_names: Option<BTreeSet<String>>,
    excluded_tool_names: BTreeSet<String>,
    default_tool_modifiers: Vec<String>,
    base_system_prompt_options: RwLock<NormalizedBuildSystemPromptOptions>,
    run_system_prompt_options: RwLock<Option<NormalizedBuildSystemPromptOptions>>,
    compaction_settings: RwLock<CompactionSettings>,
    retry_policy: RwLock<RetryPolicy>,
    steering_messages: Mutex<Vec<String>>,
    follow_up_messages: Mutex<Vec<String>>,
    pending_bash_messages: Mutex<Vec<BashExecutionMessage>>,
    pending_custom_messages: Mutex<Vec<CustomMessage>>,
    pending_next_turn_messages: Mutex<Vec<CustomMessage>>,
    entry_ids_by_timestamp_and_role: Mutex<Vec<(String, u64, String)>>,
    last_assistant_message: Mutex<Option<(AssistantMessage, Option<String>)>>,
    failed_response: Mutex<Option<(Model, ModelThinkingLevel, AssistantMessage)>>,
    overflow_recovery_attempted: AtomicBool,
    retry_attempt: AtomicU64,
    retry_cancel: Mutex<Option<CancellationToken>>,
    compaction_cancel: Mutex<Option<CancellationToken>>,
    branch_summary_cancel: Mutex<Option<CancellationToken>>,
    reload_cancel: Mutex<Option<CancellationToken>>,
    is_agent_run_active: AtomicBool,
    active_run_workers: AtomicU64,
    run_cancel: Mutex<Option<CancellationToken>>,
    agent_run_abort_requested: AtomicBool,
    is_emitting_agent_settled: AtomicBool,
    deferred_settled_actions: Mutex<Vec<DeferredSettledAction>>,
    idle_notify: Notify,
    turn_index: AtomicU64,
    turn_started_at_ms: AtomicU64,
    next_run_seq: AtomicU64,
    next_input_timestamp: AtomicU64,
    pending_input_correlations: Mutex<BTreeMap<u64, (String, PromptDisposition)>>,
    pending_admission_callbacks: Mutex<BTreeMap<u64, PromptAdmittedCallback>>,
    startup_report: Mutex<StartupReconciliationReport>,
    fatal_runtime_error: Mutex<Option<String>>,
    listeners: Mutex<Vec<(u64, AgentSessionListener)>>,
    async_listeners: Mutex<Vec<(u64, AsyncAgentSessionListener)>>,
    next_listener_id: AtomicU64,
    nested_runner: OnceLock<Arc<NestedToolCallRunner>>,
    lifecycle_state: AtomicU8,
    shutdown_lock: Mutex<Option<ShutdownResultReceiver>>,
    operation_admission: Mutex<()>,
    before_session_invalidate: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    on_session_closing: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    default_bash_executor: RwLock<Option<BashExecutor>>,
    active_bash_cancels: Mutex<BTreeMap<u64, CancellationToken>>,
    active_input_cancels: Mutex<BTreeMap<u64, CancellationToken>>,
    next_input_hook_id: AtomicU64,
    next_bash_id: AtomicU64,
    block_images: RwLock<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    normalize_tool_result_content: RwLock<Option<NormalizeToolResultContentFn>>,
    on_resolved_session_request: RwLock<Option<ResolvedRequestObserver>>,
    on_agent_settled: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    on_reload: RwLock<Option<SessionReloadHook>>,
    skills: RwLock<Vec<SkillPromptEntry>>,
    prompt_templates: RwLock<Vec<PromptTemplateEntry>>,
    enable_skill_commands: AtomicBool,
    session_generation: AtomicU64,
    selection_generation: AtomicU64,
    request_seq: AtomicU64,
}

/// 会话与扩展核心控制器（整合 `Agent`、`SessionManager`、`ExtensionRunner`、`NestedToolCallRunner` 与 `ExecutionLogController`）。
#[derive(Clone)]
pub struct AgentSession {
    inner: Arc<AgentSessionInner>,
}

impl Drop for AgentSessionInner {
    fn drop(&mut self) {
        if SessionLifecycleState::from_u8(self.lifecycle_state.load(Ordering::SeqCst))
            != SessionLifecycleState::Open
        {
            return;
        }
        // Drop cannot await the normal shutdown lifecycle. It must still revoke all
        // capabilities, cancel background work, and release persistent locks.
        self.lifecycle_state
            .store(SessionLifecycleState::Closed as u8, Ordering::SeqCst);

        for slot in [
            &self.run_cancel,
            &self.retry_cancel,
            &self.compaction_cancel,
            &self.branch_summary_cancel,
            &self.reload_cancel,
        ] {
            if let Ok(guard) = slot.lock()
                && let Some(token) = guard.as_ref()
            {
                token.cancel();
            }
        }
        if let Ok(guard) = self.active_input_cancels.lock() {
            for token in guard.values() {
                token.cancel();
            }
        }
        if let Ok(guard) = self.active_bash_cancels.lock() {
            for token in guard.values() {
                token.cancel();
            }
        }
        self.agent.abort();

        if let Ok(mut callback) = self.on_session_closing.lock()
            && let Some(callback) = callback.take()
        {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback()));
        }
        if let Ok(mut callback) = self.before_session_invalidate.lock()
            && let Some(callback) = callback.take()
        {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback()));
        }
        if let Ok(runner) = self.extension_runner.read() {
            runner.invalidate(Some("Session was dropped".to_owned()));
            runner.detach_models();
        }
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.clear();
        }
        if let Ok(mut listeners) = self.async_listeners.lock() {
            listeners.clear();
        }
        if let Ok(mut manager) = self.session_manager.lock() {
            manager.release_persistent_store();
        }
        if let Ok(mut log) = self.execution_log.lock() {
            log.release_persistent_store();
        }
        self.idle_notify.notify_waiters();
    }
}

#[derive(Clone)]
struct WeakAgentSession {
    inner: std::sync::Weak<AgentSessionInner>,
}

impl WeakAgentSession {
    fn upgrade(&self) -> Option<AgentSession> {
        self.inner.upgrade().map(|inner| AgentSession { inner })
    }
}

struct ActiveRunGuard {
    session: WeakAgentSession,
}

impl Drop for ActiveRunGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.upgrade() {
            if std::thread::panicking() {
                session.set_fatal_runtime_error("Session run task failed");
            }
            if session
                .inner
                .active_run_workers
                .fetch_sub(1, Ordering::SeqCst)
                == 1
            {
                session
                    .inner
                    .is_agent_run_active
                    .store(false, Ordering::SeqCst);
            }
            session.inner.idle_notify.notify_waiters();
        }
    }
}

struct ActiveReloadGuard {
    session: WeakAgentSession,
}

impl Drop for ActiveReloadGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.upgrade() {
            session
                .inner
                .reload_cancel
                .lock()
                .expect("reload cancel lock")
                .take();
            if std::thread::panicking() {
                session.set_fatal_runtime_error("Session reload task failed");
            }
            session.inner.idle_notify.notify_waiters();
        }
    }
}

struct ActiveInputGuard {
    session: WeakAgentSession,
    id: u64,
}
impl Drop for ActiveInputGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.upgrade() {
            session
                .inner
                .active_input_cancels
                .lock()
                .expect("input cancels lock")
                .remove(&self.id);
            session.inner.idle_notify.notify_waiters();
        }
    }
}

struct ActiveAuxiliaryGuard {
    session: WeakAgentSession,
    compaction: bool,
}

impl Drop for ActiveAuxiliaryGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.upgrade() {
            let slot = if self.compaction {
                &session.inner.compaction_cancel
            } else {
                &session.inner.branch_summary_cancel
            };
            slot.lock().expect("auxiliary cancel lock").take();
            if std::thread::panicking() {
                session.set_fatal_runtime_error("Session auxiliary task failed");
            }
            session.inner.idle_notify.notify_waiters();
        }
    }
}

struct ActiveBashGuard {
    session: WeakAgentSession,
    id: u64,
}

impl Drop for ActiveBashGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.upgrade() {
            session
                .inner
                .active_bash_cancels
                .lock()
                .expect("bash cancels lock")
                .remove(&self.id);
            session.inner.idle_notify.notify_waiters();
        }
    }
}

struct SessionNestedHost {
    session: WeakAgentSession,
}

impl NestedToolCallHost for SessionNestedHost {
    fn get_tools(&self) -> Vec<AgentTool> {
        self.session
            .upgrade()
            .map_or_else(Vec::new, |s| s.get_callable_tools())
    }

    fn is_sequential(&self) -> bool {
        self.session
            .upgrade()
            .is_some_and(|s| s.agent().tool_execution() == ToolExecutionMode::Sequential)
    }

    fn run_tool_call(
        &self,
        tool_call: ToolCall,
        parent_tool_call_id: String,
        cancel_token: Option<CancellationToken>,
        on_update: AgentToolUpdateSink,
    ) -> BoxFuture<'static, AgentToolCallOutcome> {
        let weak_session = self.session.clone();
        Box::pin(async move {
            let Some(session) = weak_session.upgrade() else {
                return AgentToolCallOutcome {
                    tool_call,
                    result: pi_agent::AgentToolResult::error_text("Session has been dropped"),
                    is_error: true,
                    duration_ms: None,
                };
            };
            if let Some(fatal) = session.fatal_runtime_error() {
                return AgentToolCallOutcome {
                    tool_call,
                    result: pi_agent::AgentToolResult::error_text(fatal),
                    is_error: true,
                    duration_ms: None,
                };
            }
            let assistant_message = session
                .find_last_assistant_message()
                .unwrap_or_else(|| AssistantMessage::new_pending(&session.model()));
            let s_before = session.clone();
            let pid_before = parent_tool_call_id.clone();
            let s_after = session.clone();
            let pid_after = parent_tool_call_id.clone();
            let state = session.agent().state();

            run_tool_call(
                tool_call,
                RunToolCallOptions {
                    tools: session.get_callable_tools(),
                    assistant_message,
                    context: AgentContext {
                        messages: state.messages,
                        tools: state.tools,
                    },
                    cancel_token,
                    on_update: Some(on_update),
                    before_tool_call: Some(Arc::new(move |ctx, cancel| {
                        let s = s_before.clone();
                        let pid = pid_before.clone();
                        Box::pin(async move {
                            s.in_callback(s.handle_before_tool_call(ctx, Some(pid), cancel))
                                .await
                        })
                    })),
                    after_tool_call: Some(Arc::new(move |ctx, cancel| {
                        let s = s_after.clone();
                        let pid = pid_after.clone();
                        Box::pin(async move {
                            s.in_callback(s.handle_after_tool_call(ctx, Some(pid), cancel))
                                .await
                        })
                    })),
                },
            )
            .await
        })
    }

    fn emit_event(&self, event: AgentEvent) -> BoxFuture<'static, ()> {
        let weak_session = self.session.clone();
        Box::pin(async move {
            if let Some(session) = weak_session.upgrade() {
                session.agent().emit_external_event(event).await;
            }
        })
    }
}

impl AgentSession {
    pub fn new(options: AgentSessionOptions) -> Result<Self, ExtensionInitError> {
        let runner = match options.extension_runner {
            Some(existing) => existing,
            None => ExtensionRunner::from_extensions(options.cwd.clone(), options.extensions)?,
        };
        if let Some(ui) = options.ui_context.clone() {
            runner.set_ui_context(Some(ui), options.mode);
        }

        runner.bind_models(options.models.clone());

        let stream_fn = options
            .stream_fn
            .unwrap_or_else(|| models_stream_fn(options.models.clone()));

        let projection = options.session_manager.build_session_projection();
        // Model selection (including authenticated history fallback) belongs to the SDK policy.
        // None must remain unconfigured and must not revive an unauthenticated historical identity.
        let selected_model = options.model.filter(|m| !is_unconfigured_model(m));
        let initial_has_configured_model = selected_model.is_some();
        let initial_model = selected_model.unwrap_or_else(unconfigured_model);

        let initial_thinking = if initial_has_configured_model {
            // The SDK resolves persisted session state before constructing the session.
            // Keep the caller's explicit choice authoritative here.
            options.thinking_level
        } else {
            ModelThinkingLevel::Off
        };

        let block_images_for_convert = options.block_images.clone();
        let mut agent_opts = AgentOptions::new(stream_fn);
        agent_opts.initial_state = Some(AgentInitialState {
            system_prompt: None,
            model: Some(initial_model),
            thinking_level: Some(initial_thinking),
            tools: Some(Vec::new()),
            messages: Some(projection.messages),
        });
        agent_opts.convert_to_llm = Some(Arc::new(move |messages| {
            let block_fn = block_images_for_convert.clone();
            Box::pin(async move {
                let block = block_fn.as_ref().is_some_and(|f| f());
                convert_to_llm_with_options(&messages, block)
            })
        }));
        agent_opts.tool_execution = Some(options.tool_execution);
        agent_opts.session_id = Some(options.session_manager.get_session_id().to_owned());

        let agent = Agent::new(agent_opts);

        let base_map: BTreeMap<String, ToolDefinition> = options
            .base_tools
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect();

        let base_prompt_opts =
            normalize_build_system_prompt_options(&options.system_prompt_options.unwrap_or_else(
                || BuildSystemPromptOptions {
                    cwd: options.cwd.clone(),
                    ..BuildSystemPromptOptions::default()
                },
            ));

        let exec_log = match options.execution_log {
            Some(log) => log,
            None => match options.execution_log_factory.as_ref().map_or_else(
                || Self::default_execution_log(&options.session_manager),
                |factory| factory(&options.session_manager),
            ) {
                Ok(log) => log,
                Err(e) => {
                    runner.detach_models();
                    runner.invalidate(None);
                    return Err(ExtensionInitError {
                        extension_path: "<execution-log>".to_owned(),
                        message: e.to_string(),
                    });
                }
            },
        };

        let inner = Arc::new(AgentSessionInner {
            cwd: options.cwd,
            models: options.models,
            agent,
            has_configured_model: AtomicBool::new(initial_has_configured_model),
            no_models_error_fn: options.no_models_error_fn,
            session_manager: Mutex::new(options.session_manager),
            execution_log: Mutex::new(exec_log),
            execution_log_factory: options.execution_log_factory,
            data_policy: options.data_policy,
            extension_runner: RwLock::new(runner),
            base_tools: RwLock::new(base_map),
            custom_tools: RwLock::new(options.custom_tools),
            tool_definitions: RwLock::new(BTreeMap::new()),
            tool_registry: RwLock::new(BTreeMap::new()),
            active_tool_names: RwLock::new(Vec::new()),
            requested_tool_names: RwLock::new(Vec::new()),
            initial_active_tool_names: options.initial_active_tool_names.clone(),
            pending_tool_names: RwLock::new(BTreeSet::new()),
            hidden_declarations: RwLock::new(BTreeSet::new()),
            allowed_tool_names: options.allowed_tool_names.map(|v| v.into_iter().collect()),
            excluded_tool_names: options
                .excluded_tool_names
                .unwrap_or_default()
                .into_iter()
                .collect(),
            default_tool_modifiers: options.default_tool_modifiers,
            base_system_prompt_options: RwLock::new(base_prompt_opts),
            run_system_prompt_options: RwLock::new(None),
            compaction_settings: RwLock::new(options.compaction_settings),
            retry_policy: RwLock::new(options.retry_policy),
            steering_messages: Mutex::new(Vec::new()),
            follow_up_messages: Mutex::new(Vec::new()),
            pending_bash_messages: Mutex::new(Vec::new()),
            pending_custom_messages: Mutex::new(Vec::new()),
            pending_next_turn_messages: Mutex::new(Vec::new()),
            entry_ids_by_timestamp_and_role: Mutex::new(Vec::new()),
            last_assistant_message: Mutex::new(None),
            failed_response: Mutex::new(None),
            overflow_recovery_attempted: AtomicBool::new(false),
            retry_attempt: AtomicU64::new(0),
            retry_cancel: Mutex::new(None),
            compaction_cancel: Mutex::new(None),
            branch_summary_cancel: Mutex::new(None),
            reload_cancel: Mutex::new(None),
            is_agent_run_active: AtomicBool::new(false),
            active_run_workers: AtomicU64::new(0),
            run_cancel: Mutex::new(None),
            agent_run_abort_requested: AtomicBool::new(false),
            is_emitting_agent_settled: AtomicBool::new(false),
            deferred_settled_actions: Mutex::new(Vec::new()),
            idle_notify: Notify::new(),
            turn_index: AtomicU64::new(0),
            turn_started_at_ms: AtomicU64::new(0),
            next_run_seq: AtomicU64::new(1),
            next_input_timestamp: AtomicU64::new(0),
            pending_input_correlations: Mutex::new(BTreeMap::new()),
            pending_admission_callbacks: Mutex::new(BTreeMap::new()),
            startup_report: Mutex::new(StartupReconciliationReport::default()),
            fatal_runtime_error: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
            async_listeners: Mutex::new(Vec::new()),
            next_listener_id: AtomicU64::new(1),
            nested_runner: OnceLock::new(),
            lifecycle_state: AtomicU8::new(SessionLifecycleState::Open as u8),
            shutdown_lock: Mutex::new(None),
            operation_admission: Mutex::new(()),
            before_session_invalidate: Mutex::new(None),
            on_session_closing: Mutex::new(options.on_session_closing),
            default_bash_executor: RwLock::new(options.default_bash_executor),
            active_bash_cancels: Mutex::new(BTreeMap::new()),
            active_input_cancels: Mutex::new(BTreeMap::new()),
            next_input_hook_id: AtomicU64::new(1),
            next_bash_id: AtomicU64::new(1),
            block_images: RwLock::new(options.block_images),
            normalize_tool_result_content: RwLock::new(options.normalize_tool_result_content),
            on_resolved_session_request: RwLock::new(options.on_resolved_session_request),
            on_agent_settled: RwLock::new(options.on_agent_settled),
            on_reload: RwLock::new(options.on_reload),
            skills: RwLock::new(options.skills),
            prompt_templates: RwLock::new(options.prompt_templates),
            enable_skill_commands: AtomicBool::new(options.enable_skill_commands),
            session_generation: AtomicU64::new(1),
            selection_generation: AtomicU64::new(1),
            request_seq: AtomicU64::new(1),
        });

        let session = Self { inner };
        let nested_host = Arc::new(SessionNestedHost {
            session: session.downgrade(),
        });
        let _ = session
            .inner
            .nested_runner
            .set(Arc::new(NestedToolCallRunner::new(nested_host)));

        session.bind_extension_runner();
        session.refresh_tool_registry(options.initial_active_tool_names);
        session.restore_tools_from_transcript();
        session.refresh_finalized_context();
        session.install_agent_hooks();
        let report = match session.reconcile_startup() {
            Ok(rep) => rep,
            Err(e) => {
                session.extension_runner().detach_models();
                session.extension_runner().invalidate(None);
                return Err(ExtensionInitError {
                    extension_path: "<startup-reconciliation>".to_owned(),
                    message: e.to_string(),
                });
            }
        };
        *session
            .inner
            .startup_report
            .lock()
            .expect("startup report lock") = report;

        Ok(session)
    }

    /// Open the default execution sidecar for a manager whose session write lock is held.
    pub fn default_execution_log(
        sm: &SessionManager,
    ) -> Result<ExecutionLogController, SessionStoreError> {
        match sm.get_session_file() {
            Some(path) if sm.is_persisted() => {
                let store = JsonFileExecutionStateStore::for_session_file(path);
                #[cfg(feature = "test-support")]
                let fault = store.fault_injection();
                let log = ExecutionLogController::new(Box::new(store))?;
                #[cfg(feature = "test-support")]
                let log = log.with_fault_injection(fault);
                Ok(log)
            }
            None if sm.is_persisted() => Err(SessionStoreError::InvalidOperation(
                "Persistent stores without a session file require an execution_log or execution_log_factory".to_owned(),
            )),
            _ => Ok(ExecutionLogController::in_memory()),
        }
    }

    /// Preflight a replacement store without closing or aborting this session.
    pub fn prepare_replacement_execution_log(
        &self,
        manager: &mut SessionManager,
    ) -> Result<ExecutionLogController, SessionStoreError> {
        let mut log = self.inner.execution_log_factory.as_ref().map_or_else(
            || Self::default_execution_log(manager),
            |factory| factory(manager),
        )?;
        let policies = self
            .inner
            .tool_definitions
            .read()
            .expect("tool defs lock")
            .iter()
            .map(|(name, (def, _))| (name.clone(), def.replay_policy))
            .collect();
        reconcile_on_startup(manager, &mut log, &policies)?;
        Ok(log)
    }

    async fn in_callback<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        RUN_SESSION
            .scope(Arc::as_ptr(&self.inner) as usize, future)
            .await
    }

    /// True inside this session's run or shutdown callbacks, which cannot await their own settlement.
    pub fn is_in_callback_context(&self) -> bool {
        let id = Arc::as_ptr(&self.inner) as usize;
        StoreCallbackGuard::contains(id, None)
            || SHUTDOWN_SESSION
                .try_with(|current| *current == id)
                .unwrap_or(false)
            || RUN_SESSION
                .try_with(|current| *current == id)
                .unwrap_or(false)
    }

    pub fn startup_reconciliation_report(&self) -> StartupReconciliationReport {
        self.inner
            .startup_report
            .lock()
            .expect("startup report lock")
            .clone()
    }

    fn input_timestamp(&self) -> u64 {
        let now = now_ms();
        let prev = self
            .inner
            .next_input_timestamp
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |prev| {
                Some(now.max(prev + 1))
            })
            .expect("timestamp update");
        now.max(prev + 1)
    }

    fn downgrade(&self) -> WeakAgentSession {
        WeakAgentSession {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn lifecycle_state(&self) -> SessionLifecycleState {
        SessionLifecycleState::from_u8(self.inner.lifecycle_state.load(Ordering::SeqCst))
    }

    pub fn is_closed(&self) -> bool {
        self.lifecycle_state() == SessionLifecycleState::Closed
    }

    pub fn is_closing_or_closed(&self) -> bool {
        self.lifecycle_state() != SessionLifecycleState::Open
    }

    pub fn set_before_session_invalidate(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock") = cb;
    }

    pub fn set_on_session_closing(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .on_session_closing
            .lock()
            .expect("on closing lock") = cb;
    }

    pub fn set_on_resolved_session_request(&self, cb: Option<ResolvedRequestObserver>) {
        *self
            .inner
            .on_resolved_session_request
            .write()
            .expect("resolved req lock") = cb;
    }

    pub fn set_on_agent_settled(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .on_agent_settled
            .write()
            .expect("on settled lock") = cb;
    }

    pub fn set_on_reload(&self, cb: Option<SessionReloadHook>) {
        *self.inner.on_reload.write().expect("on reload lock") = cb;
    }

    pub fn set_default_bash_executor(&self, executor: Option<BashExecutor>) {
        *self
            .inner
            .default_bash_executor
            .write()
            .expect("bash executor lock") = executor;
    }

    pub fn set_block_images_fn(&self, cb: Option<Arc<dyn Fn() -> bool + Send + Sync>>) {
        *self.inner.block_images.write().expect("block images lock") = cb.clone();
        self.inner
            .agent
            .set_convert_to_llm(Arc::new(move |messages| {
                let block_fn = cb.clone();
                Box::pin(async move {
                    let block = block_fn.as_ref().is_some_and(|f| f());
                    convert_to_llm_with_options(&messages, block)
                })
            }));
    }

    pub fn is_block_images_enabled(&self) -> bool {
        self.inner
            .block_images
            .read()
            .expect("block images lock")
            .as_ref()
            .is_some_and(|f| f())
    }

    pub fn skills(&self) -> Vec<SkillPromptEntry> {
        self.inner.skills.read().expect("skills lock").clone()
    }

    pub fn set_skills(&self, skills: Vec<SkillPromptEntry>) {
        *self.inner.skills.write().expect("skills lock") = skills;
    }

    pub fn prompt_templates(&self) -> Vec<PromptTemplateEntry> {
        self.inner
            .prompt_templates
            .read()
            .expect("templates lock")
            .clone()
    }

    pub fn set_prompt_templates(&self, templates: Vec<PromptTemplateEntry>) {
        *self.inner.prompt_templates.write().expect("templates lock") = templates;
    }

    pub fn enable_skill_commands(&self) -> bool {
        self.inner.enable_skill_commands.load(Ordering::SeqCst)
    }

    pub fn set_enable_skill_commands(&self, enabled: bool) {
        self.inner
            .enable_skill_commands
            .store(enabled, Ordering::SeqCst);
    }

    pub fn session_generation(&self) -> u64 {
        self.inner.session_generation.load(Ordering::SeqCst)
    }

    pub fn selection_generation(&self) -> u64 {
        self.inner.selection_generation.load(Ordering::SeqCst)
    }

    pub fn create_replaced_session_context(&self) -> ReplacedSessionContext {
        ReplacedSessionContext::new(self.extension_runner().create_context())
    }

    fn cancel_active_operations(&self) {
        if let Some(token) = self
            .inner
            .run_cancel
            .lock()
            .expect("run cancel lock")
            .as_ref()
        {
            token.cancel();
        }
        if self.inner.is_agent_run_active.load(Ordering::SeqCst) {
            self.inner
                .agent_run_abort_requested
                .store(true, Ordering::SeqCst);
        }
        if let Some(c) = self.inner.retry_cancel.lock().expect("lock").as_ref() {
            c.cancel();
        }
        if let Some(c) = self.inner.compaction_cancel.lock().expect("lock").as_ref() {
            c.cancel();
        }
        if let Some(c) = self
            .inner
            .branch_summary_cancel
            .lock()
            .expect("lock")
            .as_ref()
        {
            c.cancel();
        }
        if let Some(cancel) = self
            .inner
            .reload_cancel
            .lock()
            .expect("reload cancel lock")
            .as_ref()
        {
            cancel.cancel();
        }
        let input_tokens: Vec<_> = self
            .inner
            .active_input_cancels
            .lock()
            .expect("input cancels lock")
            .values()
            .cloned()
            .collect();
        for token in input_tokens {
            token.cancel();
        }
        let bash_tokens: Vec<CancellationToken> = self
            .inner
            .active_bash_cancels
            .lock()
            .expect("bash cancels lock")
            .values()
            .cloned()
            .collect();
        for token in bash_tokens {
            token.cancel();
        }
        self.inner.agent.abort();
    }

    pub fn is_bash_running(&self) -> bool {
        !self
            .inner
            .active_bash_cancels
            .lock()
            .expect("bash cancels lock")
            .is_empty()
    }

    pub fn abort_retry(&self) {
        if let Some(cancel) = self
            .inner
            .retry_cancel
            .lock()
            .expect("retry cancel lock")
            .as_ref()
        {
            cancel.cancel();
        }
    }

    pub fn abort_bash(&self) {
        let tokens: Vec<CancellationToken> = self
            .inner
            .active_bash_cancels
            .lock()
            .expect("bash cancels lock")
            .values()
            .cloned()
            .collect();
        for t in tokens {
            t.cancel();
        }
    }

    /// 幂等异步关闭会话：进入 `Closing` 拒绝外部新操作与新工具启动，等待已启动工具与消息落盘结算，
    /// 发出一次 `session_shutdown`，执行 `before_session_invalidate`，释放持久化 `.lock` 文件锁并转入 `Closed`。
    pub async fn shutdown(&self) -> Result<(), String> {
        self.shutdown_with_reason("quit", None).await
    }

    pub async fn shutdown_with_reason(
        &self,
        reason: &str,
        target_session_file: Option<PathBuf>,
    ) -> Result<(), String> {
        if self.is_in_callback_context() {
            return Err(
                "Cannot await shutdown from its own session callback; use dispose".to_owned(),
            );
        }
        let mut completion = self.start_shutdown(reason.to_owned(), target_session_file);
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            completion
                .changed()
                .await
                .map_err(|_| "Session shutdown task stopped unexpectedly".to_owned())?;
        }
    }

    fn start_shutdown(
        &self,
        reason: String,
        target_session_file: Option<PathBuf>,
    ) -> tokio::sync::watch::Receiver<Option<Result<(), String>>> {
        let mut slot = self.inner.shutdown_lock.lock().expect("shutdown lock");
        if let Some(existing) = slot.as_ref() {
            return existing.clone();
        }
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.inner
                .lifecycle_state
                .store(SessionLifecycleState::Closing as u8, Ordering::SeqCst);
        }
        let (done, receiver) = tokio::sync::watch::channel(None);
        *slot = Some(receiver.clone());
        let session = self.clone();
        let work = async move {
            let id = Arc::as_ptr(&session.inner) as usize;
            let result = SHUTDOWN_SESSION
                .scope(id, session.finish_shutdown(reason, target_session_file))
                .await;
            let _ = done.send(Some(result));
        };
        spawn_detached(work);
        receiver
    }

    async fn finish_shutdown(
        &self,
        reason: String,
        target_session_file: Option<PathBuf>,
    ) -> Result<(), String> {
        let on_closing = self
            .inner
            .on_session_closing
            .lock()
            .expect("on closing lock")
            .take();
        if let Some(cb) = on_closing
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb())).is_err()
        {
            self.set_fatal_runtime_error("Session closing callback panicked");
        }
        self.cancel_active_operations();
        self.inner.agent.wait_for_idle().await;
        self.wait_for_idle().await;

        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionShutdown {
                reason,
                target_session_file,
                sigint: false,
            })
            .await;

        let before_invalidate = self
            .inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock")
            .take();
        if let Some(cb) = before_invalidate
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb())).is_err()
        {
            self.set_fatal_runtime_error("Session invalidation callback panicked");
        }
        let runner = self.extension_runner();
        runner.invalidate(None);
        runner.detach_models();
        self.inner.listeners.lock().expect("listeners lock").clear();
        self.inner
            .async_listeners
            .lock()
            .expect("async listeners lock")
            .clear();
        self.inner
            .on_resolved_session_request
            .write()
            .expect("observer lock")
            .take();
        self.inner
            .on_agent_settled
            .write()
            .expect("settled lock")
            .take();
        self.inner.on_reload.write().expect("reload lock").take();

        let result = self.fatal_runtime_error().map_or(Ok(()), Err);
        self.with_session_manager_internal(SessionManager::release_persistent_store);
        self.with_execution_log_internal(ExecutionLogController::release_persistent_store);
        self.inner
            .lifecycle_state
            .store(SessionLifecycleState::Closed as u8, Ordering::SeqCst);
        self.inner.idle_notify.notify_waiters();
        result
    }

    /// Start the same detached close task without waiting for lifecycle callbacks.
    pub fn dispose(&self) {
        if StoreCallbackGuard::contains(Arc::as_ptr(&self.inner) as usize, None) {
            let session = self.clone();
            std::thread::spawn(move || {
                session.start_shutdown("quit".to_owned(), None);
            });
            return;
        }
        self.start_shutdown("quit".to_owned(), None);
    }

    pub fn agent(&self) -> &Agent {
        &self.inner.agent
    }

    pub fn models(&self) -> &Arc<Models> {
        &self.inner.models
    }

    pub fn extension_runner(&self) -> ExtensionRunner {
        self.inner
            .extension_runner
            .read()
            .expect("runner lock")
            .clone()
    }

    pub fn with_session_manager<R>(&self, f: impl FnOnce(&SessionManager) -> R) -> R {
        assert!(
            !StoreCallbackGuard::contains(Arc::as_ptr(&self.inner) as usize, Some(0)),
            "Cannot reenter session storage from its mutation callback"
        );
        let guard = self.inner.session_manager.lock().expect("sm lock");
        f(&guard)
    }

    pub fn with_session_manager_mut<R>(
        &self,
        f: impl FnOnce(&mut SessionManager) -> Result<R, SessionStoreError>,
    ) -> Result<R, SessionStoreError> {
        self.with_session_manager_mut_if(|| true, f)
            .map(|value| value.expect("unconditional mutation"))
    }

    /// Rechecks a read-only condition while holding operation admission, then commits.
    /// The condition may inspect session state; reentrant mutations are rejected.
    pub fn with_session_manager_mut_if<R>(
        &self,
        condition: impl FnOnce() -> bool,
        f: impl FnOnce(&mut SessionManager) -> Result<R, SessionStoreError>,
    ) -> Result<Option<R>, SessionStoreError> {
        if StoreCallbackGuard::contains(Arc::as_ptr(&self.inner) as usize, None) {
            return Err(SessionStoreError::InvalidOperation(
                "Cannot reenter session mutation from a storage callback".into(),
            ));
        }
        let _admission = self
            .inner
            .operation_admission
            .lock()
            .expect("admission lock");
        let current = {
            let _callback = StoreCallbackGuard::enter(Arc::as_ptr(&self.inner) as usize, 2);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(condition)).map_err(|_| {
                SessionStoreError::InvalidOperation("Session mutation condition failed".into())
            })?
        };
        if !current {
            return Ok(None);
        }
        self.ensure_external_open()
            .map_err(SessionStoreError::InvalidOperation)?;
        self.with_session_manager_internal(|manager| {
            let _callback = StoreCallbackGuard::enter(Arc::as_ptr(&self.inner) as usize, 0);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(manager)))
                .unwrap_or_else(|_| {
                    Err(SessionStoreError::InvalidOperation(
                        "Session storage mutation callback failed".into(),
                    ))
                })
                .map(Some)
        })
    }

    fn with_session_manager_internal<R>(&self, f: impl FnOnce(&mut SessionManager) -> R) -> R {
        let mut guard = self.inner.session_manager.lock().expect("sm lock");
        f(&mut guard)
    }

    pub fn with_execution_log<R>(&self, f: impl FnOnce(&ExecutionLogController) -> R) -> R {
        assert!(
            !StoreCallbackGuard::contains(Arc::as_ptr(&self.inner) as usize, Some(1)),
            "Cannot reenter execution storage from its mutation callback"
        );
        let guard = self.inner.execution_log.lock().expect("exec log lock");
        f(&guard)
    }

    pub fn with_execution_log_mut<R>(
        &self,
        f: impl FnOnce(&mut ExecutionLogController) -> Result<R, SessionStoreError>,
    ) -> Result<R, SessionStoreError> {
        if StoreCallbackGuard::contains(Arc::as_ptr(&self.inner) as usize, None) {
            return Err(SessionStoreError::InvalidOperation(
                "Cannot reenter session mutation from a storage callback".into(),
            ));
        }
        let _admission = self
            .inner
            .operation_admission
            .lock()
            .expect("admission lock");
        self.ensure_external_open()
            .map_err(SessionStoreError::InvalidOperation)?;
        self.with_execution_log_internal(|log| {
            let _callback = StoreCallbackGuard::enter(Arc::as_ptr(&self.inner) as usize, 1);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(log))).unwrap_or_else(|_| {
                Err(SessionStoreError::InvalidOperation(
                    "Execution storage mutation callback failed".into(),
                ))
            })
        })
    }

    fn with_execution_log_internal<R>(
        &self,
        f: impl FnOnce(&mut ExecutionLogController) -> R,
    ) -> R {
        let mut guard = self.inner.execution_log.lock().expect("exec log lock");
        f(&mut guard)
    }

    /// 执行启动时恢复核对（核对 `Delivering` 输入、`Running` 中断标记与未闭合受控工具调用）。
    pub fn reconcile_startup(&self) -> Result<StartupReconciliationReport, SessionStoreError> {
        if self.is_streaming() {
            return Err(SessionStoreError::InvalidOperation(
                "Cannot reconcile an active run".to_owned(),
            ));
        }
        let policies: BTreeMap<String, ToolReplayPolicy> = self
            .inner
            .tool_definitions
            .read()
            .expect("tool defs lock")
            .iter()
            .map(|(name, (def, _))| (name.clone(), def.replay_policy))
            .collect();
        let mut sm = self.inner.session_manager.lock().expect("sm lock");
        let mut log = self.inner.execution_log.lock().expect("exec log lock");
        reconcile_on_startup(&mut sm, &mut log, &policies)
    }

    pub fn fatal_runtime_error(&self) -> Option<String> {
        self.inner
            .fatal_runtime_error
            .lock()
            .expect("fatal lock")
            .clone()
    }

    fn ensure_external_open(&self) -> Result<(), String> {
        if StoreCallbackGuard::contains(Arc::as_ptr(&self.inner) as usize, None) {
            return Err("Cannot reenter session operations from a storage callback".into());
        }
        if self.is_closing_or_closed() {
            return Err(SESSION_CLOSED_ERROR.to_owned());
        }
        self.ensure_runtime_writable()
    }

    fn ensure_runtime_writable(&self) -> Result<(), String> {
        if self.is_closed() {
            return Err(SESSION_CLOSED_ERROR.to_owned());
        }
        if let Some(err) = self.fatal_runtime_error() {
            return Err(err);
        }
        if self.with_session_manager(SessionManager::is_poisoned) {
            let err =
                "Session storage is poisoned; reopen and reconcile before execution".to_owned();
            self.set_fatal_runtime_error(err.clone());
            return Err(err);
        }
        Ok(())
    }

    fn no_models_error_message(&self) -> String {
        self.inner
            .no_models_error_fn
            .as_ref()
            .map_or_else(format_no_models_available_message, |f| f())
    }

    /// Stop new model/tool work after a host-side persistence failure.
    pub fn set_fatal_runtime_error(&self, err: impl Into<String>) {
        let msg = err.into();
        let mut guard = self.inner.fatal_runtime_error.lock().expect("fatal lock");
        if guard.is_none() {
            *guard = Some(msg);
        }
        drop(guard);
        self.inner
            .agent_run_abort_requested
            .store(true, Ordering::SeqCst);
        self.inner.agent.abort();
    }

    pub fn subscribe(&self, listener: AgentSessionListener) -> Box<dyn FnOnce() + Send + Sync> {
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .listeners
            .lock()
            .expect("listeners lock")
            .push((id, listener));
        let inner = Arc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = inner.upgrade()
                && let Ok(mut guard) = inner.listeners.lock()
            {
                guard.retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }

    /// 注册可等待的异步事件订阅器（用于 JSON/RPC 输出流向事件源施加异步背压，无需 `blocking_send` 或按事件 `spawn`）。
    pub fn subscribe_async(
        &self,
        listener: AsyncAgentSessionListener,
    ) -> Box<dyn FnOnce() + Send + Sync> {
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .async_listeners
            .lock()
            .expect("async listeners lock")
            .push((id, listener));
        let inner = Arc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = inner.upgrade()
                && let Ok(mut guard) = inner.async_listeners.lock()
            {
                guard.retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }

    fn project_event(&self, event: AgentSessionEvent) -> Option<AgentSessionEvent> {
        let Some(policy) = &self.inner.data_policy else {
            return Some(event);
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| policy.project_event(event)))
        {
            Ok(Ok(event)) => Some(event),
            _ => {
                self.set_fatal_runtime_error("Session event projection failed");
                None
            }
        }
    }

    fn emit(&self, event: AgentSessionEvent) {
        if let Some(event) = self.project_event(event) {
            self.emit_projected(event);
        }
    }

    fn emit_projected(&self, event: AgentSessionEvent) {
        let listeners: Vec<AgentSessionListener> = self
            .inner
            .listeners
            .lock()
            .expect("listeners lock")
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            l(event.clone());
        }
    }

    async fn emit_async(&self, event: AgentSessionEvent) {
        let Some(event) = self.project_event(event) else {
            return;
        };
        self.emit_projected(event.clone());
        let async_listeners: Vec<AsyncAgentSessionListener> = self
            .inner
            .async_listeners
            .lock()
            .expect("async listeners lock")
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in async_listeners {
            RUN_SESSION
                .scope(Arc::as_ptr(&self.inner) as usize, l(event.clone()))
                .await;
        }
    }

    pub fn state(&self) -> AgentState {
        self.inner.agent.state()
    }

    pub fn model(&self) -> Model {
        self.inner.agent.state().model
    }

    /// 返回当前已配置的模型；当会话处于无可用模型（Unconfigured）状态时返回 `None`。
    pub fn configured_model(&self) -> Option<Model> {
        if !self.has_configured_model() {
            return None;
        }
        let m = self.model();
        (!is_unconfigured_model(&m)).then_some(m)
    }

    pub fn has_configured_model(&self) -> bool {
        self.inner.has_configured_model.load(Ordering::SeqCst)
            && !is_unconfigured_model(&self.model())
    }

    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.inner.agent.state().thinking_level
    }

    pub fn is_streaming(&self) -> bool {
        self.inner.is_agent_run_active.load(Ordering::SeqCst)
            || self.inner.agent.state().is_streaming
    }

    pub fn is_compacting(&self) -> bool {
        self.inner.compaction_cancel.lock().expect("lock").is_some()
            || self
                .inner
                .branch_summary_cancel
                .lock()
                .expect("lock")
                .is_some()
    }

    pub fn is_idle(&self) -> bool {
        !self.is_streaming()
            && !self.is_compacting()
            && !self.is_bash_running()
            && self
                .inner
                .active_input_cancels
                .lock()
                .expect("input cancels lock")
                .is_empty()
            && self
                .inner
                .reload_cancel
                .lock()
                .expect("reload cancel lock")
                .is_none()
            && self.inner.active_run_workers.load(Ordering::SeqCst) == 0
    }

    pub async fn wait_for_idle(&self) {
        loop {
            if self.is_idle() {
                return;
            }
            let notified = self.inner.idle_notify.notified();
            if self.is_idle() {
                return;
            }
            notified.await;
        }
    }

    /// Request cancellation without waiting for callbacks or tool finalization.
    pub fn request_abort(&self) {
        self.cancel_active_operations();
    }

    pub async fn abort(&self) {
        self.request_abort();
        self.inner.agent.wait_for_idle().await;
        self.wait_for_idle().await;
    }

    pub fn session_id(&self) -> String {
        self.with_session_manager(|sm| sm.get_session_id().to_owned())
    }

    pub fn session_file(&self) -> Option<PathBuf> {
        self.with_session_manager(|sm| sm.get_session_file().map(Path::to_path_buf))
    }

    pub fn session_name(&self) -> Option<String> {
        self.with_session_manager(SessionManager::get_session_name)
    }

    pub fn set_session_name(&self, name: impl Into<String>) -> Result<String, SessionStoreError> {
        if self.is_closing_or_closed() {
            return Err(SessionStoreError::InvalidOperation(
                SESSION_CLOSED_ERROR.to_owned(),
            ));
        }
        let entry_id = self.with_session_manager_mut(|sm| sm.append_session_info(name))?;
        let current_name = self.session_name();
        self.emit(AgentSessionEvent::SessionInfoChanged {
            name: current_name.clone(),
        });
        let runner = self.extension_runner();
        let sid = self.session_id();
        let sfile = self.session_file();
        spawn_detached(async move {
            let _ = runner
                .emit_lifecycle(SessionLifecycleEvent::SessionInfoChanged {
                    session_id: sid,
                    session_file: sfile,
                    name: current_name,
                })
                .await;
        });
        Ok(entry_id)
    }

    pub fn system_prompt(&self) -> String {
        let opts = self
            .inner
            .run_system_prompt_options
            .read()
            .expect("lock")
            .clone()
            .unwrap_or_else(|| {
                self.inner
                    .base_system_prompt_options
                    .read()
                    .expect("lock")
                    .clone()
            });
        build_system_prompt(&opts.to_build_options()).unwrap_or_default()
    }

    pub fn set_base_system_prompt_options(&self, options: BuildSystemPromptOptions) {
        let mut norm = normalize_build_system_prompt_options(&options);
        let active = self.get_active_tool_names();
        norm.selected_tools = active.clone();
        *self.inner.base_system_prompt_options.write().expect("lock") = norm;
        self.rebuild_base_system_prompt(&active);
    }

    pub async fn set_model(&self, model: Model) -> Result<(), String> {
        self.ensure_external_open()?;
        let (prev, is_real, changed_thinking) = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            let prev = self.configured_model();
            let is_real = !is_unconfigured_model(&model);
            let previous_thinking = self.thinking_level();
            let effective = if is_real {
                clamp_thinking_level(&model, previous_thinking)
            } else {
                ModelThinkingLevel::Off
            };
            let persisted = self.with_session_manager_internal(|sm| {
                if is_real {
                    sm.append_model_change(&model.provider, &model.id)?;
                }
                if effective != previous_thinking {
                    sm.append_thinking_level_change(effective.as_str())?;
                }
                Ok::<_, SessionStoreError>(())
            });
            if let Err(error) = persisted {
                let message = format!("Failed to persist model selection: {error}");
                self.set_fatal_runtime_error(message.clone());
                return Err(message);
            }
            self.inner
                .has_configured_model
                .store(is_real, Ordering::SeqCst);
            self.inner
                .selection_generation
                .fetch_add(1, Ordering::SeqCst);
            self.inner.agent.set_model(model.clone());
            self.inner.agent.set_thinking_level(effective);
            (
                prev,
                is_real,
                (effective != previous_thinking).then_some(effective),
            )
        };
        if let Some(level) = changed_thinking {
            self.emit(AgentSessionEvent::ThinkingLevelChanged { level });
        }
        if is_real
            && prev
                .as_ref()
                .is_none_or(|p| p.provider != model.provider || p.id != model.id)
        {
            let _ = self
                .in_callback(self.extension_runner().emit_lifecycle(
                    SessionLifecycleEvent::ModelSelect {
                        model,
                        previous_model: prev,
                        source: "set".to_owned(),
                    },
                ))
                .await;
        }
        Ok(())
    }

    pub fn set_thinking_level(&self, level: ModelThinkingLevel) -> Result<(), String> {
        self.ensure_external_open()?;
        let changed = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            let model = self.model();
            let effective = if !self.has_configured_model() {
                ModelThinkingLevel::Off
            } else if is_thinking_level_supported(&model, level) {
                level
            } else {
                clamp_thinking_level(&model, level)
            };
            let prev = self.thinking_level();
            if effective == prev {
                None
            } else {
                if let Err(error) = self.with_session_manager_internal(|sm| {
                    sm.append_thinking_level_change(effective.as_str())
                }) {
                    let message = format!("Failed to persist thinking level: {error}");
                    self.set_fatal_runtime_error(message.clone());
                    return Err(message);
                }
                self.inner.agent.set_thinking_level(effective);
                Some(effective)
            }
        };
        if let Some(level) = changed {
            self.emit(AgentSessionEvent::ThinkingLevelChanged { level });
        }
        Ok(())
    }

    pub fn set_compaction_settings(&self, settings: CompactionSettings) {
        *self.inner.compaction_settings.write().expect("lock") = settings;
    }

    pub fn compaction_settings(&self) -> CompactionSettings {
        *self.inner.compaction_settings.read().expect("lock")
    }

    pub fn set_retry_policy(&self, policy: RetryPolicy) {
        *self.inner.retry_policy.write().expect("lock") = policy;
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        self.inner.retry_policy.read().expect("lock").clone()
    }

    pub fn get_active_tool_names(&self) -> Vec<String> {
        self.inner
            .active_tool_names
            .read()
            .expect("active tools lock")
            .clone()
    }

    pub fn get_callable_tool_names(&self) -> Vec<String> {
        self.get_callable_tools()
            .into_iter()
            .map(|t| t.name)
            .collect()
    }

    pub fn get_callable_tools(&self) -> Vec<AgentTool> {
        let active: BTreeSet<String> = self.get_active_tool_names().into_iter().collect();
        let defs = self.inner.tool_definitions.read().expect("defs lock");
        let reg = self.inner.tool_registry.read().expect("reg lock");
        reg.values()
            .filter(|tool| {
                let exposure = defs
                    .get(&tool.name)
                    .map_or(ToolExposure::Direct, |(d, _)| d.exposure);
                matches!(exposure, ToolExposure::Codemode | ToolExposure::Deferred)
                    || (exposure == ToolExposure::Direct && active.contains(&tool.name))
            })
            .cloned()
            .collect()
    }

    pub fn get_all_tools(&self) -> Vec<ToolInfo> {
        let defs = self.inner.tool_definitions.read().expect("defs lock");
        defs.values()
            .map(|(d, source_info)| ToolInfo {
                name: d.name.clone(),
                description: d.description.clone(),
                parameters: d.parameters.clone(),
                output_schema: d.output_schema.clone(),
                prompt_guidelines: d.prompt_guidelines.clone(),
                namespace: d.namespace.clone(),
                annotations: d.annotations.clone(),
                source_info: source_info.clone(),
                exposure: d.exposure,
                replay_policy: d.replay_policy,
            })
            .collect()
    }

    /// 保序激活工具集（`#11495`：保持原本已激活且仍在请求集合中的工具相对顺序不变，新激活工具追加在末尾）。
    pub fn set_active_tools_by_name(&self, tool_names: Vec<String>) -> Result<(), String> {
        self.ensure_external_open()?;
        if *self
            .inner
            .requested_tool_names
            .read()
            .expect("requested tools lock")
            != tool_names
        {
            let state = ToolSelectionState {
                selected: tool_names.clone(),
                known: self
                    .inner
                    .tool_definitions
                    .read()
                    .expect("defs lock")
                    .keys()
                    .cloned()
                    .collect(),
            };
            let id = self
                .with_session_manager_mut(|sm| {
                    sm.append_custom_entry(TOOL_SELECTION_ENTRY, Some(json!(state)))
                })
                .map_err(|error| error.to_string())?;
            if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&id).cloned()) {
                self.emit(AgentSessionEvent::EntryAppended { entry });
            }
        }
        *self
            .inner
            .requested_tool_names
            .write()
            .expect("requested tools lock") = tool_names.clone();
        let previous = self.get_active_tool_names();
        self.apply_tool_loadout(&tool_names);
        let active_now: BTreeSet<String> = self.get_active_tool_names().into_iter().collect();
        if previous.iter().any(|n| !active_now.contains(n)) {
            self.inner
                .pending_tool_names
                .write()
                .expect("pending lock")
                .clear();
        }
        Ok(())
    }

    fn is_allowed_tool(&self, name: &str) -> bool {
        if self.inner.excluded_tool_names.contains(name) {
            return false;
        }
        if let Some(allowed) = &self.inner.allowed_tool_names {
            return allowed.contains(name);
        }
        true
    }

    fn refresh_tool_registry(&self, explicit_active_names: Option<Vec<String>>) {
        let runner = self.extension_runner();
        let mut defs_map: BTreeMap<String, (ToolDefinition, SourceInfo)> = BTreeMap::new();

        for (name, def) in self.inner.base_tools.read().expect("base lock").iter() {
            if self.is_allowed_tool(name) {
                defs_map.insert(
                    name.clone(),
                    (def.clone(), SourceInfo::builtin(name.clone())),
                );
            }
        }
        for reg_tool in runner.get_all_registered_tools() {
            if self.is_allowed_tool(&reg_tool.definition.name) {
                defs_map.insert(
                    reg_tool.definition.name.clone(),
                    (reg_tool.definition, reg_tool.source_info),
                );
            }
        }
        for custom in self.inner.custom_tools.read().expect("custom lock").iter() {
            if self.is_allowed_tool(&custom.name) {
                defs_map.insert(
                    custom.name.clone(),
                    (
                        custom.clone(),
                        SourceInfo::synthetic(format!("<sdk:{}>", custom.name), "sdk"),
                    ),
                );
            }
        }

        let mut reg_map: BTreeMap<String, AgentTool> = BTreeMap::new();
        for (name, (def, _)) in &defs_map {
            let r = runner.clone();
            let mut agent_tool =
                def.to_agent_tool(move |tc_id, cancel| r.create_tool_context(tc_id, cancel));
            let execute = agent_tool.execute.clone();
            let weak = self.downgrade();
            agent_tool.execute = Arc::new(move |id, arguments, cancel, update| {
                let (execute, session) = (execute.clone(), weak.upgrade());
                Box::pin(async move {
                    let session = session.ok_or_else(|| SESSION_CLOSED_ERROR.to_owned())?;
                    session.ensure_external_open()?;
                    session
                        .in_callback(execute(id, arguments, cancel, update))
                        .await
                })
            });
            reg_map.insert(name.clone(), agent_tool);
        }

        let previous_definitions: BTreeSet<String> = self
            .inner
            .tool_definitions
            .read()
            .expect("defs lock")
            .keys()
            .cloned()
            .collect();
        *self.inner.tool_definitions.write().expect("defs lock") = defs_map.clone();
        *self.inner.tool_registry.write().expect("reg lock") = reg_map;

        let initial_names = explicit_active_names.unwrap_or_else(|| {
            let mut names = self
                .inner
                .requested_tool_names
                .read()
                .expect("requested tools lock")
                .clone();
            let mentioned: BTreeSet<String> = self
                .with_session_manager(|sm| sm.get_branch(None))
                .into_iter()
                .flat_map(|entry| match entry {
                    SessionEntry::Message(entry) => match entry.message {
                        AgentMessage::System(message) => message
                            .tools_added
                            .unwrap_or_default()
                            .into_iter()
                            .map(|tool| tool.name)
                            .chain(
                                message
                                    .tools_removed
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|tool| tool.name),
                            )
                            .collect(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                })
                .collect();
            for (name, (definition, _)) in &defs_map {
                if !previous_definitions.contains(name)
                    && !mentioned.contains(name)
                    && definition.is_default_active()
                    && !names.contains(name)
                {
                    names.push(name.clone());
                }
            }
            names
        });
        *self
            .inner
            .requested_tool_names
            .write()
            .expect("requested tools lock") = initial_names.clone();
        self.apply_tool_loadout(&initial_names);
    }

    fn apply_tool_loadout(&self, requested_names: &[String]) -> Vec<AgentTool> {
        let requested_set: BTreeSet<String> = requested_names.iter().cloned().collect();
        let current_active = self.get_active_tool_names();

        // 保序：保留原先已激活且仍在 requested_set 中的工具顺序，再按 requested_names 顺序追加新工具
        let mut ordered_names: Vec<String> = current_active
            .into_iter()
            .filter(|n| requested_set.contains(n))
            .collect();
        for name in requested_names {
            if !ordered_names.contains(name) {
                ordered_names.push(name.clone());
            }
        }

        let defs = self.inner.tool_definitions.read().expect("defs lock");
        let reg = self.inner.tool_registry.read().expect("reg lock");

        let mut valid_active_names = Vec::new();
        let mut base_declared_tools = Vec::new();
        for name in ordered_names {
            if let Some((def, _)) = defs.get(&name)
                && def.is_activatable()
                && self.is_allowed_tool(&name)
                && let Some(tool) = reg.get(&name)
            {
                valid_active_names.push(name);
                base_declared_tools.push(tool.clone());
            }
        }

        let all_defs: Vec<ToolDefinition> = defs.values().map(|(d, _)| d.clone()).collect();
        let loadout = ToolLoadout {
            all_tools: all_defs,
            active_tool_names: valid_active_names.clone(),
            declared_tools: base_declared_tools.clone(),
        };

        let mut description_overrides: BTreeMap<String, String> = BTreeMap::new();
        let mut hidden_set: BTreeSet<String> = BTreeSet::new();

        for name in &valid_active_names {
            if let Some((def, source_info)) = defs.get(name)
                && let Some(prep_fn) = &def.prepare_loadout
            {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prep_fn(&loadout))) {
                    Ok(Some(changes)) => {
                        for (k, v) in changes.descriptions {
                            description_overrides.insert(k, v);
                        }
                        for h in changes.hidden_declarations {
                            hidden_set.insert(h);
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        self.extension_runner().emit_error(ExtensionError {
                            extension_path: source_info.path.clone(),
                            event: "prepare_loadout".to_owned(),
                            error: format!("prepare_loadout panicked in tool {name}"),
                        });
                    }
                }
            }
        }

        let final_declared: Vec<AgentTool> = base_declared_tools
            .into_iter()
            .map(|mut tool| {
                if let Some(desc) = description_overrides.get(&tool.name) {
                    tool.description = desc.clone();
                }
                tool
            })
            .collect();

        drop(defs);
        drop(reg);

        *self.inner.active_tool_names.write().expect("active lock") = valid_active_names.clone();
        *self.inner.hidden_declarations.write().expect("hidden lock") = hidden_set;
        self.inner.agent.set_tools(final_declared.clone());
        self.rebuild_base_system_prompt(&valid_active_names);
        final_declared
    }

    fn rebuild_base_system_prompt(&self, active_tool_names: &[String]) {
        let defs = self.inner.tool_definitions.read().expect("defs lock");
        let hidden = self
            .inner
            .hidden_declarations
            .read()
            .expect("hidden lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let mut snippets = BTreeMap::new();
        let mut guidelines = BTreeMap::new();
        for (name, (def, _)) in defs.iter() {
            if let Some(snip) = &def.prompt_snippet {
                let normalized = snip.split_whitespace().collect::<Vec<_>>().join(" ");
                if !normalized.is_empty() {
                    snippets.insert(name.clone(), normalized);
                }
            }
            if let Some(gl) = &def.prompt_guidelines
                && !gl.is_empty()
            {
                guidelines.insert(name.clone(), gl.clone());
            }
        }
        drop(defs);

        let mut base = self
            .inner
            .base_system_prompt_options
            .write()
            .expect("base prompt lock");
        base.selected_tools = active_tool_names.to_vec();
        base.hidden_tools = hidden;
        for (k, v) in snippets {
            base.tool_snippets.insert(k, v);
        }
        for (k, v) in guidelines {
            base.tool_guidelines.insert(k, v);
        }
    }

    fn restore_tools_from_transcript(&self) {
        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        // Selection intent survives temporary inventory loss and its model-visible removal deltas.
        if let Some(state) = branch.iter().rev().find_map(|entry| match entry {
            SessionEntry::Custom(entry) if entry.custom_type == TOOL_SELECTION_ENTRY => entry
                .data
                .clone()
                .and_then(|data| serde_json::from_value::<ToolSelectionState>(data).ok()),
            _ => None,
        }) {
            let mut names = state.selected;
            for (name, (definition, _)) in self
                .inner
                .tool_definitions
                .read()
                .expect("defs lock")
                .iter()
            {
                if !state.known.contains(name)
                    && definition.is_default_active()
                    && !names.contains(name)
                {
                    names.push(name.clone());
                }
            }
            *self
                .inner
                .requested_tool_names
                .write()
                .expect("requested tools lock") = names.clone();
            self.apply_tool_loadout(&names);
            return;
        }
        let sys_msgs: Vec<AgentMessage> = branch
            .into_iter()
            .filter_map(|e| match e {
                SessionEntry::Message(m) if m.message.role() == "system" => Some(m.message),
                _ => None,
            })
            .collect();

        if sys_msgs.is_empty() {
            let defaults = self
                .inner
                .initial_active_tool_names
                .clone()
                .unwrap_or_else(|| {
                    self.inner
                        .tool_definitions
                        .read()
                        .expect("defs lock")
                        .values()
                        .filter(|(definition, _)| definition.is_default_active())
                        .map(|(definition, _)| definition.name.clone())
                        .collect()
                });
            let modified = apply_tool_modifiers(&defaults, &self.inner.default_tool_modifiers);
            *self
                .inner
                .requested_tool_names
                .write()
                .expect("requested tools lock") = modified.clone();
            self.apply_tool_loadout(&modified);
            return;
        }

        let mut mentioned = BTreeSet::new();
        for msg in &sys_msgs {
            if let AgentMessage::System(s) = msg {
                for t in s.tools_added.as_deref().unwrap_or_default() {
                    mentioned.insert(t.name.clone());
                }
                for t in s.tools_removed.as_deref().unwrap_or_default() {
                    mentioned.insert(t.name.clone());
                }
            }
        }

        let base_tools = self.inner.base_tools.read().expect("base lock");
        let defs = self.inner.tool_definitions.read().expect("defs lock");
        let new_ext_tools: Vec<String> = defs
            .iter()
            .filter(|(name, (d, _))| {
                !mentioned.contains(*name)
                    && !base_tools.contains_key(*name)
                    && d.is_default_active()
            })
            .map(|(name, _)| name.clone())
            .collect();
        drop(defs);
        drop(base_tools);

        let mut names: Vec<String> = get_agent_current_tools(&sys_msgs)
            .into_iter()
            .map(|t| t.name)
            .collect();
        for n in new_ext_tools {
            if !names.contains(&n) {
                names.push(n);
            }
        }
        let modified = apply_tool_modifiers(&names, &self.inner.default_tool_modifiers);
        *self
            .inner
            .requested_tool_names
            .write()
            .expect("requested tools lock") = modified.clone();
        self.apply_tool_loadout(&modified);
    }

    pub fn refresh_finalized_context(&self) {
        let projection = self.with_session_manager(SessionManager::build_session_projection);
        {
            let mut map_guard = self
                .inner
                .entry_ids_by_timestamp_and_role
                .lock()
                .expect("entry map lock");
            map_guard.clear();
            for proj_entry in &projection.entries {
                for msg in &proj_entry.messages {
                    map_guard.push((
                        msg.role().to_owned(),
                        msg.timestamp(),
                        proj_entry.source_entry.id().to_owned(),
                    ));
                }
            }
        }
        self.inner.agent.set_messages(projection.messages);
    }

    fn bind_extension_runner(&self) {
        let runner = self.extension_runner();
        let w = self.downgrade();
        let s1 = w.clone();
        let s2 = w.clone();
        let s3 = w.clone();
        let s4 = w.clone();
        let s5 = w.clone();
        let s6 = w.clone();
        let s7 = w.clone();
        let s8 = w.clone();
        let s9 = w.clone();
        let s10 = w.clone();
        let s11 = w.clone();
        let s12 = w.clone();
        let s13 = w.clone();
        let s14 = w.clone();
        let requested_session = w.clone();

        let actions = ExtensionActions {
            send_message: Arc::new(move |draft, opts| {
                let s = s1.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                tokio::spawn(async move {
                    let _ = s.send_custom_message(draft, opts).await;
                });
                Ok(())
            }),
            send_user_message: Arc::new(move |text, images, deliver_as| {
                let s = s2.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                tokio::spawn(async move {
                    let _ = s
                        .prompt(
                            text,
                            Some(PromptOptions {
                                images,
                                streaming_behavior: deliver_as,
                                source: Some(InputSource::Extension),
                                expand_prompt_templates: Some(false),
                                ..PromptOptions::default()
                            }),
                        )
                        .await;
                });
                Ok(())
            }),
            append_entry: Arc::new(move |custom_type, data| {
                let s = s3.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                let id = s
                    .with_session_manager_mut(|sm| sm.append_custom_entry(custom_type, data))
                    .map_err(|e| e.to_string())?;
                if let Some(entry) = s.with_session_manager(|sm| sm.get_entry(&id).cloned()) {
                    s.emit(AgentSessionEvent::EntryAppended { entry });
                }
                Ok(id)
            }),
            set_session_name: Arc::new(move |name| {
                let s = s4.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                s.set_session_name(name).map_err(|e| e.to_string())
            }),
            get_session_name: Arc::new(move || s5.upgrade().and_then(|s| s.session_name())),
            set_label: Arc::new(move |target_id, label| {
                let s = s6.upgrade().ok_or_else(|| "Session dropped".to_owned())?;
                s.ensure_external_open()?;
                s.with_session_manager_mut(|sm| sm.append_label_change(&target_id, label))
                    .map_err(|e| e.to_string())
            }),
            get_active_tools: Arc::new(move || {
                s7.upgrade()
                    .map_or_else(Vec::new, |s| s.get_active_tool_names())
            }),
            get_requested_tools: Arc::new(move || {
                requested_session
                    .upgrade()
                    .map(|s| {
                        s.inner
                            .requested_tool_names
                            .read()
                            .expect("requested tools lock")
                            .clone()
                    })
                    .unwrap_or_default()
            }),
            get_all_tools: Arc::new(move || {
                s8.upgrade().map_or_else(Vec::new, |s| s.get_all_tools())
            }),
            set_active_tools: Arc::new(move |names| {
                s9.upgrade()
                    .ok_or_else(|| SESSION_CLOSED_ERROR.to_owned())?
                    .set_active_tools_by_name(names)
            }),
            refresh_tools: Arc::new(move || {
                if let Some(s) = s10.upgrade()
                    && !s.is_closing_or_closed()
                {
                    s.refresh_tool_registry(None);
                }
            }),
            get_commands: Arc::new(move || {
                s11.upgrade()
                    .map_or_else(Vec::new, |s| s.get_slash_commands())
            }),
            set_model: Arc::new(move |model| {
                let s_opt = s12.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.set_model(model).await?;
                    Ok(true)
                })
            }),
            get_thinking_level: Arc::new(move || {
                s13.upgrade()
                    .map_or(ModelThinkingLevel::Off, |s| s.thinking_level())
            }),
            set_thinking_level: Arc::new(move |lvl| {
                if let Some(s) = s14.upgrade() {
                    let _ = s.set_thinking_level(lvl);
                }
            }),
        };

        let c1 = w.clone();
        let c2 = w.clone();
        let c3 = w.clone();
        let c4 = w.clone();
        let c5 = w.clone();
        let c6 = w.clone();
        let c7 = w.clone();
        let c8 = w.clone();
        let c9 = w.clone();
        let c10 = w.clone();
        let c11 = w.clone();
        let c12 = w.clone();
        let c13 = w.clone();
        let c14 = w.clone();
        let c15 = w.clone();
        let c16 = w.clone();
        let c17 = w.clone();

        let context_actions = ExtensionContextActions {
            get_model: Arc::new(move || c1.upgrade().and_then(|s| s.configured_model())),
            is_idle: Arc::new(move || c2.upgrade().is_none_or(|s| s.is_idle())),
            get_cancel_token: Arc::new(move || c3.upgrade().and_then(|s| s.agent().cancel_token())),
            abort: Arc::new(move || {
                if let Some(s) = c4.upgrade() {
                    tokio::spawn(async move {
                        s.abort().await;
                    });
                }
            }),
            has_pending_messages: Arc::new(move || {
                c5.upgrade().is_some_and(|s| s.pending_message_count() > 0)
            }),
            shutdown: Arc::new(move || {
                if let Some(s) = c6.upgrade() {
                    s.dispose();
                }
            }),
            get_context_usage: Arc::new(move || c7.upgrade().and_then(|s| s.get_context_usage())),
            compact: Arc::new(move |opts| {
                if let Some(s) = c8.upgrade() {
                    let instructions = opts.and_then(|o| o.custom_instructions);
                    tokio::spawn(async move {
                        let _ = s.compact(instructions).await;
                    });
                }
            }),
            get_system_prompt: Arc::new(move || {
                c9.upgrade().map_or_else(String::new, |s| s.system_prompt())
            }),
            get_system_prompt_options: Arc::new(move || {
                c10.upgrade()
                    .map_or_else(BuildSystemPromptOptions::default, |s| {
                        s.inner
                            .base_system_prompt_options
                            .read()
                            .expect("lock")
                            .to_build_options()
                    })
            }),
            get_callable_tools: Arc::new(move || {
                c11.upgrade()
                    .map_or_else(Vec::new, |s| s.get_callable_tools())
            }),
            execute_tool: Some(Arc::new(move |caller_id, name, args, opts| {
                let s_opt = c12.upgrade();
                Box::pin(async move {
                    let Some(s) = s_opt else {
                        return AgentToolCallOutcome {
                            tool_call: ToolCall {
                                id: format!("{caller_id}/0"),
                                name,
                                arguments: serde_json::Map::new(),
                                thought_signature: None,
                                namespace: None,
                                arguments_error: None,
                            },
                            result: pi_agent::AgentToolResult::error_text("Session dropped"),
                            is_error: true,
                            duration_ms: None,
                        };
                    };
                    s.execute_nested_tool_call(&caller_id, &name, args, opts)
                        .await
                })
            })),
            get_session_tree: Arc::new(move || {
                c13.upgrade().map_or_else(Vec::new, |s| {
                    s.with_session_manager(SessionManager::get_tree)
                })
            }),
            get_session_branch: Arc::new(move || {
                c14.upgrade().map_or_else(Vec::new, |s| {
                    s.with_session_manager(|sm| sm.get_branch(None))
                })
            }),
            get_session_leaf_id: Arc::new(move || {
                c15.upgrade().and_then(|s| {
                    s.with_session_manager(|sm| sm.get_leaf_id().map(ToOwned::to_owned))
                })
            }),
            get_session_id: Arc::new(move || {
                c17.upgrade().map_or_else(String::new, |s| s.session_id())
            }),
            get_session_file: Arc::new(move || c16.upgrade().and_then(|s| s.session_file())),
        };

        let m1 = w.clone();
        let m2 = w.clone();
        let m3 = w.clone();
        let m4 = w.clone();
        let m5 = w.clone();
        let m6 = w;
        let command_actions = ExtensionCommandContextActions {
            wait_for_idle: Arc::new(move || {
                let s_opt = m1.upgrade();
                Box::pin(async move {
                    if let Some(s) = s_opt {
                        s.wait_for_idle().await;
                    }
                })
            }),
            new_session: Arc::new(move |parent| {
                let s_opt = m2.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.new_session(parent).await
                })
            }),
            fork: Arc::new(move |entry_id, opts| {
                let s_opt = m3.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.fork(&entry_id, opts).await
                })
            }),
            navigate_tree: Arc::new(move |target_id, opts| {
                let s_opt = m4.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    let res = s.navigate_tree(&target_id, opts).await?;
                    Ok(!res.cancelled)
                })
            }),
            switch_session: Arc::new(move |path| {
                let s_opt = m5.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.switch_session(path).await
                })
            }),
            reload: Some(Arc::new(move || {
                let s_opt = m6.upgrade();
                Box::pin(async move {
                    let s = s_opt.ok_or_else(|| "Session dropped".to_owned())?;
                    s.reload_resources().await
                })
            })),
        };

        runner.bind_core(actions, context_actions);
        runner.bind_command_context(Some(command_actions));
    }

    fn install_agent_hooks(&self) {
        let w = self.downgrade();
        let raw_stream = self.inner.agent.stream_fn();
        let stream_session = w.clone();
        self.inner
            .agent
            .set_stream_fn(Arc::new(move |model, mut transcript, mut options| {
                let raw = raw_stream.clone();
                let session = stream_session.upgrade();
                Box::pin(async move {
                    if is_unconfigured_model(&model) {
                        let err_msg = session
                            .as_ref()
                            .map_or_else(format_no_models_available_message, |s| {
                                s.no_models_error_message()
                            });
                        let stream = pi_ai::create_assistant_message_event_stream();
                        let mut message = AssistantMessage::new_pending(&model);
                        message.stop_reason = StopReason::Error;
                        message.error_message = Some(err_msg);
                        stream.push(pi_ai::AssistantMessageEvent::Error {
                            reason: StopReason::Error,
                            error: message.clone(),
                        });
                        stream.end(Some(message));
                        return stream;
                    }
                    let failure = match session.as_ref() {
                        Some(s) if s.inner.agent_run_abort_requested.load(Ordering::SeqCst) => {
                            Some("Session run cancelled".to_owned())
                        }
                        Some(s) => s.ensure_external_open().err(),
                        None => Some("Session has been dropped".to_owned()),
                    };
                    if let Some(err) = failure {
                        let stream = pi_ai::create_assistant_message_event_stream();
                        let mut message = AssistantMessage::new_pending(&model);
                        message.stop_reason = StopReason::Aborted;
                        message.error_message = Some(err);
                        stream.push(pi_ai::AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: message.clone(),
                        });
                        stream.end(Some(message));
                        return stream;
                    }
                    let session = session.expect("checked session");
                    if session.is_block_images_enabled() {
                        transcript = pi_ai::normalize_context(&pi_ai::Context {
                            system_prompt: None,
                            messages: filter_blocked_images(transcript.messages().to_vec()),
                            tools: None,
                        });
                    }
                    let runner = session.extension_runner();
                    let token = options.base.cancel_token.get_or_insert_default().clone();
                    let header_token = token.clone();
                    let header_session = session.downgrade();
                    let previous = options.base.on_headers.take();
                    options.base.on_headers = Some(Arc::new(move |headers, model| {
                        let runner = runner.clone();
                        let previous = previous.clone();
                        let token = header_token.clone();
                        let session = header_session.upgrade();
                        let id = session
                            .as_ref()
                            .map_or(0, |s| Arc::as_ptr(&s.inner) as usize);
                        Box::pin(RUN_SESSION.scope(id, async move {
                            let headers = match previous {
                                Some(cb) => {
                                    let changed = cb(headers.clone(), model).await;
                                    pi_ai::api::normalize_header_changes(&headers, changed)
                                }
                                None => headers,
                            };
                            let headers = runner.emit_before_provider_headers(headers).await;
                            if runner.runtime().check_active().is_err()
                                || session.is_none_or(|s| s.ensure_runtime_writable().is_err())
                            {
                                token.cancel();
                            }
                            headers
                        }))
                    }));
                    let previous_payload = options.base.on_payload.take();
                    let payload_session = session.downgrade();
                    let payload_runner = session.extension_runner();
                    options.base.on_payload = Some(Arc::new(move |payload, model| {
                        let previous = previous_payload.clone();
                        let token = token.clone();
                        let session = payload_session.upgrade();
                        let runner = payload_runner.clone();
                        let id = session
                            .as_ref()
                            .map_or(0, |s| Arc::as_ptr(&s.inner) as usize);
                        Box::pin(RUN_SESSION.scope(id, async move {
                            let result = match previous {
                                Some(cb) => cb(payload, model).await,
                                None => None,
                            };
                            if runner.runtime().check_active().is_err()
                                || session.is_none_or(|s| s.ensure_runtime_writable().is_err())
                            {
                                token.cancel();
                            }
                            result
                        }))
                    }));

                    let sid = session.session_id();
                    if options.base.session_id.as_deref() == Some(sid.as_str())
                        && let Some(obs) = session
                            .inner
                            .on_resolved_session_request
                            .read()
                            .expect("resolved req lock")
                            .clone()
                    {
                        let req_seq = session.inner.request_seq.fetch_add(1, Ordering::SeqCst);
                        obs(ResolvedPhysicalRequest {
                            session_id: sid,
                            session_generation: session.session_generation(),
                            selection_generation: session.selection_generation(),
                            request_seq: req_seq,
                            selected_model: session.model(),
                            physical_model: model.clone(),
                            transcript: transcript.clone(),
                            options: options.clone(),
                        });
                    }

                    session.in_callback(raw(model, transcript, options)).await
                })
            }));

        let s_transform = w.clone();
        self.inner
            .agent
            .set_transform_context(Some(Arc::new(move |messages, _cancel| {
                let s_opt = s_transform.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_transform_context(messages)).await,
                        None => messages,
                    }
                })
            })));

        let s_before = w.clone();
        self.inner
            .agent
            .set_before_tool_call(Some(Arc::new(move |ctx, cancel| {
                let s_opt = s_before.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => {
                            s.in_callback(s.handle_before_tool_call(ctx, None, cancel))
                                .await
                        }
                        None => Ok(None),
                    }
                })
            })));

        let s_after = w.clone();
        self.inner
            .agent
            .set_after_tool_call(Some(Arc::new(move |ctx, cancel| {
                let s_opt = s_after.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => {
                            s.in_callback(s.handle_after_tool_call(ctx, None, cancel))
                                .await
                        }
                        None => Ok(None),
                    }
                })
            })));

        let s_finalize = w.clone();
        self.inner
            .agent
            .set_finalize_message(Some(Arc::new(move |msg, cancel| {
                let s_opt = s_finalize.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_finalize_message(msg, cancel)).await,
                        None => Ok(msg),
                    }
                })
            })));

        let s_prep_req = w.clone();
        self.inner
            .agent
            .set_prepare_request(Some(Arc::new(move |req_ctx, cancel| {
                let s_opt = s_prep_req.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => {
                            s.in_callback(s.handle_prepare_request(req_ctx, cancel))
                                .await
                        }
                        None => None,
                    }
                })
            })));

        let s_prep_next = w.clone();
        self.inner
            .agent
            .set_prepare_next_turn(Some(Arc::new(move |turn_ctx| {
                let s_opt = s_prep_next.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_prepare_next_turn(turn_ctx)).await,
                        None => None,
                    }
                })
            })));

        let s_finish = w.clone();
        self.inner
            .agent
            .set_finish_turn(Some(Arc::new(move |turn_ctx, cancel| {
                let s_opt = s_finish.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => s.in_callback(s.handle_finish_turn(turn_ctx, cancel)).await,
                        None => None,
                    }
                })
            })));

        let s_payload = w.clone();
        self.inner
            .agent
            .set_on_payload(Some(Arc::new(move |payload, _model| {
                let s_opt = s_payload.upgrade();
                Box::pin(async move {
                    match s_opt {
                        Some(s) => Some(
                            s.in_callback(
                                s.extension_runner().emit_before_provider_request(payload),
                            )
                            .await,
                        ),
                        None => Some(payload),
                    }
                })
            })));

        let s_resp = w.clone();
        self.inner
            .agent
            .set_on_response(Some(Arc::new(move |resp, _model| {
                let s_opt = s_resp.upgrade();
                Box::pin(async move {
                    if let Some(s) = s_opt {
                        s.in_callback(s.extension_runner().emit_after_provider_response(
                            AfterProviderResponseEvent {
                                status: resp.status,
                                headers: resp.headers,
                            },
                        ))
                        .await;
                    }
                })
            })));

        let s_stream_ev = w.clone();
        self.inner
            .agent
            .set_on_provider_stream_event(Some(Arc::new(move |ev, _model| {
                let s_opt = s_stream_ev.upgrade();
                Box::pin(async move {
                    if let Some(s) = s_opt {
                        s.in_callback(s.extension_runner().emit_provider_stream_event(ev))
                            .await;
                    }
                })
            })));

        let s_sub = w;
        let _ = self.inner.agent.subscribe(Arc::new(move |ev, _cancel| {
            let s_opt = s_sub.upgrade();
            Box::pin(async move {
                if let Some(s) = s_opt {
                    s.in_callback(s.handle_agent_event(ev)).await;
                }
            })
        }));
    }

    async fn handle_transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let mut transformed = self.extension_runner().emit_context(messages).await;

        let hidden = self
            .inner
            .hidden_declarations
            .read()
            .expect("hidden lock")
            .clone();
        if !hidden.is_empty() {
            transformed = transformed
                .into_iter()
                .map(|msg| match msg {
                    AgentMessage::System(mut sys) => {
                        if let Some(added) = &mut sys.tools_added {
                            added.retain(|t| !hidden.contains(&t.name));
                            if added.is_empty() {
                                sys.tools_added = None;
                            }
                        }
                        if let Some(removed) = &mut sys.tools_removed {
                            removed.retain(|t| !hidden.contains(&t.name));
                            if removed.is_empty() {
                                sys.tools_removed = None;
                            }
                        }
                        AgentMessage::System(sys)
                    }
                    other => other,
                })
                .collect();
        }

        let forced = self
            .inner
            .run_system_prompt_options
            .read()
            .expect("lock")
            .as_ref()
            .and_then(|o| o.force_system_prompt.clone());
        if let Some(forced_text) = forced {
            let current = get_agent_current_system_message(&transformed);
            let head = SystemMessage {
                content: SystemContent::Text(forced_text),
                sections: None,
                tools_added: current.as_ref().and_then(|c| c.tools_added.clone()),
                tools_removed: None,
                timestamp: current.map_or_else(now_ms, |c| c.timestamp),
            };
            let mut out = vec![AgentMessage::System(head)];
            out.extend(transformed.into_iter().filter(|m| m.role() != "system"));
            return out;
        }

        transformed
    }

    async fn handle_before_tool_call(
        &self,
        ctx: &mut BeforeToolCallContext,
        parent_tool_call_id: Option<String>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<Option<BeforeToolCallResult>, String> {
        if self.is_closing_or_closed() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(SESSION_CLOSED_ERROR.to_owned()),
                terminate: Some(true),
                args: None,
            }));
        }
        if let Some(fatal) = self.fatal_runtime_error() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(fatal),
                terminate: Some(true),
                args: None,
            }));
        }

        let runner = self.extension_runner();
        let ext_res = runner
            .emit_tool_call(ToolCallEvent {
                tool_call_id: ctx.tool_call.id.clone(),
                tool_name: ctx.tool_call.name.clone(),
                input: ctx.args.clone(),
                parent_tool_call_id: parent_tool_call_id.clone(),
            })
            .await?;

        if let Some(r) = ext_res
            && r.block
        {
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: r.reason,
                terminate: None,
                args: None,
            }));
        }

        let _admission = self
            .inner
            .operation_admission
            .lock()
            .expect("admission lock");
        if self.is_closing_or_closed() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(SESSION_CLOSED_ERROR.to_owned()),
                terminate: Some(true),
                args: None,
            }));
        }
        if let Some(err) = self.fatal_runtime_error() {
            return Err(err);
        }
        if self.with_session_manager(SessionManager::is_poisoned) {
            let err = "Session storage is poisoned; refusing tool execution".to_owned();
            self.set_fatal_runtime_error(err.clone());
            return Err(err);
        }

        // 工具执行前可失败地将 InFlightToolRecord 写入状态日志（默认 ToolReplayPolicy::Never）
        let replay_policy = self
            .inner
            .tool_definitions
            .read()
            .expect("defs lock")
            .get(&ctx.tool_call.name)
            .map_or(ToolReplayPolicy::Never, |(d, _)| d.replay_policy);

        let record = InFlightToolRecord {
            tool_call_id: ctx.tool_call.id.clone(),
            parent_tool_call_id,
            tool_name: ctx.tool_call.name.clone(),
            replay_policy,
            started_at_ms: now_ms(),
        };

        if let Err(err) = self.with_execution_log_internal(|log| log.record_tool_start(record)) {
            let err_msg = format!("Failed to persist in-flight tool state before execution: {err}");
            self.set_fatal_runtime_error(err_msg.clone());
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(err_msg),
                terminate: Some(true),
                args: None,
            }));
        }

        Ok(None)
    }

    async fn handle_after_tool_call(
        &self,
        ctx: AfterToolCallContext,
        parent_tool_call_id: Option<String>,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<Option<AfterToolCallResult>, String> {
        let orig_content = ctx.result.content.clone();
        let orig_structured = ctx.result.structured_content.clone();
        let runner = self.extension_runner();
        let hook_res = runner
            .emit_tool_result(ToolResultEvent {
                tool_call_id: ctx.tool_call.id,
                tool_name: ctx.tool_call.name,
                input: ctx.args,
                content: ctx.result.content,
                details: Some(ctx.result.details),
                structured_content: ctx.result.structured_content,
                is_error: ctx.is_error,
                usage: ctx.result.usage,
                parent_tool_call_id,
            })
            .await;

        let current_content = hook_res
            .as_ref()
            .and_then(|r| r.content.clone())
            .unwrap_or_else(|| orig_content.clone());

        let normalize_fn = self
            .inner
            .normalize_tool_result_content
            .read()
            .expect("normalize_tool_result_content lock")
            .clone();
        let normalized_content = match normalize_fn {
            Some(cb) => cb(current_content.clone(), self.model()).await,
            None => current_content.clone(),
        };

        if hook_res.is_none() && normalized_content == current_content {
            return Ok(None);
        }

        Ok(Some(AfterToolCallResult {
            content: Some(normalized_content),
            details: hook_res.as_ref().and_then(|r| r.details.clone()),
            structured_content: if let Some(ref hr) = hook_res {
                hr.structured_content.clone()
            } else {
                orig_structured
            },
            is_error: hook_res.as_ref().and_then(|r| r.is_error),
            usage: hook_res.and_then(|r| r.usage),
            terminate: None,
        }))
    }

    async fn handle_finalize_message(
        &self,
        mut message: AgentMessage,
        _cancel_token: Option<CancellationToken>,
    ) -> Result<AgentMessage, String> {
        if let Some(fatal) = self.fatal_runtime_error() {
            return Err(fatal);
        }

        let input_correlation = if message.role() == "user" {
            self.inner
                .pending_input_correlations
                .lock()
                .expect("input correlations lock")
                .remove(&message.timestamp())
        } else {
            None
        };
        let admission_cb = if message.role() == "user" {
            self.inner
                .pending_admission_callbacks
                .lock()
                .expect("admission lock")
                .remove(&message.timestamp())
        } else {
            None
        };

        // 若是父工具结果消息：在进入 message_end 处理器及发出 message_start 前合入 nested_calls 与汇总 usage
        let orig_tool_call_id = if let AgentMessage::ToolResult(tr) = &mut message {
            let tc_id = tr.tool_call_id.clone();
            if let Some(nested_runner) = self.inner.nested_runner.get()
                && let Some(summary) = nested_runner.take_record(&tc_id)
            {
                if summary.calls.is_some() {
                    tr.nested_calls = summary.calls;
                }
                if let Some(nested_usage) = summary.usage {
                    tr.usage = Some(match &tr.usage {
                        Some(existing) => combine_usage(existing, &nested_usage),
                        None => nested_usage,
                    });
                }
            }
            Some((tc_id, tr.nested_calls.clone()))
        } else {
            None
        };

        // 触发扩展 message_end 钩子，允许链式返回同角色新消息
        if let Some(replaced) = self
            .extension_runner()
            .emit_message_end(message.clone())
            .await
        {
            message = replaced;
            if let (Some((tc_id, merged_nested)), AgentMessage::ToolResult(tr)) =
                (orig_tool_call_id, &mut message)
            {
                tr.tool_call_id = tc_id;
                if tr.nested_calls.is_none() {
                    tr.nested_calls = merged_nested;
                }
            }
        }

        if let Some(policy) = &self.inner.data_policy {
            let role = message.role().to_owned();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                policy.persisted_message(message)
            }));
            message = match result {
                Ok(Ok(projected)) if projected.role() == role => projected,
                _ => {
                    let error = "Session message projection failed".to_owned();
                    if let Some(cb) = &admission_cb {
                        cb(Err(error.clone()));
                    }
                    self.set_fatal_runtime_error(error.clone());
                    return Err(error);
                }
            };
        }

        let queue_item_id = input_correlation.as_ref().and_then(|(client_id, _)| {
            self.with_execution_log(|log| {
                log.get_input(client_id)
                    .map(|record| record.queue_item_id.clone())
            })
        });
        // 将最终消息持久化到 SessionManager；失败时设置会话级终止标记并停止运行
        let persist_res: Result<String, SessionStoreError> =
            self.with_session_manager_internal(|sm| {
                if let Some((client_id, _)) = &input_correlation {
                    sm.append_queued_user_message(
                        message.clone(),
                        client_id,
                        queue_item_id.as_deref(),
                    )
                } else if let Some(cm) = CustomMessage::from_agent_message(&message) {
                    sm.append_custom_message_entry(
                        cm.custom_type,
                        cm.content,
                        cm.display,
                        cm.details,
                    )
                } else {
                    sm.append_message(message.clone())
                }
            });

        let entry_id = match persist_res {
            Ok(id) => id,
            Err(err) => {
                let err_msg = format!("Session persistence failed: {err}");
                if let Some(cb) = &admission_cb {
                    cb(Err(err_msg.clone()));
                }
                self.set_fatal_runtime_error(err_msg.clone());
                return Err(err_msg);
            }
        };

        if let Some((client_id, disposition)) = input_correlation
            && let Err(err) = self.accept_input(&client_id, disposition, Some(entry_id.clone()))
        {
            if let Some(cb) = &admission_cb {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        if let Some(cb) = admission_cb {
            cb(Ok(PromptDisposition::Started));
        }

        self.inner
            .entry_ids_by_timestamp_and_role
            .lock()
            .expect("entry map lock")
            .push((
                message.role().to_owned(),
                message.timestamp(),
                entry_id.clone(),
            ));

        if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&entry_id).cloned()) {
            self.emit_async(AgentSessionEvent::EntryAppended { entry })
                .await;
        }

        // 若是 ToolResultMessage：仅在最终 ToolResultMessage 已成功写入并同步后，才清理状态日志中的未决记录
        if let AgentMessage::ToolResult(tr) = &message
            && let Err(err) =
                self.with_execution_log_internal(|log| log.clear_persisted_tool_result(tr))
        {
            let err_msg =
                format!("Failed to clear in-flight tool state after persisting ToolResult: {err}");
            self.set_fatal_runtime_error(err_msg.clone());
            return Err(err_msg);
        }

        if let AgentMessage::Assistant(a) = &message {
            *self
                .inner
                .last_assistant_message
                .lock()
                .expect("last assistant lock") = Some((a.clone(), Some(entry_id)));

            if !matches!(a.stop_reason, StopReason::Error | StopReason::Length) {
                self.inner
                    .overflow_recovery_attempted
                    .store(false, Ordering::SeqCst);
            }

            if a.stop_reason != StopReason::Error {
                let prev_attempt = self.inner.retry_attempt.swap(0, Ordering::SeqCst);
                if prev_attempt > 0 {
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt: prev_attempt as u32,
                        final_error: None,
                    });
                }
            }
        }

        Ok(message)
    }

    async fn handle_prepare_request(
        &self,
        req_ctx: PrepareRequestContext,
        cancel_token: Option<CancellationToken>,
    ) -> Option<AgentRequestUpdate> {
        if self.fatal_runtime_error().is_some() {
            if let Some(t) = &cancel_token {
                t.cancel();
            }
            return None;
        }

        let failed = self.inner.failed_response.lock().expect("lock").take();
        let projection = self.with_session_manager(SessionManager::build_session_projection);
        let mut canonical_context = AgentContext {
            messages: projection.messages.clone(),
            tools: self.inner.agent.state().tools,
        };
        let model = self.model();
        let thinking_level = self.thinking_level();

        if !is_virtual_model(&model) {
            let _ = req_ctx;
            return Some(AgentRequestUpdate {
                context: Some(canonical_context),
                model: Some(model),
                thinking_level: Some(thinking_level),
            });
        }

        // 虚拟模型路由（pi-virtual）
        let vdef = self
            .extension_runner()
            .get_registered_virtual_models()
            .into_iter()
            .find(|def| def.provider == model.provider && def.id == model.id);
        let Some(vdef) = vdef else {
            return Some(AgentRequestUpdate {
                context: Some(canonical_context),
                model: Some(model),
                thinking_level: Some(thinking_level),
            });
        };

        let last_asst_idx = canonical_context
            .messages
            .iter()
            .rposition(|m| m.role() == "assistant");
        let tail_slice = match last_asst_idx {
            Some(idx) => &canonical_context.messages[idx + 1..],
            None => &canonical_context.messages[..],
        };
        let is_user_turn = tail_slice.iter().any(|m| m.role() == "user");
        let reason = if failed.is_some() {
            ModelRouteReason::Retry
        } else if is_user_turn {
            ModelRouteReason::User
        } else {
            ModelRouteReason::Continuation
        };

        let previous = find_latest_response(&canonical_context.messages).and_then(|resp| {
            self.inner
                .models
                .get_model(&resp.provider, &resp.model)
                .map(|m| PreviousModelRoute {
                    model: m,
                    thinking_level: resp.thinking_level,
                })
        });

        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        let prev_state = get_virtual_model_state(&branch, &model.provider, &model.id);

        let route_req = ModelRouteRequest {
            model: model.clone(),
            thinking_level,
            reason,
            previous,
            failed: failed.map(|(fm, fl, fmsg)| FailedModelRoute {
                model: fm,
                thinking_level: Some(fl),
                message: fmsg,
            }),
            state: prev_state.clone(),
            messages: convert_to_llm_with_options(
                &canonical_context.messages,
                self.is_block_images_enabled(),
            ),
            cancel_token,
        };

        let Ok(route) = (vdef.route)(route_req).await else {
            return Some(AgentRequestUpdate {
                context: Some(canonical_context),
                model: Some(model),
                thinking_level: Some(thinking_level),
            });
        };

        if route.state.is_some() && route.state != prev_state {
            let state_data = VirtualModelStateData {
                provider: model.provider.clone(),
                model_id: model.id.clone(),
                state: route.state,
            };
            let val = serde_json::to_value(state_data).expect("virtual state serialization");
            match self.with_session_manager_internal(|sm| {
                sm.append_custom_entry(VIRTUAL_MODEL_STATE_ENTRY, Some(val))
            }) {
                Ok(entry_id) => {
                    if let Some(entry) =
                        self.with_session_manager(|sm| sm.get_entry(&entry_id).cloned())
                    {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                Err(err) => {
                    self.set_fatal_runtime_error(format!(
                        "Failed to persist virtual model routing state: {err}"
                    ));
                    return None;
                }
            }
        }

        if self.exceeds_compaction_threshold(&route.model, &projection) {
            let _ = self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await;
            let refreshed = self.with_session_manager(SessionManager::build_session_projection);
            canonical_context.messages = refreshed.messages;
        }

        Some(AgentRequestUpdate {
            context: Some(canonical_context),
            model: Some(route.model),
            thinking_level: Some(route.thinking_level),
        })
    }

    async fn handle_prepare_next_turn(
        &self,
        turn_ctx: PrepareNextTurnContext,
    ) -> Option<AgentLoopTurnUpdate> {
        let mut next_context = turn_ctx.context;
        let projection = self.with_session_manager(SessionManager::build_session_projection);
        let model = self.model();
        if !is_virtual_model(&model) && self.exceeds_compaction_threshold(&model, &projection) {
            let _ = self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await;
            next_context.messages =
                self.with_session_manager(|sm| sm.build_session_projection().messages);
        } else {
            next_context.messages = projection.messages;
        }

        let mut opts = self
            .inner
            .run_system_prompt_options
            .read()
            .expect("lock")
            .clone()
            .unwrap_or_else(|| {
                self.inner
                    .base_system_prompt_options
                    .read()
                    .expect("lock")
                    .clone()
            });
        opts.selected_tools = self.get_active_tool_names();
        let update_msg = self.prepare_prompt_and_tool_loadout(&mut opts, &next_context.messages);
        *self.inner.run_system_prompt_options.write().expect("lock") = Some(opts);

        next_context.tools = self.inner.agent.state().tools;
        Some(AgentLoopTurnUpdate {
            context: Some(next_context),
            messages: update_msg.map(|sys| vec![AgentMessage::System(sys)]),
            model: Some(self.model()),
            thinking_level: Some(self.thinking_level()),
        })
    }

    async fn handle_finish_turn(
        &self,
        turn_ctx: pi_agent::AgentTurnContext,
        _cancel_token: Option<CancellationToken>,
    ) -> Option<AgentTurnDecision> {
        if self.fatal_runtime_error().is_some() {
            return Some(AgentTurnDecision::End);
        }

        // 记录轮次精确耗时（cst-web-work-duration）
        let started_ms = self.inner.turn_started_at_ms.load(Ordering::SeqCst);
        let ended_ms = now_ms();
        if started_ms > 0 {
            let duration_ms = ended_ms.saturating_sub(started_ms);
            let _ = self.with_session_manager_internal(|sm| {
                sm.append_custom_entry(
                    WORK_DURATION_CUSTOM_TYPE,
                    Some(json!({
                        "turnIndex": self.inner.turn_index.load(Ordering::SeqCst),
                        "startedAtMs": started_ms,
                        "endedAtMs": ended_ms,
                        "durationMs": duration_ms,
                    })),
                )
            });
        }

        let turn_idx = self.inner.turn_index.load(Ordering::SeqCst) as usize;
        let boundary = self
            .extension_runner()
            .emit_boundary(
                BoundaryEventKind::TurnEnd {
                    turn_index: turn_idx,
                    message: AgentMessage::Assistant(turn_ctx.message),
                    tool_results: turn_ctx.tool_results,
                },
                |drafts| self.build_boundary_context_preview(drafts, true),
            )
            .await;

        if !boundary.entries.is_empty() {
            match self
                .with_session_manager_internal(|sm| sm.commit_boundary_drafts(&boundary.entries))
            {
                Ok(appended) => {
                    self.refresh_finalized_context();
                    for entry in appended {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                Err(err) => {
                    self.set_fatal_runtime_error(format!(
                        "Failed to commit turn_end boundary drafts: {err}"
                    ));
                    return Some(AgentTurnDecision::End);
                }
            }
        }

        self.flush_pending_custom_messages();

        if boundary.r#continue {
            match self.build_boundary_context_preview(&[], true) {
                Ok(preview) if preview.can_continue => return Some(AgentTurnDecision::Continue),
                _ => {
                    self.extension_runner().emit_error(ExtensionError {
                        extension_path: "<boundary>".to_owned(),
                        event: "turn_end".to_owned(),
                        error: "turn_end requested continuation without runnable model context"
                            .to_owned(),
                    });
                }
            }
        }

        None
    }

    fn build_boundary_context_preview(
        &self,
        drafts: &[SessionBoundaryDraft],
        is_turn_end: bool,
    ) -> Result<BoundaryContextPreview, String> {
        let mut preview_sm = self.with_session_manager(SessionManager::clone_in_memory_preview);
        preview_sm
            .commit_boundary_drafts(drafts)
            .map_err(|e| e.to_string())?;
        let projection = preview_sm.build_session_projection();
        let branch = preview_sm.get_branch(None);
        let estimated_tokens = estimate_projected_context_tokens(&projection, &branch).tokens;
        let llm_messages = convert_to_llm(&projection.messages);
        let final_role = llm_messages.last().map(|m| match m {
            pi_ai::Message::System(_) => "system",
            pi_ai::Message::User(_) => "user",
            pi_ai::Message::Assistant(_) => "assistant",
            pi_ai::Message::ToolResult(_) => "toolResult",
        });
        let has_non_system = llm_messages
            .iter()
            .any(|m| !matches!(m, pi_ai::Message::System(_)));
        let context_can_continue = has_non_system && final_role != Some("assistant");
        let has_pending_custom = !self
            .inner
            .pending_custom_messages
            .lock()
            .expect("lock")
            .is_empty();
        let has_queued = self.inner.agent.has_queued_messages();

        let can_continue = context_can_continue
            || has_pending_custom
            || if is_turn_end {
                has_queued
            } else {
                final_role == Some("assistant") && has_queued
            };

        Ok(BoundaryContextPreview {
            messages: projection.messages,
            estimated_tokens,
            can_continue,
        })
    }

    async fn handle_agent_event(&self, event: AgentEvent) {
        match &event {
            AgentEvent::AgentStart => {
                if self.inner.agent_run_abort_requested.load(Ordering::SeqCst) {
                    self.inner.agent.abort();
                }
                self.inner.turn_index.store(0, Ordering::SeqCst);
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::AgentStart)
                    .await;
                self.emit_async(AgentSessionEvent::AgentStart).await;
            }
            AgentEvent::TurnStart => {
                let now = now_ms();
                self.inner.turn_started_at_ms.store(now, Ordering::SeqCst);
                let idx = self.inner.turn_index.load(Ordering::SeqCst) as usize;
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::TurnStart {
                        turn_index: idx,
                        timestamp: now,
                    })
                    .await;
                self.emit_async(AgentSessionEvent::TurnStart).await;
            }
            AgentEvent::MessageStart { message } => {
                if let AgentMessage::User(u) = message {
                    self.inner
                        .overflow_recovery_attempted
                        .store(false, Ordering::SeqCst);
                    let text = match &u.content {
                        UserContent::Text(s) => s.clone(),
                        UserContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| match b {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(""),
                    };
                    if !text.is_empty() {
                        let mut removed = false;
                        {
                            let mut steer = self.inner.steering_messages.lock().expect("lock");
                            if let Some(pos) = steer.iter().position(|s| s == &text) {
                                steer.remove(pos);
                                removed = true;
                            }
                        }
                        if !removed {
                            let mut follow = self.inner.follow_up_messages.lock().expect("lock");
                            if let Some(pos) = follow.iter().position(|s| s == &text) {
                                follow.remove(pos);
                                removed = true;
                            }
                        }
                        if removed {
                            self.emit_queue_update();
                        }
                    }
                }
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::MessageStart {
                        message: message.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::MessageStart {
                    message: message.clone(),
                })
                .await;
            }
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::MessageUpdate {
                        message: message.clone(),
                        assistant_message_event: assistant_message_event.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::MessageUpdate {
                    message: message.clone(),
                    assistant_message_event: assistant_message_event.clone(),
                })
                .await;
            }
            AgentEvent::MessageEnd { message } => {
                self.emit_async(AgentSessionEvent::MessageEnd {
                    message: message.clone(),
                })
                .await;
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::ToolExecutionStart {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        parent_tool_call_id: parent_tool_call_id.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::ToolExecutionStart {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    parent_tool_call_id: parent_tool_call_id.clone(),
                })
                .await;
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
                parent_tool_call_id,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::ToolExecutionUpdate {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        partial_result: partial_result.clone(),
                        parent_tool_call_id: parent_tool_call_id.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::ToolExecutionUpdate {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    partial_result: partial_result.clone(),
                    parent_tool_call_id: parent_tool_call_id.clone(),
                })
                .await;
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                duration_ms,
                parent_tool_call_id,
            } => {
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::ToolExecutionEnd {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: result.clone(),
                        is_error: *is_error,
                        duration_ms: *duration_ms,
                        parent_tool_call_id: parent_tool_call_id.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::ToolExecutionEnd {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    result: result.clone(),
                    is_error: *is_error,
                    duration_ms: *duration_ms,
                    parent_tool_call_id: parent_tool_call_id.clone(),
                })
                .await;
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                self.inner.turn_index.fetch_add(1, Ordering::SeqCst);
                self.emit_async(AgentSessionEvent::TurnEnd {
                    message: message.clone(),
                    tool_results: tool_results.clone(),
                })
                .await;
            }
            AgentEvent::AgentEnd { messages } => {
                if let Some(nested) = self.inner.nested_runner.get() {
                    nested.clear();
                }
                let will_retry = self.will_retry_after_agent_end(messages);
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::AgentEnd {
                        messages: messages.clone(),
                    })
                    .await;
                self.emit_async(AgentSessionEvent::AgentEnd {
                    messages: messages.clone(),
                    will_retry,
                })
                .await;
            }
        }
    }

    fn will_retry_after_agent_end(&self, messages: &[AgentMessage]) -> bool {
        if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            || self.fatal_runtime_error().is_some()
        {
            return false;
        }
        let policy = self.retry_policy();
        let current_attempt = self.inner.retry_attempt.load(Ordering::SeqCst) as u32;
        if !policy.enabled || current_attempt >= policy.max_retries {
            return false;
        }
        for msg in messages.iter().rev() {
            if let AgentMessage::Assistant(a) = msg {
                return self.is_retryable_error(a);
            }
        }
        false
    }

    pub async fn execute_nested_tool_call(
        &self,
        caller_id: &str,
        name: &str,
        args: JsonValue,
        options: NestedToolCallOptions,
    ) -> AgentToolCallOutcome {
        let runner = self
            .inner
            .nested_runner
            .get()
            .expect("nested_runner initialized");
        runner.execute(caller_id, name, args, options).await
    }

    // =========================================================================
    // Prompting, Queues, Custom Messages & Bash Execution
    // =========================================================================

    pub async fn prompt(
        &self,
        text: impl Into<String>,
        options: Option<PromptOptions>,
    ) -> Result<PromptDisposition, String> {
        let text = text.into();
        let opts = options.unwrap_or_default();

        if let Err(err) = self.ensure_external_open() {
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        // 若正在广播 agent_settled，推入延迟队列待广播结束后再执行
        if self.inner.is_emitting_agent_settled.load(Ordering::SeqCst) {
            let s = self.clone();
            let t = text;
            let o = opts;
            self.inner
                .deferred_settled_actions
                .lock()
                .expect("deferred lock")
                .push(Box::new(move || {
                    Box::pin(async move {
                        let _ = s.prompt_after_settled(t, o).await;
                    })
                }));
            return Ok(PromptDisposition::Started);
        }

        self.prompt_after_settled(text, opts).await
    }

    fn accept_input(
        &self,
        client_id: &str,
        disposition: PromptDisposition,
        entry_id: Option<String>,
    ) -> Result<(), String> {
        let result = self.with_session_manager_mut(|sm| {
            self.with_execution_log_internal(|log| {
                log.mark_input_accepted(sm, client_id, disposition, entry_id)
            })
        });
        result.map(|_| ()).map_err(|err| {
            let msg = format!("Failed to persist input acceptance: {err}");
            self.set_fatal_runtime_error(msg.clone());
            msg
        })
    }

    async fn prompt_after_settled(
        &self,
        text: String,
        opts: PromptOptions,
    ) -> Result<PromptDisposition, String> {
        if let Err(err) = self.ensure_external_open() {
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        let _input_lease = match &self.inner.data_policy {
            Some(policy) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                policy.admit_input(&text, opts.expand_prompt_templates.unwrap_or(true))
            })) {
                Ok(Ok(lease)) => lease,
                _ => {
                    let error = "Session input admission declined".to_owned();
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(error.clone()));
                    }
                    return Err(error);
                }
            },
            None => None,
        };

        // 幂等 client_message_id 预检与去重
        if let Some(client_id) = opts.client_message_id.as_deref() {
            let persisted_text = match &self.inner.data_policy {
                Some(policy) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    policy.persisted_input(&text)
                })) {
                    Ok(Ok(text)) => text,
                    _ => {
                        let error = "Session input projection failed".to_owned();
                        if let Some(cb) = &opts.on_admitted {
                            cb(Err(error.clone()));
                        }
                        self.set_fatal_runtime_error(error.clone());
                        return Err(error);
                    }
                },
                None => text.clone(),
            };
            let (record, is_dup) = self
                .with_execution_log_mut(|log| {
                    log.begin_input_delivery(
                        client_id,
                        opts.queue_item_id.as_deref(),
                        &persisted_text,
                        opts.images.clone().unwrap_or_default(),
                    )
                })
                .map_err(|e| {
                    let msg = e.to_string();
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(msg.clone()));
                    }
                    msg
                })?;

            if is_dup {
                let res = match record.status {
                    InputDeliveryStatus::Accepted { disposition, .. } => Ok(disposition),
                    InputDeliveryStatus::Delivering | InputDeliveryStatus::Queued => {
                        Ok(PromptDisposition::Started)
                    }
                    InputDeliveryStatus::Failed { error } => Err(error),
                    InputDeliveryStatus::Uncertain { reason } => Err(reason),
                };
                if let Some(cb) = &opts.on_admitted {
                    cb(res.clone());
                }
                return res;
            }
        }

        let expand_templates = opts.expand_prompt_templates.unwrap_or(true);
        if expand_templates
            && text.starts_with('/')
            && self.try_execute_extension_command(&text).await?
        {
            if let Some(client_id) = opts.client_message_id.as_deref()
                && let Err(err) =
                    self.accept_input(client_id, PromptDisposition::HandledByExtension, None)
            {
                if let Some(cb) = &opts.on_admitted {
                    cb(Err(err.clone()));
                }
                return Err(err);
            }
            if let Some(cb) = &opts.on_admitted {
                cb(Ok(PromptDisposition::HandledByExtension));
            }
            return Ok(PromptDisposition::HandledByExtension);
        }

        if self.is_compacting() {
            let err =
                "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
                    .to_owned();
            if let Some(client_id) = opts.client_message_id.as_deref() {
                let _ =
                    self.with_execution_log_internal(|log| log.mark_input_failed(client_id, &err));
            }
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        let input_res = self
            .emit_input_cancellable(
                text.clone(),
                opts.images.clone(),
                opts.source.unwrap_or(InputSource::Interactive),
                self.is_streaming()
                    .then_some(opts.streaming_behavior)
                    .flatten(),
            )
            .await;
        let input_res =
            match input_res.and_then(|result| self.ensure_external_open().map(|()| result)) {
                Ok(result) => result,
                Err(error) => {
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(error.clone()));
                    }
                    return Err(error);
                }
            };

        let (mut current_text, current_images) = match input_res {
            InputEventResult::Handled => {
                if let Some(client_id) = opts.client_message_id.as_deref()
                    && let Err(err) =
                        self.accept_input(client_id, PromptDisposition::HandledByExtension, None)
                {
                    if let Some(cb) = &opts.on_admitted {
                        cb(Err(err.clone()));
                    }
                    return Err(err);
                }
                if let Some(cb) = &opts.on_admitted {
                    cb(Ok(PromptDisposition::HandledByExtension));
                }
                return Ok(PromptDisposition::HandledByExtension);
            }
            InputEventResult::Transform { text: t, images: i } => (t, i.or(opts.images)),
            InputEventResult::Continue => (text, opts.images),
        };

        if expand_templates {
            current_text = self.expand_skill_command(&current_text);
            let templates = self.prompt_templates();
            if !templates.is_empty() {
                current_text = expand_prompt_template_text(&current_text, &templates);
            }
        }

        if self.is_streaming() {
            let Some(behavior) = opts.streaming_behavior else {
                let err = "Agent is already processing. Specify streaming_behavior ('steer' or 'followUp') to queue the message.".to_owned();
                if let Some(client_id) = opts.client_message_id.as_deref() {
                    let _ = self
                        .with_execution_log_internal(|log| log.mark_input_failed(client_id, &err));
                }
                if let Some(cb) = &opts.on_admitted {
                    cb(Err(err.clone()));
                }
                return Err(err);
            };
            let disposition = match behavior {
                StreamingInputBehavior::Steer => {
                    self.queue_steer_correlated(
                        current_text,
                        current_images,
                        opts.client_message_id.clone(),
                    );
                    PromptDisposition::Steered
                }
                StreamingInputBehavior::FollowUp => {
                    self.queue_follow_up_correlated(
                        current_text,
                        current_images,
                        opts.client_message_id.clone(),
                    );
                    PromptDisposition::FollowedUp
                }
            };
            if let Some(cb) = &opts.on_admitted {
                cb(Ok(disposition));
            }
            // Keep Delivering until the queued message and its correlation are committed.
            return Ok(disposition);
        }

        if !self.has_configured_model() {
            let err = self.no_models_error_message();
            if let Some(client_id) = opts.client_message_id.as_deref() {
                let _ =
                    self.with_execution_log_internal(|log| log.mark_input_failed(client_id, &err));
            }
            if let Some(cb) = &opts.on_admitted {
                cb(Err(err.clone()));
            }
            return Err(err);
        }

        self.start_run_with_admission(
            BeforeAgentStartTrigger::User {
                prompt: current_text,
                images: current_images,
            },
            opts.client_message_id,
            opts.on_admitted,
        )
        .await?;

        Ok(PromptDisposition::Started)
    }

    async fn emit_input_cancellable(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: InputSource,
        behavior: Option<StreamingInputBehavior>,
    ) -> Result<InputEventResult, String> {
        let cancel = CancellationToken::new();
        let id = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            let id = self
                .inner
                .next_input_hook_id
                .fetch_add(1, Ordering::Relaxed);
            self.inner
                .active_input_cancels
                .lock()
                .expect("input cancels lock")
                .insert(id, cancel.clone());
            id
        };
        let session = self.clone();
        tokio::spawn(async move {
            let _guard = ActiveInputGuard {
                session: session.downgrade(),
                id,
            };
            session
                .in_callback(async {
                    let runner = session.extension_runner();
                    tokio::select! { biased;
                        _ = cancel.cancelled() => Err("Session input cancelled".to_owned()),
                        result = runner.emit_input(text, images, source, behavior) => Ok(result),
                    }
                })
                .await
        })
        .await
        .map_err(|_| "Session input task failed".to_owned())?
    }

    fn expand_skill_command(&self, text: &str) -> String {
        if !self.enable_skill_commands() || !text.starts_with("/skill:") {
            return text.to_owned();
        }
        let after_prefix = &text["/skill:".len()..];
        let (skill_name, args) = match after_prefix.find(' ') {
            Some(idx) => (&after_prefix[..idx], after_prefix[idx + 1..].trim()),
            None => (after_prefix, ""),
        };
        let skills = self.skills();
        let Some(skill) = skills.iter().find(|s| s.name == skill_name) else {
            return text.to_owned();
        };
        match std::fs::read_to_string(&skill.file_path) {
            Ok(raw) => {
                let stripped = strip_yaml_frontmatter(&raw).trim();
                let block = format!(
                    "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
                    skill.name, skill.file_path, skill.base_dir, stripped
                );
                if args.is_empty() {
                    block
                } else {
                    format!("{block}\n\n{args}")
                }
            }
            Err(err) => {
                self.extension_runner().emit_error(ExtensionError {
                    extension_path: skill.file_path.clone(),
                    event: "skill_expansion".to_owned(),
                    error: err.to_string(),
                });
                text.to_owned()
            }
        }
    }

    async fn try_execute_extension_command(&self, text: &str) -> Result<bool, String> {
        let without_slash = &text[1..];
        let (cmd_name, args) = match without_slash.find(' ') {
            Some(idx) => (&without_slash[..idx], without_slash[idx + 1..].to_owned()),
            None => (without_slash, String::new()),
        };

        let runner = self.extension_runner();
        let Some(resolved) = runner.get_command(cmd_name) else {
            return Ok(false);
        };
        let ctx = runner.create_command_context();
        if let Err(err) = (resolved.command.handler)(args, ctx).await {
            runner.emit_error(ExtensionError {
                extension_path: format!("command:{cmd_name}"),
                event: "command".to_owned(),
                error: err,
            });
        }
        Ok(true)
    }

    pub async fn steer(
        &self,
        text: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let text = text.into();
        if let Some(stripped) = text.strip_prefix('/') {
            let cmd_name = stripped.split_whitespace().next().unwrap_or_default();
            if self.extension_runner().get_command(cmd_name).is_some() {
                return Err(format!(
                    "Extension command \"/{cmd_name}\" cannot be queued. Use prompt() when not streaming."
                ));
            }
        }
        self.queue_steer(text, images);
        Ok(())
    }

    pub async fn follow_up(
        &self,
        text: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let text = text.into();
        if let Some(stripped) = text.strip_prefix('/') {
            let cmd_name = stripped.split_whitespace().next().unwrap_or_default();
            if self.extension_runner().get_command(cmd_name).is_some() {
                return Err(format!(
                    "Extension command \"/{cmd_name}\" cannot be queued. Use prompt() when not streaming."
                ));
            }
        }
        self.queue_follow_up(text, images);
        Ok(())
    }

    pub fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        self.inner.agent.clear_all_queues();
        let steering = std::mem::take(&mut *self.inner.steering_messages.lock().expect("lock"));
        let follow_up = std::mem::take(&mut *self.inner.follow_up_messages.lock().expect("lock"));
        self.emit_queue_update();
        (steering, follow_up)
    }

    fn queue_steer(&self, text: String, images: Option<Vec<ImageContent>>) {
        self.queue_steer_correlated(text, images, None);
    }

    fn queue_steer_correlated(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        client_id: Option<String>,
    ) {
        let timestamp = self.input_timestamp();
        if let Some(id) = client_id {
            self.inner
                .pending_input_correlations
                .lock()
                .expect("input correlations lock")
                .insert(timestamp, (id, PromptDisposition::Steered));
        }
        self.inner
            .steering_messages
            .lock()
            .expect("lock")
            .push(text.clone());
        self.emit_queue_update();
        let mut blocks = vec![UserContentBlock::Text(TextContent::new(text))];
        if let Some(imgs) = images {
            blocks.extend(imgs.into_iter().map(UserContentBlock::Image));
        }
        self.inner.agent.steer(AgentMessage::User(UserMessage {
            content: UserContent::Blocks(blocks),
            timestamp,
        }));
    }

    fn queue_follow_up(&self, text: String, images: Option<Vec<ImageContent>>) {
        self.queue_follow_up_correlated(text, images, None);
    }

    fn queue_follow_up_correlated(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        client_id: Option<String>,
    ) {
        let timestamp = self.input_timestamp();
        if let Some(id) = client_id {
            self.inner
                .pending_input_correlations
                .lock()
                .expect("input correlations lock")
                .insert(timestamp, (id, PromptDisposition::FollowedUp));
        }
        self.inner
            .follow_up_messages
            .lock()
            .expect("lock")
            .push(text.clone());
        self.emit_queue_update();
        let mut blocks = vec![UserContentBlock::Text(TextContent::new(text))];
        if let Some(imgs) = images {
            blocks.extend(imgs.into_iter().map(UserContentBlock::Image));
        }
        self.inner.agent.follow_up(AgentMessage::User(UserMessage {
            content: UserContent::Blocks(blocks),
            timestamp,
        }));
    }

    fn emit_queue_update(&self) {
        let steering = self.inner.steering_messages.lock().expect("lock").clone();
        let follow_up = self.inner.follow_up_messages.lock().expect("lock").clone();
        self.emit(AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
        });
    }

    pub fn pending_message_count(&self) -> usize {
        self.inner.steering_messages.lock().expect("lock").len()
            + self.inner.follow_up_messages.lock().expect("lock").len()
    }

    pub async fn send_custom_message(
        &self,
        draft: BeforeAgentStartCustomMessageDraft,
        options: SendMessageOptions,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let custom_msg = CustomMessage {
            custom_type: draft.custom_type,
            content: draft.content,
            display: draft.display,
            details: draft.details,
            timestamp: now_ms(),
        };

        if self.is_streaming() && options.trigger_turn {
            match options.deliver_as.unwrap_or(StreamingInputBehavior::Steer) {
                StreamingInputBehavior::Steer => {
                    self.inner.agent.steer(custom_msg.to_agent_message());
                }
                StreamingInputBehavior::FollowUp => {
                    self.inner.agent.follow_up(custom_msg.to_agent_message());
                }
            }
            return Ok(());
        }

        if options.trigger_turn {
            if self.inner.is_emitting_agent_settled.load(Ordering::SeqCst) {
                let s = self.clone();
                let d = CustomMessage {
                    custom_type: custom_msg.custom_type.clone(),
                    content: custom_msg.content.clone(),
                    display: custom_msg.display,
                    details: custom_msg.details.clone(),
                    timestamp: custom_msg.timestamp,
                };
                self.inner
                    .deferred_settled_actions
                    .lock()
                    .expect("lock")
                    .push(Box::new(move || {
                        Box::pin(async move {
                            let _ = s
                                .start_run(BeforeAgentStartTrigger::Custom { message: d }, None)
                                .await;
                        })
                    }));
                return Ok(());
            }
            return self
                .start_run(
                    BeforeAgentStartTrigger::Custom {
                        message: custom_msg,
                    },
                    None,
                )
                .await;
        }

        if self.is_streaming() {
            // 流式期间严禁立即插入会话树破坏 toolCall/toolResult 配对顺序，暂存至 turn_end / agent_end 再刷入
            self.inner
                .pending_custom_messages
                .lock()
                .expect("lock")
                .push(custom_msg);
        } else {
            self.append_custom_message_now(&custom_msg)?;
        }
        Ok(())
    }

    fn append_custom_message_now(&self, custom_msg: &CustomMessage) -> Result<(), String> {
        let id = self
            .with_session_manager_internal(|sm| {
                sm.append_custom_message_entry(
                    &custom_msg.custom_type,
                    custom_msg.content.clone(),
                    custom_msg.display,
                    custom_msg.details.clone(),
                )
            })
            .map_err(|e| e.to_string())?;
        if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&id).cloned()) {
            self.emit(AgentSessionEvent::EntryAppended { entry });
        }
        self.refresh_finalized_context();
        let agent_msg = custom_msg.to_agent_message();
        self.emit(AgentSessionEvent::MessageStart {
            message: agent_msg.clone(),
        });
        self.emit(AgentSessionEvent::MessageEnd { message: agent_msg });
        Ok(())
    }

    fn flush_pending_custom_messages(&self) {
        let pending: Vec<CustomMessage> = self
            .inner
            .pending_custom_messages
            .lock()
            .expect("lock")
            .drain(..)
            .collect();
        for msg in pending {
            if let Err(err) = self.append_custom_message_now(&msg) {
                self.set_fatal_runtime_error(err);
                break;
            }
        }
    }

    /// Run a user command using host-provided operations or an extension replacement.
    pub async fn execute_bash(
        &self,
        command: impl Into<String>,
        mut options: ExecuteBashOptions,
    ) -> Result<BashResult, String> {
        let command = command.into();
        let id;
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            id = self.inner.next_bash_id.fetch_add(1, Ordering::Relaxed);
            let token = options.cancel_token.get_or_insert_default().clone();
            self.inner
                .active_bash_cancels
                .lock()
                .expect("bash cancels lock")
                .insert(id, token);
        }
        let session = self.clone();
        // The worker owns registration through result persistence, even if its caller is dropped.
        tokio::spawn(async move {
            let _active = ActiveBashGuard {
                session: session.downgrade(),
                id,
            };
            session.execute_bash_internal(command, options).await
        })
        .await
        .map_err(|_| "Bash execution task failed".to_owned())?
    }

    async fn execute_bash_internal(
        &self,
        command: String,
        options: ExecuteBashOptions,
    ) -> Result<BashResult, String> {
        self.ensure_external_open()?;
        let session_id = self.session_id();
        let runner = self.extension_runner();
        let token = options.cancel_token.clone().unwrap_or_default();
        let hook = tokio::select! {
            biased;
            _ = token.cancelled() => return Err("Bash execution cancelled".to_owned()),
            result = runner.emit_user_bash(UserBashEvent {
                command: command.clone(),
                cwd: self.inner.cwd.clone(),
                exclude_from_context: options.exclude_from_context,
            }) => result?,
        };
        runner.runtime().check_active()?;
        self.ensure_external_open()?;
        let fallback_executor = options.executor.or_else(|| {
            self.inner
                .default_bash_executor
                .read()
                .expect("bash executor lock")
                .clone()
        });
        let (result, executor) = match hook {
            Some(UserBashEventResult::Result(result)) => (Some(result), None),
            Some(UserBashEventResult::Operations(executor)) => (None, Some(executor)),
            None => (None, fallback_executor),
        };
        let result = match result {
            Some(result) => result,
            None => {
                let exec = executor.ok_or_else(|| {
                    "A bash executor must be supplied by the host or an extension".to_owned()
                })?;
                exec(BashRequest {
                    command: command.clone(),
                    cwd: self.inner.cwd.clone(),
                    env: {
                        let mut env = BTreeMap::new();
                        env.insert("PI_SESSION_ID".into(), self.session_id());
                        if let Some(path) = self.session_file() {
                            env.insert(
                                "PI_SESSION_FILE".into(),
                                path.to_string_lossy().into_owned(),
                            );
                        }
                        if let Some(model) = self.configured_model() {
                            env.insert("PI_PROVIDER".into(), model.provider);
                            env.insert("PI_MODEL".into(), model.id);
                        }
                        env.insert(
                            "PI_REASONING_LEVEL".into(),
                            self.thinking_level().as_str().into(),
                        );
                        env
                    },
                    cancel_token: token,
                    on_chunk: options.on_chunk,
                })
                .await?
            }
        };
        runner.runtime().check_active()?;
        if self.session_id() != session_id {
            return Err("Session changed while bash was running".to_owned());
        }
        self.record_bash_result_internal(BashExecutionMessage {
            command,
            output: result.output.clone(),
            exit_code: result.exit_code,
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path.clone(),
            timestamp: now_ms(),
            exclude_from_context: Some(options.exclude_from_context),
        })?;
        Ok(result)
    }

    pub fn record_bash_result(&self, bash_msg: BashExecutionMessage) -> Result<(), String> {
        self.record_bash_result_checked(bash_msg, true)
    }

    fn record_bash_result_internal(&self, bash_msg: BashExecutionMessage) -> Result<(), String> {
        self.record_bash_result_checked(bash_msg, false)
    }

    fn record_bash_result_checked(
        &self,
        bash_msg: BashExecutionMessage,
        external: bool,
    ) -> Result<(), String> {
        let id = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            if external {
                self.ensure_external_open()?;
            } else {
                self.ensure_runtime_writable()?;
            }
            if self.is_streaming() {
                self.inner
                    .pending_bash_messages
                    .lock()
                    .expect("lock")
                    .push(bash_msg);
                None
            } else {
                Some(
                    self.with_session_manager_internal(|sm| {
                        sm.append_message(bash_msg.to_agent_message())
                    })
                    .map_err(|e| e.to_string())?,
                )
            }
        };
        if let Some(id) = id {
            if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&id).cloned()) {
                self.emit(AgentSessionEvent::EntryAppended { entry });
            }
            self.refresh_finalized_context();
        }
        Ok(())
    }

    fn flush_pending_bash_messages(&self) {
        let pending: Vec<BashExecutionMessage> = self
            .inner
            .pending_bash_messages
            .lock()
            .expect("lock")
            .drain(..)
            .collect();
        if pending.is_empty() {
            return;
        }
        for bash_msg in pending {
            match self
                .with_session_manager_internal(|sm| sm.append_message(bash_msg.to_agent_message()))
            {
                Ok(id) => {
                    if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&id).cloned())
                    {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                Err(err) => {
                    self.set_fatal_runtime_error(format!(
                        "Failed to persist deferred bash result: {err}"
                    ));
                    break;
                }
            }
        }
        self.refresh_finalized_context();
    }

    fn prepare_prompt_and_tool_loadout(
        &self,
        options: &mut NormalizedBuildSystemPromptOptions,
        messages: &[AgentMessage],
    ) -> Option<SystemMessage> {
        let active = self.apply_tool_loadout(&options.selected_tools);
        options.selected_tools = active.into_iter().map(|t| t.name).collect();
        options.hidden_tools = self
            .inner
            .hidden_declarations
            .read()
            .expect("hidden lock")
            .iter()
            .cloned()
            .collect();

        let current_sections = get_agent_current_system_message(messages)
            .and_then(|s| s.sections)
            .unwrap_or_default();
        let target_sections = build_system_prompt_sections(&options.to_build_options()).ok()?;
        let patch = diff_system_prompt_sections(&current_sections, &target_sections)?;
        Some(SystemMessage {
            content: SystemContent::Text(String::new()),
            sections: Some(patch),
            tools_added: None,
            tools_removed: None,
            timestamp: now_ms(),
        })
    }

    async fn prepare_run(
        &self,
        trigger: BeforeAgentStartTrigger,
    ) -> Result<Vec<AgentMessage>, String> {
        if let Some(last_asst) = self.find_last_assistant_message() {
            let _ = self.check_compaction(&last_asst, false).await;
        }

        let base_opts = self
            .inner
            .base_system_prompt_options
            .read()
            .expect("lock")
            .to_build_options();
        let selected_before = base_opts.selected_tools.clone().unwrap_or_default();
        let mut combined = self
            .extension_runner()
            .emit_before_agent_start(trigger.clone(), &base_opts)
            .await;

        if combined.system_prompt_options.selected_tools == selected_before {
            combined.system_prompt_options.selected_tools = self.get_active_tool_names();
        }

        let mut messages: Vec<AgentMessage> = Vec::new();
        match trigger {
            BeforeAgentStartTrigger::Custom { message } => {
                messages.push(message.to_agent_message());
            }
            BeforeAgentStartTrigger::User { prompt, images } => {
                let mut blocks = vec![UserContentBlock::Text(TextContent::new(prompt))];
                if let Some(imgs) = images {
                    blocks.extend(imgs.into_iter().map(UserContentBlock::Image));
                }
                messages.push(AgentMessage::User(UserMessage {
                    content: UserContent::Blocks(blocks),
                    timestamp: self.input_timestamp(),
                }));
            }
        }

        let next_turn_msgs: Vec<CustomMessage> = self
            .inner
            .pending_next_turn_messages
            .lock()
            .expect("lock")
            .drain(..)
            .collect();
        for m in next_turn_msgs {
            messages.push(m.to_agent_message());
        }

        for draft in combined.messages {
            let cm = CustomMessage {
                custom_type: draft.custom_type,
                content: draft.content,
                display: draft.display,
                details: draft.details,
                timestamp: now_ms(),
            };
            messages.push(cm.to_agent_message());
        }

        let state_msgs = self.inner.agent.state().messages;
        let sys_patch =
            self.prepare_prompt_and_tool_loadout(&mut combined.system_prompt_options, &state_msgs);
        *self.inner.run_system_prompt_options.write().expect("lock") =
            Some(combined.system_prompt_options);

        if let Some(sys) = sys_patch {
            messages.insert(0, AgentMessage::System(sys));
        }

        Ok(messages)
    }

    async fn start_run(
        &self,
        trigger: BeforeAgentStartTrigger,
        client_message_id: Option<String>,
    ) -> Result<(), String> {
        self.start_run_with_admission(trigger, client_message_id, None)
            .await
    }

    /// Resume a runnable transcript without appending a synthetic user message.
    pub async fn continue_run(&self) -> Result<(), String> {
        self.ensure_external_open()?;
        if !self.has_configured_model() {
            return Err(self.no_models_error_message());
        }
        if self.is_compacting() {
            return Err("Cannot continue while compaction is in progress".to_owned());
        }
        self.start_session_run(None, None, None).await
    }

    async fn start_run_with_admission(
        &self,
        trigger: BeforeAgentStartTrigger,
        client_message_id: Option<String>,
        on_admitted: Option<PromptAdmittedCallback>,
    ) -> Result<(), String> {
        self.start_session_run(Some(trigger), client_message_id, on_admitted)
            .await
    }

    async fn start_session_run(
        &self,
        trigger: Option<BeforeAgentStartTrigger>,
        client_message_id: Option<String>,
        mut on_admitted: Option<PromptAdmittedCallback>,
    ) -> Result<(), String> {
        let cancel = CancellationToken::new();
        let admission = {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open().and_then(|()| {
                if self
                    .inner
                    .reload_cancel
                    .lock()
                    .expect("reload cancel lock")
                    .is_some()
                {
                    return Err("Session reload in progress".to_owned());
                }
                if self.inner.is_agent_run_active.swap(true, Ordering::SeqCst) {
                    return Err("AgentSession already has an active run".to_owned());
                }
                self.inner.active_run_workers.fetch_add(1, Ordering::SeqCst);
                self.inner
                    .agent_run_abort_requested
                    .store(false, Ordering::SeqCst);
                *self.inner.run_cancel.lock().expect("run cancel lock") = Some(cancel.clone());
                Ok(())
            })
        };
        if let Err(err) = admission {
            if let Some(cb) = on_admitted.take() {
                cb(Err(err.clone()));
            }
            return Err(err);
        }
        let session = self.clone();
        // The worker owns transcript and execution-log finalization independently of the waiter.
        tokio::spawn(async move {
            let _active = ActiveRunGuard {
                session: session.downgrade(),
            };
            let id = Arc::as_ptr(&session.inner) as usize;
            RUN_SESSION
                .scope(
                    id,
                    session.run_admitted(trigger, client_message_id, on_admitted, cancel),
                )
                .await
        })
        .await
        .map_err(|_| "Session run task failed".to_owned())?
    }

    async fn run_admitted(
        &self,
        trigger: Option<BeforeAgentStartTrigger>,
        client_message_id: Option<String>,
        mut on_admitted: Option<PromptAdmittedCallback>,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let continuation = trigger.is_none();
        let prepared = if let Some(trigger) = trigger {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err("Session run cancelled".to_owned()),
                result = self.prepare_run(trigger) => result,
            }
        } else {
            Ok(Vec::new())
        };
        let messages = match prepared {
            Ok(msgs) => msgs,
            Err(err) => {
                if let Some(cid) = client_message_id.as_deref() {
                    let _ =
                        self.with_execution_log_internal(|log| log.mark_input_failed(cid, &err));
                }
                if let Some(cb) = on_admitted.take() {
                    cb(Err(err.clone()));
                }
                self.inner
                    .is_agent_run_active
                    .store(false, Ordering::SeqCst);
                self.flush_pending_bash_messages();
                self.flush_pending_custom_messages();
                self.inner.idle_notify.notify_waiters();
                return Err(err);
            }
        };

        self.flush_pending_bash_messages();
        self.flush_pending_custom_messages();
        *self.inner.failed_response.lock().expect("lock") = None;

        let run_seq = self.inner.next_run_seq.fetch_add(1, Ordering::Relaxed);
        if let Err(err) = self.with_execution_log_internal(|log| log.start_run(run_seq)) {
            let err_str = err.to_string();
            if let Some(cb) = on_admitted.take() {
                cb(Err(err_str.clone()));
            }
            self.inner
                .is_agent_run_active
                .store(false, Ordering::SeqCst);
            self.inner.idle_notify.notify_waiters();
            return Err(err_str);
        }

        if let Some(msg) = messages.iter().find(|m| m.role() == "user") {
            let ts = msg.timestamp();
            if let Some(cid) = client_message_id {
                self.inner
                    .pending_input_correlations
                    .lock()
                    .expect("input correlations lock")
                    .insert(ts, (cid, PromptDisposition::Started));
            }
            if let Some(cb) = on_admitted.take() {
                self.inner
                    .pending_admission_callbacks
                    .lock()
                    .expect("admission lock")
                    .insert(ts, cb);
            }
        } else if let Some(cb) = on_admitted.take() {
            cb(Ok(PromptDisposition::Started));
        }

        let mut run_result = if cancel.is_cancelled() {
            Err("Session run cancelled".to_owned())
        } else if continuation {
            self.inner.agent.continue_run().await
        } else {
            self.inner.agent.prompt(messages).await
        };

        while run_result.is_ok()
            && !self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            && self.fatal_runtime_error().is_none()
        {
            if self.handle_post_agent_run().await {
                if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                    || self.fatal_runtime_error().is_some()
                {
                    break;
                }
                run_result = self.inner.agent.continue_run().await;
                continue;
            }
            if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                || self.fatal_runtime_error().is_some()
                || !self.run_before_settle_boundary().await
            {
                break;
            }
            if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                || self.fatal_runtime_error().is_some()
            {
                break;
            }
            run_result = self.inner.agent.continue_run().await;
        }

        let aborted = self.inner.agent_run_abort_requested.load(Ordering::SeqCst);
        if aborted {
            self.finish_cancelled_retry();
        }
        if self.fatal_runtime_error().is_none() {
            let err_for_log = run_result.as_ref().err().cloned();
            if let Err(err) =
                self.with_execution_log_internal(|log| log.finish_run(aborted, err_for_log))
            {
                self.set_fatal_runtime_error(format!("Failed to persist run completion: {err}"));
            }
        }

        *self.inner.failed_response.lock().expect("lock") = None;
        *self.inner.run_system_prompt_options.write().expect("lock") = None;
        self.flush_pending_bash_messages();
        self.flush_pending_custom_messages();
        self.emit_agent_settled(aborted).await;

        match self.fatal_runtime_error() {
            Some(err) => Err(err),
            None => run_result,
        }
    }

    async fn run_before_settle_boundary(&self) -> bool {
        let state_msgs = self.inner.agent.state().messages;
        let boundary = self
            .extension_runner()
            .emit_boundary(
                BoundaryEventKind::AgentBeforeSettle {
                    messages: state_msgs,
                },
                |drafts| self.build_boundary_context_preview(drafts, false),
            )
            .await;

        if !boundary.entries.is_empty() {
            match self
                .with_session_manager_internal(|sm| sm.commit_boundary_drafts(&boundary.entries))
            {
                Ok(appended) => {
                    self.refresh_finalized_context();
                    for entry in appended {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                Err(err) => {
                    self.set_fatal_runtime_error(format!(
                        "Failed to commit agent_before_settle boundary drafts: {err}"
                    ));
                    return false;
                }
            }
        }

        self.flush_pending_custom_messages();
        let should_continue = boundary.r#continue || self.inner.agent.has_queued_messages();
        if should_continue {
            match self.build_boundary_context_preview(&[], false) {
                Ok(preview) if preview.can_continue => return true,
                _ => {
                    if boundary.r#continue {
                        self.extension_runner().emit_error(ExtensionError {
                            extension_path: "<boundary>".to_owned(),
                            event: "agent_before_settle".to_owned(),
                            error: "agent_before_settle requested continuation without runnable model context".to_owned(),
                        });
                    }
                    return false;
                }
            }
        }
        false
    }

    async fn emit_agent_settled(&self, aborted: bool) {
        self.inner
            .is_agent_run_active
            .store(false, Ordering::SeqCst);
        self.inner
            .is_emitting_agent_settled
            .store(true, Ordering::SeqCst);
        let on_settled = self
            .inner
            .on_agent_settled
            .read()
            .expect("on settled lock")
            .clone();
        if let Some(cb) = on_settled {
            cb();
        }
        let state_msgs = self.inner.agent.state().messages;
        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::AgentSettled {
                messages: state_msgs,
            })
            .await;
        self.emit_async(AgentSessionEvent::AgentSettled { aborted })
            .await;
        self.inner
            .is_emitting_agent_settled
            .store(false, Ordering::SeqCst);

        let deferred: Vec<DeferredSettledAction> = self
            .inner
            .deferred_settled_actions
            .lock()
            .expect("deferred lock")
            .drain(..)
            .collect();
        for action in deferred {
            action().await;
        }
        self.inner.idle_notify.notify_waiters();
    }

    // =========================================================================
    // Auto-Retry, Auto-Compaction & Context Overflow Recovery
    // =========================================================================

    fn find_last_assistant_message(&self) -> Option<AssistantMessage> {
        let state = self.inner.agent.state();
        for msg in state.messages.iter().rev() {
            if let AgentMessage::Assistant(a) = msg {
                return Some(a.clone());
            }
        }
        None
    }

    fn model_for_message(&self, msg: &AssistantMessage) -> Option<Model> {
        let current = self.model();
        if current.provider == msg.provider && current.id == msg.model {
            return Some(current);
        }
        if is_virtual_model(&current) {
            return self.inner.models.get_model(&msg.provider, &msg.model);
        }
        None
    }

    fn limits_model(&self) -> Model {
        let current = self.model();
        if is_virtual_model(&current)
            && let Some(latest) = find_latest_response(&self.inner.agent.state().messages)
            && let Some(phys) = self.inner.models.get_model(&latest.provider, &latest.model)
        {
            return phys;
        }
        current
    }

    fn is_retryable_error(&self, message: &AssistantMessage) -> bool {
        if self.fatal_runtime_error().is_some() {
            return false;
        }
        let cw = self
            .model_for_message(message)
            .unwrap_or_else(|| self.model())
            .context_window;
        if is_context_overflow(message, (cw > 0).then_some(cw)) {
            return false;
        }
        is_retryable_assistant_error(message)
    }

    fn finish_cancelled_retry(&self) {
        let attempt = self.inner.retry_attempt.swap(0, Ordering::SeqCst);
        if attempt == 0 {
            return;
        }
        self.emit(AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt: attempt as u32,
            final_error: Some("Retry cancelled".to_owned()),
        });
    }

    async fn prepare_retry(&self, message: &AssistantMessage) -> bool {
        let policy = self.retry_policy();
        if !policy.enabled {
            return false;
        }
        let next_attempt = self.inner.retry_attempt.fetch_add(1, Ordering::SeqCst) + 1;
        if (next_attempt as u32) > policy.max_retries {
            self.inner.retry_attempt.fetch_sub(1, Ordering::SeqCst);
            return false;
        }

        let delay_ms = retry_delay_ms(&policy, next_attempt as u32);
        let err_msg = message
            .error_message
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Unknown error".to_owned());

        let token = CancellationToken::new();
        *self.inner.retry_cancel.lock().expect("lock") = Some(token.clone());
        if self.is_closing_or_closed()
            || self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
        {
            token.cancel();
        }
        self.emit(AgentSessionEvent::AutoRetryStart {
            attempt: next_attempt as u32,
            max_attempts: policy.max_retries,
            delay_ms,
            error_message: err_msg,
        });

        if let Err(err) = self.omit_recovery_attempt(message) {
            self.inner.retry_cancel.lock().expect("lock").take();
            self.set_fatal_runtime_error(err);
            return false;
        }

        let aborted = tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => false,
            _ = token.cancelled() => true,
        };
        *self.inner.retry_cancel.lock().expect("lock") = None;

        if aborted {
            self.finish_cancelled_retry();
            return false;
        }
        true
    }

    fn find_persisted_message_entry_id(&self, target: &AgentMessage) -> Option<String> {
        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        for entry in branch.iter().rev() {
            if let SessionEntry::Message(m) = entry
                && &m.message == target
            {
                return Some(m.id.clone());
            }
        }
        let guard = self
            .inner
            .entry_ids_by_timestamp_and_role
            .lock()
            .expect("entry map lock");
        guard
            .iter()
            .rev()
            .find(|(role, ts, _)| role == target.role() && *ts == target.timestamp())
            .map(|(_, _, id)| id.clone())
    }

    /// 持久化隐去失败的 `AssistantMessage` 及其紧随的配对 `ToolResultMessage`（对齐上游 `#11227`）。
    fn omit_recovery_attempt(&self, assistant_msg: &AssistantMessage) -> Result<(), String> {
        let tool_call_ids: BTreeSet<String> = assistant_msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall(tc) => Some(tc.id.clone()),
                _ => None,
            })
            .collect();

        let asst_agent_msg = AgentMessage::Assistant(assistant_msg.clone());
        let asst_entry_id = self.find_persisted_message_entry_id(&asst_agent_msg);
        let state_msgs = self.inner.agent.state().messages;

        let idx = state_msgs.iter().rposition(|m| m == &asst_agent_msg);
        let mut tool_results: Vec<AgentMessage> = Vec::new();
        if let Some(start_idx) = idx {
            for m in &state_msgs[start_idx + 1..] {
                if m.role() == "assistant" {
                    break;
                }
                if let AgentMessage::ToolResult(tr) = m
                    && tool_call_ids.contains(&tr.tool_call_id)
                {
                    tool_results.push(m.clone());
                }
            }
        }

        let mut target_ids: Vec<String> = Vec::with_capacity(1 + tool_results.len());
        if let Some(aid) = asst_entry_id {
            target_ids.push(aid);
        } else if idx.is_some() {
            return Err(
                "Cannot persist recovery omission because assistant message has no source entry"
                    .to_owned(),
            );
        }

        for tr_msg in &tool_results {
            let Some(tid) = self.find_persisted_message_entry_id(tr_msg) else {
                return Err(
                    "Cannot persist recovery omission because a projected toolResult message has no source entry"
                        .to_owned(),
                );
            };
            target_ids.push(tid);
        }

        for target_id in target_ids {
            let edit_id = self
                .with_session_manager_internal(|sm| sm.append_context_edit(&target_id, None))
                .map_err(|e| e.to_string())?;
            if let Some(entry) = self.with_session_manager(|sm| sm.get_entry(&edit_id).cloned()) {
                self.emit(AgentSessionEvent::EntryAppended { entry });
            }
        }

        self.refresh_finalized_context();
        Ok(())
    }

    async fn handle_post_agent_run(&self) -> bool {
        if self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            || self.fatal_runtime_error().is_some()
        {
            self.finish_cancelled_retry();
            return false;
        }

        let last_asst = self
            .inner
            .last_assistant_message
            .lock()
            .expect("lock")
            .take()
            .map(|(m, _)| m);

        let Some(message) = last_asst else {
            return self.inner.agent.has_queued_messages();
        };

        if self.is_retryable_error(&message) && self.prepare_retry(&message).await {
            if self.inner.agent_run_abort_requested.load(Ordering::SeqCst) {
                self.finish_cancelled_retry();
                return false;
            }
            let phys_model = self
                .model_for_message(&message)
                .unwrap_or_else(|| self.model());
            *self.inner.failed_response.lock().expect("lock") =
                Some((phys_model, self.thinking_level(), message));
            return !self.inner.agent_run_abort_requested.load(Ordering::SeqCst);
        }

        if self.inner.agent_run_abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
            return false;
        }

        if message.stop_reason == StopReason::Error {
            let attempt = self.inner.retry_attempt.swap(0, Ordering::SeqCst);
            if attempt > 0 {
                self.emit(AgentSessionEvent::AutoRetryEnd {
                    success: false,
                    attempt: attempt as u32,
                    final_error: message.error_message.clone(),
                });
            }
        }

        if self.check_compaction(&message, true).await {
            return !self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
                && self.fatal_runtime_error().is_none();
        }

        !self.inner.agent_run_abort_requested.load(Ordering::SeqCst)
            && self.fatal_runtime_error().is_none()
            && self.inner.agent.has_queued_messages()
    }

    fn exceeds_compaction_threshold(
        &self,
        model: &Model,
        projection: &crate::session_manager::SessionProjection,
    ) -> bool {
        if model.context_window == 0 {
            return false;
        }
        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        let tokens = estimate_projected_context_tokens(projection, &branch).tokens;
        should_compact(tokens, model.context_window, &self.compaction_settings())
    }

    async fn check_compaction(
        &self,
        assistant_message: &AssistantMessage,
        skip_aborted_check: bool,
    ) -> bool {
        if self.fatal_runtime_error().is_some() {
            return false;
        }
        let settings = self.compaction_settings();
        if !settings.enabled {
            return false;
        }
        if skip_aborted_check && assistant_message.stop_reason == StopReason::Aborted {
            return false;
        }

        let message_model = self.model_for_message(assistant_message);
        let same_model = message_model.is_some();
        let effective_model = message_model.clone().unwrap_or_else(|| self.model());
        let context_window = effective_model.context_window;

        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        if let Some(latest_comp) = get_latest_compaction_entry(&branch)
            && let Some(comp_ms) = parse_iso_timestamp_ms(&latest_comp.timestamp)
            && assistant_message.timestamp > 0
            && assistant_message.timestamp <= comp_ms
        {
            return false;
        }

        let current_projection =
            self.with_session_manager(SessionManager::build_session_projection);
        let asst_agent_msg = AgentMessage::Assistant(assistant_message.clone());
        let asst_entry_id = self.find_persisted_message_entry_id(&asst_agent_msg);
        let asst_is_projected = asst_entry_id.as_ref().is_none_or(|id| {
            current_projection.entries.iter().any(|e| {
                e.source_entry.id() == id && e.messages.iter().any(|m| m.role() == "assistant")
            })
        });

        let explicit_overflow = assistant_message.stop_reason == StopReason::Error
            && is_context_overflow(assistant_message, None);
        let silent_or_length_overflow = asst_is_projected
            && is_context_overflow(
                assistant_message,
                (context_window > 0).then_some(context_window),
            );
        let context_overflow = same_model && (explicit_overflow || silent_or_length_overflow);
        let recoverable_length = same_model
            && asst_is_projected
            && is_recoverable_length(assistant_message, effective_model.max_tokens);

        if context_overflow || recoverable_length {
            let will_retry = assistant_message.stop_reason != StopReason::Stop;
            if !will_retry {
                // Case 2: 静默超窗但回复已完成（stop_reason == Stop），仅压缩不重试
                return self
                    .run_auto_compaction(CompactionReason::Overflow, false)
                    .await;
            }

            if self
                .inner
                .overflow_recovery_attempted
                .swap(true, Ordering::SeqCst)
            {
                let err_msg = if context_overflow {
                    "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.".to_owned()
                } else {
                    "Truncated response recovery failed after one compact-and-retry attempt."
                        .to_owned()
                };
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Overflow,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: Some(err_msg.clone()),
                });
                let _ = self
                    .extension_runner()
                    .emit_lifecycle(SessionLifecycleEvent::SessionCompactFailed {
                        reason: CompactionReason::Overflow,
                        error_message: err_msg,
                        will_retry: false,
                    })
                    .await;
                return false;
            }

            if let Err(err) = self.omit_recovery_attempt(assistant_message) {
                self.set_fatal_runtime_error(err);
                return false;
            }

            let retry = self
                .run_auto_compaction(CompactionReason::Overflow, true)
                .await;
            if retry {
                *self.inner.failed_response.lock().expect("lock") = Some((
                    effective_model,
                    self.thinking_level(),
                    assistant_message.clone(),
                ));
            }
            return retry;
        }

        // Case 3: 阈值触发自动压缩
        let has_context_edits = current_projection
            .entries
            .iter()
            .any(|e| matches!(e.source_entry, SessionEntry::ContextEdit(_)));
        let direct_tokens = calculate_context_tokens(&assistant_message.usage);
        let context_tokens = if has_context_edits {
            estimate_projected_context_tokens(&current_projection, &branch).tokens
        } else if assistant_message.stop_reason == StopReason::Error || direct_tokens == 0 {
            estimate_context_tokens(&self.inner.agent.state().messages).tokens
        } else {
            direct_tokens
        };

        if should_compact(context_tokens, context_window, &settings) {
            return self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await;
        }

        false
    }

    async fn run_auto_compaction(&self, reason: CompactionReason, will_retry: bool) -> bool {
        let model = self.limits_model();
        let settings = self.compaction_settings();
        let branch = self.with_session_manager(|sm| sm.get_branch(None));
        let Some(preparation) = prepare_compaction(&branch, settings) else {
            return false;
        };

        let cancel = CancellationToken::new();
        *self.inner.compaction_cancel.lock().expect("lock") = Some(cancel.clone());
        self.emit(AgentSessionEvent::CompactionStart { reason });

        let outcome = self
            .execute_compaction_core(
                &preparation,
                &branch,
                &model,
                None,
                reason,
                will_retry,
                cancel.clone(),
            )
            .await;
        *self.inner.compaction_cancel.lock().expect("lock") = None;
        self.inner.idle_notify.notify_waiters();

        match outcome {
            Ok(result) => {
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason,
                    result: Some(result),
                    aborted: false,
                    will_retry,
                    error_message: None,
                });
                if will_retry {
                    true
                } else {
                    self.inner.agent.has_queued_messages()
                }
            }
            Err((aborted, err)) => {
                let err_msg = (!aborted).then(|| match reason {
                    CompactionReason::Overflow => {
                        format!("Context overflow recovery failed: {err}")
                    }
                    _ => format!("Auto-compaction failed: {err}"),
                });
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: err_msg.clone(),
                });
                if let Some(em) = err_msg {
                    let _ = self
                        .extension_runner()
                        .emit_lifecycle(SessionLifecycleEvent::SessionCompactFailed {
                            reason,
                            error_message: em,
                            will_retry: false,
                        })
                        .await;
                }
                false
            }
        }
    }

    /// 手动压缩上下文（`/compact`，支持短会话自适应折半缩减保留预算与 `session_before_compact` 钩子拦截）。
    pub async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<CompactionResult, String> {
        self.ensure_external_open()?;
        if !self.has_configured_model() {
            return Err(self.no_models_error_message());
        }
        self.abort().await;
        let cancel = CancellationToken::new();
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            if self.is_compacting() {
                return Err("Compaction already in progress".to_owned());
            }
            *self.inner.compaction_cancel.lock().expect("lock") = Some(cancel.clone());
        }
        let session = self.clone();
        tokio::spawn(async move {
            let _active = ActiveAuxiliaryGuard {
                session: session.downgrade(),
                compaction: true,
            };
            RUN_SESSION
                .scope(
                    Arc::as_ptr(&session.inner) as usize,
                    session.compact_internal(custom_instructions, cancel),
                )
                .await
        })
        .await
        .map_err(|_| "Session compaction task failed".to_owned())?
    }

    async fn compact_internal(
        &self,
        custom_instructions: Option<String>,
        cancel: CancellationToken,
    ) -> Result<CompactionResult, String> {
        self.emit_async(AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        })
        .await;

        let model = self.limits_model();
        let settings = self.compaction_settings();
        let branch = self.with_session_manager(|sm| sm.get_branch(None));

        let Some(preparation) = prepare_compaction_with_adaptive_budget(&branch, settings) else {
            let err = if matches!(branch.last(), Some(SessionEntry::Compaction(_))) {
                "Already compacted".to_owned()
            } else {
                "Nothing to compact (session too small)".to_owned()
            };
            self.emit_async(AgentSessionEvent::CompactionEnd {
                reason: CompactionReason::Manual,
                result: None,
                aborted: false,
                will_retry: false,
                error_message: Some(format!("Compaction failed: {err}")),
            })
            .await;
            return Err(err);
        };

        let outcome = self
            .execute_compaction_core(
                &preparation,
                &branch,
                &model,
                custom_instructions,
                CompactionReason::Manual,
                false,
                cancel,
            )
            .await;

        match outcome {
            Ok(result) => {
                self.emit_async(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: Some(result.clone()),
                    aborted: false,
                    will_retry: false,
                    error_message: None,
                })
                .await;
                Ok(result)
            }
            Err((aborted, err)) => {
                let error_message = (!aborted).then(|| format!("Compaction failed: {err}"));
                self.emit_async(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: error_message.clone(),
                })
                .await;
                if let Some(em) = error_message {
                    let _ = self
                        .extension_runner()
                        .emit_lifecycle(SessionLifecycleEvent::SessionCompactFailed {
                            reason: CompactionReason::Manual,
                            error_message: em,
                            will_retry: false,
                        })
                        .await;
                }
                Err(err)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_compaction_core(
        &self,
        preparation: &CompactionPreparation,
        branch_entries: &[SessionEntry],
        model: &Model,
        custom_instructions: Option<String>,
        _reason: CompactionReason,
        _will_retry: bool,
        cancel_token: CancellationToken,
    ) -> Result<CompactionResult, (bool, String)> {
        let mut from_extension = false;
        let mut ext_compaction: Option<CompactionResult> = None;

        let runner = self.extension_runner();
        let before = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => return Err((true, "Compaction cancelled".to_owned())),
            result = runner.emit_lifecycle(SessionLifecycleEvent::SessionBeforeCompact {
                preparation: preparation.clone(),
                branch_entries: branch_entries.to_vec(),
                custom_instructions: custom_instructions.clone(),
            }) => result,
        };
        if let SessionBeforeEventOutput::Compact(before_res) = before {
            if before_res.cancel {
                return Err((true, "Compaction cancelled".to_owned()));
            }
            if let Some(custom_comp) = before_res.compaction {
                from_extension = true;
                ext_compaction = Some(custom_comp);
            }
        }

        if cancel_token.is_cancelled() {
            return Err((true, "Compaction cancelled".to_owned()));
        }

        let base_result = if let Some(ext_res) = ext_compaction {
            ext_res
        } else {
            let sum_opts = SummarizationCallOptions {
                api_key: None,
                headers: None,
                cancel_token: Some(cancel_token.clone()),
                custom_instructions,
                thinking_level: None,
                stream_fn: Some(self.inner.agent.stream_fn()),
                retry: Some(self.retry_policy()),
                callbacks: None,
                session_id: None,
                block_images: self.is_block_images_enabled(),
            };
            compact(preparation, model, &sum_opts)
                .await
                .map_err(|e| (cancel_token.is_cancelled(), e))?
        };

        if cancel_token.is_cancelled() {
            return Err((true, "Compaction cancelled".to_owned()));
        }

        let comp_id = self
            .with_session_manager_internal(|sm| {
                sm.append_compaction(
                    &base_result.summary,
                    Some(base_result.first_kept_entry_id.clone()),
                    base_result.tokens_before,
                    base_result.details.clone(),
                    Some(from_extension),
                    base_result.usage.clone(),
                )
            })
            .map_err(|e| (false, e.to_string()))?;

        self.refresh_finalized_context();
        let proj = self.with_session_manager(SessionManager::build_session_projection);
        let estimated_after: u64 = proj.messages.iter().map(estimate_tokens).sum();

        if let Some(SessionEntry::Compaction(saved)) =
            self.with_session_manager(|sm| sm.get_entry(&comp_id).cloned())
        {
            let _ = self
                .extension_runner()
                .emit_lifecycle(SessionLifecycleEvent::SessionCompact {
                    compaction_entry: saved.clone(),
                    from_extension,
                })
                .await;
            self.emit(AgentSessionEvent::EntryAppended {
                entry: SessionEntry::Compaction(saved),
            });
        }

        Ok(CompactionResult {
            summary: base_result.summary,
            first_kept_entry_id: base_result.first_kept_entry_id,
            tokens_before: base_result.tokens_before,
            estimated_tokens_after: Some(estimated_after),
            usage: base_result.usage,
            details: base_result.details,
        })
    }

    // =========================================================================
    // Tree Navigation, Fork, NewSession, SwitchSession & ContextUsage
    // =========================================================================

    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, String> {
        let cancel = CancellationToken::new();
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            if self.is_streaming() {
                return Err(
                    "Wait for the current response to finish before navigating the session tree."
                        .to_owned(),
                );
            }
            if self.is_compacting() {
                return Err("Wait for the current compaction or tree navigation to finish before navigating the session tree.".to_owned());
            }
            *self.inner.branch_summary_cancel.lock().expect("lock") = Some(cancel.clone());
        }
        let session = self.clone();
        let target_id = target_id.to_owned();
        tokio::spawn(async move {
            let _active = ActiveAuxiliaryGuard {
                session: session.downgrade(),
                compaction: false,
            };
            RUN_SESSION
                .scope(
                    Arc::as_ptr(&session.inner) as usize,
                    session.navigate_tree_internal(&target_id, options, cancel),
                )
                .await
        })
        .await
        .map_err(|_| "Session tree navigation task failed".to_owned())?
    }

    async fn navigate_tree_internal(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
        cancel: CancellationToken,
    ) -> Result<NavigateTreeResult, String> {
        let old_leaf_id = self.with_session_manager(|sm| sm.get_leaf_id().map(ToOwned::to_owned));
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(NavigateTreeResult {
                cancelled: false,
                ..NavigateTreeResult::default()
            });
        }

        let target_entry = self
            .with_session_manager(|sm| sm.get_entry(target_id).cloned())
            .ok_or_else(|| format!("Entry {target_id} not found"))?;

        let collected = self.with_session_manager(|sm| {
            collect_entries_for_branch_summary(sm, old_leaf_id.as_deref(), Some(target_id))
        });

        let mut custom_instructions = options.custom_instructions.clone();
        let mut replace_instructions = options.replace_instructions;
        let mut label = options.label.clone();

        let prep = TreePreparation {
            target_id: target_id.to_owned(),
            old_leaf_id: old_leaf_id.clone(),
            common_ancestor_id: collected.common_ancestor_id.clone(),
            entries_to_summarize: collected.entries.clone(),
            user_wants_summary: options.summarize,
            custom_instructions: custom_instructions.clone(),
            replace_instructions,
            label: label.clone(),
        };

        let mut ext_summary: Option<BranchSummaryResult> = None;
        let mut from_extension = false;

        let runner = self.extension_runner();
        let before = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(NavigateTreeResult { cancelled: true, aborted: true, ..Default::default() }),
            result = runner.emit_lifecycle(SessionLifecycleEvent::SessionBeforeTree { preparation: prep }) => result,
        };
        if let SessionBeforeEventOutput::Tree(before_res) = before {
            if before_res.cancel {
                return Ok(NavigateTreeResult {
                    cancelled: true,
                    ..NavigateTreeResult::default()
                });
            }
            if before_res.summary.is_some() && options.summarize {
                ext_summary = before_res.summary;
                from_extension = true;
            }
            if before_res.custom_instructions.is_some() {
                custom_instructions = before_res.custom_instructions;
            }
            if let Some(rep) = before_res.replace_instructions {
                replace_instructions = rep;
            }
            if before_res.label.is_some() {
                label = before_res.label;
            }
        }

        let mut summary_text: Option<String> = None;
        let mut summary_details: Option<JsonValue> = None;
        let mut summary_usage: Option<Usage> = None;

        if options.summarize && !collected.entries.is_empty() && ext_summary.is_none() {
            if !self.has_configured_model() {
                return Err(self.no_models_error_message());
            }
            let mut gen_opts = GenerateBranchSummaryOptions::new(self.limits_model());
            gen_opts.cancel_token = Some(cancel.clone());
            gen_opts.custom_instructions = custom_instructions;
            gen_opts.replace_instructions = replace_instructions;
            gen_opts.stream_fn = Some(self.inner.agent.stream_fn());
            gen_opts.retry = Some(self.retry_policy());
            gen_opts.block_images = self.is_block_images_enabled();

            let gen_res = generate_branch_summary(&collected.entries, &gen_opts).await;
            if gen_res.aborted == Some(true) {
                return Ok(NavigateTreeResult {
                    cancelled: true,
                    aborted: true,
                    ..NavigateTreeResult::default()
                });
            }
            if let Some(err) = gen_res.error {
                return Err(err);
            }
            summary_text = gen_res.summary;
            summary_usage = gen_res.usage;
            summary_details = serde_json::to_value(BranchSummaryDetails {
                read_files: gen_res.read_files.unwrap_or_default(),
                modified_files: gen_res.modified_files.unwrap_or_default(),
            })
            .ok();
        } else if let Some(ext_s) = ext_summary {
            summary_text = ext_s.summary;
            summary_usage = ext_s.usage;
        }

        if cancel.is_cancelled() {
            return Ok(NavigateTreeResult {
                cancelled: true,
                aborted: true,
                ..Default::default()
            });
        }

        // 若目标节点为 user 或 custom_message，将叶节点移至其 parent_id 并回填编辑器文本
        let (new_leaf_id, editor_text): (Option<String>, Option<String>) = match &target_entry {
            SessionEntry::Message(m) if m.message.role() == "user" => {
                let text = match &m.message {
                    AgentMessage::User(u) => match &u.content {
                        UserContent::Text(s) => s.clone(),
                        UserContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| match b {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(""),
                    },
                    _ => String::new(),
                };
                (m.parent_id.clone(), Some(text))
            }
            SessionEntry::CustomMessage(cm) => {
                (cm.parent_id.clone(), Some(cm.content.text_summary()))
            }
            _ => (Some(target_id.to_owned()), None),
        };

        let mut summary_entry: Option<BranchSummaryEntry> = None;
        if let Some(text) = &summary_text {
            let sid = self
                .with_session_manager_internal(|sm| {
                    sm.branch_with_summary(
                        new_leaf_id.as_deref(),
                        text.clone(),
                        summary_details,
                        Some(from_extension),
                        summary_usage,
                    )
                })
                .map_err(|e| e.to_string())?;
            if let Some(lbl) = &label {
                self.with_session_manager_internal(|sm| {
                    sm.append_label_change(&sid, Some(lbl.clone()))
                })
                .map_err(|e| e.to_string())?;
            }
            if let Some(SessionEntry::BranchSummary(bs)) =
                self.with_session_manager(|sm| sm.get_entry(&sid).cloned())
            {
                summary_entry = Some(bs);
            }
        } else if let Some(nl) = &new_leaf_id {
            self.with_session_manager_internal(|sm| sm.branch(nl))
                .map_err(|e| e.to_string())?;
        } else {
            self.with_session_manager_internal(SessionManager::reset_leaf);
        }

        if summary_text.is_none()
            && let Some(lbl) = label
        {
            self.with_session_manager_internal(|sm| sm.append_label_change(target_id, Some(lbl)))
                .map_err(|e| e.to_string())?;
        }

        self.refresh_finalized_context();
        self.restore_tools_from_transcript();

        let current_leaf = self.with_session_manager(|sm| sm.get_leaf_id().map(ToOwned::to_owned));
        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionTree {
                new_leaf_id: current_leaf,
                old_leaf_id,
                from_extension: summary_text.as_ref().map(|_| from_extension),
            })
            .await;

        Ok(NavigateTreeResult {
            editor_text,
            cancelled: false,
            aborted: false,
            summary_entry,
        })
    }

    /// 从指定节点派生新会话（同时使旧 `ExtensionContext` 失效并重新绑定新会话上下文）。
    pub async fn fork(&self, entry_id: &str, options: ForkSessionOptions) -> Result<bool, String> {
        self.ensure_external_open()?;
        let position = options
            .position
            .clone()
            .unwrap_or_else(|| "before".to_owned());

        if let SessionBeforeEventOutput::Fork(before_res) = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionBeforeFork {
                entry_id: entry_id.to_owned(),
                position: position.clone(),
            })
            .await
            && before_res.cancel
        {
            return Ok(false);
        }

        self.abort().await;

        let target_leaf = if position == "at" {
            let _ = self
                .with_session_manager(|sm| sm.get_entry(entry_id).cloned())
                .ok_or_else(|| format!("Entry {entry_id} not found"))?;
            Some(entry_id.to_owned())
        } else {
            let entry = self
                .with_session_manager(|sm| sm.get_entry(entry_id).cloned())
                .ok_or_else(|| format!("Entry {entry_id} not found"))?;
            entry.parent_id().map(ToOwned::to_owned)
        };

        let prev_file = self.session_file();
        if let Some(leaf) = &target_leaf {
            self.with_session_manager_internal(|sm| sm.create_branched_session(leaf))
                .map_err(|e| e.to_string())?;
        } else {
            let cwd = self.inner.cwd.clone();
            let dir = self.with_session_manager(|sm| sm.get_session_dir().to_path_buf());
            let new_sm = if self.with_session_manager(SessionManager::is_persisted) {
                SessionManager::create(
                    cwd,
                    dir,
                    Some(NewSessionOptions {
                        id: None,
                        parent_session: prev_file
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned()),
                    }),
                )
                .map_err(|e| e.to_string())?
            } else {
                SessionManager::in_memory(cwd, None, None).map_err(|e| e.to_string())?
            };
            *self.inner.session_manager.lock().expect("sm lock") = new_sm;
        }

        self.rotate_extension_runner_after_session_replacement("fork", prev_file)
            .await?;
        Ok(true)
    }

    pub async fn new_session(&self, parent_session: Option<String>) -> Result<bool, String> {
        self.ensure_external_open()?;
        if let SessionBeforeEventOutput::Switch(before_res) = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionBeforeSwitch {
                reason: "new".to_owned(),
                target_session_file: None,
            })
            .await
            && before_res.cancel
        {
            return Ok(false);
        }

        self.abort().await;

        let prev_file = self.session_file();
        let cwd = self.inner.cwd.clone();
        let (is_persisted, dir) =
            self.with_session_manager(|sm| (sm.is_persisted(), sm.get_session_dir().to_path_buf()));
        let new_sm = if is_persisted {
            SessionManager::create(
                cwd,
                dir,
                Some(NewSessionOptions {
                    id: None,
                    parent_session,
                }),
            )
            .map_err(|e| e.to_string())?
        } else {
            SessionManager::in_memory(cwd, None, None).map_err(|e| e.to_string())?
        };
        *self.inner.session_manager.lock().expect("sm lock") = new_sm;

        self.rotate_extension_runner_after_session_replacement("new", prev_file)
            .await?;
        Ok(true)
    }

    pub async fn switch_session(&self, session_path: impl Into<PathBuf>) -> Result<bool, String> {
        self.ensure_external_open()?;
        let target_path = session_path.into();

        if let SessionBeforeEventOutput::Switch(before_res) = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionBeforeSwitch {
                reason: "resume".to_owned(),
                target_session_file: Some(target_path.clone()),
            })
            .await
            && before_res.cancel
        {
            return Ok(false);
        }

        self.abort().await;

        let prev_file = self.session_file();
        let dir = self.with_session_manager(|sm| sm.get_session_dir().to_path_buf());
        let new_sm = SessionManager::open(&target_path, Some(dir), Some(self.inner.cwd.clone()))
            .map_err(|e| e.to_string())?;
        *self.inner.session_manager.lock().expect("sm lock") = new_sm;

        self.rotate_extension_runner_after_session_replacement("resume", prev_file)
            .await?;
        Ok(true)
    }

    /// Reload resources through the host's repeatable factories.
    pub async fn reload_resources(&self) -> Result<(), String> {
        self.start_reload(None).await
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.reload_resources().await
    }

    pub async fn reload_with_extensions(
        &self,
        extensions: Vec<Box<dyn Extension>>,
    ) -> Result<(), String> {
        self.start_reload(Some(extensions)).await
    }

    async fn start_reload(
        &self,
        extensions: Option<Vec<Box<dyn Extension>>>,
    ) -> Result<(), String> {
        self.ensure_external_open()?;
        let id = Arc::as_ptr(&self.inner) as usize;
        if RUN_SESSION
            .try_with(|current| *current == id)
            .unwrap_or(false)
            || SHUTDOWN_SESSION
                .try_with(|current| *current == id)
                .unwrap_or(false)
        {
            return Err("Cannot await reload from its own session callback".to_owned());
        }
        self.abort().await;
        let cancel = CancellationToken::new();
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            let mut slot = self.inner.reload_cancel.lock().expect("reload cancel lock");
            if slot.is_some() {
                return Err("Session reload in progress".to_owned());
            }
            *slot = Some(cancel.clone());
        }
        let session = self.clone();
        tokio::spawn(async move {
            let _active = ActiveReloadGuard {
                session: session.downgrade(),
            };
            RUN_SESSION
                .scope(id, session.reload_internal(extensions, cancel))
                .await
        })
        .await
        .map_err(|_| "Session reload task failed".to_owned())?
    }

    async fn reload_internal(
        &self,
        extensions: Option<Vec<Box<dyn Extension>>>,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let hook = self.inner.on_reload.read().expect("on reload lock").clone();
        let outcome = if let Some(extensions) = extensions {
            SessionReloadOutcome {
                extension_runner: ExtensionRunner::from_extensions(
                    self.inner.cwd.clone(),
                    extensions,
                )
                .map_err(|e| e.to_string())?,
                on_committed: None,
                skills: self.skills(),
                prompt_templates: self.prompt_templates(),
                enable_skill_commands: None,
                system_prompt_options: None,
            }
        } else if let Some(hook) = hook {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err("Session reload cancelled".to_owned()),
                result = hook() => result?,
            }
        } else {
            SessionReloadOutcome {
                extension_runner: self.extension_runner().clone_with_fresh_generation(),
                on_committed: None,
                skills: self.skills(),
                prompt_templates: self.prompt_templates(),
                enable_skill_commands: None,
                system_prompt_options: None,
            }
        };
        self.ensure_external_open()?;
        if cancel.is_cancelled() {
            return Err("Session reload cancelled".to_owned());
        }
        self.swap_extension_runner(outcome).await
    }

    async fn swap_extension_runner(&self, outcome: SessionReloadOutcome) -> Result<(), String> {
        let on_committed = outcome.on_committed;
        let before_invalidate = self
            .inner
            .before_session_invalidate
            .lock()
            .expect("before invalidate lock")
            .clone();
        if let Some(callback) = before_invalidate {
            callback();
        }
        {
            let _admission = self
                .inner
                .operation_admission
                .lock()
                .expect("admission lock");
            self.ensure_external_open()?;
            self.set_skills(outcome.skills);
            self.set_prompt_templates(outcome.prompt_templates);
            if let Some(enable) = outcome.enable_skill_commands {
                self.set_enable_skill_commands(enable);
            }
            if let Some(options) = outcome.system_prompt_options {
                self.set_base_system_prompt_options(options);
            }
            let new_runner = outcome.extension_runner;
            let old_runner = self.extension_runner();
            new_runner.inherit_ui_from(&old_runner);
            old_runner.invalidate(None);
            old_runner.detach_models();
            new_runner.bind_models(self.inner.models.clone());
            *self.inner.extension_runner.write().expect("runner lock") = new_runner;
            self.inner.session_generation.fetch_add(1, Ordering::SeqCst);
            self.inner
                .selection_generation
                .fetch_add(1, Ordering::SeqCst);
            self.bind_extension_runner();
            self.refresh_tool_registry(None);
            self.restore_tools_from_transcript();
            self.refresh_finalized_context();
        }
        if let Some(on_committed) = on_committed {
            on_committed(&self.extension_runner());
        }
        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionStart {
                reason: "reload".to_owned(),
                previous_session_file: self.session_file(),
            })
            .await;
        Ok(())
    }

    async fn rotate_extension_runner_after_session_replacement(
        &self,
        reason: &str,
        previous_session_file: Option<PathBuf>,
    ) -> Result<(), String> {
        // Invalidate captured handles before opening or reconciling the target log.
        self.extension_runner().invalidate(None);
        let log = self
            .with_session_manager(|sm| {
                self.inner
                    .execution_log_factory
                    .as_ref()
                    .map_or_else(|| Self::default_execution_log(sm), |factory| factory(sm))
            })
            .map_err(|err| {
                let msg = format!("Failed to open replacement session execution log: {err}");
                self.set_fatal_runtime_error(msg.clone());
                msg
            })?;
        *self.inner.execution_log.lock().expect("execution log lock") = log;
        let report = self.reconcile_startup().map_err(|err| {
            let msg = format!("Failed to reconcile replacement session: {err}");
            self.set_fatal_runtime_error(msg.clone());
            msg
        })?;
        *self
            .inner
            .startup_report
            .lock()
            .expect("startup report lock") = report;
        *self.inner.fatal_runtime_error.lock().expect("fatal lock") = None;
        *self
            .inner
            .last_assistant_message
            .lock()
            .expect("assistant lock") = None;
        *self
            .inner
            .failed_response
            .lock()
            .expect("failed response lock") = None;
        self.inner
            .pending_input_correlations
            .lock()
            .expect("input correlations lock")
            .clear();
        self.inner
            .pending_next_turn_messages
            .lock()
            .expect("next turn lock")
            .clear();
        self.inner
            .overflow_recovery_attempted
            .store(false, Ordering::SeqCst);
        self.inner.retry_attempt.store(0, Ordering::SeqCst);
        if let Some(runner) = self.inner.nested_runner.get() {
            runner.clear();
        }
        let (projection, branch) =
            self.with_session_manager(|sm| (sm.build_session_projection(), sm.get_branch(None)));
        if let Some(selection) =
            get_branch_selection(&branch, |p, m| self.inner.models.get_model(p, m))
            && let Some(model) = self
                .inner
                .models
                .get_model(&selection.provider, &selection.model_id)
        {
            self.inner.agent.set_model(model);
        }
        if let Some(level) = parse_model_thinking_level(&projection.thinking_level) {
            self.inner.agent.set_thinking_level(level);
        }
        // 使先前被捕获的旧 ExtensionContext / ExtensionRuntime 立即失效！
        let old_runner = self.extension_runner();
        old_runner.invalidate(None);

        let new_runner = old_runner.clone_with_fresh_generation();
        *self.inner.extension_runner.write().expect("runner lock") = new_runner;
        self.inner.session_generation.fetch_add(1, Ordering::SeqCst);
        self.inner
            .selection_generation
            .fetch_add(1, Ordering::SeqCst);
        self.bind_extension_runner();
        self.refresh_tool_registry(None);
        self.restore_tools_from_transcript();
        self.refresh_finalized_context();
        self.inner.agent.set_session_id(Some(self.session_id()));

        let _ = self
            .extension_runner()
            .emit_lifecycle(SessionLifecycleEvent::SessionStart {
                reason: reason.to_owned(),
                previous_session_file,
            })
            .await;
        Ok(())
    }

    /// 获取当前上下文用量（刚完成压缩且尚无压缩后新助手回复时，`tokens` 与 `percent` 返回 `None`）。
    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        if !self.has_configured_model() {
            return None;
        }
        let model = self.limits_model();
        let context_window = model.context_window;
        if context_window == 0 {
            return None;
        }

        let (projection, branch) =
            self.with_session_manager(|sm| (sm.build_session_projection(), sm.get_branch(None)));

        if let Some(latest_comp) = get_latest_compaction_entry(&branch) {
            let projected_asst_ids: BTreeSet<String> = projection
                .entries
                .iter()
                .filter(|e| {
                    e.messages.iter().any(|m| {
                        matches!(
                            m,
                            AgentMessage::Assistant(a)
                                if !matches!(a.stop_reason, StopReason::Aborted | StopReason::Error)
                                    && calculate_context_tokens(&a.usage) > 0
                        )
                    })
                })
                .map(|e| e.source_entry.id().to_owned())
                .collect();

            let comp_idx = branch.iter().position(|e| e.id() == latest_comp.id)?;
            let has_post_compaction_usage = branch[comp_idx + 1..]
                .iter()
                .any(|e| projected_asst_ids.contains(e.id()));
            if !has_post_compaction_usage {
                return Some(ContextUsage {
                    tokens: None,
                    context_window,
                    percent: None,
                });
            }
        }

        let estimate = estimate_projected_context_tokens(&projection, &branch);
        let percent = (estimate.tokens as f64 / context_window as f64) * 100.0;
        Some(ContextUsage {
            tokens: Some(estimate.tokens),
            context_window,
            percent: Some(percent),
        })
    }

    pub fn get_session_stats(&self) -> SessionStats {
        let entries = self.with_session_manager(SessionManager::get_entries);
        let mut user_messages = 0usize;
        let mut assistant_messages = 0usize;
        let mut tool_results = 0usize;
        let mut total_messages = 0usize;
        let mut tool_calls = 0usize;
        let mut totals = create_usage_totals();

        for entry in &entries {
            match entry {
                SessionEntry::Usage(u) => add_usage_to_totals(&mut totals, &u.usage),
                SessionEntry::BranchSummary(bs) => {
                    if let Some(u) = &bs.usage {
                        add_usage_to_totals(&mut totals, u);
                    }
                }
                SessionEntry::Compaction(c) => {
                    if let Some(u) = &c.usage {
                        add_usage_to_totals(&mut totals, u);
                    }
                }
                SessionEntry::Message(m) => {
                    total_messages += 1;
                    match &m.message {
                        AgentMessage::User(_) => user_messages += 1,
                        AgentMessage::ToolResult(tr) => {
                            tool_results += 1;
                            if let Some(u) = &tr.usage {
                                add_usage_to_totals(&mut totals, u);
                            }
                        }
                        AgentMessage::Assistant(a) => {
                            assistant_messages += 1;
                            tool_calls += a
                                .content
                                .iter()
                                .filter(|b| matches!(b, ContentBlock::ToolCall(_)))
                                .count();
                            add_usage_to_totals(&mut totals, &a.usage);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        SessionStats {
            session_file: self.session_file(),
            session_id: self.session_id(),
            user_messages,
            assistant_messages,
            tool_calls,
            tool_results,
            total_messages,
            input_tokens: totals.input,
            output_tokens: totals.output,
            cache_read_tokens: totals.cache_read,
            cache_write_tokens: totals.cache_write,
            total_tokens: totals.input + totals.output + totals.cache_read + totals.cache_write,
            cost: totals.cost,
            context_usage: self.get_context_usage(),
        }
    }

    pub fn get_slash_commands(&self) -> Vec<SlashCommandInfo> {
        let mut list: Vec<SlashCommandInfo> = self
            .extension_runner()
            .get_registered_commands()
            .into_iter()
            .map(|rc| SlashCommandInfo {
                name: rc.invocation_name,
                description: rc.command.description,
                source: SlashCommandSource::Extension,
                source_info: rc.command.source_info,
            })
            .collect();
        for tpl in self.prompt_templates() {
            list.push(SlashCommandInfo {
                name: tpl.name,
                description: Some(tpl.description),
                source: SlashCommandSource::Prompt,
                source_info: tpl.source_info,
            });
        }
        if self.enable_skill_commands() {
            for skill in self.skills() {
                list.push(SlashCommandInfo {
                    name: format!("skill:{}", skill.name),
                    description: Some(skill.description),
                    source: SlashCommandSource::Skill,
                    source_info: skill.source_info,
                });
            }
        }
        for b in BUILTIN_SLASH_COMMANDS {
            list.push(SlashCommandInfo {
                name: b.name.to_owned(),
                description: Some(b.description.to_owned()),
                source: SlashCommandSource::Prompt,
                source_info: SourceInfo::builtin(b.name),
            });
        }
        list
    }

    pub fn export_to_jsonl(
        &self,
        output_path: Option<&Path>,
    ) -> Result<PathBuf, SessionStoreError> {
        self.with_session_manager(|sm| export_session_to_jsonl(sm, output_path, None))
    }
}

fn strip_yaml_frontmatter(raw: &str) -> &str {
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let Some(rest) = trimmed
        .strip_prefix("---\r\n")
        .or_else(|| trimmed.strip_prefix("---\n"))
    else {
        return trimmed;
    };
    if let Some(idx) = rest.find("\n---") {
        let after_delim = &rest[idx + "\n---".len()..];
        if let Some(after_line) = after_delim
            .strip_prefix("\r\n")
            .or_else(|| after_delim.strip_prefix('\n'))
        {
            return after_line;
        }
        if after_delim.trim().is_empty() {
            return "";
        }
    }
    trimmed
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;

    #[test]
    fn cleanup_runs_when_last_inner_reference_is_released() {
        let models = Arc::new(Models::default());
        let model = pi_ai::faux::register_faux_provider(&models).model;
        let manager = SessionManager::in_memory("/inner-drop", None, None).unwrap();
        let session = AgentSession::new(AgentSessionOptions::new(
            "/inner-drop",
            model,
            models,
            manager,
        ))
        .unwrap();
        let token = session.extension_runner().runtime().cancellation_token();
        // Models a transient strong reference obtained by a Weak::upgrade.
        let inner = session.inner.clone();
        drop(session);
        assert!(!token.is_cancelled());
        drop(inner);
        assert!(token.is_cancelled());
    }
}

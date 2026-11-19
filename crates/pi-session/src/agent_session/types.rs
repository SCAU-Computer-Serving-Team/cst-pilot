use super::*;

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
    pub(super) fn from_u8(val: u8) -> Self {
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
    pub stream_fn: Option<AgentStreamFn>,
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

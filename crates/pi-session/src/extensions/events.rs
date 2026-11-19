use super::*;

// =============================================================================
// 3. Extension Events & Handler Signatures
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputSource {
    Interactive,
    Rpc,
    Extension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingInputBehavior {
    Steer,
    FollowUp,
}

#[derive(Debug, Clone)]
pub struct InputEvent {
    pub text: String,
    pub images: Option<Vec<ImageContent>>,
    pub source: InputSource,
    pub streaming_behavior: Option<StreamingInputBehavior>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputEventResult {
    Continue,
    Transform {
        text: String,
        images: Option<Vec<ImageContent>>,
    },
    Handled,
}

#[derive(Debug, Clone)]
pub enum BeforeAgentStartTrigger {
    User {
        prompt: String,
        images: Option<Vec<ImageContent>>,
    },
    Custom {
        message: crate::messages::CustomMessage,
    },
}

/// Live prompt options shared by an event and its context. Update callbacks run
/// on a snapshot without holding the lock; changes become visible on return.
#[derive(Debug, Clone)]
pub struct BeforeAgentStartPromptOptions {
    pub(super) inner: Arc<Mutex<NormalizedBuildSystemPromptOptions>>,
}

impl BeforeAgentStartPromptOptions {
    pub fn snapshot(&self) -> NormalizedBuildSystemPromptOptions {
        self.inner.lock().expect("prompt options lock").clone()
    }

    pub fn update(&self, update: impl FnOnce(&mut NormalizedBuildSystemPromptOptions)) {
        let mut options = self.snapshot();
        update(&mut options);
        *self.inner.lock().expect("prompt options lock") = options;
    }

    pub(super) fn render(&self) -> String {
        build_system_prompt(&self.snapshot().to_build_options()).unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct BeforeAgentStartEvent {
    pub trigger: BeforeAgentStartTrigger,
    pub system_prompt_options: BeforeAgentStartPromptOptions,
}

impl BeforeAgentStartEvent {
    pub fn system_prompt(&self) -> String {
        self.system_prompt_options.render()
    }
}

#[derive(Debug, Clone, Default)]
pub struct BeforeAgentStartCustomMessageDraft {
    pub custom_type: String,
    pub content: CustomMessageContent,
    pub display: bool,
    pub details: Option<JsonValue>,
}

#[derive(Debug, Clone, Default)]
pub struct BeforeAgentStartEventResult {
    pub message: Option<BeforeAgentStartCustomMessageDraft>,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextEvent {
    pub messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone)]
pub struct ContextWithSystemEvent {
    pub messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone, Default)]
pub struct ContextEventResult {
    pub messages: Option<Vec<AgentMessage>>,
}

#[derive(Debug, Clone)]
pub struct BeforeProviderRequestEvent {
    pub payload: JsonValue,
}

#[derive(Debug, Clone)]
pub struct BeforeProviderHeadersEvent {
    pub headers: ProviderHeaders,
}

#[derive(Debug, Clone)]
pub struct AfterProviderResponseEvent {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct UserBashEvent {
    pub command: String,
    pub exclude_from_context: bool,
    pub cwd: String,
}

#[derive(Clone)]
pub enum UserBashEventResult {
    Operations(BashExecutor),
    Result(BashResult),
}

#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: JsonValue,
    pub parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallEventResult {
    pub block: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: JsonValue,
    pub content: Vec<UserContentBlock>,
    pub details: Option<JsonValue>,
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    pub usage: Option<Usage>,
    pub parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolResultEventResult {
    pub content: Option<Vec<UserContentBlock>>,
    pub details: Option<JsonValue>,
    pub structured_content: Option<JsonValue>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone)]
pub struct MessageEndEvent {
    pub message: AgentMessage,
}

#[derive(Debug, Clone)]
pub struct MessageEndEventResult {
    pub message: Option<AgentMessage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryContextPreview {
    pub messages: Vec<AgentMessage>,
    pub estimated_tokens: u64,
    pub can_continue: bool,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum BoundaryEventKind {
    TurnEnd {
        turn_index: usize,
        message: AgentMessage,
        tool_results: Vec<pi_ai::ToolResultMessage>,
    },
    AgentBeforeSettle {
        messages: Vec<AgentMessage>,
    },
}

#[derive(Debug, Clone)]
pub struct BoundaryEvent {
    pub kind: BoundaryEventKind,
    pub entries: Vec<SessionBoundaryDraft>,
    pub r#continue: bool,
    pub context: BoundaryContextPreview,
}

#[derive(Debug, Clone, Default)]
pub struct BoundaryResult {
    pub entries: Option<Vec<SessionBoundaryDraft>>,
    pub r#continue: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionBeforeSwitchResult {
    pub cancel: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SessionBeforeForkResult {
    pub cancel: bool,
    pub skip_conversation_restore: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SessionBeforeCompactResult {
    pub cancel: bool,
    pub compaction: Option<CompactionResult>,
}

#[derive(Debug, Clone)]
pub struct TreePreparation {
    pub target_id: String,
    pub old_leaf_id: Option<String>,
    pub common_ancestor_id: Option<String>,
    pub entries_to_summarize: Vec<SessionEntry>,
    pub user_wants_summary: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionBeforeTreeResult {
    pub cancel: bool,
    pub summary: Option<BranchSummaryResult>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum SessionLifecycleEvent {
    SessionStart {
        reason: String,
        previous_session_file: Option<PathBuf>,
    },
    SessionBeforeSwitch {
        reason: String,
        target_session_file: Option<PathBuf>,
    },
    SessionBeforeFork {
        entry_id: String,
        position: String,
    },
    SessionBeforeCompact {
        preparation: CompactionPreparation,
        branch_entries: Vec<SessionEntry>,
        custom_instructions: Option<String>,
    },
    SessionCompact {
        compaction_entry: CompactionEntry,
        from_extension: bool,
    },
    SessionCompactFailed {
        reason: CompactionReason,
        error_message: String,
        will_retry: bool,
    },
    SessionBeforeTree {
        preparation: TreePreparation,
    },
    SessionTree {
        new_leaf_id: Option<String>,
        old_leaf_id: Option<String>,
        from_extension: Option<bool>,
    },
    SessionInfoChanged {
        session_id: String,
        session_file: Option<PathBuf>,
        name: Option<String>,
    },
    SessionShutdown {
        reason: String,
        target_session_file: Option<PathBuf>,
        sigint: bool,
    },
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    AgentSettled {
        messages: Vec<AgentMessage>,
    },
    TurnStart {
        turn_index: usize,
        timestamp: u64,
    },
    MessageStart {
        message: AgentMessage,
    },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        partial_result: AgentToolResult,
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
        is_error: bool,
        duration_ms: Option<u64>,
        parent_tool_call_id: Option<String>,
    },
    ModelSelect {
        model: Model,
        previous_model: Option<Model>,
        source: String,
    },
    UiPromptStart {
        kind: UiPromptKind,
        title: Option<String>,
    },
    UiPromptEnd {
        kind: UiPromptKind,
        title: Option<String>,
    },
}

impl SessionLifecycleEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            SessionLifecycleEvent::SessionStart { .. } => "session_start",
            SessionLifecycleEvent::SessionBeforeSwitch { .. } => "session_before_switch",
            SessionLifecycleEvent::SessionBeforeFork { .. } => "session_before_fork",
            SessionLifecycleEvent::SessionBeforeCompact { .. } => "session_before_compact",
            SessionLifecycleEvent::SessionCompact { .. } => "session_compact",
            SessionLifecycleEvent::SessionCompactFailed { .. } => "session_compact_failed",
            SessionLifecycleEvent::SessionBeforeTree { .. } => "session_before_tree",
            SessionLifecycleEvent::SessionTree { .. } => "session_tree",
            SessionLifecycleEvent::SessionInfoChanged { .. } => "session_info_changed",
            SessionLifecycleEvent::SessionShutdown { .. } => "session_shutdown",
            SessionLifecycleEvent::AgentStart => "agent_start",
            SessionLifecycleEvent::AgentEnd { .. } => "agent_end",
            SessionLifecycleEvent::AgentSettled { .. } => "agent_settled",
            SessionLifecycleEvent::TurnStart { .. } => "turn_start",
            SessionLifecycleEvent::MessageStart { .. } => "message_start",
            SessionLifecycleEvent::MessageUpdate { .. } => "message_update",
            SessionLifecycleEvent::ToolExecutionStart { .. } => "tool_execution_start",
            SessionLifecycleEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            SessionLifecycleEvent::ToolExecutionEnd { .. } => "tool_execution_end",
            SessionLifecycleEvent::ModelSelect { .. } => "model_select",
            SessionLifecycleEvent::UiPromptStart { .. } => "ui_prompt_start",
            SessionLifecycleEvent::UiPromptEnd { .. } => "ui_prompt_end",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SessionBeforeEventOutput {
    None,
    Switch(SessionBeforeSwitchResult),
    Fork(SessionBeforeForkResult),
    Compact(SessionBeforeCompactResult),
    Tree(SessionBeforeTreeResult),
}

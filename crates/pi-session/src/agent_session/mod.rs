use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use pi_agent::{
    AfterToolCallContext, AfterToolCallResult, Agent, AgentContext, AgentEvent, AgentInitialState,
    AgentLoopTurnUpdate, AgentMessage, AgentOptions, AgentRequestUpdate, AgentState, AgentStreamFn,
    AgentTool, AgentToolCallOutcome, AgentTurnDecision, BeforeToolCallContext,
    BeforeToolCallResult, BoxFuture, PrepareNextTurnContext, PrepareRequestContext,
    RunToolCallOptions, ToolExecutionMode, ToolReplayPolicy, get_agent_current_system_message,
    get_agent_current_tools, models_stream_fn, run_tool_call,
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

mod compaction;
mod hooks;
mod navigation;
mod prompt;
mod types;

pub use types::*;

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

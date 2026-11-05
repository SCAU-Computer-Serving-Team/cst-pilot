use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures_util::FutureExt;
use pi_ai::{
    CacheRetention, ClientIdentity, Model, ModelThinkingLevel, OnHeadersFn, OnPayloadFn,
    OnProviderStreamEventFn, OnResponseFn, ProviderHeaders, SamplingParams, SystemMessage,
    ThinkingBudgets, Tool, ToolChoice, Transport, create_initial_system_message,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{
    AgentEventSink, panic_payload_to_string, run_agent_loop_continue, try_run_agent_loop,
};
use crate::types::{
    AfterToolCallFn, AgentContext, AgentEvent, AgentInitialState, AgentLoopConfig, AgentMessage,
    AgentState, AgentTool, BeforeToolCallFn, BoxFuture, ConvertToLlmFn, FinalizeMessageFn,
    FinishTurnFn, GetApiKeyFn, PrepareNextTurnFn, PrepareRequestFn, PromptInput, QueueMode,
    StreamFn, ToolExecutionMode, TransformContextFn, create_error_assistant_message,
    default_agent_model, default_convert_to_llm, get_agent_current_system_message,
    get_agent_current_system_prompt,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
pub struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    pub mode: QueueMode,
}

impl PendingMessageQueue {
    pub fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    pub fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    pub fn peek(&self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => self.messages.clone(),
            QueueMode::OneAtATime => self.messages.first().cloned().into_iter().collect(),
        }
    }

    pub fn drain(&mut self) -> Vec<AgentMessage> {
        let drained = self.peek();
        if !drained.is_empty() {
            self.messages.drain(..drained.len());
        }
        drained
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

pub type AgentListenerFn =
    Arc<dyn Fn(AgentEvent, CancellationToken) -> BoxFuture<'static, ()> + Send + Sync>;

/// 构造有状态 `Agent` 的配置项（对照 `AgentOptions`）。
#[derive(Clone)]
pub struct AgentOptions {
    pub initial_state: Option<AgentInitialState>,
    pub convert_to_llm: Option<ConvertToLlmFn>,
    pub transform_context: Option<TransformContextFn>,
    pub stream_fn: StreamFn,
    pub get_api_key: Option<GetApiKeyFn>,
    pub on_headers: Option<OnHeadersFn>,
    pub on_payload: Option<OnPayloadFn>,
    pub on_response: Option<OnResponseFn>,
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    pub finalize_message: Option<FinalizeMessageFn>,
    pub finish_turn: Option<FinishTurnFn>,
    pub prepare_request: Option<PrepareRequestFn>,
    pub prepare_next_turn: Option<PrepareNextTurnFn>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub identity: Option<ClientIdentity>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub headers: Option<ProviderHeaders>,
    pub tool_choice: Option<ToolChoice>,
}

impl AgentOptions {
    pub fn new(stream_fn: StreamFn) -> Self {
        Self {
            initial_state: None,
            convert_to_llm: None,
            transform_context: None,
            stream_fn,
            get_api_key: None,
            on_headers: None,
            on_payload: None,
            on_response: None,
            on_provider_stream_event: None,
            before_tool_call: None,
            after_tool_call: None,
            finalize_message: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: None,
            steering_mode: None,
            follow_up_mode: None,
            session_id: None,
            thinking_budgets: None,
            transport: None,
            cache_retention: None,
            max_retry_delay_ms: None,
            tool_execution: None,
            identity: None,
            temperature: None,
            sampling_params: None,
            max_tokens: None,
            headers: None,
            tool_choice: None,
        }
    }
}

struct MutableAgentState {
    model: Model,
    thinking_level: ModelThinkingLevel,
    tools: Vec<AgentTool>,
    messages: Vec<AgentMessage>,
    is_streaming: bool,
    streaming_message: Option<AgentMessage>,
    pending_tool_calls: BTreeSet<String>,
    error_message: Option<String>,
}

struct DualQueues {
    steering: PendingMessageQueue,
    follow_up: PendingMessageQueue,
}

#[derive(Clone)]
struct ActiveRun {
    run_id: u64,
    cancel_token: CancellationToken,
    idle_notify: Arc<Notify>,
    saw_agent_start: bool,
    turn_open: bool,
    saw_agent_end: bool,
}

struct PromptWaitGuard {
    agent: Agent,
    run_id: u64,
    completed: bool,
}

impl Drop for PromptWaitGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.agent.abort_run(self.run_id);
        }
    }
}

struct AgentConfigFields {
    convert_to_llm: ConvertToLlmFn,
    transform_context: Option<TransformContextFn>,
    stream_fn: StreamFn,
    get_api_key: Option<GetApiKeyFn>,
    on_headers: Option<OnHeadersFn>,
    on_payload: Option<OnPayloadFn>,
    on_response: Option<OnResponseFn>,
    on_provider_stream_event: Option<OnProviderStreamEventFn>,
    before_tool_call: Option<BeforeToolCallFn>,
    after_tool_call: Option<AfterToolCallFn>,
    finalize_message: Option<FinalizeMessageFn>,
    finish_turn: Option<FinishTurnFn>,
    prepare_request: Option<PrepareRequestFn>,
    prepare_next_turn: Option<PrepareNextTurnFn>,
    session_id: Option<String>,
    thinking_budgets: Option<ThinkingBudgets>,
    transport: Transport,
    cache_retention: Option<CacheRetention>,
    max_retry_delay_ms: Option<u64>,
    tool_execution: ToolExecutionMode,
    identity: Option<ClientIdentity>,
    temperature: Option<f64>,
    sampling_params: Option<SamplingParams>,
    max_tokens: Option<u64>,
    headers: Option<ProviderHeaders>,
    tool_choice: Option<ToolChoice>,
}

struct AgentInner {
    state: RwLock<MutableAgentState>,
    queues: Mutex<DualQueues>,
    active_run: Mutex<Option<ActiveRun>>,
    next_run_id: AtomicU64,
    listeners: Mutex<Vec<(u64, AgentListenerFn)>>,
    next_listener_id: AtomicU64,
    config: RwLock<AgentConfigFields>,
}

/// 有状态 Agent 封装（对照 `@earendil-works/pi-agent-core` 的 `Agent` 类）。
///
/// 内部采用分离的细粒度锁（状态、队列、活跃运行、监听器各自独立加锁），
/// 且严禁在持有任何锁守卫跨越 `.await` 点调用监听器、钩子或模型流函数，
/// 从而支持在监听器或工具钩子中安全重入调用 `steer()`、`follow_up()`、`abort()` 与 `state()`。
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

impl Agent {
    pub fn new(options: AgentOptions) -> Self {
        let init = options.initial_state.unwrap_or_default();
        let tools = init.tools.unwrap_or_default();
        let mut messages = init.messages.unwrap_or_default();
        let tool_decls: Vec<Tool> = tools.iter().map(AgentTool::to_tool_declaration).collect();
        let initial_sys: Option<SystemMessage> =
            create_initial_system_message(init.system_prompt.as_deref(), Some(&tool_decls));
        if messages.first().map(AgentMessage::role) != Some("system")
            && let Some(sys) = initial_sys
        {
            messages.insert(0, AgentMessage::System(sys));
        }

        let mutable_state = MutableAgentState {
            model: init.model.unwrap_or_else(default_agent_model),
            thinking_level: init.thinking_level.unwrap_or(ModelThinkingLevel::Off),
            tools,
            messages,
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: BTreeSet::new(),
            error_message: None,
        };

        let convert_to_llm = options.convert_to_llm.unwrap_or_else(|| {
            Arc::new(|msgs| Box::pin(async move { default_convert_to_llm(&msgs) }))
        });

        let config_fields = AgentConfigFields {
            convert_to_llm,
            transform_context: options.transform_context,
            stream_fn: options.stream_fn,
            get_api_key: options.get_api_key,
            on_headers: options.on_headers,
            on_payload: options.on_payload,
            on_response: options.on_response,
            on_provider_stream_event: options.on_provider_stream_event,
            before_tool_call: options.before_tool_call,
            after_tool_call: options.after_tool_call,
            finalize_message: options.finalize_message,
            finish_turn: options.finish_turn,
            prepare_request: options.prepare_request,
            prepare_next_turn: options.prepare_next_turn,
            session_id: options.session_id,
            thinking_budgets: options.thinking_budgets,
            transport: options.transport.unwrap_or(Transport::Auto),
            cache_retention: options.cache_retention,
            max_retry_delay_ms: options.max_retry_delay_ms,
            tool_execution: options
                .tool_execution
                .unwrap_or(ToolExecutionMode::Parallel),
            identity: options.identity,
            temperature: options.temperature,
            sampling_params: options.sampling_params,
            max_tokens: options.max_tokens,
            headers: options.headers,
            tool_choice: options.tool_choice,
        };

        Self {
            inner: Arc::new(AgentInner {
                state: RwLock::new(mutable_state),
                queues: Mutex::new(DualQueues {
                    steering: PendingMessageQueue::new(
                        options.steering_mode.unwrap_or(QueueMode::OneAtATime),
                    ),
                    follow_up: PendingMessageQueue::new(
                        options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
                    ),
                }),
                active_run: Mutex::new(None),
                next_run_id: AtomicU64::new(1),
                listeners: Mutex::new(Vec::new()),
                next_listener_id: AtomicU64::new(1),
                config: RwLock::new(config_fields),
            }),
        }
    }

    /// 订阅 Agent 生命周期事件，返回取消订阅闭包。
    pub fn subscribe(&self, listener: AgentListenerFn) -> Box<dyn FnOnce() + Send + Sync> {
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut guard = self
                .inner
                .listeners
                .lock()
                .expect("listeners lock poisoned");
            guard.push((id, listener));
        }
        let inner = self.inner.clone();
        Box::new(move || {
            let mut guard = inner.listeners.lock().expect("listeners lock poisoned");
            guard.retain(|(entry_id, _)| *entry_id != id);
        })
    }

    /// 获取当前状态快照（`system_prompt` 通过实时重放转录本中的系统消息计算得出）。
    pub fn state(&self) -> AgentState {
        let guard = self.inner.state.read().expect("state lock poisoned");
        AgentState {
            system_prompt: get_agent_current_system_prompt(&guard.messages),
            model: guard.model.clone(),
            thinking_level: guard.thinking_level,
            tools: guard.tools.clone(),
            messages: guard.messages.clone(),
            is_streaming: guard.is_streaming,
            streaming_message: guard.streaming_message.clone(),
            pending_tool_calls: guard.pending_tool_calls.clone(),
            error_message: guard.error_message.clone(),
        }
    }

    pub fn set_model(&self, model: Model) {
        let mut guard = self.inner.state.write().expect("state lock poisoned");
        guard.model = model;
    }

    pub fn set_thinking_level(&self, level: ModelThinkingLevel) {
        let mut guard = self.inner.state.write().expect("state lock poisoned");
        guard.thinking_level = level;
    }

    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        let mut guard = self.inner.state.write().expect("state lock poisoned");
        guard.tools = tools;
    }

    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        let mut guard = self.inner.state.write().expect("state lock poisoned");
        guard.messages = messages;
    }

    pub fn set_tool_execution(&self, mode: ToolExecutionMode) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.tool_execution = mode;
    }

    pub fn tool_execution(&self) -> ToolExecutionMode {
        self.inner
            .config
            .read()
            .expect("config lock poisoned")
            .tool_execution
    }

    pub fn set_before_tool_call(&self, hook: Option<BeforeToolCallFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.before_tool_call = hook;
    }

    pub fn set_after_tool_call(&self, hook: Option<AfterToolCallFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.after_tool_call = hook;
    }

    pub fn set_finalize_message(&self, hook: Option<FinalizeMessageFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.finalize_message = hook;
    }

    pub fn set_convert_to_llm(&self, hook: ConvertToLlmFn) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.convert_to_llm = hook;
    }

    pub fn set_transform_context(&self, hook: Option<TransformContextFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.transform_context = hook;
    }

    pub fn set_stream_fn(&self, stream_fn: StreamFn) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.stream_fn = stream_fn;
    }

    pub fn stream_fn(&self) -> StreamFn {
        self.inner
            .config
            .read()
            .expect("config lock poisoned")
            .stream_fn
            .clone()
    }

    pub fn set_get_api_key(&self, hook: Option<GetApiKeyFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.get_api_key = hook;
    }

    pub fn set_on_headers(&self, hook: Option<OnHeadersFn>) {
        self.inner
            .config
            .write()
            .expect("config lock poisoned")
            .on_headers = hook;
    }

    pub fn set_on_payload(&self, hook: Option<OnPayloadFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.on_payload = hook;
    }

    pub fn set_on_response(&self, hook: Option<OnResponseFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.on_response = hook;
    }

    pub fn set_on_provider_stream_event(&self, hook: Option<OnProviderStreamEventFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.on_provider_stream_event = hook;
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner
            .config
            .read()
            .expect("config lock poisoned")
            .session_id
            .clone()
    }

    pub fn set_session_id(&self, session_id: Option<String>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.session_id = session_id;
    }

    /// 供嵌套工具执行器等外部协作模块向当前运行（或空闲订阅器）发送事件。
    pub async fn emit_external_event(&self, event: AgentEvent) {
        let run_info = {
            let guard = self
                .inner
                .active_run
                .lock()
                .expect("active_run lock poisoned");
            guard.as_ref().map(|r| (r.run_id, r.cancel_token.clone()))
        };
        if let Some((run_id, _)) = run_info {
            self.process_event(run_id, event).await;
        } else {
            let token = CancellationToken::new();
            let listeners: Vec<AgentListenerFn> = {
                let guard = self
                    .inner
                    .listeners
                    .lock()
                    .expect("listeners lock poisoned");
                guard.iter().map(|(_, f)| f.clone()).collect()
            };
            for listener in listeners {
                let ev = event.clone();
                let t = token.clone();
                if let Ok(fut) = catch_unwind(AssertUnwindSafe(|| listener(ev, t))) {
                    let _ = AssertUnwindSafe(fut).catch_unwind().await;
                }
            }
        }
    }

    pub fn set_finish_turn(&self, hook: Option<FinishTurnFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.finish_turn = hook;
    }

    pub fn set_prepare_request(&self, hook: Option<PrepareRequestFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.prepare_request = hook;
    }

    pub fn set_prepare_next_turn(&self, hook: Option<PrepareNextTurnFn>) {
        let mut guard = self.inner.config.write().expect("config lock poisoned");
        guard.prepare_next_turn = hook;
    }

    pub fn steering_mode(&self) -> QueueMode {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .steering
            .mode
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .steering
            .mode = mode;
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .follow_up
            .mode
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .follow_up
            .mode = mode;
    }

    pub fn steer(&self, message: impl Into<AgentMessage>) {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .steering
            .enqueue(message.into());
    }

    pub fn follow_up(&self, message: impl Into<AgentMessage>) {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .follow_up
            .enqueue(message.into());
    }

    pub fn clear_steering_queue(&self) {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .steering
            .clear();
    }

    pub fn clear_follow_up_queue(&self) {
        self.inner
            .queues
            .lock()
            .expect("queues lock poisoned")
            .follow_up
            .clear();
    }

    pub fn clear_all_queues(&self) {
        let mut guard = self.inner.queues.lock().expect("queues lock poisoned");
        guard.steering.clear();
        guard.follow_up.clear();
    }

    pub fn has_queued_messages(&self) -> bool {
        let guard = self.inner.queues.lock().expect("queues lock poisoned");
        guard.steering.has_items() || guard.follow_up.has_items()
    }

    pub fn peek_queued_messages(&self) -> Vec<AgentMessage> {
        let guard = self.inner.queues.lock().expect("queues lock poisoned");
        let steering = guard.steering.peek();
        if !steering.is_empty() {
            steering
        } else {
            guard.follow_up.peek()
        }
    }

    pub fn cancel_token(&self) -> Option<CancellationToken> {
        self.inner
            .active_run
            .lock()
            .expect("active_run lock poisoned")
            .as_ref()
            .map(|r| r.cancel_token.clone())
    }

    pub fn abort(&self) {
        let token = self.cancel_token();
        if let Some(t) = token {
            t.cancel();
        }
    }

    fn abort_run(&self, run_id: u64) {
        let token = {
            let guard = self
                .inner
                .active_run
                .lock()
                .expect("active_run lock poisoned");
            guard
                .as_ref()
                .filter(|r| r.run_id == run_id)
                .map(|r| r.cancel_token.clone())
        };
        if let Some(t) = token {
            t.cancel();
        }
    }

    /// 等待当前活跃运行及其全部 `AgentEnd` 异步监听器结算完成。
    pub async fn wait_for_idle(&self) {
        loop {
            let notify_opt = {
                let guard = self
                    .inner
                    .active_run
                    .lock()
                    .expect("active_run lock poisoned");
                guard.as_ref().map(|r| (r.run_id, r.idle_notify.clone()))
            };
            let Some((run_id, notify)) = notify_opt else {
                return;
            };
            let notified = notify.notified();
            let current_run_id = self
                .inner
                .active_run
                .lock()
                .expect("active_run lock poisoned")
                .as_ref()
                .map(|run| run.run_id);
            if current_run_id != Some(run_id) {
                continue;
            }
            notified.await;
        }
    }

    /// 清空对话历史与待发队列，同时保留重放后的基准系统提示词与已声明工具。
    pub fn reset(&self) -> Result<(), String> {
        let run_guard = self
            .inner
            .active_run
            .lock()
            .expect("active_run lock poisoned");
        if run_guard.is_some() {
            return Err(
                "Agent is already processing. Wait for completion before resetting.".to_owned(),
            );
        }

        {
            let mut state_guard = self.inner.state.write().expect("state lock poisoned");
            let baseline = get_agent_current_system_message(&state_guard.messages);
            state_guard.messages = baseline
                .map(|s| vec![AgentMessage::System(s)])
                .unwrap_or_default();
            state_guard.is_streaming = false;
            state_guard.streaming_message = None;
            state_guard.pending_tool_calls.clear();
            state_guard.error_message = None;
        }
        self.clear_all_queues();
        drop(run_guard);
        Ok(())
    }

    /// 发起新提示词运行。
    pub async fn prompt(&self, input: impl Into<PromptInput>) -> Result<(), String> {
        if self
            .inner
            .active_run
            .lock()
            .expect("active_run lock poisoned")
            .is_some()
        {
            return Err(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
                    .to_owned(),
            );
        }
        let messages = input.into().into_messages();
        self.run_prompt_messages(messages, false).await
    }

    /// 从当前转录本尾部继续运行（对照上游 agent.continue）。
    pub async fn continue_run(&self) -> Result<(), String> {
        self.run_continuation().await
    }

    async fn run_prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
    ) -> Result<(), String> {
        let agent_self = self.clone();
        self.run_with_lifecycle(move || {
            let context_snapshot = agent_self.create_context_snapshot();
            let (loop_config, stream_fn) =
                agent_self.create_loop_config(skip_initial_steering_poll);
            Ok(move |run_id, cancel_token| async move {
                let emit = agent_self.event_sink(run_id);
                try_run_agent_loop(
                    messages,
                    context_snapshot,
                    loop_config,
                    emit,
                    Some(cancel_token),
                    stream_fn,
                )
                .await
                .map(|_| ())
            })
        })
        .await
    }

    async fn run_continuation(&self) -> Result<(), String> {
        let agent_self = self.clone();
        self.run_with_lifecycle(move || {
            let context_snapshot = agent_self.create_context_snapshot();
            if context_snapshot.messages.is_empty()
                || context_snapshot
                    .messages
                    .iter()
                    .all(|m| m.role() == "system")
            {
                return Err("No messages to continue from".to_owned());
            }
            // 先获得运行写权再消费队列；并发调用被拒绝时保留全部待发消息。
            let (prompts, skip_initial_steering_poll) = if context_snapshot
                .messages
                .last()
                .is_some_and(|m| m.role() == "assistant")
            {
                let mut queues = agent_self
                    .inner
                    .queues
                    .lock()
                    .expect("queues lock poisoned");
                let steering = queues.steering.drain();
                if !steering.is_empty() {
                    (Some(steering), true)
                } else {
                    let follow_up = queues.follow_up.drain();
                    if follow_up.is_empty() {
                        return Err("Cannot continue from message role: assistant".to_owned());
                    }
                    (Some(follow_up), false)
                }
            } else {
                (None, false)
            };
            let (loop_config, stream_fn) =
                agent_self.create_loop_config(skip_initial_steering_poll);
            Ok(move |run_id, cancel_token| async move {
                let emit = agent_self.event_sink(run_id);
                if let Some(messages) = prompts {
                    try_run_agent_loop(
                        messages,
                        context_snapshot,
                        loop_config,
                        emit,
                        Some(cancel_token),
                        stream_fn,
                    )
                    .await
                    .map(|_| ())
                } else {
                    run_agent_loop_continue(
                        context_snapshot,
                        loop_config,
                        emit,
                        Some(cancel_token),
                        stream_fn,
                    )
                    .await
                    .map(|_| ())
                }
            })
        })
        .await
    }

    fn event_sink(&self, run_id: u64) -> AgentEventSink {
        let agent = self.clone();
        Arc::new(move |event| {
            let agent = agent.clone();
            Box::pin(async move { agent.process_event(run_id, event).await })
        })
    }

    fn create_context_snapshot(&self) -> AgentContext {
        let guard = self.inner.state.read().expect("state lock poisoned");
        AgentContext {
            messages: guard.messages.clone(),
            tools: guard.tools.clone(),
        }
    }

    fn create_loop_config(&self, skip_initial_steering_poll: bool) -> (AgentLoopConfig, StreamFn) {
        let (model, thinking_level) = {
            let s = self.inner.state.read().expect("state lock poisoned");
            (s.model.clone(), s.thinking_level)
        };
        let cfg = self.inner.config.read().expect("config lock poisoned");
        let reasoning = match thinking_level {
            ModelThinkingLevel::Off => None,
            ModelThinkingLevel::Minimal => Some(pi_ai::ThinkingLevel::Minimal),
            ModelThinkingLevel::Low => Some(pi_ai::ThinkingLevel::Low),
            ModelThinkingLevel::Medium => Some(pi_ai::ThinkingLevel::Medium),
            ModelThinkingLevel::High => Some(pi_ai::ThinkingLevel::High),
            ModelThinkingLevel::XHigh => Some(pi_ai::ThinkingLevel::XHigh),
            ModelThinkingLevel::Max => Some(pi_ai::ThinkingLevel::Max),
        };

        let skip_flag = Arc::new(Mutex::new(skip_initial_steering_poll));
        let inner_for_steer = self.inner.clone();
        let get_steering: crate::types::GetQueuedMessagesFn = Arc::new(move || {
            let skip_flag = skip_flag.clone();
            let inner = inner_for_steer.clone();
            Box::pin(async move {
                {
                    let mut skip = skip_flag.lock().expect("skip lock poisoned");
                    if *skip {
                        *skip = false;
                        return Vec::new();
                    }
                }
                let mut q = inner.queues.lock().expect("queues lock poisoned");
                q.steering.drain()
            })
        });

        let inner_for_follow = self.inner.clone();
        let get_follow_up: crate::types::GetQueuedMessagesFn = Arc::new(move || {
            let inner = inner_for_follow.clone();
            Box::pin(async move {
                let mut q = inner.queues.lock().expect("queues lock poisoned");
                q.follow_up.drain()
            })
        });

        let stream_fn = cfg.stream_fn.clone();
        let loop_config = AgentLoopConfig {
            model,
            reasoning,
            temperature: cfg.temperature,
            sampling_params: cfg.sampling_params.clone(),
            max_tokens: cfg.max_tokens,
            api_key: None,
            headers: cfg.headers.clone(),
            transport: Some(cfg.transport),
            cache_retention: cfg.cache_retention,
            session_id: cfg.session_id.clone(),
            thinking_budgets: cfg.thinking_budgets.clone(),
            max_retry_delay_ms: cfg.max_retry_delay_ms,
            tool_choice: cfg.tool_choice,
            identity: cfg.identity.clone(),
            on_headers: cfg.on_headers.clone(),
            on_payload: cfg.on_payload.clone(),
            on_response: cfg.on_response.clone(),
            on_provider_stream_event: cfg.on_provider_stream_event.clone(),
            convert_to_llm: cfg.convert_to_llm.clone(),
            transform_context: cfg.transform_context.clone(),
            get_api_key: cfg.get_api_key.clone(),
            finish_turn: cfg.finish_turn.clone(),
            prepare_request: cfg.prepare_request.clone(),
            prepare_next_turn: cfg.prepare_next_turn.clone(),
            get_steering_messages: Some(get_steering),
            get_follow_up_messages: Some(get_follow_up),
            tool_execution: cfg.tool_execution,
            before_tool_call: cfg.before_tool_call.clone(),
            after_tool_call: cfg.after_tool_call.clone(),
            finalize_message: cfg.finalize_message.clone(),
        };

        (loop_config, stream_fn)
    }

    async fn run_with_lifecycle<P, F, Fut>(&self, prepare: P) -> Result<(), String>
    where
        P: FnOnce() -> Result<F, String> + Send,
        F: FnOnce(u64, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let run_id = self.inner.next_run_id.fetch_add(1, Ordering::Relaxed);
        let cancel_token = CancellationToken::new();
        let idle_notify = Arc::new(Notify::new());

        let executor = {
            let mut run_guard = self
                .inner
                .active_run
                .lock()
                .expect("active_run lock poisoned");
            if run_guard.is_some() {
                return Err("Agent is already processing.".to_owned());
            }
            // 准备阶段仅同步读取状态及消费队列，不调用用户钩子。
            let executor = prepare()?;
            *run_guard = Some(ActiveRun {
                run_id,
                cancel_token: cancel_token.clone(),
                idle_notify: idle_notify.clone(),
                saw_agent_start: false,
                turn_open: false,
                saw_agent_end: false,
            });
            let mut state_guard = self.inner.state.write().expect("state lock poisoned");
            state_guard.is_streaming = true;
            state_guard.streaming_message = None;
            state_guard.error_message = None;
            executor
        };

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let agent_for_task = self.clone();
        let token_for_task = cancel_token.clone();

        // 由独立受控任务拥有整个 run 的生命周期：
        // 即使公开 prompt/continue_run 的等待 Future 被外部 timeout 丢弃，也仅通过 PromptWaitGuard 请求取消，
        // 后台任务继续收敛工具更新、发出闭合事件序列并等待全部 AgentEnd 监听器结算后才调用 finish_run(run_id)。
        tokio::spawn(async move {
            let exec_outcome: Result<(), String> = match catch_unwind(AssertUnwindSafe(|| {
                executor(run_id, token_for_task.clone())
            })) {
                Ok(fut) => match AssertUnwindSafe(fut).catch_unwind().await {
                    Ok(res) => res,
                    Err(payload) => Err(format!(
                        "Agent run panicked: {}",
                        panic_payload_to_string(payload)
                    )),
                },
                Err(payload) => Err(format!(
                    "Agent run panicked: {}",
                    panic_payload_to_string(payload)
                )),
            };

            if let Err(err_msg) = &exec_outcome {
                agent_for_task
                    .handle_run_failure(run_id, err_msg.clone(), token_for_task.is_cancelled())
                    .await;
            }

            agent_for_task.finish_run(run_id);
            let _ = done_tx.send(exec_outcome);
        });

        let mut wait_guard = PromptWaitGuard {
            agent: self.clone(),
            run_id,
            completed: false,
        };
        let res = done_rx.await.unwrap_or(Ok(()));
        wait_guard.completed = true;
        res
    }

    async fn handle_run_failure(&self, run_id: u64, error_message: String, aborted: bool) {
        let (saw_agent_start, turn_open, saw_agent_end) = {
            let guard = self
                .inner
                .active_run
                .lock()
                .expect("active_run lock poisoned");
            let Some(active) = guard.as_ref().filter(|r| r.run_id == run_id) else {
                return;
            };
            (
                active.saw_agent_start,
                active.turn_open,
                active.saw_agent_end,
            )
        };

        if saw_agent_end {
            let mut state_guard = self.inner.state.write().expect("state lock poisoned");
            state_guard.error_message = Some(error_message);
            return;
        }

        let (model, has_open_message) = {
            let state_guard = self.inner.state.read().expect("state lock poisoned");
            (
                state_guard.model.clone(),
                state_guard.streaming_message.is_some(),
            )
        };
        let failure_msg = AgentMessage::Assistant(create_error_assistant_message(
            &model,
            error_message,
            aborted,
        ));

        if !saw_agent_start {
            self.process_event(run_id, AgentEvent::AgentStart).await;
        }
        if !turn_open {
            self.process_event(run_id, AgentEvent::TurnStart).await;
        }
        if !has_open_message {
            self.process_event(
                run_id,
                AgentEvent::MessageStart {
                    message: failure_msg.clone(),
                },
            )
            .await;
        }
        self.process_event(
            run_id,
            AgentEvent::MessageEnd {
                message: failure_msg.clone(),
            },
        )
        .await;
        self.process_event(
            run_id,
            AgentEvent::TurnEnd {
                message: failure_msg.clone(),
                tool_results: Vec::new(),
            },
        )
        .await;
        self.process_event(
            run_id,
            AgentEvent::AgentEnd {
                messages: vec![failure_msg],
            },
        )
        .await;
    }

    fn finish_run(&self, run_id: u64) {
        let finished = {
            let mut run_guard = self
                .inner
                .active_run
                .lock()
                .expect("active_run lock poisoned");
            if run_guard.as_ref().map(|r| r.run_id) != Some(run_id) {
                return;
            }
            let mut state_guard = self.inner.state.write().expect("state lock poisoned");
            state_guard.is_streaming = false;
            state_guard.streaming_message = None;
            state_guard.pending_tool_calls.clear();
            run_guard.take()
        };
        if let Some(run) = finished {
            run.idle_notify.notify_waiters();
        }
    }

    async fn process_event(&self, run_id: u64, event: AgentEvent) {
        let cancel_token = {
            let mut run_guard = self
                .inner
                .active_run
                .lock()
                .expect("active_run lock poisoned");
            let Some(active) = run_guard.as_mut() else {
                return;
            };
            if active.run_id != run_id {
                return;
            };
            match &event {
                AgentEvent::AgentStart => {
                    active.saw_agent_start = true;
                }
                AgentEvent::TurnStart => {
                    active.turn_open = true;
                }
                AgentEvent::TurnEnd { .. } => {
                    active.turn_open = false;
                }
                AgentEvent::AgentEnd { .. } => {
                    active.saw_agent_end = true;
                }
                _ => {}
            }
            active.cancel_token.clone()
        };

        {
            let mut state_guard = self.inner.state.write().expect("state lock poisoned");
            match &event {
                AgentEvent::MessageStart { message }
                | AgentEvent::MessageUpdate { message, .. } => {
                    state_guard.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageEnd { message } => {
                    state_guard.streaming_message = None;
                    state_guard.messages.push(message.clone());
                }
                AgentEvent::ToolExecutionStart {
                    tool_call_id,
                    parent_tool_call_id,
                    ..
                } => {
                    if parent_tool_call_id.is_none() {
                        state_guard.pending_tool_calls.insert(tool_call_id.clone());
                    }
                }
                AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    parent_tool_call_id,
                    ..
                } => {
                    if parent_tool_call_id.is_none() {
                        state_guard.pending_tool_calls.remove(tool_call_id);
                    }
                }
                AgentEvent::TurnEnd { message, .. } => {
                    if let AgentMessage::Assistant(a) = message
                        && let Some(err) = &a.error_message
                    {
                        state_guard.error_message = Some(err.clone());
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    state_guard.streaming_message = None;
                }
                AgentEvent::AgentStart
                | AgentEvent::TurnStart
                | AgentEvent::ToolExecutionUpdate { .. } => {}
            }
        }

        let listeners: Vec<AgentListenerFn> = {
            let guard = self
                .inner
                .listeners
                .lock()
                .expect("listeners lock poisoned");
            guard.iter().map(|(_, f)| f.clone()).collect()
        };

        for listener in listeners {
            let ev = event.clone();
            let token = cancel_token.clone();
            if let Ok(fut) = catch_unwind(AssertUnwindSafe(|| listener(ev, token))) {
                let _ = AssertUnwindSafe(fut).catch_unwind().await;
            }
        }
    }
}

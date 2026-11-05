use std::any::Any;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use futures_util::StreamExt;
use futures_util::future::join_all;
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, ClientIdentity, ContentBlock, Context, JsonValue,
    ModelThinkingLevel, SimpleStreamOptions, StopReason, StreamOptions, SystemContent,
    SystemMessage, ToolCall, ToolResultMessage, ToolStateChanges, get_tool_state_changes,
    merge_client_identity, normalize_context, now_ms,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::event_stream::{AgentEventStream, create_agent_event_stream};
use crate::types::{
    AfterToolCallContext, AfterToolCallFn, AgentContext, AgentEvent, AgentLoopConfig, AgentMessage,
    AgentTool, AgentToolCallOutcome, AgentToolResult, AgentTurnContext, AgentTurnDecision,
    BeforeToolCallContext, BeforeToolCallFn, BoxFuture, PrepareNextTurnContext,
    PrepareRequestContext, StreamFn, ToolExecutionMode, create_error_assistant_message,
    get_agent_current_tools,
};
use crate::validation::validate_tool_arguments;

pub(crate) fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

pub type AgentEventSink = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>;

pub type ToolUpdateSink = Arc<dyn Fn(AgentToolResult) -> BoxFuture<'static, ()> + Send + Sync>;

/// 返回 Agent 循环默认的客户端身份标识（携带 `PI_ATTRIBUTION`）。
pub fn default_agent_identity() -> ClientIdentity {
    ClientIdentity::with_pi_attribution()
}

/// 启动带初始提示消息的无状态 Agent 事件流（对照 `agentLoop`）。
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> AgentEventStream {
    let stream_fn = isolate_stream_fn(stream_fn);
    let stream = create_agent_event_stream();
    let writer = stream.clone();

    tokio::spawn(async move {
        let emit: AgentEventSink = Arc::new({
            let writer = writer.clone();
            move |event| {
                let writer = writer.clone();
                Box::pin(async move {
                    writer.push(event);
                })
            }
        });
        let messages =
            run_agent_loop(prompts, context, config, emit, cancel_token, stream_fn).await;
        writer.end(Some(messages));
    });

    stream
}

/// 从现有上下文继续执行无状态 Agent 事件流（对照 `agentLoopContinue`）。
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Result<AgentEventStream, String> {
    if context.messages.is_empty() {
        return Err("Cannot continue: no messages in context".to_owned());
    }
    if context
        .messages
        .last()
        .is_some_and(|m| m.role() == "assistant")
    {
        return Err("Cannot continue from message role: assistant".to_owned());
    }

    let stream_fn = isolate_stream_fn(stream_fn);
    let stream = create_agent_event_stream();
    let writer = stream.clone();

    tokio::spawn(async move {
        let emit: AgentEventSink = Arc::new({
            let writer = writer.clone();
            move |event| {
                let writer = writer.clone();
                Box::pin(async move {
                    writer.push(event);
                })
            }
        });
        if let Ok(messages) =
            run_agent_loop_continue(context, config, emit, cancel_token, stream_fn).await
        {
            writer.end(Some(messages));
        } else {
            writer.end(Some(Vec::new()));
        }
    });

    Ok(stream)
}

// The stateless APIs have no Agent lifecycle owner to settle provider failures.
// Isolate only the caller-supplied stream function; normal loop finalization then
// persists the error message and emits MessageEnd, TurnEnd and AgentEnd.
fn isolate_stream_fn(stream_fn: StreamFn) -> StreamFn {
    Arc::new(move |model, context, options| {
        let stream_fn = stream_fn.clone();
        Box::pin(async move {
            let cancel = options.base.cancel_token.clone();
            match AssertUnwindSafe(async { stream_fn(model.clone(), context, options).await })
                .catch_unwind()
                .await
            {
                Ok(stream) => stream,
                Err(payload) => {
                    let error = create_error_assistant_message(
                        &model,
                        format!("stream_fn panicked: {}", panic_payload_to_string(payload)),
                        cancel.as_ref().is_some_and(CancellationToken::is_cancelled),
                    );
                    let stream = pi_ai::create_assistant_message_event_stream();
                    stream.push(AssistantMessageEvent::Error {
                        reason: error.stop_reason,
                        error,
                    });
                    stream
                }
            }
        })
    })
}

/// 运行带初始提示消息的无状态 Agent 循环并返回本次运行新增的消息列表（对照 `runAgentLoop`）。
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    cancel_token: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Vec<AgentMessage> {
    match try_run_agent_loop_with_messages(prompts, context, config, emit, cancel_token, stream_fn)
        .await
    {
        Ok(msgs) | Err((_, msgs)) => msgs,
    }
}

/// 运行带初始提示消息的无状态 Agent 循环；若定稿钩子（`finalize_message`）失败则立即终止并返回 `Err`。
pub async fn try_run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    cancel_token: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Result<Vec<AgentMessage>, String> {
    try_run_agent_loop_with_messages(prompts, context, config, emit, cancel_token, stream_fn)
        .await
        .map_err(|(err, _)| err)
}

async fn try_run_agent_loop_with_messages(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    cancel_token: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Result<Vec<AgentMessage>, (String, Vec<AgentMessage>)> {
    let initial_messages = declare_tool_changes(&context, &prompts);
    let mut new_messages = Vec::new();
    let mut current_context = context;

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    for message in initial_messages {
        emit(AgentEvent::MessageStart {
            message: message.clone(),
        })
        .await;
        let finalized =
            match finalize_message_with_hook(message, &config, cancel_token.clone()).await {
                Ok(msg) => msg,
                Err(err) => {
                    if let Some(t) = &cancel_token {
                        t.cancel();
                    }
                    emit(AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    })
                    .await;
                    return Err((err, new_messages));
                }
            };
        current_context.messages.push(finalized.clone());
        new_messages.push(finalized.clone());
        emit(AgentEvent::MessageEnd { message: finalized }).await;
    }

    if let Err(err) = run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        cancel_token,
        emit,
        stream_fn,
    )
    .await
    {
        return Err((err, new_messages));
    }

    Ok(new_messages)
}

/// 从现有上下文继续运行无状态 Agent 循环（对照 `runAgentLoopContinue`）。
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    cancel_token: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Result<Vec<AgentMessage>, String> {
    if context.messages.is_empty() {
        return Err("Cannot continue: no messages in context".to_owned());
    }
    if context
        .messages
        .last()
        .is_some_and(|m| m.role() == "assistant")
    {
        return Err("Cannot continue from message role: assistant".to_owned());
    }

    let mut new_messages = Vec::new();
    let mut current_context = context;

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;

    run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        cancel_token,
        emit,
        stream_fn,
    )
    .await?;

    Ok(new_messages)
}

async fn finalize_message_with_hook(
    message: AgentMessage,
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
) -> Result<AgentMessage, String> {
    let Some(finalize) = &config.finalize_message else {
        return Ok(message);
    };
    let expected_role = message.role().to_owned();
    let res = match catch_unwind(AssertUnwindSafe(|| finalize(message, cancel_token))) {
        Ok(fut) => match AssertUnwindSafe(fut).catch_unwind().await {
            Ok(res) => res,
            Err(payload) => Err(format!(
                "finalize_message panicked: {}",
                panic_payload_to_string(payload)
            )),
        },
        Err(payload) => Err(format!(
            "finalize_message panicked: {}",
            panic_payload_to_string(payload)
        )),
    }?;
    if res.role() != expected_role {
        return Err(format!(
            "finalize_message changed message role from \"{expected_role}\" to \"{}\"",
            res.role()
        ));
    }
    Ok(res)
}

// =============================================================================
// Core Double Loop State Machine
// =============================================================================

async fn run_loop(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    initial_config: AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: AgentEventSink,
    stream_fn: StreamFn,
) -> Result<(), String> {
    let mut config = initial_config;
    let mut last_completed_turn: Option<PrepareNextTurnContext> = None;
    let mut explicit_continuation = false;
    let mut pending_messages: Vec<AgentMessage> = match &config.get_steering_messages {
        Some(get_steering) => get_steering().await,
        None => Vec::new(),
    };

    loop {
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending_messages.is_empty() {
            let mut prepared_messages: Vec<AgentMessage> = Vec::new();
            if let Some(last_turn) = &last_completed_turn {
                if let Some(prep_next) = &config.prepare_next_turn
                    && let Some(next_turn_snapshot) = prep_next(last_turn.clone()).await
                {
                    if let Some(next_ctx) = next_turn_snapshot.context {
                        *current_context = next_ctx;
                    }
                    if let Some(msgs) = next_turn_snapshot.messages {
                        prepared_messages = msgs;
                    }
                    if let Some(next_model) = next_turn_snapshot.model {
                        config.model = next_model;
                    }
                    if let Some(next_level) = next_turn_snapshot.thinking_level {
                        config.reasoning = model_thinking_to_reasoning(next_level);
                    }
                }

                // 若上一轮结束时的轮询未取到消息，则在 prepare_next_turn 完成后补轮询一次
                if pending_messages.is_empty()
                    && let Some(get_steering) = &config.get_steering_messages
                {
                    pending_messages = get_steering().await;
                }
                emit(AgentEvent::TurnStart).await;
            }

            let combined_pending =
                [prepared_messages.as_slice(), pending_messages.as_slice()].concat();
            for message in declare_tool_changes(current_context, &combined_pending) {
                emit(AgentEvent::MessageStart {
                    message: message.clone(),
                })
                .await;
                let finalized = match finalize_message_with_hook(
                    message,
                    &config,
                    cancel_token.clone(),
                )
                .await
                {
                    Ok(msg) => msg,
                    Err(err) => {
                        if let Some(t) = &cancel_token {
                            t.cancel();
                        }
                        emit(AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        })
                        .await;
                        return Err(err);
                    }
                };
                current_context.messages.push(finalized.clone());
                new_messages.push(finalized.clone());
                emit(AgentEvent::MessageEnd { message: finalized }).await;
            }
            pending_messages.clear();

            if let Some(prep_req) = &config.prepare_request {
                let req_ctx = PrepareRequestContext {
                    context: current_context.clone(),
                    model: config.model.clone(),
                    thinking_level: config
                        .reasoning
                        .map(ModelThinkingLevel::from)
                        .unwrap_or(ModelThinkingLevel::Off),
                };
                if let Some(req_update) = prep_req(req_ctx, cancel_token.clone()).await {
                    if let Some(next_ctx) = req_update.context {
                        *current_context = next_ctx;
                    }
                    if let Some(next_model) = req_update.model {
                        config.model = next_model;
                    }
                    if let Some(next_level) = req_update.thinking_level {
                        config.reasoning = model_thinking_to_reasoning(next_level);
                    }
                }
            }

            let message = match stream_assistant_response(
                current_context,
                &config,
                cancel_token.clone(),
                &emit,
                &stream_fn,
            )
            .await
            {
                Ok(msg) => msg,
                Err(err) => {
                    if let Some(t) = &cancel_token {
                        t.cancel();
                    }
                    emit(AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    })
                    .await;
                    return Err(err);
                }
            };
            new_messages.push(AgentMessage::Assistant(message.clone()));

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                let turn_ctx = AgentTurnContext {
                    message: message.clone(),
                    tool_results: Vec::new(),
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };
                if let Some(finish) = &config.finish_turn {
                    let _ = finish(turn_ctx, cancel_token.clone()).await;
                }
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Assistant(message),
                    tool_results: Vec::new(),
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            let tool_calls: Vec<ToolCall> = message
                .content
                .iter()
                .filter_map(|c| match c {
                    ContentBlock::ToolCall(tc) => Some(tc.clone()),
                    _ => None,
                })
                .collect();

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;

            if !tool_calls.is_empty() {
                let batch_res = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated_message(
                        &tool_calls,
                        &config,
                        cancel_token.clone(),
                        &emit,
                        current_context,
                        new_messages,
                        &mut tool_results,
                    )
                    .await
                } else {
                    execute_tool_calls(
                        current_context,
                        new_messages,
                        &mut tool_results,
                        &message,
                        &tool_calls,
                        &config,
                        cancel_token.clone(),
                        &emit,
                    )
                    .await
                };
                match batch_res {
                    Ok(terminate) => {
                        has_more_tool_calls = !terminate;
                    }
                    Err(err) => {
                        if let Some(t) = &cancel_token {
                            t.cancel();
                        }
                        emit(AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        })
                        .await;
                        return Err(err);
                    }
                }
            }

            let turn_ctx = AgentTurnContext {
                message: message.clone(),
                tool_results: tool_results.clone(),
                context: current_context.clone(),
                new_messages: new_messages.clone(),
            };
            last_completed_turn = Some(turn_ctx.clone());

            let decision = match &config.finish_turn {
                Some(finish) => finish(turn_ctx, cancel_token.clone()).await,
                None => None,
            };

            emit(AgentEvent::TurnEnd {
                message: AgentMessage::Assistant(message),
                tool_results,
            })
            .await;

            if decision == Some(AgentTurnDecision::End) {
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            explicit_continuation = decision == Some(AgentTurnDecision::Continue);
            pending_messages = match &config.get_steering_messages {
                Some(get_steering) => get_steering().await,
                None => Vec::new(),
            };
            if has_more_tool_calls || !pending_messages.is_empty() {
                explicit_continuation = false;
            }
        }

        let follow_up_messages = match &config.get_follow_up_messages {
            Some(get_follow_up) => get_follow_up().await,
            None => Vec::new(),
        };
        if !follow_up_messages.is_empty() {
            explicit_continuation = false;
            pending_messages = follow_up_messages;
            continue;
        }

        if explicit_continuation {
            explicit_continuation = false;
            continue;
        }

        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    })
    .await;
    Ok(())
}

fn model_thinking_to_reasoning(level: ModelThinkingLevel) -> Option<pi_ai::ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(pi_ai::ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(pi_ai::ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(pi_ai::ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(pi_ai::ThinkingLevel::High),
        ModelThinkingLevel::XHigh => Some(pi_ai::ThinkingLevel::XHigh),
        ModelThinkingLevel::Max => Some(pi_ai::ThinkingLevel::Max),
    }
}

// =============================================================================
// 3.2 Tool Loadout Declaration Synchronization (declareToolChanges)
// =============================================================================

pub fn declare_tool_changes(
    context: &AgentContext,
    pending_messages: &[AgentMessage],
) -> Vec<AgentMessage> {
    let system_index = pending_messages
        .iter()
        .rposition(|m| matches!(m, AgentMessage::System(_)));

    let pending_sys = system_index.and_then(|idx| match &pending_messages[idx] {
        AgentMessage::System(s) => Some(s.clone()),
        _ => None,
    });

    let no_changes = ToolStateChanges {
        tools_added: Vec::new(),
        tools_removed: Vec::new(),
    };

    let baseline: Vec<AgentMessage> = if let (Some(idx), Some(sys)) = (system_index, &pending_sys) {
        pending_messages
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                if i == idx {
                    AgentMessage::System(with_tool_changes(sys, &no_changes))
                } else {
                    msg.clone()
                }
            })
            .collect()
    } else {
        pending_messages.to_vec()
    };

    let combined_history: Vec<AgentMessage> =
        [context.messages.as_slice(), baseline.as_slice()].concat();
    let declared_tools = get_agent_current_tools(&combined_history);
    let executable_tools: Vec<pi_ai::Tool> = context
        .tools
        .iter()
        .map(AgentTool::to_tool_declaration)
        .collect();
    let changes = get_tool_state_changes(&declared_tools, &executable_tools);
    let unchanged = changes.tools_added.is_empty() && changes.tools_removed.is_empty();

    if let (Some(idx), Some(sys)) = (system_index, &pending_sys) {
        let had_added = sys.tools_added.as_ref().is_some_and(|a| !a.is_empty());
        let had_removed = sys.tools_removed.as_ref().is_some_and(|r| !r.is_empty());
        if unchanged && !had_added && !had_removed {
            return pending_messages.to_vec();
        }
        return baseline
            .into_iter()
            .enumerate()
            .map(|(i, msg)| {
                if i == idx {
                    AgentMessage::System(with_tool_changes(sys, &changes))
                } else {
                    msg
                }
            })
            .collect();
    }

    if unchanged {
        return pending_messages.to_vec();
    }

    let synthetic = AgentMessage::System(with_tool_changes(
        &SystemMessage {
            content: SystemContent::Text(String::new()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: now_ms(),
        },
        &changes,
    ));

    let insert_idx = pending_messages
        .iter()
        .position(|m| !matches!(m, AgentMessage::System(_)))
        .unwrap_or(pending_messages.len());

    let mut result = Vec::with_capacity(pending_messages.len() + 1);
    result.extend_from_slice(&pending_messages[..insert_idx]);
    result.push(synthetic);
    result.extend_from_slice(&pending_messages[insert_idx..]);
    result
}

fn with_tool_changes(message: &SystemMessage, changes: &ToolStateChanges) -> SystemMessage {
    SystemMessage {
        content: message.content.clone(),
        sections: message.sections.clone(),
        tools_added: if changes.tools_added.is_empty() {
            None
        } else {
            Some(changes.tools_added.clone())
        },
        tools_removed: if changes.tools_removed.is_empty() {
            None
        } else {
            Some(changes.tools_removed.clone())
        },
        timestamp: message.timestamp,
    }
}

// =============================================================================
// Stream Assistant Response
// =============================================================================

async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: &AgentEventSink,
    stream_fn: &StreamFn,
) -> Result<AssistantMessage, String> {
    let mut messages = context.messages.clone();
    if let Some(transform) = &config.transform_context {
        messages = transform(messages, cancel_token.clone()).await;
    }

    let llm_messages = (config.convert_to_llm)(messages).await;
    let llm_context = normalize_context(&Context {
        system_prompt: None,
        messages: llm_messages,
        tools: None,
    });

    let resolved_api_key = match &config.get_api_key {
        Some(get_key) => get_key(config.model.provider.clone())
            .await
            .or_else(|| config.api_key.clone()),
        None => config.api_key.clone(),
    };

    let default_id = default_agent_identity();
    let merged_identity = merge_client_identity([Some(&default_id), config.identity.as_ref()]);

    let stream_options = SimpleStreamOptions {
        base: StreamOptions {
            cancel_token: cancel_token.clone(),
            api_key: resolved_api_key,
            env: None,
            headers: config.headers.clone(),
            identity: merged_identity,
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: config.max_retry_delay_ms,
            temperature: config.temperature,
            sampling_params: config.sampling_params.clone(),
            max_tokens: config.max_tokens,
            transport: config.transport,
            cache_retention: config.cache_retention,
            session_id: config.session_id.clone(),
            metadata: None,
            reasoning_effort: config.reasoning,
            reasoning_summary: None,
            service_tier: None,
            tool_choice: config.tool_choice,
            thinking_budgets: config.thinking_budgets.clone(),
            on_headers: config.on_headers.clone(),
            on_payload: config.on_payload.clone(),
            on_response: config.on_response.clone(),
            on_provider_stream_event: config.on_provider_stream_event.clone(),
        },
        tool_choice: config.tool_choice,
        reasoning: config.reasoning,
        thinking_budgets: config.thinking_budgets.clone(),
    };

    const CANCEL_GRACE: Duration = Duration::from_millis(50);

    let requested_thinking_level = config
        .reasoning
        .map(ModelThinkingLevel::from)
        .unwrap_or(ModelThinkingLevel::Off);

    let mut cancel_deadline: Option<tokio::time::Instant> = cancel_token
        .as_ref()
        .filter(|t| t.is_cancelled())
        .map(|_| tokio::time::Instant::now() + CANCEL_GRACE);

    let stream_fut = stream_fn(config.model.clone(), llm_context, stream_options);
    let mut response = if let Some(deadline) = cancel_deadline {
        match tokio::time::timeout_at(deadline, stream_fut).await {
            Ok(res) => res,
            Err(_) => {
                let aborted_msg =
                    build_aborted_assistant_message(None, &config.model, requested_thinking_level);
                return finish_assistant_message(
                    context,
                    aborted_msg,
                    false,
                    config,
                    cancel_token,
                    emit,
                )
                .await;
            }
        }
    } else if let Some(token) = &cancel_token {
        let mut pinned_fut = std::pin::pin!(stream_fut);
        tokio::select! {
            res = &mut pinned_fut => res,
            _ = token.cancelled() => {
                let deadline = *cancel_deadline.get_or_insert_with(|| tokio::time::Instant::now() + CANCEL_GRACE);
                match tokio::time::timeout_at(deadline, &mut pinned_fut).await {
                    Ok(res) => res,
                    Err(_) => {
                        let aborted_msg = build_aborted_assistant_message(
                            None,
                            &config.model,
                            requested_thinking_level,
                        );
                        return finish_assistant_message(
                            context,
                            aborted_msg,
                            false,
                            config,
                            cancel_token,
                            emit,
                        )
                        .await;
                    }
                }
            }
        }
    } else {
        stream_fut.await
    };

    let mut added_partial = false;
    let mut latest_partial: Option<AssistantMessage> = None;

    loop {
        if cancel_deadline.is_none() && cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
            cancel_deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
        }

        let next_event = if let Some(deadline) = cancel_deadline {
            if tokio::time::Instant::now() >= deadline {
                None
            } else {
                tokio::time::timeout_at(deadline, response.next())
                    .await
                    .unwrap_or(None)
            }
        } else if let Some(token) = &cancel_token {
            tokio::select! {
                ev = response.next() => ev,
                _ = token.cancelled() => {
                    let deadline = *cancel_deadline.get_or_insert_with(|| tokio::time::Instant::now() + CANCEL_GRACE);
                    tokio::time::timeout_at(deadline, response.next())
                        .await
                        .unwrap_or(None)
                }
            }
        } else {
            response.next().await
        };

        let Some(event) = next_event else {
            break;
        };

        match &event {
            AssistantMessageEvent::Start { partial } => {
                latest_partial = Some(partial.clone());
                context
                    .messages
                    .push(AgentMessage::Assistant(partial.clone()));
                added_partial = true;
                let emit_started = tokio::time::Instant::now();
                emit(AgentEvent::MessageStart {
                    message: AgentMessage::Assistant(partial.clone()),
                })
                .await;
                if let Some(deadline) = &mut cancel_deadline {
                    *deadline += emit_started.elapsed();
                }
            }
            AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolCallStart { partial, .. }
            | AssistantMessageEvent::ToolCallDelta { partial, .. }
            | AssistantMessageEvent::ToolCallEnd { partial, .. } => {
                latest_partial = Some(partial.clone());
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::Assistant(partial.clone());
                    }
                    let emit_started = tokio::time::Instant::now();
                    emit(AgentEvent::MessageUpdate {
                        message: AgentMessage::Assistant(partial.clone()),
                        assistant_message_event: event,
                    })
                    .await;
                    if let Some(deadline) = &mut cancel_deadline {
                        *deadline += emit_started.elapsed();
                    }
                }
            }
            AssistantMessageEvent::Done {
                message: ev_msg, ..
            }
            | AssistantMessageEvent::Error { error: ev_msg, .. } => {
                let ev_fallback = ev_msg.clone();
                let mut final_message = if let Some(deadline) = cancel_deadline {
                    if tokio::time::Instant::now() >= deadline {
                        ev_fallback
                    } else {
                        tokio::time::timeout_at(deadline, response.result())
                            .await
                            .unwrap_or(ev_fallback)
                    }
                } else {
                    response.result().await
                };
                final_message.thinking_level = Some(requested_thinking_level);
                return finish_assistant_message(
                    context,
                    final_message,
                    added_partial,
                    config,
                    cancel_token,
                    emit,
                )
                .await;
            }
        }
    }

    if cancel_deadline.is_none() && cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
        cancel_deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
    }

    let mut final_message = if let Some(deadline) = cancel_deadline {
        if tokio::time::Instant::now() >= deadline {
            build_aborted_assistant_message(latest_partial, &config.model, requested_thinking_level)
        } else {
            tokio::time::timeout_at(deadline, response.result())
                .await
                .unwrap_or_else(|_| {
                    build_aborted_assistant_message(
                        latest_partial,
                        &config.model,
                        requested_thinking_level,
                    )
                })
        }
    } else {
        response.result().await
    };
    final_message.thinking_level = Some(requested_thinking_level);
    finish_assistant_message(
        context,
        final_message,
        added_partial,
        config,
        cancel_token,
        emit,
    )
    .await
}

async fn finish_assistant_message(
    context: &mut AgentContext,
    final_message: AssistantMessage,
    added_partial: bool,
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> Result<AssistantMessage, String> {
    if !added_partial {
        context
            .messages
            .push(AgentMessage::Assistant(final_message.clone()));
        emit(AgentEvent::MessageStart {
            message: AgentMessage::Assistant(final_message.clone()),
        })
        .await;
    } else if let Some(last) = context.messages.last_mut() {
        *last = AgentMessage::Assistant(final_message.clone());
    }

    match finalize_message_with_hook(AgentMessage::Assistant(final_message), config, cancel_token)
        .await
    {
        Ok(AgentMessage::Assistant(finalized)) => {
            if let Some(last) = context.messages.last_mut() {
                *last = AgentMessage::Assistant(finalized.clone());
            }
            emit(AgentEvent::MessageEnd {
                message: AgentMessage::Assistant(finalized.clone()),
            })
            .await;
            Ok(finalized)
        }
        Ok(other) => {
            context.messages.pop();
            Err(format!(
                "finalize_message returned non-assistant role \"{}\" for assistant message",
                other.role()
            ))
        }
        Err(err) => {
            context.messages.pop();
            Err(err)
        }
    }
}

fn build_aborted_assistant_message(
    latest_partial: Option<AssistantMessage>,
    model: &pi_ai::Model,
    requested_thinking_level: ModelThinkingLevel,
) -> AssistantMessage {
    if let Some(mut partial) = latest_partial {
        partial.stop_reason = StopReason::Aborted;
        if partial.error_message.is_none() {
            partial.error_message = Some("Request was aborted".to_owned());
        }
        partial.thinking_level = Some(requested_thinking_level);
        partial
    } else {
        let mut msg = create_error_assistant_message(model, "Request was aborted", true);
        msg.thinking_level = Some(requested_thinking_level);
        msg
    }
}

// =============================================================================
// 3.3 & 3.4 Tool Execution Pipeline & ToolUpdateGate
// =============================================================================

#[derive(Clone, Default)]
pub struct ToolCallHooks {
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
}

/// `run_tool_call` 的调用选项（对照 `RunToolCallOptions`）。
#[derive(Clone)]
pub struct RunToolCallOptions {
    pub tools: Vec<AgentTool>,
    pub assistant_message: AssistantMessage,
    pub context: AgentContext,
    pub cancel_token: Option<CancellationToken>,
    pub on_update: Option<ToolUpdateSink>,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
}

/// 独立执行单个工具调用的完整管线（不直接向主循环发射事件，供嵌套工具调用等场景复用）。
pub async fn run_tool_call(
    tool_call: ToolCall,
    options: RunToolCallOptions,
) -> AgentToolCallOutcome {
    let hooks = ToolCallHooks {
        before_tool_call: options.before_tool_call,
        after_tool_call: options.after_tool_call,
    };
    let preparation = prepare_tool_call(
        &options.context,
        &options.assistant_message,
        &tool_call,
        &hooks,
        options.cancel_token.clone(),
        &options.tools,
    )
    .await;

    match preparation {
        ToolPreparation::Immediate { result, is_error } => AgentToolCallOutcome {
            tool_call,
            result,
            is_error,
            duration_ms: None,
        },
        ToolPreparation::Prepared(prepared) => {
            let sink: ToolUpdateSink = options
                .on_update
                .unwrap_or_else(|| Arc::new(|_| Box::pin(async {})));
            let executed =
                execute_prepared_tool_call(&prepared, options.cancel_token.clone(), sink).await;
            finalize_executed_tool_call(
                &options.context,
                &options.assistant_message,
                prepared,
                executed,
                &hooks,
                options.cancel_token,
            )
            .await
        }
    }
}

async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: &AgentEventSink,
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    tool_results: &mut Vec<ToolResultMessage>,
) -> Result<bool, String> {
    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: JsonValue::Object(tool_call.arguments.clone()),
            parent_tool_call_id: None,
        })
        .await;

        let finalized = AgentToolCallOutcome {
            tool_call: tool_call.clone(),
            result: AgentToolResult::error_text(format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            is_error: true,
            duration_ms: None,
        };
        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_msg = create_tool_result_message(&finalized);
        let final_tr = finalize_and_emit_tool_result_message(
            tool_result_msg,
            config,
            cancel_token.clone(),
            emit,
        )
        .await?;
        current_context
            .messages
            .push(AgentMessage::ToolResult(final_tr.clone()));
        new_messages.push(AgentMessage::ToolResult(final_tr.clone()));
        tool_results.push(final_tr);
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    tool_results: &mut Vec<ToolResultMessage>,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> Result<bool, String> {
    let has_sequential_tool = tool_calls.iter().any(|tc| {
        current_context
            .tools
            .iter()
            .find(|t| t.name == tc.name)
            .is_some_and(|t| t.execution_mode == Some(ToolExecutionMode::Sequential))
    });

    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential_tool {
        execute_tool_calls_sequential(
            current_context,
            new_messages,
            tool_results,
            assistant_message,
            tool_calls,
            config,
            cancel_token,
            emit,
        )
        .await
    } else {
        execute_tool_calls_parallel(
            current_context,
            new_messages,
            tool_results,
            assistant_message,
            tool_calls,
            config,
            cancel_token,
            emit,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls_sequential(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    tool_results: &mut Vec<ToolResultMessage>,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> Result<bool, String> {
    let hooks = ToolCallHooks {
        before_tool_call: config.before_tool_call.clone(),
        after_tool_call: config.after_tool_call.clone(),
    };
    let mut finalized_calls = Vec::new();

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: JsonValue::Object(tool_call.arguments.clone()),
            parent_tool_call_id: None,
        })
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            &hooks,
            cancel_token.clone(),
            &current_context.tools,
        )
        .await;

        let finalized = match preparation {
            ToolPreparation::Immediate { result, is_error } => AgentToolCallOutcome {
                tool_call: tool_call.clone(),
                result,
                is_error,
                duration_ms: None,
            },
            ToolPreparation::Prepared(prepared) => {
                let update_sink = make_tool_update_sink(tool_call, emit);
                let executed =
                    execute_prepared_tool_call(&prepared, cancel_token.clone(), update_sink).await;
                finalize_executed_tool_call(
                    current_context,
                    assistant_message,
                    prepared,
                    executed,
                    &hooks,
                    cancel_token.clone(),
                )
                .await
            }
        };

        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_msg = create_tool_result_message(&finalized);
        let final_tr = finalize_and_emit_tool_result_message(
            tool_result_msg,
            config,
            cancel_token.clone(),
            emit,
        )
        .await?;
        current_context
            .messages
            .push(AgentMessage::ToolResult(final_tr.clone()));
        new_messages.push(AgentMessage::ToolResult(final_tr.clone()));
        tool_results.push(final_tr);
        finalized_calls.push(finalized);

        if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
            break;
        }
    }

    Ok(should_terminate_tool_batch(&finalized_calls))
}

enum ParallelSlot {
    Immediate(AgentToolCallOutcome),
    Prepared(PreparedToolCall),
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls_parallel(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    tool_results: &mut Vec<ToolResultMessage>,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> Result<bool, String> {
    let hooks = ToolCallHooks {
        before_tool_call: config.before_tool_call.clone(),
        after_tool_call: config.after_tool_call.clone(),
    };
    let mut slots: Vec<ParallelSlot> = Vec::new();

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: JsonValue::Object(tool_call.arguments.clone()),
            parent_tool_call_id: None,
        })
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            &hooks,
            cancel_token.clone(),
            &current_context.tools,
        )
        .await;

        match preparation {
            ToolPreparation::Immediate { result, is_error } => {
                let finalized = AgentToolCallOutcome {
                    tool_call: tool_call.clone(),
                    result,
                    is_error,
                    duration_ms: None,
                };
                emit_tool_execution_end(&finalized, emit).await;
                slots.push(ParallelSlot::Immediate(finalized));
                if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                    break;
                }
            }
            ToolPreparation::Prepared(prepared) => {
                slots.push(ParallelSlot::Prepared(prepared));
                if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                    break;
                }
            }
        }
    }

    let snapshot_context = current_context.clone();
    let tasks = slots.into_iter().map(|slot| {
        let current_context = snapshot_context.clone();
        let assistant_message = assistant_message.clone();
        let hooks = hooks.clone();
        let cancel_token = cancel_token.clone();
        let emit = emit.clone();

        async move {
            match slot {
                ParallelSlot::Immediate(finalized) => finalized,
                ParallelSlot::Prepared(prepared) => {
                    if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                        let finalized = AgentToolCallOutcome {
                            tool_call: prepared.tool_call,
                            result: AgentToolResult::error_text("Operation aborted"),
                            is_error: true,
                            duration_ms: None,
                        };
                        emit_tool_execution_end(&finalized, &emit).await;
                        return finalized;
                    }
                    let update_sink = make_tool_update_sink(&prepared.tool_call, &emit);
                    let executed =
                        execute_prepared_tool_call(&prepared, cancel_token.clone(), update_sink)
                            .await;
                    let finalized = finalize_executed_tool_call(
                        &current_context,
                        &assistant_message,
                        prepared,
                        executed,
                        &hooks,
                        cancel_token,
                    )
                    .await;
                    emit_tool_execution_end(&finalized, &emit).await;
                    finalized
                }
            }
        }
    });

    let ordered_finalized_calls = join_all(tasks).await;
    for finalized in &ordered_finalized_calls {
        let tool_result_msg = create_tool_result_message(finalized);
        let final_tr = finalize_and_emit_tool_result_message(
            tool_result_msg,
            config,
            cancel_token.clone(),
            emit,
        )
        .await?;
        current_context
            .messages
            .push(AgentMessage::ToolResult(final_tr.clone()));
        new_messages.push(AgentMessage::ToolResult(final_tr.clone()));
        tool_results.push(final_tr);
    }

    Ok(should_terminate_tool_batch(&ordered_finalized_calls))
}

#[derive(Clone)]
struct PreparedToolCall {
    tool_call: ToolCall,
    tool: AgentTool,
    args: JsonValue,
}

enum ToolPreparation {
    Prepared(PreparedToolCall),
    Immediate {
        result: AgentToolResult,
        is_error: bool,
    },
}

struct ExecutedToolCallOutcome {
    result: AgentToolResult,
    is_error: bool,
    duration_ms: u64,
}

fn should_terminate_tool_batch(finalized_calls: &[AgentToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|f| f.result.terminate == Some(true))
}

fn prepare_tool_call_arguments(tool: &AgentTool, tool_call: &ToolCall) -> Result<ToolCall, String> {
    let Some(prep) = &tool.prepare_arguments else {
        return Ok(tool_call.clone());
    };
    let raw_val = JsonValue::Object(tool_call.arguments.clone());
    let prepared_val = catch_unwind(AssertUnwindSafe(|| prep(&raw_val))).map_err(|payload| {
        format!(
            "prepare_arguments panicked: {}",
            panic_payload_to_string(payload)
        )
    })?;
    match prepared_val {
        JsonValue::Object(map) => {
            let mut cloned = tool_call.clone();
            cloned.arguments = map;
            Ok(cloned)
        }
        _ => Err(format!(
            "Tool \"{}\" prepareArguments must return a JSON object",
            tool.name
        )),
    }
}

async fn prepare_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    hooks: &ToolCallHooks,
    cancel_token: Option<CancellationToken>,
    tools: &[AgentTool],
) -> ToolPreparation {
    let Some(tool) = tools.iter().find(|t| t.name == tool_call.name).cloned() else {
        return ToolPreparation::Immediate {
            result: AgentToolResult::error_text(format!("Tool {} not found", tool_call.name)),
            is_error: true,
        };
    };

    let prepared_tc = match prepare_tool_call_arguments(&tool, tool_call) {
        Ok(tc) => tc,
        Err(err) => {
            return ToolPreparation::Immediate {
                result: AgentToolResult::error_text(err),
                is_error: true,
            };
        }
    };

    let validated_args = match validate_tool_arguments(&tool, &prepared_tc) {
        Ok(args) => args,
        Err(err) => {
            return ToolPreparation::Immediate {
                result: AgentToolResult::error_text(err),
                is_error: true,
            };
        }
    };

    let mut final_args = validated_args;

    if let Some(before_fn) = &hooks.before_tool_call {
        let mut before_ctx = BeforeToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call: tool_call.clone(),
            args: final_args,
            context: current_context.clone(),
        };
        let before_res = match AssertUnwindSafe(async {
            before_fn(&mut before_ctx, cancel_token.clone()).await
        })
        .catch_unwind()
        .await
        {
            Ok(res) => res,
            Err(payload) => Err(format!(
                "before_tool_call panicked: {}",
                panic_payload_to_string(payload)
            )),
        };
        match before_res {
            Err(err_msg) => {
                return ToolPreparation::Immediate {
                    result: AgentToolResult::error_text(err_msg),
                    is_error: true,
                };
            }
            Ok(Some(res)) if res.block == Some(true) => {
                let reason = res
                    .reason
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Tool execution was blocked".to_owned());
                let mut result = AgentToolResult::error_text(reason);
                if res.terminate == Some(true) {
                    result.terminate = Some(true);
                }
                return ToolPreparation::Immediate {
                    result,
                    is_error: true,
                };
            }
            Ok(Some(res)) => {
                if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                    return ToolPreparation::Immediate {
                        result: AgentToolResult::error_text("Operation aborted"),
                        is_error: true,
                    };
                }
                final_args = res.args.unwrap_or(before_ctx.args);
            }
            Ok(None) => {
                if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                    return ToolPreparation::Immediate {
                        result: AgentToolResult::error_text("Operation aborted"),
                        is_error: true,
                    };
                }
                final_args = before_ctx.args;
            }
        }
    }

    if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
        return ToolPreparation::Immediate {
            result: AgentToolResult::error_text("Operation aborted"),
            is_error: true,
        };
    }

    ToolPreparation::Prepared(PreparedToolCall {
        tool_call: tool_call.clone(),
        tool,
        args: final_args,
    })
}

fn make_tool_update_sink(tool_call: &ToolCall, emit: &AgentEventSink) -> ToolUpdateSink {
    let tc_id = tool_call.id.clone();
    let tc_name = tool_call.name.clone();
    let tc_args = JsonValue::Object(tool_call.arguments.clone());
    let emit = emit.clone();
    Arc::new(move |partial_result| {
        let emit = emit.clone();
        let tc_id = tc_id.clone();
        let tc_name = tc_name.clone();
        let tc_args = tc_args.clone();
        Box::pin(async move {
            emit(AgentEvent::ToolExecutionUpdate {
                tool_call_id: tc_id,
                tool_name: tc_name,
                args: tc_args,
                partial_result,
                parent_tool_call_id: None,
            })
            .await;
        })
    })
}

struct UpdateGateState {
    accepting: bool,
    queue: VecDeque<AgentToolResult>,
    in_flight: usize,
}

/// 共享同一把互斥锁的 `on_update` 同步门控（消除多线程抢占导致的迟到进度事件竞态）。
pub struct ToolUpdateGate {
    state: Mutex<UpdateGateState>,
    notify: Notify,
}

impl Default for ToolUpdateGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolUpdateGate {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(UpdateGateState {
                accepting: true,
                queue: VecDeque::new(),
                in_flight: 0,
            }),
            notify: Notify::new(),
        }
    }

    /// 在持有 `Mutex` 的临界区内原子完成「检查 `accepting == true` + 入队」；关闭后直接丢弃。
    pub fn try_enqueue(&self, partial: AgentToolResult) -> bool {
        let mut guard = self.state.lock().expect("tool update gate lock poisoned");
        if !guard.accepting {
            return false;
        }
        guard.queue.push_back(partial);
        drop(guard);
        self.notify.notify_waiters();
        true
    }

    /// 尝试从队列弹出一条待发送更新，并在同一把锁内登记 `in_flight += 1`。
    fn pop_for_send(&self) -> Option<AgentToolResult> {
        let mut guard = self.state.lock().expect("tool update gate lock poisoned");
        if let Some(item) = guard.queue.pop_front() {
            guard.in_flight += 1;
            Some(item)
        } else {
            None
        }
    }

    fn complete_send(&self) {
        let mut guard = self.state.lock().expect("tool update gate lock poisoned");
        guard.in_flight = guard.in_flight.saturating_sub(1);
        drop(guard);
        self.notify.notify_waiters();
    }

    /// 原子关闭接纳并唤醒等待协程（用于 RAII 异常/取消兜底清理）。
    pub fn close(&self) {
        let mut guard = self.state.lock().expect("tool update gate lock poisoned");
        guard.accepting = false;
        drop(guard);
        self.notify.notify_waiters();
    }

    /// 在锁内原子将 `accepting` 置为 `false` 并取出全部剩余更新，随后释放锁并在锁外等待所有已接受更新发送完毕。
    pub async fn close_and_drain(&self, sink: &ToolUpdateSink) {
        let remaining: Vec<AgentToolResult> = {
            let mut guard = self.state.lock().expect("tool update gate lock poisoned");
            guard.accepting = false;
            guard.queue.drain(..).collect()
        };
        self.notify.notify_waiters();

        // 等待后台并发发送协程（若正在发送上一条更新）完成，锁外等待
        loop {
            let notified = self.notify.notified();
            {
                let guard = self.state.lock().expect("tool update gate lock poisoned");
                if guard.in_flight == 0 {
                    break;
                }
            }
            notified.await;
        }

        // 在锁外按顺序发送门控关闭前已入队的全部剩余更新
        for partial in remaining {
            deliver_tool_update(sink, partial).await;
        }
    }
}

async fn deliver_tool_update(sink: &ToolUpdateSink, partial: AgentToolResult) {
    let _ = AssertUnwindSafe(async { sink(partial).await })
        .catch_unwind()
        .await;
}

struct ToolUpdateGateDropGuard {
    gate: Arc<ToolUpdateGate>,
}

impl Drop for ToolUpdateGateDropGuard {
    fn drop(&mut self) {
        self.gate.close();
    }
}

struct InFlightSendGuard<'a> {
    gate: &'a ToolUpdateGate,
}

impl Drop for InFlightSendGuard<'_> {
    fn drop(&mut self) {
        self.gate.complete_send();
    }
}

async fn execute_prepared_tool_call(
    prepared: &PreparedToolCall,
    cancel_token: Option<CancellationToken>,
    on_update: ToolUpdateSink,
) -> ExecutedToolCallOutcome {
    let gate = Arc::new(ToolUpdateGate::new());
    let _gate_drop_guard = ToolUpdateGateDropGuard { gate: gate.clone() };
    let gate_for_cb = gate.clone();
    let update_cb: crate::types::AgentToolUpdateCallback = Arc::new(move |partial| {
        let _ = gate_for_cb.try_enqueue(partial);
    });

    // 后台实时消费队列中的增量更新（锁内取元素并登记 in_flight，锁外 await 发送）
    let gate_for_worker = gate.clone();
    let sink_for_worker = on_update.clone();
    let worker_handle = tokio::spawn(async move {
        loop {
            while let Some(item) = gate_for_worker.pop_for_send() {
                let _in_flight_guard = InFlightSendGuard {
                    gate: &gate_for_worker,
                };
                deliver_tool_update(&sink_for_worker, item).await;
            }
            let notified = gate_for_worker.notify.notified();
            {
                let guard = gate_for_worker
                    .state
                    .lock()
                    .expect("tool update gate lock poisoned");
                if !guard.queue.is_empty() {
                    continue;
                }
                if !guard.accepting {
                    break;
                }
            }
            notified.await;
        }
    });

    let tool_call_id = prepared.tool_call.id.clone();
    let args = prepared.args.clone();
    let started_at = Instant::now();
    let exec_res: Result<AgentToolResult, String> = match catch_unwind(AssertUnwindSafe(|| {
        (prepared.tool.execute)(tool_call_id, args, cancel_token, Some(update_cb))
    })) {
        Ok(fut) => match AssertUnwindSafe(fut).catch_unwind().await {
            Ok(res) => res,
            Err(payload) => Err(format!(
                "Tool execution panicked: {}",
                panic_payload_to_string(payload)
            )),
        },
        Err(payload) => Err(format!(
            "Tool execution panicked: {}",
            panic_payload_to_string(payload)
        )),
    };
    let duration_ms = started_at
        .elapsed()
        .as_secs_f64()
        .mul_add(1000.0, 0.0)
        .round() as u64;

    // 锁内关闭门控并取出剩余队列，锁外等待 worker 及全部已接受更新发送完毕后回收 worker
    gate.close_and_drain(&on_update).await;
    let _ = worker_handle.await;

    match exec_res {
        Ok(result) => {
            let is_error = result.is_error;
            ExecutedToolCallOutcome {
                result,
                is_error,
                duration_ms,
            }
        }
        Err(err_msg) => ExecutedToolCallOutcome {
            result: AgentToolResult::error_text(err_msg),
            is_error: true,
            duration_ms,
        },
    }
}

async fn finalize_executed_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: PreparedToolCall,
    executed: ExecutedToolCallOutcome,
    hooks: &ToolCallHooks,
    cancel_token: Option<CancellationToken>,
) -> AgentToolCallOutcome {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_fn) = &hooks.after_tool_call {
        let after_ctx = AfterToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call: prepared.tool_call.clone(),
            args: prepared.args.clone(),
            result: result.clone(),
            is_error,
            context: current_context.clone(),
        };
        let after_outcome =
            match catch_unwind(AssertUnwindSafe(|| after_fn(after_ctx, cancel_token))) {
                Ok(fut) => match AssertUnwindSafe(fut).catch_unwind().await {
                    Ok(res) => res,
                    Err(payload) => Err(format!(
                        "after_tool_call panicked: {}",
                        panic_payload_to_string(payload)
                    )),
                },
                Err(payload) => Err(format!(
                    "after_tool_call panicked: {}",
                    panic_payload_to_string(payload)
                )),
            };
        match after_outcome {
            Ok(Some(after_res)) => {
                // 若替换了 content 但未同时提供 structured_content，必须丢弃旧的 structured_content
                let next_structured = after_res.structured_content.or_else(|| {
                    if after_res.content.is_some() {
                        None
                    } else {
                        result.structured_content.clone()
                    }
                });
                if let Some(content) = after_res.content {
                    result.content = content;
                }
                if let Some(details) = after_res.details {
                    result.details = details;
                }
                if let Some(usage) = after_res.usage {
                    result.usage = Some(usage);
                }
                if let Some(terminate) = after_res.terminate {
                    result.terminate = Some(terminate);
                }
                result.structured_content = next_structured;
                if let Some(err_flag) = after_res.is_error {
                    is_error = err_flag;
                }
            }
            Ok(None) => {}
            Err(err_msg) => {
                result = AgentToolResult::error_text(err_msg);
                is_error = true;
            }
        }
    }

    AgentToolCallOutcome {
        tool_call: prepared.tool_call,
        result,
        is_error,
        duration_ms: Some(executed.duration_ms),
    }
}

async fn emit_tool_execution_end(finalized: &AgentToolCallOutcome, emit: &AgentEventSink) {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: finalized.result.clone(),
        is_error: finalized.is_error,
        duration_ms: finalized.duration_ms,
        parent_tool_call_id: None,
    })
    .await;
}

fn create_tool_result_message(finalized: &AgentToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: Some(finalized.result.details.clone()),
        usage: finalized.result.usage.clone(),
        nested_calls: None,
        is_error: finalized.is_error,
        timestamp: now_ms(),
        duration_ms: finalized.duration_ms,
    }
}

async fn finalize_and_emit_tool_result_message(
    msg: ToolResultMessage,
    config: &AgentLoopConfig,
    cancel_token: Option<CancellationToken>,
    emit: &AgentEventSink,
) -> Result<ToolResultMessage, String> {
    let original_tool_call_id = msg.tool_call_id.clone();
    let finalized =
        finalize_message_with_hook(AgentMessage::ToolResult(msg), config, cancel_token).await?;
    let AgentMessage::ToolResult(mut final_tr) = finalized else {
        return Err(format!(
            "finalize_message returned non-toolResult role \"{}\" for toolResult message",
            finalized.role()
        ));
    };
    final_tr.tool_call_id = original_tool_call_id;
    let wrapped = AgentMessage::ToolResult(final_tr.clone());
    emit(AgentEvent::MessageStart {
        message: wrapped.clone(),
    })
    .await;
    emit(AgentEvent::MessageEnd { message: wrapped }).await;
    Ok(final_tr)
}

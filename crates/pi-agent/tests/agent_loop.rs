use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::{
    AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig, AgentLoopTurnUpdate,
    AgentMessage, AgentRequestUpdate, AgentTool, AgentToolCallOutcome, AgentToolResult,
    AgentTurnDecision, BeforeToolCallResult, CustomAgentMessage, RunToolCallOptions, StreamFn,
    ToolExecutionMode, declare_tool_changes, run_agent_loop, run_tool_call,
};
use pi_ai::faux::{faux_assistant_message, faux_text, faux_text_message, faux_tool_call};
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, JsonObject, ModelThinkingLevel, StopReason,
    SystemContent, SystemMessage, TextContent, ToolCall, UserContentBlock, UserMessage,
    create_assistant_message_event_stream, now_ms,
};
use serde_json::json;

fn scripted_stream_fn(steps: Vec<AssistantMessage>) -> (StreamFn, Arc<AtomicUsize>) {
    let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(steps)));
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = calls.clone();

    let stream_fn: StreamFn = Arc::new(move |model, _ctx, _opts| {
        let queue = queue.clone();
        let calls_clone = calls_clone.clone();
        Box::pin(async move {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            let stream = create_assistant_message_event_stream();
            let writer = stream.clone();
            let mut msg = queue
                .lock()
                .expect("queue lock")
                .pop_front()
                .unwrap_or_else(|| faux_text_message("default done"));
            msg.model = model.id.clone();
            msg.provider = model.provider.clone();
            msg.api = model.api.clone();

            tokio::spawn(async move {
                let mut partial = AssistantMessage::new_pending(&model);
                if matches!(msg.stop_reason, StopReason::Error | StopReason::Aborted) {
                    writer.push(AssistantMessageEvent::Error {
                        reason: msg.stop_reason,
                        error: msg,
                    });
                    writer.end(None);
                    return;
                }
                writer.push(AssistantMessageEvent::Start {
                    partial: partial.clone(),
                });
                partial.content = msg.content.clone();
                writer.push(AssistantMessageEvent::Done {
                    reason: msg.stop_reason,
                    message: msg,
                });
                writer.end(None);
            });

            stream
        })
    });

    (stream_fn, calls)
}

#[test]
fn agent_event_and_tool_outcome_serde_use_snake_case_tags_and_camel_case_fields() {
    let assistant_msg = faux_text_message("hello");
    let update_ev = AgentEvent::MessageUpdate {
        message: AgentMessage::Assistant(assistant_msg.clone()),
        assistant_message_event: AssistantMessageEvent::Start {
            partial: assistant_msg.clone(),
        },
    };
    let update_json = serde_json::to_value(&update_ev).expect("serialize message_update");
    assert_eq!(update_json["type"], "message_update");
    assert!(update_json.get("assistantMessageEvent").is_some());
    assert!(update_json.get("assistant_message_event").is_none());

    let roundtrip_update: AgentEvent =
        serde_json::from_value(update_json).expect("deserialize message_update");
    assert_eq!(roundtrip_update, update_ev);

    let tool_end_no_dur = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc_1".to_owned(),
        tool_name: "read".to_owned(),
        result: AgentToolResult {
            content: vec![UserContentBlock::Text(TextContent::new("content"))],
            details: json!({"lines": 5}),
            structured_content: Some(json!({"ok": true})),
            usage: None,
            is_error: false,
            terminate: None,
        },
        is_error: false,
        duration_ms: None,
        parent_tool_call_id: None,
    };
    let end_json = serde_json::to_value(&tool_end_no_dur).expect("serialize tool_execution_end");
    assert_eq!(end_json["type"], "tool_execution_end");
    assert_eq!(end_json["toolCallId"], "tc_1");
    assert_eq!(end_json["toolName"], "read");
    assert_eq!(end_json["isError"], false);
    assert!(end_json.get("durationMs").is_none());
    assert_eq!(end_json["result"]["structuredContent"], json!({"ok": true}));

    let outcome = AgentToolCallOutcome {
        tool_call: ToolCall {
            id: "tc_1".to_owned(),
            name: "read".to_owned(),
            arguments: JsonObject::new(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        },
        result: AgentToolResult::text("done"),
        is_error: false,
        duration_ms: Some(12),
    };
    let outcome_json = serde_json::to_value(&outcome).expect("serialize outcome");
    assert_eq!(outcome_json["toolCall"]["id"], "tc_1");
    assert_eq!(outcome_json["isError"], false);
    assert_eq!(outcome_json["durationMs"], 12);

    // CustomAgentMessage flattens role, timestamp, and data fields
    let mut custom_data = JsonObject::new();
    custom_data.insert("command".to_owned(), json!("cargo test"));
    let custom_msg =
        AgentMessage::Custom(CustomAgentMessage::new("bashExecution", custom_data, 99));
    let custom_json = serde_json::to_value(&custom_msg).expect("serialize custom msg");
    assert_eq!(
        custom_json,
        json!({
            "role": "bashExecution",
            "command": "cargo test",
            "timestamp": 99
        })
    );
    let custom_back: AgentMessage =
        serde_json::from_value(custom_json).expect("deserialize custom msg");
    assert_eq!(custom_back, custom_msg);
}

#[test]
fn declare_tool_changes_inserts_or_replaces_system_message_tool_deltas() {
    let tool_a = AgentTool::new(
        "alpha",
        "Alpha",
        "Alpha tool",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("a")) })),
    );
    let tool_b = AgentTool::new(
        "beta",
        "Beta",
        "Beta tool",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("b")) })),
    );

    let ctx = AgentContext {
        messages: Vec::new(),
        tools: vec![tool_a.clone()],
    };
    let prompts = vec![AgentMessage::User(UserMessage::new("hi"))];
    let declared = declare_tool_changes(&ctx, &prompts);
    assert_eq!(declared.len(), 2);
    assert_eq!(declared[0].role(), "system");
    let sys0 = declared[0].as_system().unwrap();
    assert_eq!(sys0.tools_added.as_ref().unwrap().len(), 1);
    assert_eq!(sys0.tools_added.as_ref().unwrap()[0].name, "alpha");

    // When a pending system message already exists, its tool fields are replaced with the true delta
    let ctx2 = AgentContext {
        messages: declared,
        tools: vec![tool_b.clone()],
    };
    let pending_with_sys = vec![
        AgentMessage::System(SystemMessage {
            content: SystemContent::Text("Updated prompt".to_owned()),
            sections: None,
            tools_added: Some(vec![tool_a.to_tool_declaration()]),
            tools_removed: None,
            timestamp: now_ms(),
        }),
        AgentMessage::User(UserMessage::new("next")),
    ];
    let declared2 = declare_tool_changes(&ctx2, &pending_with_sys);
    assert_eq!(declared2.len(), 2);
    let sys1 = declared2[0].as_system().unwrap();
    assert_eq!(sys1.tools_added.as_ref().unwrap()[0].name, "beta");
    assert_eq!(sys1.tools_removed.as_ref().unwrap()[0].name, "alpha");
}

#[tokio::test]
async fn truncated_length_response_fails_all_tool_calls_without_executing() {
    let executed_count = Arc::new(AtomicUsize::new(0));
    let exec_clone = executed_count.clone();
    let tool = AgentTool::new(
        "dangerous_write",
        "Write",
        "Writes data",
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        }),
        Arc::new(move |_, _, _, _| {
            let exec_clone = exec_clone.clone();
            Box::pin(async move {
                exec_clone.fetch_add(1, Ordering::SeqCst);
                Ok(AgentToolResult::text("written"))
            })
        }),
    );

    let mut args = JsonObject::new();
    args.insert("text".to_owned(), json!("partial..."));
    let truncated_assistant = faux_assistant_message(
        vec![faux_tool_call(
            "dangerous_write",
            args,
            Some("call_trunc".to_owned()),
        )],
        Some(StopReason::Length),
    );
    let final_reply = faux_text_message("Re-issued safely.");

    let (stream_fn, _) = scripted_stream_fn(vec![truncated_assistant, final_reply]);
    let config = AgentLoopConfig::new(pi_agent::default_agent_model());
    let ctx = AgentContext {
        messages: Vec::new(),
        tools: vec![tool],
    };

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let emit = Arc::new(move |ev: AgentEvent| {
        let events_clone = events_clone.clone();
        Box::pin(async move {
            events_clone.lock().unwrap().push(ev);
        }) as _
    });

    let new_msgs = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("go"))],
        ctx,
        config,
        emit,
        None,
        stream_fn,
    )
    .await;

    assert_eq!(executed_count.load(Ordering::SeqCst), 0);
    let tr = new_msgs
        .iter()
        .find_map(AgentMessage::as_tool_result)
        .expect("should have error toolResult");
    assert!(tr.is_error);
    assert!(tr.duration_ms.is_none());
    let text = match &tr.content[0] {
        UserContentBlock::Text(t) => &t.text,
        _ => panic!("expected text"),
    };
    assert!(text.contains("hit the output token limit"));
}

#[tokio::test]
async fn parallel_tool_calls_emit_end_in_completion_order_and_messages_in_source_order() {
    let slow_tool = AgentTool::new(
        "slow_tool",
        "Slow",
        "Slow tool",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                Ok(AgentToolResult::text("slow_done"))
            })
        }),
    );
    let fast_tool = AgentTool::new(
        "fast_tool",
        "Fast",
        "Fast tool",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok(AgentToolResult::text("fast_done"))
            })
        }),
    );

    let assistant_with_two_tools = faux_assistant_message(
        vec![
            faux_tool_call("slow_tool", JsonObject::new(), Some("call_slow".to_owned())),
            faux_tool_call("fast_tool", JsonObject::new(), Some("call_fast".to_owned())),
        ],
        Some(StopReason::ToolUse),
    );
    let final_reply = faux_text_message("Both finished.");

    let (stream_fn, _) = scripted_stream_fn(vec![assistant_with_two_tools, final_reply]);
    let mut config = AgentLoopConfig::new(pi_agent::default_agent_model());
    config.tool_execution = ToolExecutionMode::Parallel;

    let ctx = AgentContext {
        messages: Vec::new(),
        tools: vec![slow_tool, fast_tool],
    };

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let emit = Arc::new(move |ev: AgentEvent| {
        let events_clone = events_clone.clone();
        Box::pin(async move {
            events_clone.lock().unwrap().push(ev);
        }) as _
    });

    let _ = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("run parallel"))],
        ctx,
        config,
        emit,
        None,
        stream_fn,
    )
    .await;

    let recorded = events.lock().unwrap();
    let end_ids: Vec<String> = recorded
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                duration_ms,
                ..
            } => {
                assert!(duration_ms.is_some());
                Some(tool_call_id.clone())
            }
            _ => None,
        })
        .collect();
    // Fast tool finishes first, so its ToolExecutionEnd is emitted before slow tool
    assert_eq!(
        end_ids,
        vec!["call_fast".to_owned(), "call_slow".to_owned()]
    );

    // But ToolResultMessage artifacts are emitted in assistant source order: call_slow then call_fast!
    let msg_ids: Vec<String> = recorded
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd {
                message: AgentMessage::ToolResult(tr),
            } => Some(tr.tool_call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        msg_ids,
        vec!["call_slow".to_owned(), "call_fast".to_owned()]
    );
}

#[tokio::test]
async fn tool_update_gate_serializes_concurrent_multithreaded_updates_and_blocks_late_updates() {
    let after_hook_saw_updates = Arc::new(AtomicUsize::new(0));
    let update_count_seen_by_emit = Arc::new(AtomicUsize::new(0));

    let tool = AgentTool::new(
        "threaded_updater",
        "Threaded",
        "Spawns OS threads calling on_update before and after return",
        json!({"type": "object"}),
        Arc::new(|_id, _args, _cancel, on_update| {
            Box::pin(async move {
                let cb = on_update.expect("on_update callback must be provided");
                let mut handles = Vec::new();
                // 8 OS threads concurrently pushing updates right before execute returns
                for idx in 0..8 {
                    let cb_clone = cb.clone();
                    handles.push(std::thread::spawn(move || {
                        cb_clone(AgentToolResult::text(format!("pre_{idx}")));
                    }));
                }
                for h in handles {
                    let _ = h.join();
                }

                // Also spawn a detached thread that races with execute returning and calls on_update after return
                let cb_late = cb.clone();
                std::thread::spawn(move || {
                    for idx in 0..20 {
                        cb_late(AgentToolResult::text(format!("race_{idx}")));
                    }
                });

                Ok(AgentToolResult::text("final_output"))
            })
        }),
    );

    let assistant_call = faux_assistant_message(
        vec![faux_tool_call(
            "threaded_updater",
            JsonObject::new(),
            Some("call_gate".to_owned()),
        )],
        Some(StopReason::ToolUse),
    );
    let (stream_fn, _) = scripted_stream_fn(vec![assistant_call, faux_text_message("done")]);

    let mut config = AgentLoopConfig::new(pi_agent::default_agent_model());
    let emit_counter_for_after = update_count_seen_by_emit.clone();
    let after_counter = after_hook_saw_updates.clone();
    config.after_tool_call = Some(Arc::new(move |_ctx, _cancel| {
        let current = emit_counter_for_after.load(Ordering::SeqCst);
        after_counter.store(current, Ordering::SeqCst);
        Box::pin(async move { Ok(None) })
    }));

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let emit_counter = update_count_seen_by_emit.clone();
    let emit = Arc::new(move |ev: AgentEvent| {
        let events_clone = events_clone.clone();
        let emit_counter = emit_counter.clone();
        Box::pin(async move {
            if matches!(ev, AgentEvent::ToolExecutionUpdate { .. }) {
                tokio::time::sleep(Duration::from_millis(1)).await;
                emit_counter.fetch_add(1, Ordering::SeqCst);
            }
            events_clone.lock().unwrap().push(ev);
        }) as _
    });

    let ctx = AgentContext {
        messages: Vec::new(),
        tools: vec![tool],
    };
    let _ = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("test gate"))],
        ctx,
        config,
        emit,
        None,
        stream_fn,
    )
    .await;

    // Wait briefly to confirm no detached thread can emit a late ToolExecutionUpdate after ToolExecutionEnd
    tokio::time::sleep(Duration::from_millis(20)).await;

    let recorded = events.lock().unwrap();
    let total_updates = update_count_seen_by_emit.load(Ordering::SeqCst);
    assert!(
        total_updates >= 8,
        "all 8 pre-return updates must be delivered, got {total_updates}"
    );
    assert_eq!(
        after_hook_saw_updates.load(Ordering::SeqCst),
        total_updates,
        "all accepted updates must complete before after_tool_call runs"
    );

    let end_pos = recorded
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }))
        .expect("ToolExecutionEnd must exist");
    for (idx, ev) in recorded.iter().enumerate() {
        if matches!(ev, AgentEvent::ToolExecutionUpdate { .. }) {
            assert!(
                idx < end_pos,
                "ToolExecutionUpdate at index {idx} appeared after ToolExecutionEnd at {end_pos}"
            );
        }
    }
}

#[tokio::test]
async fn hooks_after_tool_call_drops_stale_structured_content_and_terminate_stops_batch() {
    let tool = AgentTool::new(
        "structured_tool",
        "Structured",
        "Returns structured content",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("raw"))],
                    details: json!({"orig": 1}),
                    structured_content: Some(json!({"secret": 42})),
                    usage: None,
                    is_error: false,
                    terminate: None,
                })
            })
        }),
    );

    let assistant_msg = faux_assistant_message(
        vec![faux_tool_call(
            "structured_tool",
            JsonObject::new(),
            Some("call_s".to_owned()),
        )],
        Some(StopReason::ToolUse),
    );

    let outcome = run_tool_call(
        ToolCall {
            id: "call_s".to_owned(),
            name: "structured_tool".to_owned(),
            arguments: JsonObject::new(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        },
        RunToolCallOptions {
            tools: vec![tool.clone()],
            assistant_message: assistant_msg.clone(),
            context: AgentContext {
                messages: Vec::new(),
                tools: vec![tool.clone()],
            },
            cancel_token: None,
            on_update: None,
            before_tool_call: None,
            after_tool_call: Some(Arc::new(|_ctx, _cancel| {
                Box::pin(async move {
                    Ok(Some(AfterToolCallResult {
                        content: Some(vec![UserContentBlock::Text(TextContent::new("redacted"))]),
                        details: None,
                        structured_content: None,
                        is_error: None,
                        usage: None,
                        terminate: Some(true),
                    }))
                })
            })),
        },
    )
    .await;

    assert_eq!(outcome.result.first_text(), Some("redacted"));
    // Because content was replaced without providing structured_content, structured_content is cleared!
    assert!(outcome.result.structured_content.is_none());
    assert_eq!(outcome.result.terminate, Some(true));
    assert!(outcome.duration_ms.is_some());

    // Verify batch early termination when before_tool_call blocks with terminate: true
    let (stream_fn, call_count) = scripted_stream_fn(vec![
        assistant_msg,
        faux_text_message("should not be reached"),
    ]);
    let mut config = AgentLoopConfig::new(pi_agent::default_agent_model());
    config.before_tool_call = Some(Arc::new(|_ctx, _cancel| {
        Box::pin(async move {
            Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some("Stopped by policy".to_owned()),
                terminate: Some(true),
                args: None,
            }))
        })
    }));

    let emit = Arc::new(|_ev: AgentEvent| Box::pin(async {}) as _);
    let _ = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("start"))],
        AgentContext {
            messages: Vec::new(),
            tools: vec![tool],
        },
        config,
        emit,
        None,
        stream_fn,
    )
    .await;

    // Only 1 LLM call occurred because terminate: true stopped the loop after the tool batch!
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn finish_turn_continue_triggers_exactly_one_context_only_turn_and_does_not_stack_on_natural_continuation()
 {
    // Case 1: No tool calls and no queued messages -> FinishTurn::Continue triggers 1 extra request (total 2 calls)
    let (stream_fn_1, calls_1) = scripted_stream_fn(vec![
        faux_text_message("turn 1"),
        faux_text_message("turn 2 after continue"),
        faux_text_message("turn 3 unused"),
    ]);
    let mut config_1 = AgentLoopConfig::new(pi_agent::default_agent_model());
    let turn_counter_1 = Arc::new(AtomicUsize::new(0));
    let tc1 = turn_counter_1.clone();
    config_1.finish_turn = Some(Arc::new(move |_turn_ctx, _cancel| {
        let idx = tc1.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Some(AgentTurnDecision::Continue)
            } else {
                None
            }
        })
    }));

    let emit_noop = Arc::new(|_ev: AgentEvent| Box::pin(async {}) as _);
    let msgs_1 = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("hello"))],
        AgentContext::default(),
        config_1,
        emit_noop.clone(),
        None,
        stream_fn_1,
    )
    .await;
    assert_eq!(calls_1.load(Ordering::SeqCst), 2);
    assert_eq!(msgs_1.len(), 3); // 1 user + 2 assistant

    // Case 2: Turn already has a steering message (natural continuation) -> FinishTurn::Continue does NOT add a 3rd request
    let (stream_fn_2, calls_2) = scripted_stream_fn(vec![
        faux_text_message("turn 1"),
        faux_text_message("turn 2 for steering"),
        faux_text_message("turn 3 should not run"),
    ]);
    let mut config_2 = AgentLoopConfig::new(pi_agent::default_agent_model());
    let tc2 = Arc::new(AtomicUsize::new(0));
    let tc2_clone = tc2.clone();
    config_2.finish_turn = Some(Arc::new(move |_turn_ctx, _cancel| {
        let idx = tc2_clone.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Some(AgentTurnDecision::Continue)
            } else {
                None
            }
        })
    }));

    let steer_polls = Arc::new(AtomicUsize::new(0));
    let sp_clone = steer_polls.clone();
    config_2.get_steering_messages = Some(Arc::new(move || {
        let poll_idx = sp_clone.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            // Poll 0 is initial poll (empty); Poll 1 is end of turn 1 (returns 1 steering message)
            if poll_idx == 1 {
                vec![AgentMessage::User(UserMessage::new("steered"))]
            } else {
                Vec::new()
            }
        })
    }));

    let _ = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("hello"))],
        AgentContext::default(),
        config_2,
        emit_noop,
        None,
        stream_fn_2,
    )
    .await;
    assert_eq!(
        calls_2.load(Ordering::SeqCst),
        2,
        "steering message satisfies Continue without adding a 3rd LLM call"
    );
}

#[tokio::test]
async fn prepare_next_turn_and_prepare_request_hooks_update_model_thinking_and_messages() {
    let (stream_fn, calls) = scripted_stream_fn(vec![
        faux_assistant_message(vec![faux_text("first")], Some(StopReason::Stop)),
        faux_assistant_message(vec![faux_text("second")], Some(StopReason::Stop)),
    ]);

    let mut config = AgentLoopConfig::new(pi_agent::default_agent_model());
    let turn_idx = Arc::new(AtomicUsize::new(0));
    let ti = turn_idx.clone();
    config.finish_turn = Some(Arc::new(move |_, _| {
        let i = ti.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if i == 0 {
                Some(AgentTurnDecision::Continue)
            } else {
                None
            }
        })
    }));

    config.prepare_next_turn = Some(Arc::new(|_last_turn| {
        Box::pin(async move {
            Some(AgentLoopTurnUpdate {
                context: None,
                messages: Some(vec![AgentMessage::User(UserMessage::new(
                    "injected by prepare_next_turn",
                ))]),
                model: None,
                thinking_level: Some(ModelThinkingLevel::High),
            })
        })
    }));

    let seen_levels = Arc::new(Mutex::new(Vec::new()));
    let sl = seen_levels.clone();
    config.prepare_request = Some(Arc::new(move |req_ctx, _cancel| {
        sl.lock().unwrap().push(req_ctx.thinking_level);
        Box::pin(async move {
            Some(AgentRequestUpdate {
                context: None,
                model: None,
                thinking_level: Some(ModelThinkingLevel::Medium),
            })
        })
    }));

    let emit_noop = Arc::new(|_ev: AgentEvent| Box::pin(async {}) as _);
    let new_msgs = run_agent_loop(
        vec![AgentMessage::User(UserMessage::new("start"))],
        AgentContext::default(),
        config,
        emit_noop,
        None,
        stream_fn,
    )
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        *seen_levels.lock().unwrap(),
        vec![ModelThinkingLevel::Off, ModelThinkingLevel::High]
    );
    // And prepare_request overrode thinking_level to Medium for the actual response!
    let last_assistant = new_msgs.last().unwrap().as_assistant().unwrap();
    assert_eq!(
        last_assistant.thinking_level,
        Some(ModelThinkingLevel::Medium)
    );
}

#[tokio::test]
async fn finalize_message_rewrites_assistant_tool_calls_tool_results_and_terminates_on_error() {
    let mut tc_args = JsonObject::new();
    tc_args.insert("path".to_owned(), json!("/orig"));
    let (stream_fn, calls) = scripted_stream_fn(vec![
        faux_assistant_message(
            vec![faux_tool_call("read", tc_args, Some("tc_1".to_owned()))],
            Some(StopReason::ToolUse),
        ),
        faux_assistant_message(vec![faux_text("done")], Some(StopReason::Stop)),
    ]);

    let seen_tool_args = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen_tool_args.clone();
    let read_tool = AgentTool::new(
        "read",
        "Read",
        "Read file",
        json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        Arc::new(move |_id, args, _cancel, _upd| {
            seen_clone.lock().unwrap().push(args);
            Box::pin(async move { Ok(AgentToolResult::text("raw-tool-output")) })
        }),
    );

    let mut opts = pi_agent::AgentOptions::new(stream_fn);
    opts.initial_state = Some(pi_agent::AgentInitialState {
        tools: Some(vec![read_tool.clone()]),
        ..Default::default()
    });
    opts.finalize_message = Some(Arc::new(|msg, _cancel| {
        Box::pin(async move {
            match msg {
                AgentMessage::Assistant(mut a) => {
                    for block in &mut a.content {
                        if let pi_ai::ContentBlock::ToolCall(tc) = block {
                            tc.arguments.insert("path".to_owned(), json!("/rewritten"));
                        }
                    }
                    Ok(AgentMessage::Assistant(a))
                }
                AgentMessage::ToolResult(mut tr) => {
                    tr.content = vec![UserContentBlock::Text(TextContent::new(
                        "finalized-tool-output",
                    ))];
                    Ok(AgentMessage::ToolResult(tr))
                }
                other => Ok(other),
            }
        })
    }));

    let agent = pi_agent::Agent::new(opts);
    let saw_tool_start_content = Arc::new(Mutex::new(Vec::new()));
    let stc = saw_tool_start_content.clone();
    let _unsub = agent.subscribe(Arc::new(move |ev, _| {
        let stc = stc.clone();
        Box::pin(async move {
            if let AgentEvent::MessageStart {
                message: AgentMessage::ToolResult(tr),
            } = ev
            {
                stc.lock().unwrap().push(tr.content.clone());
            }
        })
    }));

    agent.prompt("read file").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // Tool execution received rewritten tool call arguments from finalized assistant message!
    assert_eq!(
        *seen_tool_args.lock().unwrap(),
        vec![json!({"path": "/rewritten"})]
    );
    // ToolResult MessageStart already saw finalized content!
    assert_eq!(
        saw_tool_start_content.lock().unwrap()[0],
        vec![UserContentBlock::Text(TextContent::new(
            "finalized-tool-output"
        ))]
    );
    // AgentState also stores the finalized tool result
    let state = agent.state();
    let tr_in_state = state
        .messages
        .iter()
        .find_map(|m| m.as_tool_result())
        .unwrap();
    assert_eq!(
        tr_in_state.content,
        vec![UserContentBlock::Text(TextContent::new(
            "finalized-tool-output"
        ))]
    );

    // Now test failure in finalize_message on assistant response: must NOT execute tool!
    let mut tc_args2 = JsonObject::new();
    tc_args2.insert("path".to_owned(), json!("/never"));
    let (stream_fn2, calls2) = scripted_stream_fn(vec![faux_assistant_message(
        vec![faux_tool_call("read", tc_args2, Some("tc_2".to_owned()))],
        Some(StopReason::ToolUse),
    )]);
    let tool_ran = Arc::new(AtomicUsize::new(0));
    let tr_clone = tool_ran.clone();
    let read_tool2 = AgentTool::new(
        "read",
        "Read",
        "Read file",
        json!({"type": "object"}),
        Arc::new(move |_, _, _, _| {
            tr_clone.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(AgentToolResult::text("should not run")) })
        }),
    );
    let mut opts2 = pi_agent::AgentOptions::new(stream_fn2);
    opts2.initial_state = Some(pi_agent::AgentInitialState {
        tools: Some(vec![read_tool2]),
        ..Default::default()
    });
    opts2.finalize_message = Some(Arc::new(|msg, _| {
        Box::pin(async move {
            if matches!(msg, AgentMessage::Assistant(_)) {
                Err("disk persistence failed".to_owned())
            } else {
                Ok(msg)
            }
        })
    }));
    let agent2 = pi_agent::Agent::new(opts2);
    let err = agent2.prompt("try fail").await.unwrap_err();
    assert!(err.contains("disk persistence failed"));
    assert_eq!(calls2.load(Ordering::SeqCst), 1);
    assert_eq!(tool_ran.load(Ordering::SeqCst), 0);
    assert_eq!(
        agent2.state().error_message.as_deref(),
        Some("disk persistence failed")
    );
}

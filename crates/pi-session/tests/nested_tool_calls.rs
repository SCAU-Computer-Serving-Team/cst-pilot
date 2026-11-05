use std::sync::{Arc, Mutex};

use pi_agent::{AgentMessage, AgentToolResult, ToolExecutionMode};
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text_message, faux_tool_call,
    register_faux_provider,
};
use pi_ai::{JsonObject, Models, StopReason, Usage, UsageCost};
use pi_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, InlineExtension,
    NESTED_MAX_ARGUMENT_BYTES_PER_CALL, NESTED_MAX_CALLS, NestedCallRecorder,
    NestedToolCallOptions, SessionManager, ToolDefinition, ToolExposure,
};
use serde_json::json;

#[test]
fn nested_call_recorder_enforces_call_count_argument_bytes_and_error_char_limits() {
    let mut rec = NestedCallRecorder::new();

    // 1. 单次参数超过 8 KiB：省略 arguments，记录 arguments_bytes，且 complete 变为 false
    let mut big_args = JsonObject::new();
    big_args.insert(
        "payload".to_owned(),
        json!("x".repeat(NESTED_MAX_ARGUMENT_BYTES_PER_CALL + 100)),
    );
    let tc_big = pi_ai::ToolCall {
        id: "c1/1".to_owned(),
        name: "heavy".to_owned(),
        arguments: big_args,
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    };
    let idx_big = rec.start(&tc_big);
    assert_eq!(idx_big, Some(0));
    let long_err = "e".repeat(700);
    rec.finish(idx_big, true, &long_err);

    let snap1 = rec.snapshot().unwrap();
    assert!(!snap1.complete);
    assert!(snap1.calls[0].arguments.is_none());
    assert!(snap1.calls[0].arguments_bytes.unwrap() > NESTED_MAX_ARGUMENT_BYTES_PER_CALL);
    assert_eq!(snap1.calls[0].status, "error");
    assert_eq!(snap1.calls[0].error.as_ref().unwrap().len(), 500);

    // 2. 超过 256 次调用上限：第 257 次 start 返回 None，但 add_usage 仍累加其用量
    let mut rec2 = NestedCallRecorder::new();
    for i in 1..=NESTED_MAX_CALLS {
        let tc = pi_ai::ToolCall {
            id: format!("c/{i}"),
            name: "noop".to_owned(),
            arguments: JsonObject::new(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        };
        let idx = rec2.start(&tc);
        assert!(idx.is_some());
        rec2.finish(idx, false, "");
    }
    assert!(rec2.snapshot().unwrap().complete);

    let tc_257 = pi_ai::ToolCall {
        id: "c/257".to_owned(),
        name: "noop".to_owned(),
        arguments: JsonObject::new(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    };
    assert!(rec2.start(&tc_257).is_none());
    rec2.add_usage(&Usage {
        input: 7,
        output: 3,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 10,
        cost: UsageCost::default(),
    });
    let snap2 = rec2.snapshot().unwrap();
    assert!(!snap2.complete);
    assert_eq!(snap2.calls.len(), NESTED_MAX_CALLS);
    assert_eq!(rec2.total_usage().unwrap().total_tokens, 10);
}

#[tokio::test]
async fn sequential_reentrant_nested_calls_assign_hierarchical_ids_and_merge_before_message_start()
{
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    let mut args = JsonObject::new();
    args.insert("seed".to_owned(), json!(10));
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "parent_seq",
                args,
                Some("root_call".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("sequential nested calls succeeded")),
    ]);

    let ext = InlineExtension::new("<test:nested-seq>", |api| {
        // 孙子工具（Codemode + Sequential）
        let mut leaf_tool = ToolDefinition::new(
            "leaf_seq",
            "Leaf Seq",
            "Leaf tool",
            json!({
                "type": "object",
                "properties": { "v": { "type": "integer" } },
                "required": ["v"]
            }),
            Arc::new(|_id, args, _cancel, _upd, _ctx| {
                Box::pin(async move {
                    let v = args["v"].as_i64().unwrap_or(0);
                    let mut r = AgentToolResult::text(format!("{}", v + 1));
                    r.usage = Some(Usage {
                        input: 4,
                        output: 6,
                        cache_read: 0,
                        cache_write: 0,
                        cache_write_1h: None,
                        reasoning: None,
                        total_tokens: 10,
                        cost: UsageCost::default(),
                    });
                    Ok(r)
                })
            }),
        );
        leaf_tool.exposure = ToolExposure::Codemode;
        leaf_tool.execution_mode = Some(ToolExecutionMode::Sequential);
        api.register_tool(leaf_tool);

        // 子工具（Codemode + Sequential）：在内部再次嵌套调用 leaf_seq，验证串行重入不死锁且 ID 为 root_call/1/1
        let mut mid_tool = ToolDefinition::new(
            "mid_seq",
            "Mid Seq",
            "Middle tool",
            json!({
                "type": "object",
                "properties": { "v": { "type": "integer" } },
                "required": ["v"]
            }),
            Arc::new(|_id, args, _cancel, _upd, ctx| {
                Box::pin(async move {
                    let v = args["v"].as_i64().unwrap_or(0);
                    let child = ctx
                        .execute_tool(
                            "leaf_seq",
                            json!({ "v": v * 2 }),
                            NestedToolCallOptions::default(),
                        )
                        .await;
                    let mut r =
                        AgentToolResult::text(child.result.first_text().unwrap_or("0").to_owned());
                    r.usage = Some(Usage {
                        input: 2,
                        output: 3,
                        cache_read: 0,
                        cache_write: 0,
                        cache_write_1h: None,
                        reasoning: None,
                        total_tokens: 5,
                        cost: UsageCost::default(),
                    });
                    Ok(r)
                })
            }),
        );
        mid_tool.exposure = ToolExposure::Codemode;
        mid_tool.execution_mode = Some(ToolExecutionMode::Sequential);
        api.register_tool(mid_tool);

        // 顶层工具（Direct + Sequential）：调用 mid_seq，同时尝试调用 ModelOnly 工具验证被拒绝
        let mut model_only_tool = ToolDefinition::new(
            "model_only_op",
            "Model Only",
            "Not callable via execute_tool",
            json!({"type": "object"}),
            Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("forbidden")) })),
        );
        model_only_tool.exposure = ToolExposure::ModelOnly;
        api.register_tool(model_only_tool);

        let mut parent_tool = ToolDefinition::new(
            "parent_seq",
            "Parent Seq",
            "Parent sequential tool",
            json!({
                "type": "object",
                "properties": { "seed": { "type": "integer" } },
                "required": ["seed"]
            }),
            Arc::new(|_id, args, _cancel, _upd, ctx| {
                Box::pin(async move {
                    let seed = args["seed"].as_i64().unwrap_or(0);
                    let out1 = ctx
                        .execute_tool(
                            "mid_seq",
                            json!({ "v": seed }),
                            NestedToolCallOptions::default(),
                        )
                        .await;
                    // ModelOnly 工具不可通过 execute_tool 调用
                    let forbidden = ctx
                        .execute_tool("model_only_op", json!({}), NestedToolCallOptions::default())
                        .await;
                    assert!(forbidden.is_error);
                    Ok(AgentToolResult::text(
                        out1.result.first_text().unwrap_or("").to_owned(),
                    ))
                })
            }),
        );
        parent_tool.execution_mode = Some(ToolExecutionMode::Sequential);
        api.register_tool(parent_tool);
        Ok(())
    });

    let sm = SessionManager::in_memory("/proj", None, None).unwrap();
    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models, sm);
    opts.tool_execution = ToolExecutionMode::Sequential;
    opts.extensions = vec![Box::new(ext)];

    let session = AgentSession::new(opts).unwrap();
    let msg_start_nested: Arc<Mutex<Option<pi_ai::NestedToolCalls>>> = Arc::new(Mutex::new(None));
    let msn = msg_start_nested.clone();
    let nested_events = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    let ne = nested_events.clone();

    let _unsub = session.subscribe(Arc::new(move |ev| match ev {
        AgentSessionEvent::ToolExecutionStart {
            tool_call_id,
            parent_tool_call_id,
            ..
        } => {
            ne.lock().unwrap().push((tool_call_id, parent_tool_call_id));
        }
        AgentSessionEvent::MessageStart {
            message: AgentMessage::ToolResult(tr),
        } => {
            *msn.lock().unwrap() = tr.nested_calls.clone();
        }
        _ => {}
    }));

    session
        .prompt("run sequential nested test", None)
        .await
        .unwrap();

    // 1. 验证 ToolExecutionStart 事件中携带了正确的 parent_tool_call_id 层级链
    let recorded_starts = nested_events.lock().unwrap().clone();
    assert_eq!(
        recorded_starts,
        vec![
            ("root_call".to_owned(), None),
            ("root_call/1".to_owned(), Some("root_call".to_owned())),
            ("root_call/1/1".to_owned(), Some("root_call/1".to_owned())),
            ("root_call/2".to_owned(), Some("root_call".to_owned())),
        ]
    );

    // 2. 验证父工具结果在发出 MessageStart 时就已经合入了完整的 nested_calls！
    let start_nc = msg_start_nested
        .lock()
        .unwrap()
        .clone()
        .expect("nested_calls must be present on MessageStart");
    assert!(start_nc.complete);
    let call_ids: Vec<&str> = start_nc.calls.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        call_ids,
        vec!["root_call/1", "root_call/1/1", "root_call/2"]
    );

    // 3. 验证汇总 usage (10 + 5 = 15 tokens) 已合入父 ToolResultMessage
    let state = session.state();
    let tr = state
        .messages
        .iter()
        .find_map(|m| m.as_tool_result())
        .unwrap();
    assert_eq!(tr.usage.as_ref().unwrap().total_tokens, 15);
}

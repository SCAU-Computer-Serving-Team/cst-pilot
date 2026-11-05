use std::sync::{Arc, Mutex};

use pi_agent::{AgentMessage, AgentToolResult, models_stream_fn};
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text_message, faux_tool_call,
    register_faux_provider,
};
use pi_ai::{
    ImageContent, JsonObject, Models, NestedToolCallRecord, NestedToolCalls, RetryPolicy,
    StopReason, TextContent, ToolResultMessage, Usage, UsageCost, UserContent, UserContentBlock,
    UserMessage,
};
use pi_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, CompactionDetails, CompactionReason,
    CompactionSettings, ContextEditReplacement, SessionEntry, SessionManager,
    SummarizationCallOptions, ToolDefinition, compact, estimate_projected_context_tokens,
    estimate_tokens, find_projected_cut_point, prepare_compaction,
    prepare_compaction_with_adaptive_budget,
};
use serde_json::json;

#[test]
fn token_estimation_invalidates_stale_usage_after_edit_or_compaction_and_cut_point_skips_tool_result()
 {
    // 1. 图片估算：单张图片固定 4800 字符 = 1200 tokens
    let img_msg = AgentMessage::User(UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Image(ImageContent {
            data: "AAAA".to_owned(),
            mime_type: "image/png".to_owned(),
        })]),
        timestamp: 100,
    });
    assert_eq!(estimate_tokens(&img_msg), 1200);

    // 2. 当最后一条带 usage 的助手消息之后发生了 ContextEdit 或 Compaction 时，自动作废旧 usage 改用纯消息估算
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();
    let u1 = sm
        .append_message(AgentMessage::User(UserMessage::new("short user")))
        .unwrap();
    let mut asst = faux_text_message("short assistant");
    asst.usage = Usage {
        input: 80_000,
        output: 10_000,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 90_000,
        cost: UsageCost::default(),
    };
    let a1 = sm.append_message(AgentMessage::Assistant(asst)).unwrap();

    // 未发生编辑前，采用 usage = 90_000
    let proj_before = sm.build_session_projection();
    let branch_before = sm.get_branch(None);
    let est_before = estimate_projected_context_tokens(&proj_before, &branch_before);
    assert_eq!(est_before.tokens, 90_000);
    assert!(est_before.last_usage_index.is_some());

    // 追加一条 ContextEdit 后，旧 usage 失效，回退到纯文本长度估算（远小于 90_000）
    sm.append_context_edit(
        &a1,
        Some(ContextEditReplacement::from_text("tiny edited reply")),
    )
    .unwrap();
    let proj_after = sm.build_session_projection();
    let branch_after = sm.get_branch(None);
    let est_after = estimate_projected_context_tokens(&proj_after, &branch_after);
    assert!(est_after.tokens < 100);
    assert!(est_after.last_usage_index.is_none());

    // 3. 切点查找绝不切在 toolResult 上：若尾部巨型 toolResult 自身超预算，切点应落在其前置的 assistant 工具调用上
    let mut sm2 = SessionManager::in_memory("/proj", None, None).unwrap();
    let _ = sm2
        .append_message(AgentMessage::User(UserMessage::new("turn 1 user")))
        .unwrap();
    let mut tc_args = JsonObject::new();
    tc_args.insert("path".to_owned(), json!("src/main.rs"));
    let asst_tc = faux_assistant_message(
        vec![faux_tool_call("read", tc_args, Some("tc_big".to_owned()))],
        Some(StopReason::ToolUse),
    );
    let asst_id = sm2
        .append_message(AgentMessage::Assistant(asst_tc))
        .unwrap();
    let big_tr = ToolResultMessage {
        tool_call_id: "tc_big".to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new("x".repeat(4000)))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 200,
        duration_ms: None,
    };
    let _tr_id = sm2
        .append_message(AgentMessage::ToolResult(big_tr))
        .unwrap();

    let proj2 = sm2.build_session_projection();
    let cut = find_projected_cut_point(&proj2.entries, 0, proj2.entries.len(), 500);
    assert_eq!(
        proj2.entries[cut.first_kept_entry_index].source_entry.id(),
        asst_id,
        "cut point must land on preceding assistant message, never on toolResult"
    );
    assert!(cut.is_split_turn);
    assert_eq!(cut.turn_start_index, Some(0));
    let _ = u1;
}

#[tokio::test]
async fn split_turn_compaction_file_ops_aggregation_and_truncated_summary_rejection_work() {
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();
    // 先放一条前序非 hook 压缩记录，带有 read_files: ["a.rs", "b.rs"], modified_files: ["c.rs"]
    let u0 = sm
        .append_message(AgentMessage::User(UserMessage::new("early")))
        .unwrap();
    let prev_details = serde_json::to_value(CompactionDetails {
        read_files: vec!["a.rs".to_owned(), "b.rs".to_owned()],
        modified_files: vec!["c.rs".to_owned()],
    })
    .unwrap();
    let _ = sm
        .append_compaction(
            "previous summary text",
            Some(u0),
            200,
            Some(prev_details),
            Some(false),
            None,
        )
        .unwrap();

    // 轮次 1：读取 d.rs，并在 toolResult.nested_calls 中编辑 b.rs（使 b.rs 从只读升级为已修改）
    let _u1 = sm
        .append_message(AgentMessage::User(UserMessage::new(
            "turn 1 request ".repeat(30),
        )))
        .unwrap();
    let mut read_args = JsonObject::new();
    read_args.insert("path".to_owned(), json!("d.rs"));
    let a1 = faux_assistant_message(
        vec![faux_tool_call("read", read_args, Some("tc_1".to_owned()))],
        Some(StopReason::ToolUse),
    );
    let _ = sm.append_message(AgentMessage::Assistant(a1)).unwrap();

    let mut nested_edit_args = JsonObject::new();
    nested_edit_args.insert("path".to_owned(), json!("b.rs"));
    let tr1 = ToolResultMessage {
        tool_call_id: "tc_1".to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new("content of d.rs"))],
        details: None,
        usage: None,
        nested_calls: Some(NestedToolCalls {
            calls: vec![NestedToolCallRecord {
                id: "tc_1/1".to_owned(),
                name: "edit".to_owned(),
                arguments: Some(nested_edit_args),
                arguments_bytes: None,
                status: "ok".to_owned(),
                duration_ms: Some(2),
                error: None,
            }],
            complete: true,
        }),
        is_error: false,
        timestamp: 300,
        duration_ms: None,
    };
    let _ = sm.append_message(AgentMessage::ToolResult(tr1)).unwrap();
    let _ = sm
        .append_message(AgentMessage::Assistant(faux_text_message(
            "turn 1 final reply ".repeat(30),
        )))
        .unwrap();

    // 轮次 2（将被分裂的轮次）：用户提问 -> 助手步骤 1（写入 e.rs）-> 工具结果 -> 助手步骤 2（长回复，落入保留区间）
    let _u2 = sm
        .append_message(AgentMessage::User(UserMessage::new(
            "turn 2 split user request ".repeat(20),
        )))
        .unwrap();
    let mut write_args = JsonObject::new();
    write_args.insert("path".to_owned(), json!("e.rs"));
    let a2_step1 = faux_assistant_message(
        vec![faux_tool_call("write", write_args, Some("tc_2".to_owned()))],
        Some(StopReason::ToolUse),
    );
    let _ = sm
        .append_message(AgentMessage::Assistant(a2_step1))
        .unwrap();
    let tr2 = ToolResultMessage {
        tool_call_id: "tc_2".to_owned(),
        tool_name: "write".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new("wrote e.rs"))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 400,
        duration_ms: None,
    };
    let _ = sm.append_message(AgentMessage::ToolResult(tr2)).unwrap();
    let a2_step2_id = sm
        .append_message(AgentMessage::Assistant(faux_text_message(
            "kept step 2 assistant reply ".repeat(40),
        )))
        .unwrap();

    let branch = sm.get_branch(None);
    let prep = prepare_compaction(
        &branch,
        CompactionSettings {
            enabled: true,
            reserve_tokens: 1000,
            keep_recent_tokens: 200,
        },
    )
    .expect("compaction preparation should succeed");

    assert!(prep.is_split_turn);
    assert_eq!(prep.first_kept_entry_id, a2_step2_id);
    assert!(!prep.messages_to_summarize.is_empty());
    assert!(!prep.turn_prefix_messages.is_empty());

    // 1. 验证总结被 Length 截断时必须报错拒绝
    let mut truncated_resp = faux_text_message("incomplete half summary...");
    truncated_resp.stop_reason = StopReason::Length;
    reg.set_responses(vec![FauxResponseStep::Static(truncated_resp)]);

    let sum_opts = SummarizationCallOptions {
        stream_fn: Some(models_stream_fn(models.clone())),
        ..SummarizationCallOptions::default()
    };
    let err = compact(&prep, &reg.model, &sum_opts).await.unwrap_err();
    assert!(err.contains("generation hit the token cap and the summary is incomplete"));

    // 2. 正常生成分裂轮次双总结（历史总结 + 分裂轮次前缀总结）并拼接 <read-files> 与 <modified-files>
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_text_message("## Goal\nUpdated history summary")),
        FauxResponseStep::Static(faux_text_message(
            "## Original Request\nTurn 2 prefix summary",
        )),
    ]);
    let comp_res = compact(&prep, &reg.model, &sum_opts).await.unwrap();
    assert!(
        comp_res
            .summary
            .contains("## Goal\nUpdated history summary")
    );
    assert!(comp_res.summary.contains(
        "---\n\n**Turn Context (split turn):**\n\n## Original Request\nTurn 2 prefix summary"
    ));
    // a.rs 和 d.rs 仅被读取；b.rs、c.rs、e.rs 被修改（b.rs 已从 read-files 中去重剔除）
    assert!(
        comp_res
            .summary
            .contains("<read-files>\na.rs\nd.rs\n</read-files>")
    );
    assert!(
        comp_res
            .summary
            .contains("<modified-files>\nb.rs\nc.rs\ne.rs\n</modified-files>")
    );
}

#[tokio::test]
async fn auto_compaction_three_cases_paired_omission_and_transient_retry_work() {
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    // 构造支持工具调用的会话以验证：
    // 1) 瞬态 503 错误自动重试 + 成对隐去失败尝试
    // 2) 报错上下文溢出触发单次自动压缩（将失败的 AssistantMessage 及配对的 ToolResultMessage 一并 ContextEdit(None) 隐去）并自动续跑
    // 3) 若单次溢出恢复后再次溢出，停止重试并发出明确失败事件
    let mut tc_args = JsonObject::new();
    tc_args.insert("cmd".to_owned(), json!("check"));
    let asst_tool = faux_assistant_message(
        vec![faux_tool_call(
            "probe",
            tc_args,
            Some("tc_probe".to_owned()),
        )],
        Some(StopReason::ToolUse),
    );
    let mut transient_503 = faux_text_message("");
    transient_503.stop_reason = StopReason::Error;
    transient_503.error_message = Some("HTTP 503 Service Temporarily Unavailable".to_owned());

    let mut overflow_err = faux_text_message("");
    overflow_err.stop_reason = StopReason::Error;
    overflow_err.error_message =
        Some("prompt is too long: 210000 tokens > 200000 maximum".to_owned());

    reg.set_responses(vec![
        // 第一轮：模型发起 probe 工具调用
        FauxResponseStep::Static(asst_tool),
        // 工具结果返回后：模型先遇到瞬态 503 错误（触发 _prepareRetry）
        FauxResponseStep::Static(transient_503),
        // 自动重试后：模型遇到上下文溢出错误（触发 _omitRecoveryAttempt + _runAutoCompaction(Overflow, will_retry=true)）
        FauxResponseStep::Static(overflow_err),
        // 自动压缩（分裂轮次：历史总结 + 轮次前缀总结）
        FauxResponseStep::Static(faux_text_message(
            "## Goal\nCompacted history after overflow",
        )),
        FauxResponseStep::Static(faux_text_message(
            "## Original Request\nCompacted split turn prefix",
        )),
        // 压缩完成后自动续跑（agent.continue_run）：成功返回最终答案！
        FauxResponseStep::Static(faux_text_message("Final answer after overflow recovery")),
    ]);

    let probe_tool = ToolDefinition::new(
        "probe",
        "Probe",
        "Probe tool",
        json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
        Arc::new(|_id, _args, _cancel, _upd, _ctx| {
            Box::pin(async { Ok(AgentToolResult::text("probe output ".repeat(50))) })
        }),
    );

    let sm = SessionManager::in_memory("/proj", None, None).unwrap();
    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm);
    opts.base_tools = vec![probe_tool];
    opts.compaction_settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 1000,
        keep_recent_tokens: 20,
    };
    opts.retry_policy = RetryPolicy {
        enabled: true,
        max_retries: 2,
        base_delay_ms: 5,
        max_agent_delay_ms: Some(20),
    };

    let session = AgentSession::new(opts).unwrap();
    let events = Arc::new(Mutex::new(Vec::<AgentSessionEvent>::new()));
    let ev_clone = events.clone();
    let _unsub = session.subscribe(Arc::new(move |ev| {
        ev_clone.lock().unwrap().push(ev);
    }));

    // 先写入一轮历史，保证压缩切点有足够前置消息可总结
    reg.append_responses(vec![]);
    session
        .with_session_manager_mut(|sm| {
            sm.append_message(AgentMessage::User(UserMessage::new(
                "earlier user message ".repeat(30),
            )))?;
            sm.append_message(AgentMessage::Assistant(faux_text_message(
                "earlier assistant reply ".repeat(30),
            )))
        })
        .unwrap();
    session.refresh_finalized_context();

    session
        .prompt("trigger tool + retry + overflow recovery", None)
        .await
        .unwrap();

    let state = session.state();
    assert_eq!(
        state
            .messages
            .last()
            .unwrap()
            .as_assistant()
            .unwrap()
            .text(),
        "Final answer after overflow recovery"
    );

    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, AgentSessionEvent::AutoRetryStart { attempt: 1, .. }))
    );
    assert!(recorded.iter().any(|e| matches!(
        e,
        AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Overflow,
            aborted: false,
            will_retry: true,
            result: Some(_),
            ..
        }
    )));

    // 验证原始分支中保留了失败的错误消息与 ContextEdit(None) 记录，但模型投影中已将失败尝试隐去！
    let branch = session.with_session_manager(|sm| sm.get_branch(None));
    let edit_count = branch
        .iter()
        .filter(|e| matches!(e, SessionEntry::ContextEdit(ce) if ce.replacement.is_none()))
        .count();
    assert!(
        edit_count >= 2,
        "both transient 503 and overflow errors must be omitted via ContextEdit(None)"
    );

    let proj = session.with_session_manager(SessionManager::build_session_projection);
    assert!(
        !proj.messages.iter().any(|m| {
            m.as_assistant()
                .is_some_and(|a| a.stop_reason == StopReason::Error)
        }),
        "projected context must not contain omitted error assistant messages"
    );

    // 验证短会话自适应折半预算（prepare_compaction_with_adaptive_budget）
    let mut short_sm = SessionManager::in_memory("/proj", None, None).unwrap();
    short_sm
        .append_message(AgentMessage::User(UserMessage::new("short q1")))
        .unwrap();
    short_sm
        .append_message(AgentMessage::Assistant(faux_text_message("short a1")))
        .unwrap();
    short_sm
        .append_message(AgentMessage::User(UserMessage::new("short q2")))
        .unwrap();
    let short_branch = short_sm.get_branch(None);
    // 默认 keep_recent_tokens = 20_000 下 prepare_compaction 返回 None，而自适应预算可找到切点！
    assert!(prepare_compaction(&short_branch, CompactionSettings::default()).is_none());
    assert!(
        prepare_compaction_with_adaptive_budget(&short_branch, CompactionSettings::default())
            .is_some()
    );
}

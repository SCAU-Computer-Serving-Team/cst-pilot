use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::Mutex;
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_agent::{AgentMessage, AgentToolResult, ToolReplayPolicy};
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text_message, faux_tool_call,
    register_faux_provider,
};
use pi_ai::{ImageContent, JsonObject, Models, StopReason, UserMessage};
use pi_session::{
    AgentSession, AgentSessionOptions, ExecutionLogController, InlineExtension,
    InputDeliveryStatus, JsonFileExecutionStateStore, NESTED_MAX_CALLS, NestedToolCallOptions,
    SessionManager, ToolDefinition, ToolExposure, reconcile_on_startup,
};
#[cfg(feature = "test-support")]
use pi_session::{AgentSessionEvent, PromptDisposition, PromptOptions, RunExecutionState};
use serde_json::json;

fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from("E:/tmp/2026-11-04/argon-phase3-development/tests")
        .join(format!("{name}-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn client_message_id_deduplicates_and_pre_execution_log_failure_prevents_tool_execution_and_halts_session()
 {
    let dir = temp_dir("exec-pre-log");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    reg.set_responses(vec![FauxResponseStep::Static(faux_text_message(
        "first response only",
    ))]);

    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let state_store = JsonFileExecutionStateStore::new(dir.join("state.json"));
    let exec_fault = state_store.fault_injection();
    let exec_log = ExecutionLogController::new(Box::new(state_store))
        .unwrap()
        .with_fault_injection(exec_fault.clone());

    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm);
    opts.execution_log = Some(exec_log);
    let session = AgentSession::new(opts).unwrap();

    // 1. 幂等 client_message_id 提交：第二次使用相同 client_message_id 直接返回原 Accepted 状态，不触发第二次模型请求
    let disp1 = session
        .prompt(
            "idempotent hello",
            Some(PromptOptions {
                client_message_id: Some("msg-1001".to_owned()),
                ..PromptOptions::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(disp1, PromptDisposition::Started);
    assert_eq!(session.get_session_stats().user_messages, 1);

    let disp2 = session
        .prompt(
            "idempotent hello duplicate",
            Some(PromptOptions {
                client_message_id: Some("msg-1001".to_owned()),
                ..PromptOptions::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(disp2, PromptDisposition::Started);
    assert_eq!(
        session.get_session_stats().user_messages,
        1,
        "duplicate client_message_id must not trigger a second turn"
    );

    // 2. 工具默认 ToolReplayPolicy::Never；当执行前登记状态日志失败时，工具必须零次执行，且会话立即终止！
    let tool_exec_count = Arc::new(AtomicUsize::new(0));
    let tec = tool_exec_count.clone();
    let second_tool_count = Arc::new(AtomicUsize::new(0));
    let stc = second_tool_count.clone();

    let tool_a = ToolDefinition::new(
        "bitlocker_unlock",
        "BitLocker Unlock",
        "Unlocks volume",
        json!({"type": "object"}),
        Arc::new(move |_, _, _, _, _| {
            tec.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(AgentToolResult::text("unlocked")) })
        }),
    );
    // 验证默认重放策略为 Never
    assert_eq!(tool_a.replay_policy, ToolReplayPolicy::Never);

    let tool_b = ToolDefinition::new(
        "follow_tool",
        "Follow Tool",
        "Second tool in batch",
        json!({"type": "object"}),
        Arc::new(move |_, _, _, _, _| {
            stc.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(AgentToolResult::text("ran b")) })
        }),
    );

    let sm2 = SessionManager::create("/proj", &dir, None).unwrap();
    let state_store2 = JsonFileExecutionStateStore::new(dir.join("state2.json"));
    let exec_fault2 = state_store2.fault_injection();
    let exec_log2 = ExecutionLogController::new(Box::new(state_store2))
        .unwrap()
        .with_fault_injection(exec_fault2.clone());

    let mut opts2 = AgentSessionOptions::new("/proj", reg.model.clone(), models, sm2);
    opts2.execution_log = Some(exec_log2);
    opts2.base_tools = vec![tool_a, tool_b];
    let session2 = AgentSession::new(opts2).unwrap();

    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![
                faux_tool_call(
                    "bitlocker_unlock",
                    JsonObject::new(),
                    Some("tc_bl".to_owned()),
                ),
                faux_tool_call("follow_tool", JsonObject::new(), Some("tc_fl".to_owned())),
            ],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("should never reach second model call")),
    ]);

    // 注入执行前工具登记写入失败
    exec_fault2.lock().unwrap().fail_next_tool_start_write = true;
    let res = session2.prompt("unlock volume", None).await;
    assert!(
        res.is_err(),
        "pre-execution state log failure must fail the run"
    );
    assert_eq!(
        tool_exec_count.load(Ordering::SeqCst),
        0,
        "tool_a must NOT execute when pre-execution state logging fails"
    );
    assert_eq!(
        second_tool_count.load(Ordering::SeqCst),
        0,
        "subsequent tool_b must NOT execute after session-level fatal error"
    );
    assert!(session2.fatal_runtime_error().is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn in_flight_tool_retained_at_tool_execution_end_and_idempotently_cleaned_or_flagged_on_startup()
 {
    let dir = temp_dir("exec-in-flight-lifecycle");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let state_path = dir.join("exec_state.json");
    let state_store = JsonFileExecutionStateStore::new(&state_path);
    let exec_fault = state_store.fault_injection();
    let exec_log = ExecutionLogController::new(Box::new(state_store))
        .unwrap()
        .with_fault_injection(exec_fault.clone());

    let tool = ToolDefinition::new(
        "sys_modify",
        "Sys Modify",
        "Modifies system state",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("modified")) })),
    );

    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm);
    opts.execution_log = Some(exec_log);
    opts.base_tools = vec![tool];
    let session = AgentSession::new(opts).unwrap();

    // 1. 验证在收到 ToolExecutionEnd 事件时（此时 ToolResultMessage 尚未持久化），未决记录仍保留在状态日志中！
    let in_flight_at_tool_end = Arc::new(Mutex::new(Vec::<String>::new()));
    let ife = in_flight_at_tool_end.clone();
    let s_for_listener = session.clone();
    let _unsub = session.subscribe(Arc::new(move |ev| {
        if matches!(ev, AgentSessionEvent::ToolExecutionEnd { .. }) {
            s_for_listener.with_execution_log(|log| {
                if let Some(RunExecutionState::Running {
                    in_flight_tools, ..
                }) = &log.state().active_run
                {
                    *ife.lock().unwrap() = in_flight_tools
                        .iter()
                        .map(|t| t.tool_call_id.clone())
                        .collect();
                }
            });
        }
    }));

    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "sys_modify",
                JsonObject::new(),
                Some("tc_mod_1".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("done 1")),
    ]);

    session.prompt("run modify 1", None).await.unwrap();
    assert_eq!(
        *in_flight_at_tool_end.lock().unwrap(),
        vec!["tc_mod_1".to_owned()],
        "InFlightToolRecord must still be retained when ToolExecutionEnd fires (before ToolResult persistence)"
    );

    // 2. 模拟 ToolResultMessage 已成功写入会话 JSONL，但随后清理状态日志失败（fail_next_tool_clear_write）：
    // 当前运行停止并保留未决记录；重启调用 reconcile_on_startup 时发现 ToolResult 已在 JSONL 中，自动幂等清理！
    let dir2 = temp_dir("exec-post-persist-clear-fail");
    let sm2 = SessionManager::create("/proj", &dir2, None).unwrap();
    let session_file2 = sm2.get_session_file().unwrap().to_path_buf();
    let state_path2 = dir2.join("state2.json");
    let state_store2 = JsonFileExecutionStateStore::new(&state_path2);
    let exec_fault2 = state_store2.fault_injection();
    let exec_log2 = ExecutionLogController::new(Box::new(state_store2))
        .unwrap()
        .with_fault_injection(exec_fault2.clone());

    let tool2 = ToolDefinition::new(
        "sys_modify",
        "Sys Modify",
        "Modifies system state",
        json!({"type": "object"}),
        Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("modified")) })),
    );
    let mut opts2 = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm2);
    opts2.execution_log = Some(exec_log2);
    opts2.base_tools = vec![tool2];
    let session2 = AgentSession::new(opts2).unwrap();

    reg.set_responses(vec![FauxResponseStep::Static(faux_assistant_message(
        vec![faux_tool_call(
            "sys_modify",
            JsonObject::new(),
            Some("tc_already_persisted".to_owned()),
        )],
        Some(StopReason::ToolUse),
    ))]);
    exec_fault2.lock().unwrap().fail_next_tool_clear_write = true;

    let clear_fail_res = session2.prompt("run modify 2", None).await;
    assert!(clear_fail_res.is_err());
    drop(session2);

    // 模拟重启：重新打开 session_file2 与 state_path2 并执行 reconcile_on_startup
    let mut reopened_sm2 = SessionManager::open(&session_file2, Some(dir2.clone()), None).unwrap();
    let reopened_store2 = JsonFileExecutionStateStore::new(&state_path2);
    let mut reopened_log2 = ExecutionLogController::new(Box::new(reopened_store2)).unwrap();
    let mut policies = BTreeMap::new();
    policies.insert("sys_modify".to_owned(), ToolReplayPolicy::Never);

    let report2 = reconcile_on_startup(&mut reopened_sm2, &mut reopened_log2, &policies).unwrap();
    assert_eq!(
        report2.idempotently_cleaned_tool_ids,
        vec!["tc_already_persisted".to_owned()],
        "already persisted ToolResult must be idempotently cleaned on startup"
    );
    assert!(
        report2.unresolved_tool_calls.is_empty(),
        "idempotently cleaned tool call must not be reported as unresolved"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[tokio::test]
async fn crash_during_delivering_and_truncated_nested_calls_require_verification_and_forbid_replay()
{
    let dir = temp_dir("exec-crash-and-truncated-nested");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    // 1. 测试嵌套调用超过 256 次导致 nested_calls 被截断时：
    // 第 257 次子调用的未决记录在 clear_persisted_tool_result 后仍被保留，重启核对要求验证系统状态且禁止自动重放！
    let ext = InlineExtension::new("<test:truncated-nested>", |api| {
        let mut child_tool = ToolDefinition::new(
            "sub_op",
            "Sub Op",
            "Child operation",
            json!({"type": "object"}),
            Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("ok")) })),
        );
        child_tool.exposure = ToolExposure::Codemode;
        api.register_tool(child_tool);

        let parent_tool = ToolDefinition::new(
            "batch_runner",
            "Batch Runner",
            "Runs 257 nested calls",
            json!({"type": "object"}),
            Arc::new(|_, _, _, _, ctx| {
                Box::pin(async move {
                    for _ in 0..=NESTED_MAX_CALLS {
                        let _ = ctx
                            .execute_tool("sub_op", json!({}), NestedToolCallOptions::default())
                            .await;
                    }
                    Ok(AgentToolResult::text("ran 257 nested calls"))
                })
            }),
        );
        api.register_tool(parent_tool);
        Ok(())
    });

    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let session_file = sm.get_session_file().unwrap().to_path_buf();
    let state_path = dir.join("state_nested.json");
    let state_store = JsonFileExecutionStateStore::new(&state_path);
    let exec_log = ExecutionLogController::new(Box::new(state_store)).unwrap();

    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models, sm);
    opts.execution_log = Some(exec_log);
    opts.extensions = vec![Box::new(ext)];
    let session = AgentSession::new(opts).unwrap();

    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "batch_runner",
                JsonObject::new(),
                Some("tc_batch".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("batch finished")),
    ]);

    session.prompt("run 257 nested calls", None).await.unwrap();
    drop(session);

    // 重新打开会话与状态日志，并额外模拟：
    // - 一条未落入 JSONL 的 Delivering 输入 ("client-lost-1")
    // - 一条已落入 JSONL 的 Delivering 输入 ("client-saved-2")
    let mut reopened_sm = SessionManager::open(&session_file, Some(dir.clone()), None).unwrap();
    let uid_saved = reopened_sm
        .append_message(AgentMessage::User(UserMessage::new("saved prompt")))
        .unwrap();
    reopened_sm
        .append_custom_entry(
            "cst-web-input-message",
            Some(json!({
                "clientMessageId": "client-saved-2",
                "userEntryId": uid_saved,
            })),
        )
        .unwrap();

    let reopened_store = JsonFileExecutionStateStore::new(&state_path);
    let mut reopened_log = ExecutionLogController::new(Box::new(reopened_store)).unwrap();
    reopened_log
        .begin_input_delivery(
            "client-lost-1",
            Some("q-1"),
            "unsent prompt text",
            vec![ImageContent {
                data: "BBBB".to_owned(),
                mime_type: "image/png".to_owned(),
            }],
        )
        .unwrap();
    reopened_log
        .begin_input_delivery("client-saved-2", Some("q-2"), "saved prompt", Vec::new())
        .unwrap();

    let mut policies = BTreeMap::new();
    policies.insert("batch_runner".to_owned(), ToolReplayPolicy::Never);
    policies.insert("sub_op".to_owned(), ToolReplayPolicy::Never);

    let report = reconcile_on_startup(&mut reopened_sm, &mut reopened_log, &policies).unwrap();

    // 验证已落入 JSONL 的 client-saved-2 自动恢复为 Accepted
    assert_eq!(
        report.recovered_accepted_inputs,
        vec!["client-saved-2".to_owned()]
    );
    assert!(matches!(
        reopened_log.get_input("client-saved-2").unwrap().status,
        InputDeliveryStatus::Accepted { .. }
    ));

    // 验证未落入 JSONL 的 client-lost-1 转为 Uncertain，保留原始文本与图片，且不自动重发
    assert_eq!(report.uncertain_inputs.len(), 1);
    let unc = &report.uncertain_inputs[0];
    assert_eq!(unc.client_message_id, "client-lost-1");
    assert!(unc.uncertain);
    assert_eq!(unc.text, "unsent prompt text");
    assert_eq!(unc.images.len(), 1);
    assert!(matches!(unc.status, InputDeliveryStatus::Uncertain { .. }));

    // 验证超出 256 次上限被截断的第 257 次子调用 ("tc_batch/257") 与父工具截断记录均被报告为需要状态核对且禁止自动重放！
    assert!(
        report
            .unresolved_tool_calls
            .iter()
            .any(|u| u.tool_call_id == "tc_batch/257"
                && u.requires_state_verification
                && !u.can_auto_replay),
        "dropped 257th nested call must remain in unresolved_tool_calls with requires_state_verification=true"
    );
    assert!(
        report
            .unresolved_tool_calls
            .iter()
            .any(|u| u.tool_call_id == "tc_batch/truncated"
                && u.requires_state_verification
                && !u.can_auto_replay)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

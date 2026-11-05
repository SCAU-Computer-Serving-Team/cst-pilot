use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_agent::{AgentMessage, AgentToolResult, ToolReplayPolicy};
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text_message, faux_tool_call,
    register_faux_provider,
};
use pi_ai::{JsonObject, ModelThinkingLevel, Models, StopReason, UserMessage};
use pi_session::{
    AgentSession, AgentSessionOptions, ExecutionStateStore, InputDeliveryStatus,
    JsonFileExecutionStateStore, JsonlSessionStore, PromptOptions, SessionEntry, SessionManager,
    SessionStore, SessionStoreError, ToolDefinition,
};
use serde_json::json;

fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from("E:/tmp/2026-11-04/argon-phase3-development/review-tests")
        .join(format!("{name}-{}", pi_ai::uuid_v7()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn explicit_thinking_level_wins_over_persisted_session_history() {
    let models = Arc::new(Models::default());
    let registered = register_faux_provider(&models);
    let mut manager = SessionManager::in_memory("/thinking-override", None, None).unwrap();
    manager.append_thinking_level_change("high").unwrap();

    let mut options =
        AgentSessionOptions::new("/thinking-override", registered.model, models, manager);
    options.thinking_level = ModelThinkingLevel::Off;
    let session = AgentSession::new(options).unwrap();
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Off);
}

#[test]
fn dropping_last_session_handle_revokes_extension_runtime() {
    let models = Arc::new(Models::default());
    let registered = register_faux_provider(&models);
    let manager = SessionManager::in_memory("/drop-cleanup", None, None).unwrap();
    let session = AgentSession::new(AgentSessionOptions::new(
        "/drop-cleanup",
        registered.model,
        models,
        manager,
    ))
    .unwrap();
    let cancellation = session.extension_runner().runtime().cancellation_token();

    drop(session);

    assert!(cancellation.is_cancelled());
}

#[test]
fn setting_session_name_without_tokio_runtime_does_not_panic() {
    let models = Arc::new(Models::default());
    let registered = register_faux_provider(&models);
    let manager = SessionManager::in_memory("/session-name", None, None).unwrap();
    let session = AgentSession::new(AgentSessionOptions::new(
        "/session-name",
        registered.model,
        models,
        manager,
    ))
    .unwrap();

    session.set_session_name("safe-name").unwrap();
    assert_eq!(session.session_name().as_deref(), Some("safe-name"));
}

#[test]
fn subprocess_lock_probe() {
    let Some(path) = std::env::var_os("PI_SESSION_REVIEW_LOCK_PATH") else {
        return;
    };
    assert!(matches!(
        JsonlSessionStore::open(PathBuf::from(path)),
        Err(SessionStoreError::SingleWriterLocked(_))
    ));
}

#[test]
fn writer_lock_survives_snapshot_replacement_and_excludes_other_processes() {
    let dir = temp_dir("process-lock");
    let mut sm = SessionManager::create("/proj", &dir, None).unwrap();
    sm.append_message(AgentMessage::User(UserMessage::new("hello")))
        .unwrap();
    let path = sm.get_session_file().unwrap().to_path_buf();
    sm.append_correlated_user_message(AgentMessage::User(UserMessage::new("next")), "c1")
        .unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_lock_probe", "--nocapture"])
        .env("PI_SESSION_REVIEW_LOCK_PATH", &path)
        .status()
        .unwrap();
    assert!(result.success());
    drop(sm);
    assert!(SessionManager::open(path, None, None).is_ok());
}

#[test]
fn invalid_utf8_tail_is_repaired_using_original_byte_offsets() {
    let dir = temp_dir("utf8-tail");
    let mut sm = SessionManager::create("/proj", &dir, None).unwrap();
    sm.append_message(AgentMessage::User(UserMessage::new("中文")))
        .unwrap();
    let path = sm.get_session_file().unwrap().to_path_buf();
    drop(sm);
    let clean = fs::read(&path).unwrap();
    let mut broken = clean.clone();
    broken.extend_from_slice(b"{\"type\":\"message\",\"content\":\"");
    broken.extend_from_slice(&[0xe4, 0xb8]);
    fs::write(&path, broken).unwrap();
    let reopened = SessionManager::open(&path, None, None).unwrap();
    assert_eq!(reopened.get_entry_count(), 1);
    assert_eq!(fs::read(&path).unwrap(), clean);
    drop(reopened);
    let mut interior = clean.clone();
    interior.extend_from_slice(b"\xff\n");
    interior.extend_from_slice(&clean[clean.iter().position(|b| *b == b'\n').unwrap() + 1..]);
    fs::write(&path, interior).unwrap();
    let mut store = JsonlSessionStore::open(path).unwrap();
    assert_eq!(store.load_entries().unwrap().len(), 3);
    assert_eq!(store.skipped_intermediate_corrupt_lines(), 1);
}

#[tokio::test]
async fn default_log_and_input_acceptance_are_durable_before_tool_execution() {
    let dir = temp_dir("durable-input");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let path = sm.get_session_file().unwrap().to_path_buf();
    let holder: Arc<Mutex<Option<AgentSession>>> = Arc::new(Mutex::new(None));
    let h = holder.clone();
    let tool = ToolDefinition::new(
        "modify",
        "Modify",
        "Modify",
        json!({"type":"object"}),
        Arc::new(move |_, _, _, _, _| {
            let session = h.lock().unwrap().clone().unwrap();
            session.with_execution_log(|log| {
                assert!(matches!(
                    log.get_input("c1").unwrap().status,
                    InputDeliveryStatus::Accepted {
                        user_entry_id: Some(_),
                        ..
                    }
                ))
            });
            session.with_session_manager(|sm| assert!(sm.get_entries().iter().any(|e| matches!(e, SessionEntry::Custom(c) if c.custom_type == "pi.input-correlation"))));
            Box::pin(async { Ok(AgentToolResult::text("done")) })
        }),
    );
    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm);
    opts.base_tools = vec![tool];
    let session = AgentSession::new(opts).unwrap();
    *holder.lock().unwrap() = Some(session.clone());
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "modify",
                JsonObject::new(),
                Some("t1".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("done")),
    ]);
    session
        .prompt(
            "hello",
            Some(PromptOptions {
                client_message_id: Some("c1".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let state = JsonFileExecutionStateStore::for_session_file(&path)
        .load()
        .unwrap();
    assert!(matches!(
        state.inputs["c1"].status,
        InputDeliveryStatus::Accepted { .. }
    ));
    *holder.lock().unwrap() = None;
    drop(session);
    let sm = SessionManager::open(&path, None, None).unwrap();
    let session = AgentSession::new(AgentSessionOptions::new(
        "/proj",
        reg.model.clone(),
        models,
        sm,
    ))
    .unwrap();
    session
        .prompt(
            "duplicate",
            Some(PromptOptions {
                client_message_id: Some("c1".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(session.get_session_stats().user_messages, 1);
    session.new_session(None).await.unwrap();
    session.with_execution_log(|log| assert!(log.get_input("c1").is_none()));
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn acceptance_save_failure_stops_before_model_and_recovers_from_correlation() {
    let dir = temp_dir("acceptance-failure");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let path = sm.get_session_file().unwrap().to_path_buf();
    let holder: Arc<Mutex<Option<AgentSession>>> = Arc::new(Mutex::new(None));
    let h = holder.clone();
    // EntryAppended for the system message occurs after start_run. Fail the
    // next save, which must be input acceptance before the first model call.
    let session = AgentSession::new(AgentSessionOptions::new(
        "/proj",
        reg.model.clone(),
        models.clone(),
        sm,
    ))
    .unwrap();
    *holder.lock().unwrap() = Some(session.clone());
    let unsub = session.subscribe(Arc::new(move |event| {
        if matches!(event, pi_session::AgentSessionEvent::EntryAppended { entry: SessionEntry::Message(ref m) } if m.message.role() == "system") {
            h.lock().unwrap().as_ref().unwrap().with_execution_log(|log| log.fault_injection().lock().unwrap().fail_next_save = true);
        }
    }));
    let result = session
        .prompt(
            "hello",
            Some(PromptOptions {
                client_message_id: Some("c1".into()),
                ..Default::default()
            }),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(session.get_session_stats().assistant_messages, 0);
    unsub();
    *holder.lock().unwrap() = None;
    drop(session);
    let sm = SessionManager::open(path, None, None).unwrap();
    let reopened =
        AgentSession::new(AgentSessionOptions::new("/proj", reg.model, models, sm)).unwrap();
    assert_eq!(
        reopened
            .startup_reconciliation_report()
            .recovered_accepted_inputs,
        vec!["c1"]
    );
    reopened.with_execution_log(|log| {
        assert!(matches!(
            log.get_input("c1").unwrap().status,
            InputDeliveryStatus::Accepted { .. }
        ))
    });
}

#[tokio::test]
async fn queued_input_is_accepted_only_when_its_own_message_commits() {
    let dir = temp_dir("queued-correlation");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let holder: Arc<Mutex<Option<AgentSession>>> = Arc::new(Mutex::new(None));
    let h = holder.clone();
    let tool = ToolDefinition::new(
        "queue",
        "Queue",
        "Queue input",
        json!({"type":"object"}),
        Arc::new(move |_, _, _, _, _| {
            let s = h.lock().unwrap().clone().unwrap();
            Box::pin(async move {
                s.prompt(
                    "second",
                    Some(PromptOptions {
                        client_message_id: Some("c2".into()),
                        streaming_behavior: Some(pi_session::StreamingInputBehavior::FollowUp),
                        ..Default::default()
                    }),
                )
                .await
                .unwrap();
                s.with_execution_log(|log| {
                    assert!(matches!(
                        log.get_input("c1").unwrap().status,
                        InputDeliveryStatus::Accepted { .. }
                    ));
                    assert!(matches!(
                        log.get_input("c2").unwrap().status,
                        InputDeliveryStatus::Delivering
                    ));
                });
                Ok(AgentToolResult::text("queued"))
            })
        }),
    );
    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models, sm);
    opts.base_tools = vec![tool];
    let s = AgentSession::new(opts).unwrap();
    *holder.lock().unwrap() = Some(s.clone());
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "queue",
                JsonObject::new(),
                Some("q1".into()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("first done")),
        FauxResponseStep::Static(faux_text_message("second done")),
    ]);
    s.prompt(
        "first",
        Some(PromptOptions {
            client_message_id: Some("c1".into()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    for (client, expected) in [("c1", "first"), ("c2", "second")] {
        let id = s.with_execution_log(|log| match &log.get_input(client).unwrap().status {
            InputDeliveryStatus::Accepted {
                user_entry_id: Some(id),
                ..
            } => id.clone(),
            status => panic!("Unexpected state: {status:?}"),
        });
        s.with_session_manager(|sm| {
            let SessionEntry::Message(m) = sm.get_entry(&id).unwrap() else {
                panic!("not message")
            };
            assert!(
                serde_json::to_string(&m.message)
                    .unwrap()
                    .contains(expected)
            );
        });
    }
    *holder.lock().unwrap() = None;
}

#[cfg(feature = "test-support")]
#[test]
fn correlated_input_snapshot_failure_never_publishes_a_half_delivered_input() {
    let dir = temp_dir("atomic-input");
    let mut sm = SessionManager::create("/proj", &dir, None).unwrap();
    sm.append_message(AgentMessage::User(UserMessage::new("existing")))
        .unwrap();
    let path = sm.get_session_file().unwrap().to_path_buf();
    let original = fs::read(&path).unwrap();
    sm.fault_injection()
        .unwrap()
        .lock()
        .unwrap()
        .fail_rewrite_before_rename = true;
    assert!(
        sm.append_correlated_user_message(AgentMessage::User(UserMessage::new("new")), "c1")
            .is_err()
    );
    assert_eq!(sm.get_entry_count(), 1);
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn deep_nested_records_reconcile_from_complete_top_level_result() {
    use pi_session::{ExecutionLogController, InFlightToolRecord, reconcile_on_startup};
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();
    let mut log = ExecutionLogController::in_memory();
    log.start_run(1).unwrap();
    log.record_tool_start(InFlightToolRecord {
        tool_call_id: "top/1/1".into(),
        parent_tool_call_id: Some("top/1".into()),
        tool_name: "child".into(),
        replay_policy: ToolReplayPolicy::Never,
        started_at_ms: 1,
    })
    .unwrap();
    let result = pi_ai::ToolResultMessage {
        tool_call_id: "top".into(),
        tool_name: "parent".into(),
        content: vec![],
        details: None,
        usage: None,
        nested_calls: Some(pi_ai::NestedToolCalls {
            complete: true,
            calls: vec![pi_ai::NestedToolCallRecord {
                id: "top/1/1".into(),
                name: "child".into(),
                arguments: None,
                arguments_bytes: None,
                status: "ok".into(),
                duration_ms: Some(1),
                error: None,
            }],
        }),
        is_error: false,
        timestamp: 1,
        duration_ms: None,
    };
    sm.append_message(AgentMessage::ToolResult(result)).unwrap();
    let report = reconcile_on_startup(&mut sm, &mut log, &Default::default()).unwrap();
    assert_eq!(report.idempotently_cleaned_tool_ids, vec!["top/1/1"]);
    assert!(report.unresolved_tool_calls.is_empty());
}

#[test]
fn empty_execution_state_is_rejected_and_unknown_tool_policy_requires_verification() {
    use pi_session::{ExecutionLogController, InFlightToolRecord, reconcile_on_startup};
    let dir = temp_dir("corrupt-state");
    let path = dir.join("state.json");
    fs::write(&path, "").unwrap();
    assert!(JsonFileExecutionStateStore::new(path).load().is_err());
    let mut log = ExecutionLogController::in_memory();
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();
    log.start_run(1).unwrap();
    log.record_tool_start(InFlightToolRecord {
        tool_call_id: "unknown".into(),
        parent_tool_call_id: None,
        tool_name: "removed-tool".into(),
        replay_policy: ToolReplayPolicy::Safe,
        started_at_ms: 1,
    })
    .unwrap();
    let report = reconcile_on_startup(&mut sm, &mut log, &Default::default()).unwrap();
    assert_eq!(
        report.unresolved_tool_calls[0].replay_policy,
        ToolReplayPolicy::Never
    );
    assert!(report.unresolved_tool_calls[0].requires_state_verification);
}

#[test]
fn unknown_replay_policy_defaults_to_never() {
    assert_eq!(
        serde_json::from_str::<ToolReplayPolicy>("\"future-policy\"").unwrap(),
        ToolReplayPolicy::Never
    );
}

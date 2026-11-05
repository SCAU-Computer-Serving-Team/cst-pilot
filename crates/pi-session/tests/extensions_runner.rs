use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent::{AgentMessage, AgentToolResult};
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text_message, faux_tool_call,
    register_faux_provider,
};
use pi_ai::{ContentBlock, JsonObject, Models, StopReason};
use pi_session::{
    AgentSession, AgentSessionOptions, BoundaryResult, ContextEventResult, ExtensionContext,
    ExtensionError, ExtensionFlagType, ExtensionFlagValue, ExtensionRunner, InlineExtension,
    InputEventResult, MessageEndEventResult, PromptDisposition, SessionBeforeEventOutput,
    SessionBoundaryDraft, SessionEntry, SessionManager, ToolDefinition, ToolLoadoutChanges,
};
use serde_json::json;

fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from("E:/tmp/2026-11-04/argon-phase3-development/tests")
        .join(format!("{name}-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn replaceable_extensions_duplicate_commands_reserved_shortcuts_and_order_preserving_tools_work() {
    // 1. replaceable: true 的内置扩展遇到同名工具冲突时整包退让
    let replaceable_ext = InlineExtension::new("builtin:mcp", |api| {
        api.register_tool(ToolDefinition::new(
            "mcp_connect",
            "Builtin MCP",
            "Builtin MCP description",
            json!({"type": "object"}),
            Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("builtin")) })),
        ));
        api.register_flag(
            "mcp-only-flag",
            ExtensionFlagType::Boolean,
            Some(ExtensionFlagValue::Bool(true)),
            None,
        );
        Ok(())
    })
    .with_replaceable(true);

    let override_ext = InlineExtension::new("<ext:custom-mcp>", |api| {
        api.register_tool(ToolDefinition::new(
            "mcp_connect",
            "Custom MCP",
            "Custom MCP description",
            json!({"type": "object"}),
            Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("custom")) })),
        ));
        api.register_simple_command("deploy", "Deploy 1", |_, _| async { Ok(()) });
        api.register_shortcut(
            "ctrl+c",
            Some("Conflict with interrupt".to_owned()),
            |_| async {},
        );
        api.register_shortcut(
            "ctrl+k",
            Some("Allowed custom shortcut".to_owned()),
            |_| async {},
        );
        Ok(())
    });

    let second_cmd_ext = InlineExtension::new("<ext:second-cmd>", |api| {
        api.register_simple_command("deploy", "Deploy 2", |_, _| async { Ok(()) });
        Ok(())
    });

    let runner = ExtensionRunner::from_extensions(
        "/proj",
        vec![
            Box::new(replaceable_ext),
            Box::new(override_ext),
            Box::new(second_cmd_ext),
        ],
    )
    .unwrap();

    // replaceable 扩展已整包退让（其独有的 mcp-only-flag 也不存在），mcp_connect 来自 custom-mcp
    assert_eq!(
        runner.get_extension_paths(),
        vec!["<ext:custom-mcp>", "<ext:second-cmd>"]
    );
    assert!(!runner.get_flags().contains_key("mcp-only-flag"));
    assert_eq!(
        runner.get_tool_definition("mcp_connect").unwrap().label,
        "Custom MCP"
    );

    // 同名命令自动分配 deploy:1 与 deploy:2
    let cmds = runner.get_registered_commands();
    let inv_names: Vec<&str> = cmds.iter().map(|c| c.invocation_name.as_str()).collect();
    assert_eq!(inv_names, vec!["deploy:1", "deploy:2"]);

    // 保留快捷键 app.interrupt (ctrl+c) 被跳过并记录诊断，非保留快捷键 ctrl+k 正常生效
    let mut builtin_keys = BTreeMap::new();
    builtin_keys.insert("app.interrupt".to_owned(), vec!["Ctrl+C".to_owned()]);
    let shortcuts = runner.get_shortcuts(&builtin_keys);
    assert!(!shortcuts.contains_key("ctrl+c"));
    assert!(shortcuts.contains_key("ctrl+k"));
    assert_eq!(runner.get_shortcut_diagnostics().len(), 1);
}

#[tokio::test]
async fn message_end_replacement_propagates_to_tool_execution_state_and_disk_and_persistence_failure_stops_run()
 {
    let dir = temp_dir("ext-finalize-and-boundary");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    let mut orig_args = JsonObject::new();
    orig_args.insert("target".to_owned(), json!("unrewritten"));
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "inspect",
                orig_args,
                Some("tc_inspect".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("final reply")),
    ]);

    let executed_targets = Arc::new(Mutex::new(Vec::<String>::new()));
    let exec_targets_clone = executed_targets.clone();
    let ext_errors = Arc::new(Mutex::new(Vec::<ExtensionError>::new()));
    let ext_errors_clone = ext_errors.clone();

    let ext = InlineExtension::new("<ext:message-end-and-boundary>", move |api| {
        let et = exec_targets_clone.clone();
        let mut inspect_tool = ToolDefinition::new(
            "inspect",
            "Inspect",
            "Inspect target",
            json!({
                "type": "object",
                "properties": { "target": { "type": "string" } },
                "required": ["target"]
            }),
            Arc::new(move |_id, args, _cancel, _upd, _ctx| {
                let target = args["target"].as_str().unwrap_or_default().to_owned();
                et.lock().unwrap().push(target.clone());
                Box::pin(async move { Ok(AgentToolResult::text(format!("inspected:{target}"))) })
            }),
        );
        inspect_tool.prepare_loadout = Some(Arc::new(|_loadout| {
            Some(ToolLoadoutChanges {
                descriptions: BTreeMap::new(),
                hidden_declarations: vec!["secret_internal".to_owned()],
            })
        }));
        api.register_tool(inspect_tool);

        api.register_tool(ToolDefinition::new(
            "secret_internal",
            "Secret Internal",
            "Hidden from model declarations",
            json!({"type": "object"}),
            Arc::new(|_, _, _, _, _| Box::pin(async { Ok(AgentToolResult::text("secret")) })),
        ));

        // 1. message_end 处理器 1：故意返回错误角色，验证报告 ExtensionError 且不影响原消息
        api.on_message_end(|ev, _ctx| async move {
            if ev.message.role() == "user" {
                return Ok(Some(MessageEndEventResult {
                    message: Some(AgentMessage::Assistant(faux_text_message("wrong role"))),
                }));
            }
            Ok(None)
        });

        // 2. message_end 处理器 2：改写助手消息中的 toolCall 参数（unrewritten -> rewritten-by-message-end）
        api.on_message_end(|mut ev, _ctx| async move {
            if let AgentMessage::Assistant(ref mut a) = ev.message {
                for block in &mut a.content {
                    if let ContentBlock::ToolCall(tc) = block {
                        tc.arguments
                            .insert("target".to_owned(), json!("rewritten-by-message-end"));
                    }
                }
                return Ok(Some(MessageEndEventResult {
                    message: Some(ev.message),
                }));
            }
            Ok(None)
        });

        // 3. context 钩子：裁剪消息后验证 SystemMessage 自动恢复到下标 0
        api.on_context(Arc::new(|ev, _ctx| {
            Box::pin(async move {
                let kept = ev.messages.clone();
                Ok(Some(ContextEventResult {
                    messages: Some(kept),
                }))
            })
        }));

        Ok(())
    });

    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let session_file = sm.get_session_file().unwrap().to_path_buf();
    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm);
    opts.extensions = vec![Box::new(ext)];

    let session = AgentSession::new(opts).unwrap();
    session.extension_runner().on_error(Arc::new(move |err| {
        ext_errors_clone.lock().unwrap().push(err.clone());
    }));

    // 验证保序激活工具集（#11495）
    session
        .set_active_tools_by_name(vec!["inspect".to_owned(), "secret_internal".to_owned()])
        .unwrap();
    session
        .set_active_tools_by_name(vec!["secret_internal".to_owned(), "inspect".to_owned()])
        .unwrap();
    assert_eq!(
        session.get_active_tool_names(),
        vec!["inspect".to_owned(), "secret_internal".to_owned()]
    );

    session.prompt("please inspect", None).await.unwrap();

    // 验证工具执行收到了经 message_end 改写后的参数 "rewritten-by-message-end"
    assert_eq!(
        *executed_targets.lock().unwrap(),
        vec!["rewritten-by-message-end".to_owned()]
    );
    // 验证错误角色的 message_end 返回已记录到 ExtensionError
    assert!(
        ext_errors
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.event == "message_end" && e.error.contains("same role"))
    );

    // 验证磁盘 JSONL 中持久化的助手消息参数同样是改写后的 "rewritten-by-message-end"
    let disk_content = std::fs::read_to_string(&session_file).unwrap();
    assert!(disk_content.contains("rewritten-by-message-end"));
    assert!(!disk_content.contains("unrewritten"));

    #[cfg(feature = "test-support")]
    {
        // 4. 验证当定稿落盘失败（注入 append 失败）时：立即终止运行，绝不继续执行工具或发起自动重试！
        let tool_runs_on_fail = Arc::new(AtomicUsize::new(0));
        let trf = tool_runs_on_fail.clone();
        let mut fail_args = JsonObject::new();
        fail_args.insert("target".to_owned(), json!("should-never-run"));
        reg.set_responses(vec![FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "never_tool",
                fail_args,
                Some("tc_never".to_owned()),
            )],
            Some(StopReason::ToolUse),
        ))]);

        let sm2 = SessionManager::create("/proj", &dir, None).unwrap();
        let fault2 = sm2.fault_injection().unwrap();
        let mut opts2 = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm2);
        opts2.base_tools = vec![ToolDefinition::new(
            "never_tool",
            "Never Tool",
            "Must not run if assistant persistence fails",
            json!({"type": "object", "properties": {"target": {"type": "string"}}}),
            Arc::new(move |_, _, _, _, _| {
                trf.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolResult::text("ran")) })
            }),
        )];
        let session2 = AgentSession::new(opts2).unwrap();
        // 先完成首轮落盘以创建文件
        reg.set_responses(vec![FauxResponseStep::Static(faux_text_message("warmup"))]);
        session2.prompt("warmup", None).await.unwrap();

        // 下一轮在持久化助手消息时注入部分写入失败
        reg.set_responses(vec![FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "never_tool",
                JsonObject::new(),
                Some("tc_never".to_owned()),
            )],
            Some(StopReason::ToolUse),
        ))]);
        fault2.lock().unwrap().fail_append_after_bytes = Some(8);
        let fail_res = session2.prompt("trigger persistence failure", None).await;
        assert!(fail_res.is_err());
        assert_eq!(
            tool_runs_on_fail.load(Ordering::SeqCst),
            0,
            "tool must never execute when assistant message persistence fails"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn boundary_invalid_draft_discard_stale_context_rejection_and_settled_reentrant_prompt_work()
{
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_text_message("reply 1")),
        FauxResponseStep::Static(faux_text_message("reply 2 from settled reentrant prompt")),
    ]);

    let captured_ctx: Arc<Mutex<Option<ExtensionContext>>> = Arc::new(Mutex::new(None));
    let cap_clone = captured_ctx.clone();
    let reentered = Arc::new(AtomicUsize::new(0));
    let reentered_clone = reentered.clone();
    let boundary_errors = Arc::new(Mutex::new(Vec::<ExtensionError>::new()));
    let be_clone = boundary_errors.clone();

    let ext = InlineExtension::new("<ext:lifecycle-safety>", move |api| {
        let cap = cap_clone.clone();
        api.on_input(move |_ev, ctx| {
            let mut guard = cap.lock().unwrap();
            if guard.is_none() {
                *guard = Some(ctx);
            }
            async move { Ok(Some(InputEventResult::Continue)) }
        });

        // turn_end 返回指向不存在 ID 的非法 ContextEdit 草案：预览必须拦截并丢弃全部草案
        api.on_turn_end(|_ev, _ctx| async move {
            Ok(Some(BoundaryResult {
                entries: Some(vec![
                    SessionBoundaryDraft::Custom {
                        custom_type: "should-be-discarded".to_owned(),
                        data: None,
                    },
                    SessionBoundaryDraft::ContextEdit {
                        target_id: "non-existent-id".to_owned(),
                        replacement: None,
                    },
                ]),
                r#continue: Some(false),
            }))
        });

        // agent_settled 期间重入调用 send_user_message (prompt)：必须进入延迟队列并在 settled 结束后顺序执行
        let re = reentered_clone.clone();
        api.on_lifecycle("agent_settled", move |_ev, ctx| {
            let re = re.clone();
            async move {
                if re.fetch_add(1, Ordering::SeqCst) == 0 {
                    let _ =
                        ctx.runtime()
                            .send_user_message("follow-up from agent_settled", None, None);
                }
                Ok(SessionBeforeEventOutput::None)
            }
        });

        Ok(())
    });

    let sm = SessionManager::in_memory("/proj", None, None).unwrap();
    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models, sm);
    opts.extensions = vec![Box::new(ext)];

    let session = AgentSession::new(opts).unwrap();
    session.extension_runner().on_error(Arc::new(move |err| {
        be_clone.lock().unwrap().push(err.clone());
    }));

    let disp = session.prompt("first prompt", None).await.unwrap();
    assert_eq!(disp, PromptDisposition::Started);
    // 等待 agent_settled 触发的延迟重入运行完成
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    session.wait_for_idle().await;

    // 验证非法边界草案已被报错并整批丢弃（should-be-discarded 未写入真实会话）
    assert!(
        boundary_errors
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.event == "turn_end" && e.error.contains("Invalid boundary entries"))
    );
    let entries = session.with_session_manager(SessionManager::get_entries);
    assert!(!entries.iter().any(|e| matches!(
        e,
        SessionEntry::Custom(ce) if ce.custom_type == "should-be-discarded"
    )));

    // 验证重入的第二次提问已完成
    assert_eq!(session.get_session_stats().user_messages, 2);

    // 执行 new_session 后，先前捕获的旧 ExtensionContext 必须立即失效！
    let old_ctx = captured_ctx.lock().unwrap().clone().expect("captured ctx");
    assert!(old_ctx.check_active().is_ok());

    session.new_session(None).await.unwrap();
    assert!(old_ctx.check_active().is_err());
    assert!(old_ctx.runtime().set_session_name("illegal").is_err());
    let panic_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = old_ctx.session_id();
    }));
    assert!(
        panic_res.is_err(),
        "calling methods on stale ExtensionContext must panic/fail"
    );
}

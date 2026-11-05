use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_agent::AgentToolResult;
use pi_ai::faux::{
    FauxResponseStep, faux_assistant_message, faux_text_message, faux_tool_call,
    register_faux_provider,
};
use pi_ai::{JsonObject, Models, StopReason, Usage, UsageCost};
use pi_session::{
    AgentSession, AgentSessionOptions, BashExecutionMessage, BeforeAgentStartCustomMessageDraft,
    ChannelUiContext, ExtensionMode, ForkSessionOptions, InlineExtension, NavigateTreeOptions,
    SendMessageOptions, SessionEntry, SessionManager, ToolDefinition, UiPromptRequest,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from("E:/tmp/2026-11-04/argon-phase3-development/tests")
        .join(format!("{name}-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn agent_session_full_e2e_lifecycle_tools_deferred_flushing_compaction_tree_fork_and_ui() {
    let dir = temp_dir("agent-session-e2e");
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);

    let (ui_ctx, mut ui_rx) = ChannelUiContext::pair();
    tokio::spawn(async move {
        while let Some(req) = ui_rx.recv().await {
            if let UiPromptRequest::Confirm { respond, .. } = req {
                let _ = respond.send(true);
            }
        }
    });

    let mut tc_args = JsonObject::new();
    tc_args.insert("path".to_owned(), json!("config.toml"));
    let mut asst_with_usage = faux_text_message("Read config.toml and completed turn 1.");
    asst_with_usage.usage = Usage {
        input: 500,
        output: 120,
        cache_read: 50,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 670,
        cost: UsageCost {
            input: 0.01,
            output: 0.01,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.02,
            total_from_provider: Some(true),
        },
    };

    let mut post_compact_asst = faux_text_message("Post-compaction turn 2 answer.");
    post_compact_asst.usage = Usage {
        input: 200,
        output: 80,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 280,
        cost: UsageCost::default(),
    };

    reg.set_responses(vec![
        // 1. 第一轮：调用 read_config 工具 -> 返回带 usage 的最终回复
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call(
                "read_config",
                tc_args,
                Some("call_cfg".to_owned()),
            )],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(asst_with_usage),
        // 2. 手动压缩调用模型生成总结
        FauxResponseStep::Static(faux_text_message("## Goal\nCompacted turn 1 history")),
        // 3. 压缩后的第二轮提问
        FauxResponseStep::Static(post_compact_asst),
        // 4. navigate_tree 分支总结调用
        FauxResponseStep::Static(faux_text_message(
            "## Goal\nBranch summary before jumping back",
        )),
    ]);

    let session_holder: Arc<Mutex<Option<AgentSession>>> = Arc::new(Mutex::new(None));
    let sh_for_tool = session_holder.clone();

    let ext = InlineExtension::new("<e2e:extension>", move |api| {
        api.on_tool_call(|ev, ctx| async move {
            if ev.tool_name == "read_config" {
                let ok = ctx
                    .ui()
                    .confirm(
                        "Confirm Read",
                        "Allow reading config.toml?",
                        None,
                        CancellationToken::new(),
                    )
                    .await;
                assert!(ok);
            }
            Ok(None)
        });

        let sh = sh_for_tool.clone();
        api.register_tool(ToolDefinition::new(
            "read_config",
            "Read Config",
            "Reads configuration file",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            Arc::new(move |_id, _args, _cancel, _upd, _ctx| {
                let sh = sh.clone();
                Box::pin(async move {
                    // 在工具执行期间（is_streaming == true）记录一条 bash 结果和一条非触发轮次自定义消息：
                    // 它们必须延迟到本轮 toolResult 落盘之后再刷入，绝不能插在 toolCall 与 toolResult 之间！
                    let sess_opt = sh.lock().unwrap().clone();
                    if let Some(sess) = sess_opt {
                        sess.record_bash_result(BashExecutionMessage {
                            command: "echo in-stream".to_owned(),
                            output: "in-stream".to_owned(),
                            exit_code: Some(0),
                            cancelled: false,
                            truncated: false,
                            full_output_path: None,
                            timestamp: 1000,
                            exclude_from_context: None,
                        })
                        .unwrap();
                        sess.send_custom_message(
                            BeforeAgentStartCustomMessageDraft {
                                custom_type: "deferred.note".to_owned(),
                                content: "deferred note during tool execution".into(),
                                display: true,
                                details: None,
                            },
                            SendMessageOptions {
                                trigger_turn: false,
                                deliver_as: None,
                            },
                        )
                        .await
                        .unwrap();
                    }
                    Ok(AgentToolResult::text("port = 8080"))
                })
            }),
        ));

        Ok(())
    });

    let sm = SessionManager::create("/proj", &dir, None).unwrap();
    let mut opts = AgentSessionOptions::new("/proj", reg.model.clone(), models.clone(), sm);
    opts.extensions = vec![Box::new(ext)];
    opts.ui_context = Some(Arc::new(ui_ctx));
    opts.mode = ExtensionMode::Interactive;

    let session = AgentSession::new(opts).unwrap();
    *session_holder.lock().unwrap() = Some(session.clone());

    // 1. 执行第一轮对话（含工具调用、UI 确认弹窗及流式期间的延迟 bash/custom 消息）
    session
        .prompt("Please read config.toml", None)
        .await
        .unwrap();

    // 验证会话分支中 toolCall (assistant) 后紧跟的是配对的 toolResult，延迟的 custom_message 和 bashExecution 均排在后面！
    let branch_after_t1 = session.with_session_manager(|sm| sm.get_branch(None));
    let roles_in_order: Vec<String> = branch_after_t1
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => Some(m.message.role().to_owned()),
            SessionEntry::CustomMessage(_) => Some("custom_message".to_owned()),
            _ => None,
        })
        .collect();
    let first_asst_pos = roles_in_order
        .iter()
        .position(|r| r == "assistant")
        .unwrap();
    assert_eq!(
        roles_in_order[first_asst_pos + 1],
        "toolResult",
        "deferred custom/bash messages must not split assistant toolCall from its toolResult"
    );
    assert!(roles_in_order.contains(&"custom_message".to_owned()));
    assert!(roles_in_order.contains(&"bashExecution".to_owned()));

    // 2. 验证压缩前的 get_context_usage() 返回真实 token 估算
    let usage_before_compact = session.get_context_usage().unwrap();
    assert!(usage_before_compact.tokens.unwrap() >= 670);

    // 3. 执行手动压缩（/compact）：刚压缩完且尚无新助手回复时，get_context_usage().tokens 必须返回 None！
    let comp_res = session
        .compact(Some("keep config port".to_owned()))
        .await
        .unwrap();
    assert!(comp_res.summary.contains("Compacted turn 1 history"));
    let usage_right_after_compact = session.get_context_usage().unwrap();
    assert!(
        usage_right_after_compact.tokens.is_none(),
        "get_context_usage().tokens must be None immediately after compaction before next assistant reply"
    );

    // 4. 发送第二轮提问：收到新助手回复后，get_context_usage().tokens 恢复为新回复的真实用量
    session.prompt("What port was it?", None).await.unwrap();
    let usage_after_turn2 = session.get_context_usage().unwrap();
    assert_eq!(usage_after_turn2.tokens, Some(280));

    // 5. 验证 get_session_stats() 跨压缩边界汇总全部历史消息与账单
    let stats = session.get_session_stats();
    assert_eq!(stats.user_messages, 2);
    assert_eq!(stats.assistant_messages, 3);
    assert_eq!(stats.tool_calls, 1);
    assert_eq!(stats.tool_results, 1);
    assert!(stats.total_tokens >= 950);

    // 6. 验证 navigate_tree：跳转到第一条 user 消息时，叶节点移至其 parent_id (None)，editor_text 返回原提问文本，并生成 BranchSummaryEntry！
    let first_user_id = branch_after_t1
        .iter()
        .find_map(|e| match e {
            SessionEntry::Message(m) if m.message.role() == "user" => Some(m.id.clone()),
            _ => None,
        })
        .unwrap();

    let nav_res = session
        .navigate_tree(
            &first_user_id,
            NavigateTreeOptions {
                summarize: true,
                custom_instructions: None,
                replace_instructions: false,
                label: Some("navigated-root".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(!nav_res.cancelled);
    assert_eq!(
        nav_res.editor_text.as_deref(),
        Some("Please read config.toml")
    );
    assert!(nav_res.summary_entry.is_some());

    // 7. 验证导出 JSONL 与 fork 派生新会话
    let export_path = dir.join("exported_branch.jsonl");
    session.export_to_jsonl(Some(&export_path)).unwrap();
    assert!(export_path.exists());

    let summary_id = nav_res.summary_entry.unwrap().id;
    let forked = session
        .fork(
            &summary_id,
            ForkSessionOptions {
                position: Some("at".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(forked);

    let _ = std::fs::remove_dir_all(&dir);
}

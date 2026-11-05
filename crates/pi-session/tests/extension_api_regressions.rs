use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::faux::register_faux_provider;
use pi_ai::{ApiKeyAuth, CreateProviderOptions, Models, ProviderApiConfig, ProviderAuth};
use pi_session::{
    AgentSession, AgentSessionOptions, ExtensionRunner, InlineExtension, ModelRoute,
    SessionManager, VirtualModelDefinition,
};

fn provider_config(model: pi_ai::Model) -> CreateProviderOptions {
    CreateProviderOptions {
        id: model.provider.clone(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth::api_key(ApiKeyAuth::keyless("test", "local-key")),
        models: vec![pi_ai::AnyModel::Chat(model)],
        fetch_models: None,
        filter_models: None,
        api: Some(ProviderApiConfig::ByApi(pi_ai::default_api_map())),
        classifiers: None,
    }
}

fn virtual_definition(model: pi_ai::Model) -> VirtualModelDefinition {
    VirtualModelDefinition {
        provider: "virtual-test".into(),
        id: "route".into(),
        name: "Route".into(),
        thinking_levels: None,
        context_window: Some(100_000),
        max_tokens: Some(2048),
        input: None,
        route: Arc::new(move |_| {
            let model = model.clone();
            Box::pin(async move {
                Ok(ModelRoute {
                    model,
                    thinking_level: pi_ai::ModelThinkingLevel::Off,
                    state: None,
                })
            })
        }),
    }
}

#[test]
fn queued_and_live_provider_changes_restore_baselines_and_reject_stale_handles() {
    let models = Arc::new(Models::default());
    let model = register_faux_provider(&models).model;
    let baseline = models.get_provider("faux").unwrap();
    let m = model.clone();
    let ext = InlineExtension::new("catalog", move |api| {
        api.register_provider_config("queued", provider_config(m.clone()))
            .unwrap();
        api.unregister_provider("queued");
        api.register_provider_config("live", provider_config(m.clone()))
            .unwrap();
        api.register_virtual_model(virtual_definition(m.clone()));
        api.unregister_virtual_model("virtual-test", "route");
        Ok(())
    });
    let runner = ExtensionRunner::from_extensions("/proj", vec![Box::new(ext)]).unwrap();
    assert!(models.get_provider("live").is_none());
    runner.bind_models(models.clone());
    assert!(models.get_provider("queued").is_none());
    assert!(models.get_provider("virtual-test").is_none());
    assert!(models.get_provider("live").is_some());
    let runtime = runner.runtime();
    runtime
        .register_provider_config("faux", provider_config(model.clone()))
        .unwrap();
    runtime.unregister_provider("faux").unwrap();
    assert_eq!(models.get_provider("faux").unwrap().name(), baseline.name());
    runtime
        .register_virtual_model(virtual_definition(model))
        .unwrap();
    assert!(models.get_model("virtual-test", "route").is_some());
    runtime
        .unregister_virtual_model("virtual-test", "route")
        .unwrap();
    assert!(models.get_provider("virtual-test").is_none());
    runtime.unregister_provider("live").unwrap();
    assert!(models.get_provider("live").is_none());
    runner.invalidate(None);
    assert!(runtime.register_provider(baseline).is_err());
}

fn local_http() -> (String, std::thread::JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
            if let Some(i) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..i]);
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= i + 4 + length {
                    break;
                }
            }
        }
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        String::from_utf8(request).unwrap()
    });
    (url, handle)
}

#[tokio::test]
async fn session_headers_hook_sees_assembled_auth_and_changes_actual_http_request() {
    let (url, request) = local_http();
    let models = Arc::new(Models::default());
    let mut model = register_faux_provider(&models).model;
    model.api = pi_ai::known_api::OPENAI_COMPLETIONS.into();
    model.provider = "local".into();
    model.base_url = url;
    model.headers = Some(BTreeMap::from([("x-model".into(), "model".into())]));
    models.set_provider(pi_ai::create_provider(provider_config(model.clone())).unwrap());
    let seen = Arc::new(Mutex::new(false));
    let s = seen.clone();
    let ext = InlineExtension::new("headers", move |api| {
        let s = s.clone();
        api.on_before_provider_headers(Arc::new(move |event, _| {
            assert_eq!(
                event.headers["authorization"].as_deref(),
                Some("Bearer local-key")
            );
            assert_eq!(
                event.headers["content-type"].as_deref(),
                Some("application/json")
            );
            assert_eq!(event.headers["x-model"].as_deref(), Some("model"));
            *s.lock().unwrap() = true;
            event.headers.insert("Authorization".into(), None);
            event
                .headers
                .insert("x-model".into(), Some("changed".into()));
            event
                .headers
                .insert("x-extension".into(), Some("yes".into()));
            Box::pin(async { Ok(()) })
        }));
        Ok(())
    });
    let mut opts = AgentSessionOptions::new(
        "/proj",
        model,
        models,
        SessionManager::in_memory("/proj", None, None).unwrap(),
    );
    opts.extensions = vec![Box::new(ext)];
    let session = AgentSession::new(opts).unwrap();
    session.prompt("hello", None).await.unwrap();
    let request = request.join().unwrap().to_ascii_lowercase();
    assert!(*seen.lock().unwrap());
    assert!(!request.contains("authorization:"));
    assert!(request.contains("x-model: changed"));
    assert!(request.contains("x-extension: yes"));
}

#[tokio::test]
async fn handler_panics_are_isolated_but_permission_panics_propagate() {
    use pi_session::{MessageEndEventResult, ToolCallEvent, UserBashEvent};
    let ext = InlineExtension::new("panic", |api| {
        api.on_message_end(
            |_, _| -> pi_agent::BoxFuture<'static, Result<Option<MessageEndEventResult>, String>> {
                panic!("sync panic")
            },
        );
        api.on_message_end(|_, _| async { panic!("async panic") });
        api.on_message_end(|event, _| async move {
            let mut message = event.message;
            if let pi_agent::AgentMessage::User(ref mut user) = message {
                user.content = pi_ai::UserContent::Text("changed".into());
            }
            Ok(Some(MessageEndEventResult {
                message: Some(message),
            }))
        });
        api.on_tool_call(|_, _| async { panic!("deny tool") });
        api.on_user_bash(Arc::new(|_, _| panic!("deny bash")));
        Ok(())
    });
    let runner = ExtensionRunner::from_extensions("/proj", vec![Box::new(ext)]).unwrap();
    let result = runner
        .emit_message_end(pi_agent::AgentMessage::User(pi_ai::UserMessage::new(
            "original",
        )))
        .await
        .unwrap();
    assert!(
        matches!(result, pi_agent::AgentMessage::User(user) if user.content == pi_ai::UserContent::Text("changed".into()))
    );
    assert!(
        runner
            .emit_tool_call(ToolCallEvent {
                tool_call_id: "t1".into(),
                tool_name: "tool".into(),
                input: serde_json::json!({}),
                parent_tool_call_id: None,
            })
            .await
            .unwrap_err()
            .contains("deny tool")
    );
    assert!(
        runner
            .emit_user_bash(UserBashEvent {
                command: "echo test".into(),
                cwd: "/proj".into(),
                exclude_from_context: false,
            })
            .await
            .err()
            .unwrap()
            .contains("deny bash")
    );
}

#[tokio::test]
async fn reload_invalidates_old_contexts_and_replaces_catalog_overrides() {
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let mut opts = AgentSessionOptions::new(
        "/proj",
        reg.model.clone(),
        models.clone(),
        SessionManager::in_memory("/proj", None, None).unwrap(),
    );
    let model = reg.model.clone();
    opts.extensions = vec![Box::new(InlineExtension::new("old", move |api| {
        api.runtime()
            .register_provider_config("temporary", provider_config(model.clone()))
            .unwrap();
        api.unregister_provider("temporary");
        api.register_provider_config("old-only", provider_config(model.clone()))
            .unwrap();
        Ok(())
    }))];
    let session = AgentSession::new(opts).unwrap();
    let old_runner = session.extension_runner();
    let old_context = old_runner.create_context();
    assert!(models.get_provider("temporary").is_none());
    assert!(models.get_provider("old-only").is_some());
    session.reload_with_extensions(Vec::new()).await.unwrap();
    assert!(old_context.check_active().is_err());
    assert!(old_runner.runtime().unregister_provider("faux").is_err());
    assert!(models.get_provider("old-only").is_none());
    assert!(models.get_provider("faux").is_some());
    let live = session.extension_runner();
    let broken = InlineExtension::new("broken", |_| {
        Err(pi_session::ExtensionInitError {
            extension_path: "broken".into(),
            message: "bad config".into(),
        })
    });
    assert!(
        session
            .reload_with_extensions(vec![Box::new(broken)])
            .await
            .is_err()
    );
    assert!(!live.is_invalidated());
    assert!(!session.extension_runner().is_invalidated());
}

#[tokio::test]
async fn bash_execution_is_injectable_and_permission_errors_stop_execution() {
    use pi_session::{BashResult, ExecuteBashOptions};
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let mut opts = AgentSessionOptions::new(
        "/proj",
        reg.model.clone(),
        models,
        SessionManager::in_memory("/proj", None, None).unwrap(),
    );
    opts.extensions = vec![Box::new(InlineExtension::new("bash", |api| {
        api.on_user_bash(Arc::new(|event, _| {
            Box::pin(async move {
                if event.command == "blocked" {
                    return Err("permission denied".into());
                }
                Ok(None)
            })
        }));
        Ok(())
    }))];
    let session = AgentSession::new(opts).unwrap();
    let called = Arc::new(Mutex::new(0));
    let c = called.clone();
    let options = ExecuteBashOptions {
        exclude_from_context: true,
        executor: Some(Arc::new(move |request| {
            *c.lock().unwrap() += 1;
            assert_eq!(request.command, "safe");
            assert_eq!(request.cwd, "/proj");
            Box::pin(async {
                Ok(BashResult {
                    output: "done".into(),
                    exit_code: Some(0),
                    ..Default::default()
                })
            })
        })),
        ..Default::default()
    };
    assert!(
        session
            .execute_bash("blocked", options.clone())
            .await
            .unwrap_err()
            .contains("permission denied")
    );
    assert_eq!(*called.lock().unwrap(), 0);
    assert_eq!(
        session.execute_bash("safe", options).await.unwrap().output,
        "done"
    );
    session.with_session_manager(|sm| {
        let message = sm
            .get_entries()
            .iter()
            .find_map(|entry| match entry {
                pi_session::SessionEntry::Message(m) => {
                    pi_session::BashExecutionMessage::from_agent_message(&m.message)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(message.exclude_from_context, Some(true));
    });
}

#[tokio::test]
async fn live_virtual_registration_is_used_by_the_next_request() {
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    reg.set_responses(vec![pi_ai::faux::FauxResponseStep::Static(
        pi_ai::faux::faux_text_message("routed"),
    )]);
    let session = AgentSession::new(AgentSessionOptions::new(
        "/proj",
        reg.model.clone(),
        models.clone(),
        SessionManager::in_memory("/proj", None, None).unwrap(),
    ))
    .unwrap();
    session
        .extension_runner()
        .runtime()
        .register_virtual_model(virtual_definition(reg.model))
        .unwrap();
    session
        .set_model(models.get_model("virtual-test", "route").unwrap())
        .await
        .unwrap();
    session.prompt("hello", None).await.unwrap();
    assert_eq!(session.get_session_stats().assistant_messages, 1);
    assert_eq!(session.model().provider, "virtual-test");
}

#[tokio::test]
async fn bash_results_created_in_a_tool_are_deferred_until_after_the_tool_result() {
    use pi_ai::faux::{
        FauxResponseStep, faux_assistant_message, faux_text_message, faux_tool_call,
    };
    use pi_session::{BashResult, ExecuteBashOptions, SessionEntry, ToolDefinition};
    let models = Arc::new(Models::default());
    let reg = register_faux_provider(&models);
    let holder: Arc<Mutex<Option<AgentSession>>> = Arc::new(Mutex::new(None));
    let h = holder.clone();
    let tool = ToolDefinition::new(
        "run",
        "Run",
        "Run",
        serde_json::json!({"type":"object"}),
        Arc::new(move |_, _, _, _, _| {
            let session = h.lock().unwrap().clone().unwrap();
            Box::pin(async move {
                assert!(session.is_streaming());
                session
                    .execute_bash(
                        "local",
                        ExecuteBashOptions {
                            executor: Some(Arc::new(|_| {
                                Box::pin(async {
                                    Ok(BashResult {
                                        output: "done".into(),
                                        ..Default::default()
                                    })
                                })
                            })),
                            ..Default::default()
                        },
                    )
                    .await?;
                session.with_session_manager(|sm| assert!(!sm.get_entries().iter().any(|entry| matches!(entry, SessionEntry::Message(m) if m.message.role() == "bashExecution"))));
                Ok(pi_agent::AgentToolResult::text("done"))
            })
        }),
    );
    let mut opts = AgentSessionOptions::new(
        "/proj",
        reg.model.clone(),
        models,
        SessionManager::in_memory("/proj", None, None).unwrap(),
    );
    opts.base_tools = vec![tool];
    let session = AgentSession::new(opts).unwrap();
    *holder.lock().unwrap() = Some(session.clone());
    reg.set_responses(vec![
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call("run", Default::default(), Some("t1".into()))],
            Some(pi_ai::StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("done")),
    ]);
    session.prompt("run", None).await.unwrap();
    session.with_session_manager(|sm| {
        let roles: Vec<_> = sm
            .get_entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::Message(m) => Some(m.message.role().to_owned()),
                _ => None,
            })
            .collect();
        let tool = roles.iter().position(|role| *role == "toolResult").unwrap();
        let bash = roles
            .iter()
            .position(|role| *role == "bashExecution")
            .unwrap();
        assert!(bash > tool);
    });
    *holder.lock().unwrap() = None;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn a_header_hook_that_poisons_storage_cannot_send_the_http_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let models = Arc::new(Models::default());
    let mut model = register_faux_provider(&models).model;
    model.api = pi_ai::known_api::OPENAI_COMPLETIONS.into();
    model.provider = "local".into();
    model.base_url = format!("http://{}", listener.local_addr().unwrap());
    models.set_provider(pi_ai::create_provider(provider_config(model.clone())).unwrap());
    let dir = std::path::PathBuf::from("E:/tmp/2026-11-04/argon-phase3-development/review-tests")
        .join(pi_ai::uuid_v7());
    let sm = SessionManager::create("/proj", dir, None).unwrap();
    let fault = sm.fault_injection().unwrap();
    let ext = InlineExtension::new("poison", move |api| {
        let fault = fault.clone();
        api.on_before_provider_headers(Arc::new(move |_, ctx| {
            {
                let mut f = fault.lock().unwrap();
                f.fail_append_after_bytes = Some(1);
                f.fail_append_recovery = true;
            }
            let result = ctx.runtime().append_entry("poison", None).map(|_| ());
            Box::pin(async move { result })
        }));
        Ok(())
    });
    let mut opts = AgentSessionOptions::new("/proj", model, models, sm);
    opts.extensions = vec![Box::new(ext)];
    let session = AgentSession::new(opts).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), session.prompt("hello", None))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

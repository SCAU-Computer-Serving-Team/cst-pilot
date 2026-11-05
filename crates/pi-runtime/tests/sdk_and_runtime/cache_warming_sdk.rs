use pi_agent::AgentMessage;
use pi_ai::*;
use pi_runtime::*;
use pi_session::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

async fn wait_for(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !check() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local mock request did not settle");
}

#[tokio::test]
async fn sdk_warms_final_physical_context_silently_and_rejects_late_generations() {
    let root = std::path::PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("sdk-warm-{}", uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let clock = ManualTestClock::new(0);
    let mut model = unconfigured_model();
    model.provider = "sdk-warm-mock".into();
    model.id = "physical".into();
    model.api = known_api::OPENAI_COMPLETIONS.into();
    model.base_url = "https://mock.invalid/v1".into();
    model.context_window = 100_000;
    model.max_tokens = 100;
    model.prompt_cache = Some(ModelPromptCache {
        short: Some(60.0),
        long: Some(60.0),
    });
    model.cost = ModelCost {
        input: 20.0,
        output: 10.0,
        cache_read: 1.0,
        cache_write: 20.0,
        ..Default::default()
    };
    let calls = Arc::new(Mutex::new(Vec::<(Model, TranscriptContext, bool)>::new()));
    let held = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(Mutex::new(Vec::new()));
    let (calls_copy, held_copy, pending_copy) = (calls.clone(), held.clone(), pending.clone());
    let provider = create_provider(CreateProviderOptions {
        id: model.provider.clone(), name: None, base_url: None, headers: None,
        auth: ProviderAuth::api_key(ApiKeyAuth::keyless("sdk-warm-mock", "local-mock")),
        models: vec![AnyModel::Chat(model.clone())], fetch_models: None, filter_models: None, classifiers: None,
        api: Some(ProviderApiConfig::Single(ProviderStreams::new(Arc::new(|_,_,_| panic!("unexpected full stream")), Arc::new(move |m,c,o| {
            let (model,context,options) = (m.clone(),c.clone(),o.cloned().unwrap());
            let stream = create_assistant_message_event_stream();
            let (writer,calls,held,pending) = (stream.clone(),calls_copy.clone(),held_copy.clone(),pending_copy.clone());
            tokio::spawn(async move {
                let warm = options.base.max_tokens == Some(1);
                // A native provider invokes the same hooks as an HTTP provider without doing I/O.
                if let Some(headers) = options.base.on_headers { let headers = headers(Default::default(),model.clone()).await; assert_eq!(headers.get("x-warm-hook").and_then(|v|v.as_deref()),Some("yes")); }
                if let Some(payload) = options.base.on_payload { assert_eq!(payload(serde_json::json!({}),model.clone()).await.unwrap()["warmHook"],true); }
                calls.lock().unwrap().push((model.clone(), context, warm));
                let mut msg = AssistantMessage::new_pending(&model); msg.stop_reason=StopReason::Stop; msg.usage.input=10_000;
                msg.content = vec![ContentBlock::Text(TextContent::new("mock answer"))];
                if warm && held.load(Ordering::SeqCst) { pending.lock().unwrap().push((writer,msg)); }
                else { writer.push(AssistantMessageEvent::Done { reason:StopReason::Stop,message:msg.clone() }); writer.end(Some(msg)); }
            }); stream
        }))))
    }).unwrap();
    let rt = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    rt.register_provider(provider);
    let settings = SettingsManager::in_memory();
    settings
        .set_cache_warming_mode(CacheWarmingMode::Idle, SettingsScope::Global)
        .unwrap();
    settings
        .set_block_images(true, SettingsScope::Global)
        .unwrap();
    let headers_seen = Arc::new(AtomicUsize::new(0));
    let payloads_seen = Arc::new(AtomicUsize::new(0));
    let physical = model.clone();
    let definition = VirtualModelDefinition {
        provider: "warm-virtual".into(),
        id: "selected".into(),
        name: "Selected".into(),
        thinking_levels: None,
        context_window: Some(100_000),
        max_tokens: Some(100),
        input: None,
        route: Arc::new(move |_| {
            let model = physical.clone();
            Box::pin(async move {
                Ok(ModelRoute {
                    model,
                    thinking_level: ModelThinkingLevel::Off,
                    state: None,
                })
            })
        }),
    };
    let virtual_model = create_virtual_model(&definition);
    let (h, p) = (headers_seen.clone(), payloads_seen.clone());
    let factory: ExtensionFactory = Arc::new(move || {
        let (definition, h, p) = (definition.clone(), h.clone(), p.clone());
        Ok(Box::new(InlineExtension::new("<warm-sdk>", move |api| {
            api.register_virtual_model(definition.clone());
            let h = h.clone();
            api.on_before_provider_headers(Arc::new(move |event, _| {
                h.fetch_add(1, Ordering::SeqCst);
                event
                    .headers
                    .insert("x-warm-hook".into(), Some("yes".into()));
                Box::pin(async { Ok(()) })
            }));
            let p = p.clone();
            api.on_before_provider_request(move |mut event, _| {
                p.fetch_add(1, Ordering::SeqCst);
                event.payload["warmHook"] = serde_json::json!(true);
                async move { Ok(Some(event.payload)) }
            });
            api.on_context(Arc::new(|event, _| {
                let mut messages = event.messages.clone();
                messages.push(AgentMessage::User(UserMessage::new(
                    "transformed-final-context",
                )));
                Box::pin(async move {
                    Ok(Some(ContextEventResult {
                        messages: Some(messages),
                    }))
                })
            }));
            api.on_cache_warming_decision(|_, _| async {
                Ok(Some(CacheWarmingDecisionEventResult {
                    action: Some(CacheWarmingAction::Warm),
                }))
            });
            Ok(())
        })))
    });
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: root.clone(),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(rt),
        settings_manager: Some(settings.clone()),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            extension_factories: vec![factory],
            no_skills: true,
            no_context_files: true,
            no_prompt_templates: true,
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    let result = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(root.clone()),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(services.model_runtime),
        settings_manager: Some(settings),
        resource_loader: Some(services.resource_loader),
        model: Some(virtual_model),
        clock: Some(Arc::new(clock.clone())),
        session_manager: Some(
            SessionManager::create(root.to_string_lossy(), root.join("sessions"), None).unwrap(),
        ),
        ..Default::default()
    })
    .await
    .unwrap();
    let session = result.session.clone();
    let events = Arc::new(AtomicUsize::new(0));
    let ev = events.clone();
    let unsubscribe = session.subscribe(Arc::new(move |_| {
        ev.fetch_add(1, Ordering::SeqCst);
    }));
    session
        .prompt(
            "real request",
            Some(PromptOptions {
                images: Some(vec![ImageContent {
                    data: "mock-image".into(),
                    mime_type: "image/png".into(),
                }]),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(result.cache_warmer.status().state, "scheduled");
    let before_events = events.load(Ordering::SeqCst);
    let before_messages = session.state().messages.clone();
    let before_entries = session.with_session_manager(|sm| sm.get_entries());
    clock.set_ms(result.cache_warmer.status().next_warm_at.unwrap());
    wait_for(|| {
        session.with_session_manager(|sm| {
            sm.get_entries()
                .iter()
                .any(|e| matches!(e, SessionEntry::Usage(_)))
        })
    })
    .await;
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(headers_seen.load(Ordering::SeqCst), 2);
    assert_eq!(payloads_seen.load(Ordering::SeqCst), 2);
    {
        let captured = calls.lock().unwrap();
        assert_eq!(captured[0].0.id, "physical");
        assert_eq!(captured[0].1, captured[1].1);
        assert!(captured[1].2);
        let serialized = serde_json::to_string(captured[1].1.messages()).unwrap();
        assert!(serialized.contains("transformed-final-context"));
        assert!(!serialized.contains("mock-image"));
    }
    assert_eq!(events.load(Ordering::SeqCst), before_events);
    assert_eq!(session.state().messages, before_messages);
    assert_eq!(
        session.with_session_manager(|sm| sm.get_entries().len()),
        before_entries.len() + 1
    );

    // A completed request from an older model selection must never append usage.
    held.store(true, Ordering::SeqCst);
    clock.set_ms(result.cache_warmer.status().next_warm_at.unwrap());
    wait_for(|| !pending.lock().unwrap().is_empty()).await;
    session.set_model(model).await.unwrap();
    let before = session.with_session_manager(|sm| sm.get_entries().len());
    let (writer, msg) = pending.lock().unwrap().pop().unwrap();
    writer.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: msg.clone(),
    });
    writer.end(Some(msg));
    wait_for(|| result.cache_warmer.status().state == "inactive").await;
    // Drain the already-ready completion worker without relying on a real TTL wait.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        session.with_session_manager(|sm| sm.get_entries().len()),
        before
    );
    // Context replacement during a second in-flight replay also invalidates its usage.
    held.store(false, Ordering::SeqCst);
    session.prompt("next request", None).await.unwrap();
    held.store(true, Ordering::SeqCst);
    clock.set_ms(result.cache_warmer.status().next_warm_at.unwrap());
    wait_for(|| !pending.lock().unwrap().is_empty()).await;
    session.agent().set_messages(vec![]);
    let before = session.with_session_manager(|sm| sm.get_entries().len());
    let (writer, msg) = pending.lock().unwrap().pop().unwrap();
    writer.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: msg.clone(),
    });
    writer.end(Some(msg));
    wait_for(|| result.cache_warmer.status().state == "inactive").await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        session.with_session_manager(|sm| sm.get_entries().len()),
        before
    );

    #[cfg(feature = "test-support")]
    {
        // A real persistent append failure must surface on the session and stop warming.
        held.store(false, Ordering::SeqCst);
        session
            .prompt("request before disk fault", None)
            .await
            .unwrap();
        let fault = session.with_session_manager(|sm| sm.fault_injection().unwrap());
        fault.lock().unwrap().fail_append_after_bytes = Some(1);
        clock.set_ms(result.cache_warmer.status().next_warm_at.unwrap());
        wait_for(|| session.fatal_runtime_error().is_some()).await;
        assert!(
            result
                .cache_warmer
                .status()
                .reason
                .unwrap()
                .contains("storage error")
        );
        fault.lock().unwrap().fail_append_after_bytes = None;
        assert!(session.shutdown().await.is_err());
    }
    #[cfg(not(feature = "test-support"))]
    session.shutdown().await.unwrap();
    unsubscribe();
}

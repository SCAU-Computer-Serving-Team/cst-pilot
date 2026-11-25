use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pi_agent::AgentMessage;
use pi_ai::{
    AnyModel, ApiKeyAuth, AssistantMessage, ClientIdentity, ContentBlock, CreateProviderOptions,
    ProviderApiConfig, ProviderAuth, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    TranscriptContext, UserMessage, create_provider, default_api_map, known_api,
};
use pi_runtime::{
    CreateAgentSessionOptions, CreateAgentSessionServicesOptions, CreateModelRuntimeOptions,
    DefaultResourceLoaderOptions, ModelRuntime, SettingsManager, create_agent_session,
    create_agent_session_services,
};
use pi_session::{
    ExtensionFactory, InlineExtension, NavigateTreeOptions, SessionManager, unconfigured_model,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

type WireHeaders = BTreeMap<String, String>;

async fn capture(listener: TcpListener) -> Vec<WireHeaders> {
    let mut captured = Vec::new();
    for _ in 0..4 {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        let mut request_line = String::new();
        socket.read_line(&mut request_line).await.unwrap();
        assert_eq!(
            request_line.trim_end(),
            "POST /v1/chat/completions HTTP/1.1"
        );
        let mut headers = BTreeMap::new();
        loop {
            let mut line = String::new();
            assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
        let mut body = vec![0; headers["content-length"].parse().unwrap()];
        socket.read_exact(&mut body).await.unwrap();
        captured.push(headers);
        let sse = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Local answer\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2000,\"completion_tokens\":10,\"total_tokens\":2010}}\n\n",
            "data: [DONE]\n\n"
        );
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{sse}",
            sse.len()
        );
        socket.write_all(reply.as_bytes()).await.unwrap();
    }
    captured
}

#[tokio::test]
async fn sdk_chat_compaction_and_branch_summary_share_identity_and_header_hook() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut model = unconfigured_model();
    model.id = "loopback-chat".into();
    model.provider = "openrouter".into();
    model.api = known_api::OPENAI_COMPLETIONS.into();
    model.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    model.context_window = 100_000;
    model.max_tokens = 4096;
    let server = tokio::spawn(tokio::time::timeout(
        Duration::from_secs(20),
        capture(listener),
    ));

    let root = std::env::temp_dir().join(format!("pi-sdk-header-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(
        create_provider(CreateProviderOptions {
            id: model.provider.clone(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::api_key(ApiKeyAuth::keyless("openrouter", "loopback-key")),
            models: vec![AnyModel::Chat(model.clone())],
            fetch_models: None,
            filter_models: None,
            api: Some(ProviderApiConfig::ByApi(default_api_map())),
            classifiers: None,
        })
        .unwrap(),
    );
    let factory: ExtensionFactory = Arc::new(|| {
        Ok(Box::new(InlineExtension::new("loopback-headers", |api| {
            api.on_before_provider_headers(Arc::new(|event, _| {
                event
                    .headers
                    .insert("x-session-hook".into(), Some("applied".into()));
                Box::pin(async { Ok(()) })
            }));
            Ok(())
        })))
    });
    let settings = SettingsManager::in_memory();
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: root.clone(),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(runtime),
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
    let mut manager = SessionManager::in_memory(root.to_string_lossy(), None, None).unwrap();
    let mut first_user_id = None;
    for _ in 0..5 {
        let id = manager
            .append_message(AgentMessage::User(UserMessage::new("history ".repeat(300))))
            .unwrap();
        first_user_id.get_or_insert(id);
        let mut answer = AssistantMessage::new_pending(&model);
        answer.stop_reason = StopReason::Stop;
        answer.content = vec![ContentBlock::Text(TextContent::new("answer ".repeat(300)))];
        manager
            .append_message(AgentMessage::Assistant(answer))
            .unwrap();
    }
    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(root.clone()),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(services.model_runtime),
        settings_manager: Some(settings),
        resource_loader: Some(services.resource_loader),
        model: Some(model.clone()),
        session_manager: Some(manager),
        ..Default::default()
    })
    .await
    .unwrap();
    let session = &created.session;
    let mut compaction = session.compaction_settings();
    compaction.enabled = false;
    compaction.keep_recent_tokens = 100;
    session.set_compaction_settings(compaction);

    tokio::time::timeout(Duration::from_secs(15), async {
        session.prompt("chat", None).await.unwrap();
        session.compact(None).await.unwrap();
        let navigation = session
            .navigate_tree(
                &first_user_id.unwrap(),
                NavigateTreeOptions {
                    summarize: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(navigation.summary_entry.is_some());

        let stream = session.agent().stream_fn()(
            model,
            TranscriptContext::default(),
            SimpleStreamOptions {
                base: StreamOptions {
                    identity: Some(ClientIdentity {
                        user_agent: Some("custom-loopback-agent".into()),
                        attribution: Some(None),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
        assert_eq!(stream.result().await.stop_reason, StopReason::Stop);
    })
    .await
    .unwrap();

    let captured = server.await.unwrap().unwrap();
    for headers in &captured[..3] {
        assert_eq!(headers["authorization"], "Bearer loopback-key");
        assert_eq!(headers["x-session-hook"], "applied");
        assert_eq!(headers["x-openrouter-title"], "pi");
        assert_eq!(headers["http-referer"], "https://pi.dev");
        assert_eq!(headers["x-openrouter-categories"], "cli-agent");
        assert_eq!(headers["user-agent"], captured[0]["user-agent"]);
    }
    let explicit = &captured[3];
    assert_eq!(explicit["x-session-hook"], "applied");
    assert_eq!(explicit["user-agent"], "custom-loopback-agent");
    assert!(!explicit.contains_key("x-openrouter-title"));
    assert!(!explicit.contains_key("http-referer"));
    assert!(!explicit.contains_key("x-openrouter-categories"));
    session.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}

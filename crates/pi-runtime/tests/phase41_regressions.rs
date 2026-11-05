use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pi_agent::BoxFuture;
use pi_ai::JsonValue;
use pi_runtime::{
    CreateAgentSessionOptions, InMemoryTransport, McpClient, McpClientOptions, McpRequestOptions,
    McpTransport, StreamableHttpTransport, create_agent_session,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pi-phase41-review-{}", pi_ai::uuid_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn initialize_result() -> JsonValue {
    json!({
        "protocolVersion": "2026-07-28",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "review-server", "version": "1" }
    })
}

#[tokio::test]
async fn sdk_registers_builtin_extensions_without_caller_setup() {
    let dir = TestDirectory::new();
    let result = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(dir.0.clone()),
        agent_dir: Some(dir.0.join("agent")),
        ..Default::default()
    })
    .await
    .unwrap();
    let tools = result.session.get_all_tools();
    result.session.shutdown().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "codemode"));
    assert!(tools.iter().any(|t| t.name == "tool_search"));
}

struct BlockingSendTransport {
    inner: Arc<InMemoryTransport>,
}

impl McpTransport for BlockingSendTransport {
    fn start(&self) -> BoxFuture<'_, Result<mpsc::UnboundedReceiver<JsonValue>, String>> {
        self.inner.start()
    }

    fn send(&self, message: JsonValue) -> BoxFuture<'_, Result<(), String>> {
        if message["method"] == "blocked" {
            Box::pin(std::future::pending())
        } else {
            self.inner.send(message)
        }
    }

    fn close(&self) -> BoxFuture<'_, Result<(), String>> {
        self.inner.close()
    }
}

#[tokio::test]
async fn mcp_deadline_includes_transport_send_and_close_rejects_pending_calls() {
    let (local, remote) = InMemoryTransport::pair();
    let mut rx = remote.start().await.unwrap();
    let server = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if msg["method"] == "initialize" {
                remote
                    .send(json!({
                        "jsonrpc": "2.0", "id": msg["id"], "result": initialize_result()
                    }))
                    .await
                    .unwrap();
            }
        }
    });
    let client = McpClient::new(McpClientOptions::default());
    client
        .connect(Arc::new(BlockingSendTransport { inner: local }))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_millis(500),
        client.request(
            "blocked",
            None,
            McpRequestOptions {
                timeout_ms: Some(30),
                ..Default::default()
            },
        ),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "transport send bypassed the request deadline"
    );
    assert!(outcome.unwrap().unwrap_err().contains("timed out"));

    let pending_client = client.clone();
    let pending = tokio::spawn(async move {
        pending_client
            .request("no-response", None, McpRequestOptions::default())
            .await
    });
    tokio::task::yield_now().await;
    client.close().await.unwrap();
    let closed = tokio::time::timeout(Duration::from_millis(500), pending).await;
    assert!(closed.is_ok(), "close did not reject the pending request");
    assert!(closed.unwrap().unwrap().is_err());
    server.abort();
}

#[tokio::test]
async fn codemode_models_use_session_catalog_limit_concurrency_and_record_usage() {
    use pi_ai::{
        AnyModel, ApiKeyAuth, ClassifierResult, ClassifierStopReason, CreateProviderOptions,
        ProviderAuth, ProviderClassifier, Usage, builtin_models, create_provider,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = TestDirectory::new();
    let result = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(dir.0.clone()),
        agent_dir: Some(dir.0.join("agent")),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut model = builtin_models(None)
        .get_classifier_model("typesafe", "jev-latest")
        .unwrap();
    model.provider = "review-classifier".to_owned();
    model.base_url = "https://trusted.invalid".to_owned();
    model.headers = Some(BTreeMap::from([(
        "authorization".to_owned(),
        "catalog-secret".to_owned(),
    )]));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let classifier = ProviderClassifier::new(Arc::new({
        let active = active.clone();
        let maximum = maximum.clone();
        move |model, _context, _options| {
            assert_eq!(model.base_url, "https://trusted.invalid");
            let model = model.clone();
            let active = active.clone();
            let maximum = maximum.clone();
            Box::pin(async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                ClassifierResult {
                    api: model.api,
                    provider: model.provider,
                    model: model.id,
                    answers: BTreeMap::new(),
                    usage: Some(Usage {
                        input: 10,
                        total_tokens: 10,
                        ..Default::default()
                    }),
                    stop_reason: ClassifierStopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                }
            })
        }
    }));
    result.model_runtime.models().set_provider(
        create_provider(CreateProviderOptions {
            id: model.provider.clone(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::api_key(ApiKeyAuth::keyless("test", "unused")),
            models: vec![AnyModel::Classifier(model.clone())],
            fetch_models: None,
            filter_models: None,
            api: None,
            classifiers: Some(BTreeMap::from([(model.api.clone(), classifier)])),
        })
        .unwrap(),
    );
    let runner = result.session.extension_runner();
    let tool = runner.get_tool_definition("codemode").unwrap();
    let code = r#"
        const modelsFound = await models.getAvailableOfType("classifier", "review-classifier");
        const picked = await models.getModelOfType("classifier", "review-classifier", "jev-latest");
        if (modelsFound.length !== 1 || !picked) throw new Error("Model discovery failed");
        if ("headers" in picked || "headers" in modelsFound[0]) throw new Error("Leaked model credentials");
        picked.baseUrl = "https://untrusted.invalid";
        const context = { state: {}, questions: { ok: { type: "bool", instructions: "OK?", criteria: { true: "yes", false: "no" } } } };
        const answers = await Promise.all(Array.from({length: 9}, () => models.classify(picked, context)));
        return answers.map(a => a.stopReason);
    "#;
    let outcome = (tool.execute)(
        "models-test".to_owned(),
        json!({"code":code}),
        None,
        None,
        runner.create_tool_context("models-test", None),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error, "{:?}", outcome.content);
    assert_eq!(outcome.usage.unwrap().input, 90);
    assert_eq!(maximum.load(Ordering::SeqCst), 4);
    let failed = (tool.execute)(
        "failed-model".to_owned(),
        json!({"code":r#"await models.classify({provider:"review-classifier",id:"jev-latest"}, {state:{},questions:{ok:{type:"bool",instructions:"OK?",criteria:{true:"yes",false:"no"}}}}); throw new Error("after paid call");"#}),
        None, None, runner.create_tool_context("failed-model", None),
    ).await.unwrap();
    assert!(failed.is_error);
    assert_eq!(failed.usage.unwrap().input, 10);
    result.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_codemode_execution_cancels_pending_host_calls() {
    use pi_runtime::{
        CodemodeExecuteOptions, CodemodeSandbox, CodemodeSandboxOptions, CodemodeTool,
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let wait = CodemodeTool::new("wait", {
        let entered = entered.clone();
        let cancelled = cancelled.clone();
        move |_, token| {
            let entered = entered.clone();
            let cancelled = cancelled.clone();
            async move {
                entered.notify_one();
                token.cancelled().await;
                cancelled.notify_one();
                Ok(None)
            }
        }
    });
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions {
        tools: vec![wait],
        ..Default::default()
    })
    .unwrap();
    let execution = tokio::spawn(async move {
        sandbox
            .execute("await tools.wait({})", CodemodeExecuteOptions::default())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    execution.abort();
    assert!(execution.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), cancelled.notified())
        .await
        .expect("Dropped execution left a host call running");
}

#[tokio::test]
async fn sdk_normalizes_tool_images_to_the_selected_models_limits() {
    use base64::Engine;
    use image::ImageEncoder;
    use pi_agent::{AgentMessage, AgentToolResult};
    use pi_ai::faux::{
        FauxResponseStep, create_faux_provider, faux_assistant_message, faux_text_message,
        faux_tool_call,
    };
    use pi_ai::{
        ImageContent, ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits, StopReason,
        UserContentBlock,
    };
    use pi_runtime::{CreateModelRuntimeOptions, ModelRuntime};
    use pi_session::ToolDefinition;
    let dir = TestDirectory::new();
    let faux = create_faux_provider();
    faux.set_responses([
        FauxResponseStep::Static(faux_assistant_message(
            vec![faux_tool_call("picture", Default::default(), None)],
            Some(StopReason::ToolUse),
        )),
        FauxResponseStep::Static(faux_text_message("done")),
    ]);
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(faux.provider.clone());
    let mut model = faux.model.clone();
    model.input_limits = Some(ModelInputLimits {
        images: Some(ModelImageInputLimits {
            resize: Some(ModelImageResizeOptions {
                max_width: Some(8),
                max_height: Some(8),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    });
    let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([1, 2, 3, 255]));
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(image.as_raw(), 32, 32, image::ExtendedColorType::Rgba8)
        .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let picture = ToolDefinition::new(
        "picture",
        "picture",
        "Get picture",
        json!({"type":"object"}),
        Arc::new(move |_, _, _, _, _| {
            let encoded = encoded.clone();
            Box::pin(async move {
                let mut result = AgentToolResult::text("picture");
                result.content.push(UserContentBlock::Image(ImageContent {
                    data: encoded,
                    mime_type: "image/png".to_owned(),
                }));
                Ok(result)
            })
        }),
    );
    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(dir.0.clone()),
        agent_dir: Some(dir.0.join("agent")),
        model: Some(model),
        model_runtime: Some(runtime),
        custom_tools: vec![picture],
        ..Default::default()
    })
    .await
    .unwrap();
    created
        .session
        .prompt("picture please", None)
        .await
        .unwrap();
    let images: Vec<_> = created
        .session
        .with_session_manager(|manager| manager.get_branch(None))
        .into_iter()
        .filter_map(|entry| match entry {
            pi_session::SessionEntry::Message(message) => match message.message {
                AgentMessage::ToolResult(result) => Some(result.content),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            UserContentBlock::Image(image) => Some(image),
            _ => None,
        })
        .collect();
    created.session.shutdown().await.unwrap();
    assert_eq!(images.len(), 1);
    let decoded = image::load_from_memory(
        &base64::engine::general_purpose::STANDARD
            .decode(&images[0].data)
            .unwrap(),
    )
    .unwrap();
    assert!(decoded.width() <= 8 && decoded.height() <= 8);
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    // SAFETY: Query-only process handle, checked for null, then closed exactly once.
    unsafe {
        let handle = OpenProcess(0x1000, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0;
        let queried = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        queried != 0 && code == 259
    }
}

#[cfg(windows)]
#[tokio::test]
async fn sdk_connects_configured_stdio_mcp_and_shutdown_reaps_descendants() {
    let dir = TestDirectory::new();
    let script_path = dir.0.join("mcp-server.ps1");
    std::fs::write(&script_path, r#"
        $ErrorActionPreference = 'Stop'
        while ($null -ne ($line = [Console]::ReadLine())) {
            $message = $line | ConvertFrom-Json
            if ($null -eq $message.id) { continue }
            $result = switch ($message.method) {
                'initialize' { @{ protocolVersion='2026-07-28'; capabilities=@{tools=@{}}; serverInfo=@{name='local';version='1'} } }
                'tools/list' { @{ tools=@(@{name='spawn';description='Create child process';inputSchema=@{type='object'};outputSchema=@{type='object'}}) } }
                'tools/call' {
                    $child = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120') -PassThru -WindowStyle Hidden
                    @{ content=@(); structuredContent=@{pid=$child.Id;parent=$PID} }
                }
                default { @{} }
            }
            @{jsonrpc='2.0';id=$message.id;result=$result} | ConvertTo-Json -Depth 20 -Compress | ForEach-Object { [Console]::WriteLine($_) }
        }
    "#).unwrap();
    let agent_dir = dir.0.join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("mcp.json"), serde_json::to_vec(&json!({
        "mcpServers": { "local": { "command":"pwsh.exe", "args":["-NoProfile","-NonInteractive","-File",script_path], "exposure":"direct", "timeoutMs":5000, "startupWaitMs":5000 } }
    })).unwrap()).unwrap();
    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(dir.0.clone()),
        agent_dir: Some(agent_dir),
        ..Default::default()
    })
    .await
    .unwrap();
    created
        .session
        .extension_runner()
        .emit_before_agent_start(
            pi_session::BeforeAgentStartTrigger::User {
                prompt: "test".to_owned(),
                images: None,
            },
            &pi_session::BuildSystemPromptOptions::default(),
        )
        .await;
    assert!(
        created
            .session
            .get_active_tool_names()
            .iter()
            .any(|name| name == "mcp__local__spawn")
    );
    let runner = created.session.extension_runner();
    let codemode = runner.get_tool_definition("codemode").unwrap();
    let outcome = (codemode.execute)(
        "stdio-test".to_owned(),
        json!({"code":"return (await tools.mcp__local__spawn({})).structuredContent"}),
        None,
        None,
        runner.create_tool_context("stdio-test", None),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error, "{:?}", outcome.content);
    let pi_ai::UserContentBlock::Text(text) = &outcome.content[0] else {
        panic!("Expected PID JSON")
    };
    let pids: JsonValue = serde_json::from_str(&text.text).unwrap();
    let child = u32::try_from(pids["pid"].as_u64().unwrap()).unwrap();
    let parent = u32::try_from(pids["parent"].as_u64().unwrap()).unwrap();
    assert!(process_is_running(child));
    assert!(process_is_running(parent));
    assert!(created.session.new_session(None).await.unwrap());
    let new_runner = created.session.extension_runner();
    new_runner
        .emit_before_agent_start(
            pi_session::BeforeAgentStartTrigger::User {
                prompt: "new session".to_owned(),
                images: None,
            },
            &pi_session::BuildSystemPromptOptions::default(),
        )
        .await;
    let new_codemode = new_runner.get_tool_definition("codemode").unwrap();
    let next = (new_codemode.execute)(
        "new-stdio-test".to_owned(),
        json!({"code":"return (await tools.mcp__local__spawn({})).structuredContent"}),
        None,
        None,
        new_runner.create_tool_context("new-stdio-test", None),
    )
    .await
    .unwrap();
    assert!(!next.is_error, "{:?}", next.content);
    let pi_ai::UserContentBlock::Text(next_text) = &next.content[0] else {
        panic!("Expected PID JSON")
    };
    let next_pids: JsonValue = serde_json::from_str(&next_text.text).unwrap();
    let next_child = u32::try_from(next_pids["pid"].as_u64().unwrap()).unwrap();
    let next_parent = u32::try_from(next_pids["parent"].as_u64().unwrap()).unwrap();
    assert_ne!(parent, next_parent);
    created.session.shutdown().await.unwrap();
    for pid in [child, parent, next_child, next_parent] {
        tokio::time::timeout(Duration::from_secs(2), async {
            while process_is_running(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("MCP process survived session replacement or shutdown");
    }
    assert!(
        !process_is_running(child),
        "MCP descendant escaped process containment"
    );
    assert!(
        !process_is_running(parent),
        "MCP parent survived session shutdown"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn stdio_root_exit_reaps_descendants_and_drains_its_final_message() {
    use pi_runtime::{StdioTransport, StdioTransportOptions};
    let dir = TestDirectory::new();
    let script = dir.0.join("root-exit.ps1");
    std::fs::write(&script, r#"
        $child = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120') -PassThru -WindowStyle Hidden
        [Console]::WriteLine((@{pid=$child.Id} | ConvertTo-Json -Compress))
    "#).unwrap();
    let transport = StdioTransport::new(StdioTransportOptions {
        command: "pwsh.exe".to_owned(),
        args: vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-File".to_owned(),
            script.to_string_lossy().into_owned(),
        ],
        env: BTreeMap::new(),
        cwd: Some(dir.0.clone()),
        stderr_log_path: None,
    });
    let mut rx = transport.start().await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let pid = u32::try_from(message["pid"].as_u64().unwrap()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while process_is_running(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("MCP descendant survived its root process");
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .is_none()
    );
    transport.close().await.unwrap();
}

#[test]
fn mcp_oauth_updates_are_locked_and_preserve_corrupt_files() {
    use pi_runtime::{McpOAuthCredential, McpOAuthStore};
    let dir = TestDirectory::new();
    let credential = McpOAuthCredential {
        access_token: "test".to_owned(),
        refresh_token: None,
        expires_at: None,
        client_id: None,
        client_secret: None,
        metadata: None,
    };
    std::thread::scope(|scope| {
        for index in 0..12 {
            let path = dir.0.clone();
            let credential = credential.clone();
            scope.spawn(move || {
                McpOAuthStore::new(&path)
                    .set(&format!("server-{index}"), credential)
                    .unwrap()
            });
        }
    });
    let store = McpOAuthStore::new(&dir.0);
    assert_eq!(store.try_load_all().unwrap().len(), 12);
    assert!(dir.0.join("mcp-auth.json.lock").exists());
    std::fs::write(dir.0.join("mcp-auth.json"), b"{broken").unwrap();
    assert!(store.set("another", credential).is_err());
    assert_eq!(
        std::fs::read(dir.0.join("mcp-auth.json")).unwrap(),
        b"{broken"
    );
}

async fn read_http_json(socket: &mut tokio::net::TcpStream) -> JsonValue {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let headers = String::from_utf8_lossy(&bytes);
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    bytes.resize(header_end + length, 0);
    socket.read_exact(&mut bytes[header_end..]).await.unwrap();
    serde_json::from_slice(&bytes[header_end..]).unwrap_or(JsonValue::Null)
}

#[tokio::test]
async fn mcp_http_delivers_sse_response_before_the_server_closes_the_stream() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            connections.spawn(async move {
                let msg = read_http_json(&mut socket).await;
                if msg.get("id").is_none() {
                    socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    return;
                }
                let result = if msg["method"] == "initialize" { initialize_result() } else { json!({ "ok": true }) };
                let event = format!("data: {}\r\n\r\n", json!({ "jsonrpc": "2.0", "id": msg["id"], "result": result }));
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
                // Split both JSON and CRLF across HTTP chunks. Leave the response body open.
                for part in event.as_bytes().chunks(3) {
                    socket.write_all(format!("{:x}\r\n", part.len()).as_bytes()).await.unwrap();
                    socket.write_all(part).await.unwrap();
                    socket.write_all(b"\r\n").await.unwrap();
                }
                std::future::pending::<()>().await;
            });
        }
    });
    let client = McpClient::new(McpClientOptions {
        request_timeout_ms: 100,
        ..Default::default()
    });
    let initialized = tokio::time::timeout(
        Duration::from_millis(800),
        client.connect(Arc::new(StreamableHttpTransport::new(url, BTreeMap::new()))),
    )
    .await;
    assert!(initialized.is_ok(), "SSE response was buffered until EOF");
    initialized.unwrap().unwrap();
    assert_eq!(
        client
            .request("echo", None, McpRequestOptions::default())
            .await
            .unwrap(),
        json!({ "ok": true })
    );
    client.close().await.unwrap();
    server.abort();
}

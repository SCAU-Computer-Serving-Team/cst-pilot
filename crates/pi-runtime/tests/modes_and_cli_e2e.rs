#[path = "modes_and_cli_e2e/acceptance.rs"]
mod acceptance;

use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

fn sandbox() -> PathBuf {
    let base = if cfg!(windows) {
        PathBuf::from("E:/tmp")
    } else {
        std::env::temp_dir()
    };
    let path = base
        .join("2026-11-04/argon-phase4-development/tests")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(path.join("agent")).unwrap();
    path
}

async fn cli(args: &[&str], input: &[u8]) -> Output {
    let root = sandbox();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_pi"));
    cmd.env_clear();
    for key in ["SystemRoot", "WINDIR", "PATH", "PATHEXT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env("HOME", &root)
        .env("USERPROFILE", &root)
        .current_dir(&root)
        .arg("--agent-dir")
        .arg(root.join("agent"))
        .args([
            "--no-session",
            "--no-context-files",
            "--no-skills",
            "--no-extensions",
            "--no-prompt-templates",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).await.unwrap();
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .expect("CLI did not settle after stdin EOF")
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    output
}

async fn test_runtime(tool: Option<pi_session::ToolDefinition>) -> pi_runtime::AgentSessionRuntime {
    test_runtime_with_responses(tool, None).await
}

async fn test_runtime_with_responses(
    tool: Option<pi_session::ToolDefinition>,
    responses: Option<Vec<pi_ai::faux::FauxResponseStep>>,
) -> pi_runtime::AgentSessionRuntime {
    test_runtime_with_store(tool, responses, false).await
}

async fn test_runtime_with_store(
    tool: Option<pi_session::ToolDefinition>,
    responses: Option<Vec<pi_ai::faux::FauxResponseStep>>,
    persistent: bool,
) -> pi_runtime::AgentSessionRuntime {
    use pi_runtime::*;
    use pi_session::{InlineExtension, SessionManager};
    use std::sync::Arc;
    let root = sandbox();
    let faux = pi_ai::faux::create_faux_provider();
    if tool.is_some() {
        faux.set_responses(vec![pi_ai::faux::FauxResponseStep::Static(
            pi_ai::faux::faux_assistant_message(
                vec![pi_ai::faux::faux_tool_call(
                    "wait",
                    Default::default(),
                    Some("rpc-call".into()),
                )],
                Some(pi_ai::StopReason::ToolUse),
            ),
        )]);
    }
    if let Some(responses) = responses {
        faux.set_responses(responses);
    }
    let provider = faux.provider.clone();
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: root.clone(),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(
            ModelRuntime::create(CreateModelRuntimeOptions {
                include_builtins: Some(false),
                refresh_on_create: Some(false),
                ..Default::default()
            })
            .await,
        ),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            no_skills: true,
            no_prompt_templates: true,
            no_context_files: true,
            extension_factories: vec![Arc::new(move || {
                let provider = provider.clone();
                Ok(Box::new(InlineExtension::new("<rpc-test>", move |api| {
                    api.register_provider(provider.clone());
                    Ok(())
                })))
            })],
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    let result = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(root.clone()),
        agent_dir: Some(root.join("agent")),
        model_runtime: Some(services.model_runtime.clone()),
        resource_loader: Some(services.resource_loader.clone()),
        settings_manager: Some(services.settings_manager.clone()),
        model: Some(faux.model),
        custom_tools: tool.into_iter().collect(),
        tools: Some(vec!["wait".into()]),
        session_manager: Some(if persistent {
            SessionManager::create(root.to_string_lossy(), root.join("sessions"), None).unwrap()
        } else {
            SessionManager::in_memory(root.to_string_lossy(), None, None).unwrap()
        }),
        ..Default::default()
    })
    .await
    .unwrap();
    AgentSessionRuntime::new(
        CreateAgentSessionRuntimeResult {
            session: result.session,
            services,
            extensions_result: result.extensions_result,
            diagnostics: vec![],
            model_fallback_message: None,
        },
        Arc::new(|_| Box::pin(async { Err("replacement not used by this test".into()) })),
    )
}

fn spawn_rpc(
    host: pi_runtime::AgentSessionRuntime,
) -> (
    tokio::io::DuplexStream,
    tokio::io::BufReader<tokio::io::DuplexStream>,
    tokio::task::JoinHandle<i32>,
) {
    let (input, server_input) = tokio::io::duplex(64 * 1024);
    let (server_output, output) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        pi_runtime::run_rpc_mode(
            &host,
            tokio::io::BufReader::new(server_input),
            server_output,
            tokio::io::sink(),
        )
        .await
    });
    (input, tokio::io::BufReader::new(output), task)
}

async fn response(
    reader: &mut tokio::io::BufReader<tokio::io::DuplexStream>,
    id: &str,
) -> serde_json::Value {
    use tokio::io::AsyncBufReadExt;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut line = String::new();
            assert_ne!(
                reader.read_line(&mut line).await.unwrap(),
                0,
                "EOF before response {id}"
            );
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    })
    .await
    .expect("RPC response timed out")
}

#[tokio::test]
async fn rpc_abort_wait_does_not_block_queries() {
    use std::sync::Arc;
    use tokio::sync::Notify;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let tool = pi_session::ToolDefinition::new(
        "wait",
        "Wait",
        "wait for settlement",
        serde_json::json!({"type":"object","properties":{}}),
        {
            let (entered, release) = (entered.clone(), release.clone());
            Arc::new(move |_, _, _, _, _| {
                let (entered, release) = (entered.clone(), release.clone());
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(pi_agent::AgentToolResult::text("settled"))
                })
            })
        },
    );
    let host = test_runtime(Some(tool)).await;
    let old = host.session();
    let (mut input, mut output, task) = spawn_rpc(host.clone());
    input.write_all(b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"run\",\"clientMessageId\":\"durable-input\"}\n").await.unwrap();
    assert_eq!(response(&mut output, "p").await["success"], true);
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    input
        .write_all(b"{\"id\":\"a\",\"type\":\"abort\"}\n{\"id\":\"q\",\"type\":\"get_state\"}\n")
        .await
        .unwrap();
    assert_eq!(response(&mut output, "q").await["success"], true);
    release.notify_one();
    assert_eq!(response(&mut output, "a").await["success"], true);
    drop(input);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(old.is_closed());
    assert!(host.is_disposed());
}

#[tokio::test]
async fn rpc_abort_retry_does_not_cancel_a_non_retry_tool_run() {
    use std::sync::{Arc, Mutex};
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let token = Arc::new(Mutex::new(None));
    let tool = pi_session::ToolDefinition::new(
        "wait",
        "Wait",
        "hold a normal run",
        serde_json::json!({"type":"object","properties":{}}),
        {
            let (entered, release, token) = (entered.clone(), release.clone(), token.clone());
            Arc::new(move |_, _, cancel, _, _| {
                *token.lock().unwrap() = cancel;
                let (entered, release) = (entered.clone(), release.clone());
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(pi_agent::AgentToolResult::text("settled"))
                })
            })
        },
    );
    let host = test_runtime(Some(tool)).await;
    let session = host.session();
    let (mut input, mut output, task) = spawn_rpc(host);
    input
        .write_all(b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"run\"}\n")
        .await
        .unwrap();
    assert_eq!(response(&mut output, "p").await["success"], true);
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    input
        .write_all(b"{\"id\":\"retry\",\"type\":\"abort_retry\"}\n")
        .await
        .unwrap();
    assert_eq!(response(&mut output, "retry").await["success"], true);
    assert!(!token.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert!(session.is_streaming());
    release.notify_one();
    drop(input);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn retry_can_be_cancelled_synchronously_from_the_retry_start_notification() {
    use pi_ai::{RetryPolicy, StopReason};
    use std::sync::Arc;
    let mut error = pi_ai::faux::faux_assistant_message(vec![], Some(StopReason::Error));
    error.error_message = Some("429 rate limit exceeded".into());
    let host = test_runtime_with_responses(None, Some(vec![error.into()])).await;
    let session = host.session();
    session.set_retry_policy(RetryPolicy {
        base_delay_ms: 60_000,
        ..Default::default()
    });
    let copy = session.clone();
    let unsubscribe = session.subscribe(Arc::new(move |event| {
        if matches!(event, pi_session::AgentSessionEvent::AutoRetryStart { .. }) {
            copy.abort_retry();
        }
    }));
    tokio::time::timeout(Duration::from_secs(3), session.prompt("retry", None))
        .await
        .unwrap()
        .unwrap();
    unsubscribe();
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn rpc_eof_cancels_pending_structural_before_hook() {
    use std::sync::Arc;
    let entered = Arc::new(tokio::sync::Notify::new());
    let host = test_runtime(None).await;
    let old = host.session();
    let extension = {
        let entered = entered.clone();
        pi_session::InlineExtension::new("<pending-rpc-transition>", move |api| {
            let entered = entered.clone();
            api.on_lifecycle("session_before_switch", move |_, _| {
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    std::future::pending::<()>().await;
                    Ok(pi_session::SessionBeforeEventOutput::None)
                }
            });
            Ok(())
        })
    };
    old.reload_with_extensions(vec![Box::new(extension)])
        .await
        .unwrap();
    let (mut input, _output, task) = spawn_rpc(host.clone());
    input
        .write_all(b"{\"id\":\"n\",\"type\":\"new_session\"}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    drop(input);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("EOF must cancel pending structural hook")
            .unwrap(),
        0
    );
    assert!(host.is_disposed());
    assert!(old.is_closed());
}

#[tokio::test]
async fn rpc_eof_cancels_active_tool_before_waiting_for_idle() {
    use std::sync::Arc;
    use tokio::sync::Notify;
    let entered = Arc::new(Notify::new());
    let tool = pi_session::ToolDefinition::new(
        "wait",
        "Wait",
        "wait for cancellation",
        serde_json::json!({"type":"object","properties":{}}),
        {
            let entered = entered.clone();
            Arc::new(move |_, _, cancel, _, _| {
                let entered = entered.clone();
                Box::pin(async move {
                    entered.notify_one();
                    cancel.expect("tool cancellation token").cancelled().await;
                    Ok(pi_agent::AgentToolResult::text("cancelled"))
                })
            })
        },
    );
    let host = test_runtime(Some(tool)).await;
    let old = host.session();
    let (mut input, mut output, task) = spawn_rpc(host.clone());
    input
        .write_all(b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"run\"}\n")
        .await
        .unwrap();
    assert_eq!(response(&mut output, "p").await["success"], true);
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    drop(input);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(old.is_closed());
}

#[tokio::test]
async fn rpc_output_failure_wakes_open_input_and_disposes() {
    struct BrokenWriter;
    impl tokio::io::AsyncWrite for BrokenWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    let host = test_runtime(None).await;
    let old = host.session();
    let (mut input, server_input) = tokio::io::duplex(1024);
    let run = tokio::spawn(async move {
        pi_runtime::run_rpc_mode(
            &host,
            tokio::io::BufReader::new(server_input),
            BrokenWriter,
            tokio::io::sink(),
        )
        .await
    });
    input
        .write_all(b"{\"id\":\"q\",\"type\":\"get_state\"}\n")
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .unwrap()
            .unwrap(),
        1
    );
    assert!(old.is_closed());
    drop(input);
}

#[tokio::test]
async fn rpc_rejects_bad_optional_fields_and_continues_after_malformed_frames() {
    let output = cli(&["--mode", "rpc"], concat!(
        "not json\n",
        "{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"x\",\"clientMessageId\":12}\n",
        "{\"id\":\"b\",\"type\":\"bash\",\"command\":\"unused\",\"excludeFromContext\":\"false\"}\n",
        "{\"id\":\"q\",\"type\":\"get_state\"}\n"
    ).as_bytes()).await;
    assert_eq!(output.status.code(), Some(0));
    let values: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    for id in ["p", "b"] {
        assert_eq!(
            values.iter().find(|v| v["id"] == id).unwrap()["success"],
            false
        );
    }
    assert_eq!(
        values.iter().find(|v| v["id"] == "q").unwrap()["success"],
        true
    );
}

#[tokio::test]
async fn ui_bridge_rejects_stale_duplicate_and_late_secret_responses() {
    use pi_runtime::{BoundedStdoutQueue, MAX_STDOUT_QUEUE_BYTES, RpcUiBridge, RpcUiResponseFrame};
    use pi_session::ExtensionUiContext;
    use std::sync::{Arc, atomic::AtomicBool};
    use tokio_util::sync::CancellationToken;
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let queue = BoundedStdoutQueue::new(
        tx,
        Arc::new(tokio::sync::Semaphore::new(MAX_STDOUT_QUEUE_BYTES)),
        Arc::new(AtomicBool::new(false)),
    );
    let ui = RpcUiBridge::new(queue);
    let dialog = {
        let ui = ui.clone();
        tokio::spawn(async move {
            ui.input(
                "credential",
                None,
                Some(pi_session::UiDialogOptions {
                    secret: true,
                    default_value: Some("must-not-appear".into()),
                    ..Default::default()
                }),
                CancellationToken::new(),
            )
            .await
        })
    };
    let (line, permit) = rx.recv().await.unwrap();
    drop(permit);
    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["secret"], true);
    assert!(!line.contains("must-not-appear"));
    let id = request["id"].as_str().unwrap().to_owned();
    let generation = ui.current_generation();
    assert!(!ui.handle_response(RpcUiResponseFrame {
        id: id.clone(),
        generation: Some(generation + 1),
        value: Some("secret".into()),
        confirmed: None,
        cancelled: None
    }));
    ui.bump_generation_and_clear();
    assert_eq!(dialog.await.unwrap(), None);
    for _ in 0..2 {
        assert!(!ui.handle_response(RpcUiResponseFrame {
            id: id.clone(),
            generation: Some(generation),
            value: Some("secret".into()),
            confirmed: None,
            cancelled: None
        }));
    }
    assert!(ui.get_editor_text().is_empty());
    assert!(format!("{ui:?}").contains("pending_count: 0"));
}

#[tokio::test]
async fn ui_cancellation_and_drop_remove_pending_even_under_output_backpressure() {
    use pi_runtime::{BoundedStdoutQueue, MAX_STDOUT_QUEUE_BYTES, RpcUiBridge};
    use pi_session::ExtensionUiContext;
    use std::sync::{Arc, atomic::AtomicBool};
    use tokio_util::sync::CancellationToken;
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let queue = BoundedStdoutQueue::new(
        tx,
        Arc::new(tokio::sync::Semaphore::new(MAX_STDOUT_QUEUE_BYTES)),
        Arc::new(AtomicBool::new(false)),
    );
    queue
        .send_json(&serde_json::json!({"occupied":true}))
        .await
        .unwrap();
    let ui = RpcUiBridge::new(queue);
    for drop_waiter in [false, true] {
        let cancel = CancellationToken::new();
        let task = {
            let (ui, cancel) = (ui.clone(), cancel.clone());
            tokio::spawn(async move { ui.confirm("test", "test", None, cancel).await })
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            while !format!("{ui:?}").contains("pending_count: 1") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if drop_waiter {
            task.abort();
            let _ = task.await;
        } else {
            cancel.cancel();
            assert!(
                !tokio::time::timeout(Duration::from_secs(3), task)
                    .await
                    .unwrap()
                    .unwrap()
            );
        }
        assert!(format!("{ui:?}").contains("pending_count: 0"));
    }
}

#[tokio::test]
async fn synchronous_ui_overflow_explicitly_fails_bounded_output() {
    use pi_runtime::{BoundedStdoutQueue, MAX_STDOUT_QUEUE_BYTES, RpcUiBridge};
    use pi_session::ExtensionUiContext;
    use std::sync::{Arc, atomic::AtomicBool};
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let queue = BoundedStdoutQueue::new(
        tx,
        Arc::new(tokio::sync::Semaphore::new(MAX_STDOUT_QUEUE_BYTES)),
        Arc::new(AtomicBool::new(false)),
    );
    let ui = RpcUiBridge::new(queue.clone());
    ui.set_title("first");
    ui.set_title("overflow");
    assert!(
        queue
            .send_json(&serde_json::json!({"test":true}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rpc_input_pump_dispatches_ui_response_while_work_queue_is_full() {
    use std::sync::Arc;
    use tokio::io::AsyncBufReadExt;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;
    let accepted = Arc::new(Notify::new());
    let tool = pi_session::ToolDefinition::new(
        "wait",
        "Wait",
        "UI wait",
        serde_json::json!({"type":"object","properties":{}}),
        {
            let accepted = accepted.clone();
            Arc::new(move |_, _, _, _, ctx| {
                let accepted = accepted.clone();
                Box::pin(async move {
                    if ctx
                        .ui()
                        .confirm("Approval", "continue?", None, CancellationToken::new())
                        .await
                    {
                        accepted.notify_one();
                    }
                    Ok(pi_agent::AgentToolResult::text("done"))
                })
            })
        },
    );
    let host = test_runtime(Some(tool)).await;
    let (mut input, mut output, task) = spawn_rpc(host);
    input
        .write_all(b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"run\"}\n")
        .await
        .unwrap();
    let request = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut line = String::new();
            assert_ne!(output.read_line(&mut line).await.unwrap(), 0);
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            if v["type"] == "extension_ui_request" {
                break v;
            }
        }
    })
    .await
    .unwrap();
    for n in 0..65 {
        input
            .write_all(format!("{{\"id\":\"queued-{n}\",\"type\":\"compact\"}}\n").as_bytes())
            .await
            .unwrap();
    }
    input.write_all(format!("{{\"type\":\"extension_ui_response\",\"id\":{},\"generation\":{},\"confirmed\":true}}\n", request["id"], request["generation"]).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), accepted.notified())
        .await
        .expect("UI response must bypass saturated work queue");
    drop(input);
    let drain = tokio::spawn(async move {
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut output, &mut bytes)
            .await
            .unwrap();
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    drain.await.unwrap();
}

#[tokio::test]
async fn rpc_oversized_frame_drains_and_unicode_separators_stay_inside_json() {
    let mut input = vec![b'x'; pi_runtime::MAX_RPC_FRAME_BYTES + 1];
    input.extend_from_slice(b"\r\n");
    input.extend_from_slice(
        "{\"id\":\"state\u{2028}continued\u{2029}end\",\"type\":\"get_state\"}\r\n".as_bytes(),
    );
    let output = cli(&["--mode", "rpc"], &input).await;
    assert_eq!(output.status.code(), Some(0));
    let values: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(
        values
            .iter()
            .any(|v| v["command"] == "frame" && v["success"] == false)
    );
    assert!(
        values
            .iter()
            .any(|v| v["id"] == "state\u{2028}continued\u{2029}end" && v["success"] == true)
    );
}

#[tokio::test]
async fn text_no_model_reports_error_and_closes() {
    let output = cli(&["--print", "hello"], b"").await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("model"));
}

#[tokio::test]
async fn json_no_model_is_invocation_failure_and_closes() {
    let output = cli(&["--mode", "json", "hello"], b"").await;
    assert_eq!(output.status.code(), Some(1));
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}

#[tokio::test]
async fn rpc_query_and_eof_close_without_a_model() {
    let output = cli(
        &["--mode", "rpc"],
        b"{\"id\":\"s\",\"type\":\"get_state\"}\r\n",
    )
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let response = values
        .iter()
        .find(|v| v["id"] == "s")
        .expect("state response");
    assert_eq!(response["success"], true);
    assert!(response["data"]["model"].is_null());
}

async fn print_output(
    host: pi_runtime::AgentSessionRuntime,
    mode: pi_runtime::PrintOutputMode,
) -> (i32, String, String) {
    use tokio::io::AsyncReadExt;
    let (out_writer, mut out_reader) = tokio::io::duplex(4096);
    let (err_writer, mut err_reader) = tokio::io::duplex(4096);
    let run = pi_runtime::run_print_mode(
        &host,
        pi_runtime::PrintModeOptions {
            mode,
            initial_message: Some("test output".into()),
            ..Default::default()
        },
        out_writer,
        err_writer,
    );
    let mut out = String::new();
    let mut err = String::new();
    let (code, out_result, err_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            run,
            out_reader.read_to_string(&mut out),
            err_reader.read_to_string(&mut err)
        )
    })
    .await
    .expect("print mode did not settle");
    out_result.unwrap();
    err_result.unwrap();
    assert!(host.is_disposed());
    (code, out, err)
}

#[test]
fn json_event_strips_partial_without_changing_other_fields() {
    use pi_ai::{AssistantMessageEvent as Ev, faux::*};
    let partial = faux_assistant_message(
        vec![
            faux_tool_call("lookup", Default::default(), Some("call-1".into())),
            faux_text("x".repeat(2 * 1024 * 1024)),
        ],
        None,
    );
    let events = vec![
        Ev::Start {
            partial: partial.clone(),
        },
        Ev::TextStart {
            content_index: 1,
            partial: partial.clone(),
        },
        Ev::TextDelta {
            content_index: 1,
            delta: "next".into(),
            partial: partial.clone(),
        },
        Ev::TextEnd {
            content_index: 1,
            content: "done".into(),
            partial: partial.clone(),
        },
        Ev::ThinkingStart {
            content_index: 1,
            partial: partial.clone(),
        },
        Ev::ThinkingDelta {
            content_index: 1,
            delta: "reason".into(),
            partial: partial.clone(),
        },
        Ev::ThinkingEnd {
            content_index: 1,
            content: "done".into(),
            partial: partial.clone(),
        },
        Ev::ToolCallStart {
            content_index: 0,
            partial: partial.clone(),
        },
        Ev::ToolCallDelta {
            content_index: 0,
            delta: "{}".into(),
            partial: partial.clone(),
        },
        Ev::ToolCallEnd {
            content_index: 0,
            tool_call: match &partial.content[0] {
                pi_ai::ContentBlock::ToolCall(t) => t.clone(),
                _ => unreachable!(),
            },
            partial: partial.clone(),
        },
    ];
    for event in events {
        let mut expected = serde_json::to_value(&event).unwrap();
        expected.as_object_mut().unwrap().remove("partial");
        if matches!(event, Ev::ToolCallStart { .. }) {
            expected["id"] = serde_json::json!("call-1");
            expected["toolName"] = serde_json::json!("lookup");
        }
        let value = pi_runtime::to_json_event(&pi_session::AgentSessionEvent::MessageUpdate {
            message: pi_agent::AgentMessage::Assistant(partial.clone()),
            assistant_message_event: event,
        })
        .unwrap();
        assert_eq!(value["assistantMessageEvent"], expected);
        assert_eq!(
            value["usage"],
            serde_json::to_value(&partial.usage).unwrap()
        );
        assert!(serde_json::to_string(&value).unwrap().len() < 1000);
    }
}

#[tokio::test]
async fn print_json_slow_output_preserves_deltas_end_and_settled() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let message = pi_ai::faux::faux_assistant_message(
        (0..300)
            .map(|i| pi_ai::faux::faux_text(format!("block-{i}")))
            .collect(),
        None,
    );
    let host = test_runtime_with_responses(None, Some(vec![message.into()])).await;
    let (writer, reader) = tokio::io::duplex(1024);
    let old = host.session();
    let run = tokio::spawn(async move {
        pi_runtime::run_print_mode(
            &host,
            pi_runtime::PrintModeOptions {
                mode: pi_runtime::PrintOutputMode::Json,
                initial_message: Some("run".into()),
                ..Default::default()
            },
            writer,
            tokio::io::sink(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!run.is_finished(), "slow stdout must exert backpressure");
    let mut reader = BufReader::new(reader);
    let mut deltas = Vec::new();
    let (mut ended, mut settled) = (0, 0);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["assistantMessageEvent"]["type"] == "text_delta" {
                deltas.push(
                    value["assistantMessageEvent"]["delta"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
            if value["type"] == "message_end" && value["message"]["role"] == "assistant" {
                ended += 1;
            }
            if value["type"] == "agent_settled" {
                settled += 1;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(run.await.unwrap(), 0);
    assert_eq!(
        deltas,
        (0..300).map(|i| format!("block-{i}")).collect::<Vec<_>>()
    );
    assert_eq!((ended, settled), (1, 1));
    assert!(old.is_closed());
}

#[tokio::test]
async fn print_and_json_model_stop_reasons_have_distinct_exit_policy() {
    use pi_ai::{StopReason, faux::*};
    use pi_runtime::PrintOutputMode;
    for reason in [
        StopReason::Stop,
        StopReason::Error,
        StopReason::Aborted,
        StopReason::Length,
    ] {
        for mode in [PrintOutputMode::Text, PrintOutputMode::Json] {
            let mut message =
                faux_assistant_message(vec![faux_text("visible answer")], Some(reason));
            if matches!(reason, StopReason::Error | StopReason::Aborted) {
                message.error_message = Some("scripted model stop".into());
            }
            let host = test_runtime_with_responses(None, Some(vec![message.into()])).await;
            let (code, out, err) = print_output(host, mode).await;
            if mode == PrintOutputMode::Json {
                assert_eq!(code, 0, "JSON model stop {reason:?}: {err}");
                let events: Vec<serde_json::Value> = out
                    .lines()
                    .map(|s| serde_json::from_str(s).unwrap())
                    .collect();
                assert!(events.iter().any(|v| v["type"] == "message_end"));
                assert!(events.iter().any(|v| v["type"] == "agent_settled"));
                assert!(err.is_empty(), "{err}");
            } else {
                assert_eq!(code, i32::from(reason != StopReason::Stop));
                if matches!(reason, StopReason::Stop | StopReason::Length) {
                    assert_eq!(out, "visible answer\n");
                }
                if reason == StopReason::Length {
                    assert!(err.contains("truncated"));
                }
            }
        }
    }
}

struct FailAtToolStart;
impl tokio::io::AsyncWrite for FailAtToolStart {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if String::from_utf8_lossy(bytes).contains("tool_execution_start") {
            std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        } else {
            std::task::Poll::Ready(Ok(bytes.len()))
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn print_output_failure_cancels_in_flight_tool_and_awaits_close() {
    use std::sync::Arc;
    let tool = pi_session::ToolDefinition::new(
        "wait",
        "Wait",
        "wait for cancel",
        serde_json::json!({"type":"object","properties":{}}),
        Arc::new(|_, _, cancel, _, _| {
            Box::pin(async move {
                cancel.unwrap().cancelled().await;
                Ok(pi_agent::AgentToolResult::text("cancelled"))
            })
        }),
    );
    let host = test_runtime(Some(tool)).await;
    let old = host.session();
    let code = tokio::time::timeout(
        Duration::from_secs(3),
        pi_runtime::run_print_mode(
            &host,
            pi_runtime::PrintModeOptions {
                mode: pi_runtime::PrintOutputMode::Json,
                initial_message: Some("run".into()),
                ..Default::default()
            },
            FailAtToolStart,
            tokio::io::sink(),
        ),
    )
    .await
    .expect("output failure must cancel active work");
    assert_eq!(code, 1);
    assert!(host.is_disposed());
    assert!(old.is_closed());
}

#[tokio::test]
async fn print_json_oversize_record_fails_without_hanging() {
    let message =
        pi_ai::faux::faux_text_message("x".repeat(pi_runtime::MAX_STDOUT_QUEUE_BYTES + 1));
    let host = test_runtime_with_responses(None, Some(vec![message.into()])).await;
    let code = tokio::time::timeout(
        Duration::from_secs(10),
        pi_runtime::run_print_mode(
            &host,
            pi_runtime::PrintModeOptions {
                mode: pi_runtime::PrintOutputMode::Json,
                initial_message: Some("run".into()),
                ..Default::default()
            },
            tokio::io::sink(),
            tokio::io::sink(),
        ),
    )
    .await
    .expect("oversize output must explicitly fail the connection");
    assert_eq!(code, 1);
    assert!(host.is_disposed());
}

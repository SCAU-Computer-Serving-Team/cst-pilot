use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

async fn settled(reader: &mut BufReader<tokio::io::DuplexStream>) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["type"] == "agent_settled" {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn persistent_prompt_ack_precedes_settlement_and_client_id_survives_reopen() {
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let tool = pi_session::ToolDefinition::new(
        "wait",
        "Wait",
        "hold until released",
        serde_json::json!({"type":"object","properties":{}}),
        {
            let (calls, release) = (calls.clone(), release.clone());
            Arc::new(move |_, _, _, _, _| {
                let (calls, release) = (calls.clone(), release.clone());
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    release.notified().await;
                    Ok(pi_agent::AgentToolResult::text("complete"))
                })
            })
        },
    );
    let host = test_runtime_with_store(Some(tool), None, true).await;
    let old = host.session();
    let path = old.session_file().unwrap();
    let (mut input, mut output, task) = spawn_rpc(host.clone());
    input.write_all(b"{\"id\":\"first\",\"type\":\"prompt\",\"message\":\"original\",\"clientMessageId\":\"durable-client\"}\n").await.unwrap();
    assert_eq!(response(&mut output, "first").await["success"], true);
    assert!(old.is_streaming(), "ack must not wait for the held tool");
    release.notify_one();
    settled(&mut output).await;
    drop(input);
    assert_eq!(task.await.unwrap(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // A new session instance loads durable correlation state from the execution log.
    let manager = pi_session::SessionManager::open(&path, None, None).unwrap();
    let models = Arc::new(pi_ai::Models::default());
    let faux = pi_ai::faux::register_faux_provider(&models);
    let reopened = pi_session::AgentSession::new(pi_session::AgentSessionOptions::new(
        manager.get_cwd().to_owned(),
        faux.model.clone(),
        models,
        manager,
    ))
    .unwrap();
    reopened
        .prompt(
            "duplicate text must not be appended",
            Some(pi_session::PromptOptions {
                client_message_id: Some("durable-client".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(reopened.get_session_stats().user_messages, 1);
    assert_eq!(faux.call_count(), 0);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn rpc_saturated_stdout_still_consumes_abort_without_reading_output() {
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let tool = pi_session::ToolDefinition::new(
        "wait",
        "Wait",
        "wait for abort",
        serde_json::json!({"type":"object","properties":{}}),
        {
            let (entered, cancelled) = (entered.clone(), cancelled.clone());
            Arc::new(move |_, _, cancel, _, _| {
                let (entered, cancelled) = (entered.clone(), cancelled.clone());
                Box::pin(async move {
                    entered.notify_one();
                    cancel.unwrap().cancelled().await;
                    cancelled.notify_one();
                    Ok(pi_agent::AgentToolResult::text("cancelled"))
                })
            })
        },
    );
    let host = test_runtime(Some(tool)).await;
    let (mut input, server_input) = tokio::io::duplex(64 * 1024);
    let (server_output, output) = tokio::io::duplex(256);
    let session = host.session();
    let runner = tokio::spawn(async move {
        pi_runtime::run_rpc_mode(
            &host,
            BufReader::new(server_input),
            server_output,
            tokio::io::sink(),
        )
        .await
    });
    // Drain through tool admission, then stop consuming stdout.
    let mut output = BufReader::new(output);
    input
        .write_all(b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"run\"}\n")
        .await
        .unwrap();
    response(&mut output, "p").await;
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    for i in 0..16 {
        input
            .write_all(format!("{{\"id\":\"q{i}\",\"type\":\"get_state\"}}\n").as_bytes())
            .await
            .unwrap();
    }
    input
        .write_all(b"{\"id\":\"abort\",\"type\":\"abort\"}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(4), cancelled.notified())
        .await
        .expect("input pump blocked behind stdout");
    drop(input);
    tokio::time::timeout(Duration::from_secs(6), async {
        let mut sink = tokio::io::sink();
        tokio::io::copy(&mut output, &mut sink).await.unwrap();
        assert_eq!(runner.await.unwrap(), 0);
    })
    .await
    .unwrap();
    assert!(session.is_closed());
}

#[tokio::test]
async fn rpc_slow_output_keeps_every_delta_message_end_and_settled() {
    let msg = pi_ai::faux::faux_assistant_message(
        (0..300)
            .map(|n| pi_ai::faux::faux_text(format!("delta-{n}")))
            .collect(),
        None,
    );
    let host = test_runtime_with_responses(None, Some(vec![msg.into()])).await;
    let (mut input, server_input) = tokio::io::duplex(4096);
    let (writer, reader) = tokio::io::duplex(256);
    let task = tokio::spawn(async move {
        pi_runtime::run_rpc_mode(
            &host,
            BufReader::new(server_input),
            writer,
            tokio::io::sink(),
        )
        .await
    });
    input
        .write_all(b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"run\"}\n")
        .await
        .unwrap();
    let mut output = BufReader::new(reader);
    let (mut deltas, mut ends) = (Vec::new(), 0);
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let mut line = String::new();
            assert_ne!(output.read_line(&mut line).await.unwrap(), 0);
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
                ends += 1;
            }
            if value["type"] == "agent_settled" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        deltas,
        (0..300).map(|n| format!("delta-{n}")).collect::<Vec<_>>()
    );
    assert_eq!(ends, 1);
    drop(input);
    tokio::io::copy(&mut output, &mut tokio::io::sink())
        .await
        .unwrap();
    assert_eq!(task.await.unwrap(), 0);
}

#[tokio::test]
async fn real_cli_file_inputs_piped_stdin_and_missing_files_have_defined_exits() {
    let root = sandbox();
    let text = root.join("input.txt");
    std::fs::write(&text, "local file body\n").unwrap();
    let at = format!("@{}", text.display());
    let result = cli(&["--mode", "json", &at, "question"], b"piped context\n").await;
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("No models"));
    let missing = format!("@{}", root.join("missing.txt").display());
    let result = cli(&["--print", &missing], b"").await;
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("File not found for @"));
    let result = cli(&["--mode", "rpc"], b"\n").await;
    assert_eq!(result.status.code(), Some(0));
    std::fs::remove_dir_all(root).unwrap();
}

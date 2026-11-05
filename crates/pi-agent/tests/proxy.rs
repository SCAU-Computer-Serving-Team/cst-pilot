use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use pi_agent::{
    ProxyAssistantMessageEvent, ProxySerializableStreamOptions, ProxyStreamOptions,
    default_agent_model, process_proxy_event, stream_proxy,
};
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, StopReason,
    StreamingJsonParser, ToolCall, ToolCallArgumentsStream, Usage, UserMessage, normalize_context,
};
use serde_json::{Map, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Isolate the wall-clock performance baseline from the HTTP tests' allocator and I/O work.
static PROXY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn spawn_mock_sse_server(body_chunks: Vec<String>, status_line: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut req_buf = vec![0u8; 4096];
            let _ = socket.read(&mut req_buf).await;

            let headers = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
            );
            let _ = socket.write_all(headers.as_bytes()).await;
            for chunk in body_chunks {
                let _ = socket.write_all(chunk.as_bytes()).await;
                let _ = socket.flush().await;
            }
            let _ = socket.shutdown().await;
        }
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn stream_proxy_reconstructs_events_flushes_unterminated_final_line_and_preserves_metadata() {
    let _serial = PROXY_TEST_LOCK.lock().await;
    let mut final_args = Map::new();
    final_args.insert("path".to_owned(), json!("README.md"));
    let final_tc = ToolCall {
        id: "call_proxy_1".to_owned(),
        name: "read".to_owned(),
        arguments: final_args.clone(),
        thought_signature: Some("sig_xyz".to_owned()),
        namespace: Some("fs".to_owned()),
        arguments_error: None,
    };

    let events = vec![
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::Start).unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::ThinkingStart { content_index: 0 })
                .unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "Thinking...".to_owned(),
            })
            .unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content_signature: Some("think_sig_1".to_owned()),
            })
            .unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::ToolCallStart {
                content_index: 1,
                id: "call_proxy_1".to_owned(),
                tool_name: "read".to_owned(),
            })
            .unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::ToolCallDelta {
                content_index: 1,
                delta: "{\"path\":\"READ".to_owned(),
            })
            .unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::ToolCallDelta {
                content_index: 1,
                delta: "ME.md\"}".to_owned(),
            })
            .unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::ToolCallEnd {
                content_index: 1,
                tool_call: final_tc,
            })
            .unwrap()
        ),
        // Intentionally omit trailing newline on the final `done` event!
        format!(
            "data: {}",
            serde_json::to_string(&ProxyAssistantMessageEvent::Done {
                reason: StopReason::ToolUse,
                usage: Usage::default(),
                provider_thinking_level: Some("high".to_owned()),
            })
            .unwrap()
        ),
    ];

    let proxy_url = spawn_mock_sse_server(events, "200 OK").await;
    let model = default_agent_model();
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![pi_ai::Message::User(UserMessage::new("read readme"))],
        tools: None,
    });

    let stream = stream_proxy(
        &model,
        &ctx,
        ProxyStreamOptions {
            cancel_token: None,
            auth_token: "secret-token".to_owned(),
            proxy_url,
            stream_options: ProxySerializableStreamOptions::default(),
        },
    );

    let result = stream.result().await;
    assert_eq!(result.stop_reason, StopReason::ToolUse);
    assert_eq!(result.provider_thinking_level.as_deref(), Some("high"));
    assert_eq!(result.thinking(), "Thinking...");
    let tc = result.tool_calls()[0];
    assert_eq!(tc.name, "read");
    assert_eq!(tc.arguments, final_args);
    assert_eq!(tc.thought_signature.as_deref(), Some("sig_xyz"));
    assert_eq!(tc.namespace.as_deref(), Some("fs"));
}

#[tokio::test]
async fn stream_proxy_reports_error_when_connection_closes_without_terminal_event() {
    let _serial = PROXY_TEST_LOCK.lock().await;
    let events = vec![
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::Start).unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::TextStart { content_index: 0 })
                .unwrap()
        ),
        format!(
            "data: {}\n\n",
            serde_json::to_string(&ProxyAssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "halfway".to_owned(),
            })
            .unwrap()
        ),
    ];

    let proxy_url = spawn_mock_sse_server(events, "200 OK").await;
    let model = default_agent_model();
    let ctx = normalize_context(&Context::default());

    let mut stream = stream_proxy(
        &model,
        &ctx,
        ProxyStreamOptions {
            cancel_token: None,
            auth_token: "token".to_owned(),
            proxy_url,
            stream_options: ProxySerializableStreamOptions::default(),
        },
    );

    let mut collected = Vec::new();
    while let Some(ev) = stream.next().await {
        collected.push(ev);
    }

    let final_msg = stream.result().await;
    assert_eq!(final_msg.stop_reason, StopReason::Error);
    assert_eq!(
        final_msg.error_message.as_deref(),
        Some("Connection closed by proxy server before the response completed")
    );
    assert!(matches!(
        collected.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
}

#[test]
fn streaming_tool_call_arguments_and_closed_subtree_satisfy_linear_copy_credit_bounds() {
    let _serial = PROXY_TEST_LOCK.blocking_lock();
    // 1. Scaling ratio test: N = 100 KB vs N = 400 KB in 32-byte chunks
    let run_scaling_case = |payload_bytes: usize| -> (usize, usize) {
        let payload = format!("{{\"content\":\"{}\"}}", "a".repeat(payload_bytes));
        let mut parser = StreamingJsonParser::new();
        let mut chunks = 0usize;
        for chunk in payload.as_bytes().chunks(32) {
            let s = std::str::from_utf8(chunk).unwrap();
            parser.append(s);
            let _ = parser.snapshot_ref();
            chunks += 1;
        }
        (
            parser.total_snapshot_bytes_copied(),
            8 * payload.len() + 256 * chunks,
        )
    };

    let (copied_100k, budget_100k) = run_scaling_case(100 * 1024);
    let (copied_400k, budget_400k) = run_scaling_case(400 * 1024);
    assert!(
        copied_100k <= budget_100k,
        "100KB copied {copied_100k} exceeded budget {budget_100k}"
    );
    assert!(
        copied_400k <= budget_400k,
        "400KB copied {copied_400k} exceeded budget {budget_400k}"
    );
    let growth_ratio = copied_400k as f64 / copied_100k.max(1) as f64;
    assert!(
        growth_ratio <= 4.5,
        "expected linear scaling (<= 4.5x for 4x input), got {growth_ratio:.2}x ({copied_100k} -> {copied_400k})"
    );

    // 2. Closed large subtree inside open root Map: {"large": "<500KB>", "tail": [0, 1, 2, ... 2000]}
    let large_str = "x".repeat(500 * 1024);
    let mut tail_items = String::new();
    for i in 0..2000 {
        if i > 0 {
            tail_items.push(',');
        }
        tail_items.push_str(&i.to_string());
    }
    let closed_subtree_json = format!("{{\"large\":\"{large_str}\",\"tail\":[{tail_items}]}}");
    let mut parser = StreamingJsonParser::new();
    let mut chunk_count = 0usize;
    let mut unrefreshed_borrowed_reads = 0usize;
    for chunk in closed_subtree_json.as_bytes().chunks(32) {
        let s = std::str::from_utf8(chunk).unwrap();
        parser.append(s);
        let (refreshed, snap) = parser.snapshot_ref();
        if !refreshed && snap.is_some() {
            unrefreshed_borrowed_reads += 1;
        }
        chunk_count += 1;
    }
    let max_budget = 8 * closed_subtree_json.len() + 256 * chunk_count;
    assert!(
        parser.total_snapshot_bytes_copied() <= max_budget,
        "closed subtree copied {} exceeded linear budget {}",
        parser.total_snapshot_bytes_copied(),
        max_budget
    );
    assert!(
        unrefreshed_borrowed_reads > 10_000,
        "expected most 32B chunks to borrow cached snapshot without cloning, got {unrefreshed_borrowed_reads}"
    );

    // 3. #9265 end-to-end proxy event reconstruction: 520 KB string in 32-byte chunks (>16,000 deltas)
    let started = Instant::now();
    let model = default_agent_model();
    let mut partial = AssistantMessage::new_pending(&model);
    let mut tool_arguments: BTreeMap<usize, ToolCallArgumentsStream> = BTreeMap::new();

    process_proxy_event(
        ProxyAssistantMessageEvent::Start,
        &mut partial,
        &mut tool_arguments,
    )
    .unwrap();
    process_proxy_event(
        ProxyAssistantMessageEvent::ToolCallStart {
            content_index: 0,
            id: "call_big".to_owned(),
            tool_name: "write".to_owned(),
        },
        &mut partial,
        &mut tool_arguments,
    )
    .unwrap();

    let big_content = "z".repeat(520 * 1024);
    let full_json = format!("{{\"path\":\"out.txt\",\"content\":\"{big_content}\"}}");
    let mut last_seen_len = 0usize;
    let mut delta_count = 0usize;

    for chunk in full_json.as_bytes().chunks(32) {
        let s = std::str::from_utf8(chunk).unwrap();
        delta_count += 1;
        let ev = process_proxy_event(
            ProxyAssistantMessageEvent::ToolCallDelta {
                content_index: 0,
                delta: s.to_owned(),
            },
            &mut partial,
            &mut tool_arguments,
        )
        .unwrap()
        .unwrap();

        if let AssistantMessageEvent::ToolCallDelta { partial: p, .. } = ev
            && let ContentBlock::ToolCall(tc) = &p.content[0]
            && let Some(content_str) = tc.arguments.get("content").and_then(|v| v.as_str())
        {
            assert!(
                content_str.len() >= last_seen_len,
                "partial preview must grow monotonically"
            );
            last_seen_len = content_str.len();
        }
    }

    let parser_copied = tool_arguments[&0].parser().total_snapshot_bytes_copied();
    let linear_budget = 8 * full_json.len() + 256 * delta_count;
    assert!(
        parser_copied <= linear_budget,
        "parser copied {parser_copied} > linear budget {linear_budget}"
    );
    assert_eq!(last_seen_len, 520 * 1024);

    // Ensure no raw JSON buffer field is attached to the serialized ToolCall block
    let serialized_tc = serde_json::to_value(&partial.content[0]).unwrap();
    assert!(serialized_tc.get("partialJson").is_none());
    assert!(serialized_tc.get("rawArguments").is_none());

    let elapsed = started.elapsed();
    let budget = Duration::from_secs(if cfg!(debug_assertions) { 10 } else { 1 });
    assert!(
        elapsed < budget,
        "#9265 520KB/32B proxy reconstruction took {elapsed:?}, exceeding {budget:?} budget"
    );
    eprintln!("#9265 proxy reconstruction: {elapsed:?} (budget {budget:?})");
}

use futures_util::StreamExt;
use pi_ai::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
enum Protocol {
    Responses,
    Completions,
    Anthropic,
    Google,
}
const ALL: [Protocol; 4] = [
    Protocol::Responses,
    Protocol::Completions,
    Protocol::Anthropic,
    Protocol::Google,
];
impl Protocol {
    fn spec(self) -> (&'static str, &'static str, ProviderStreams) {
        match self {
            Self::Responses => (
                "openai",
                known_api::OPENAI_RESPONSES,
                openai_responses_api(),
            ),
            Self::Completions => (
                "moonshotai-cn",
                known_api::OPENAI_COMPLETIONS,
                openai_completions_api(),
            ),
            Self::Anthropic => (
                "anthropic",
                known_api::ANTHROPIC_MESSAGES,
                anthropic_messages_api(),
            ),
            Self::Google => (
                "google",
                known_api::GOOGLE_GENERATIVE_AI,
                google_generative_ai_api(),
            ),
        }
    }
    fn signature(self) -> &'static str {
        match self {
            Self::Responses => "encrypted-proof",
            Self::Completions => "cipher-proof",
            Self::Anthropic => "anthropic-proof",
            Self::Google => "Z29vZ2xlLXByb29m",
        }
    }
}
fn frame(v: Value) -> String {
    format!("data: {v}\n\n")
}
fn fixture(p: Protocol, tools: bool) -> String {
    let mut events = Vec::new();
    match p {
        Protocol::Responses => {
            events.push(json!({"type":"response.created","response":{"id":"resp_eval"}}));
            if tools {
                events.push(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_eval","summary":[],"encrypted_content":p.signature()}}));
                events.push(
                    json!({"type":"response.reasoning_text.delta","output_index":0,"delta":"检查"}),
                );
                events.push(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_eval","summary":[],"content":[{"type":"reasoning_text","text":"检查"}],"encrypted_content":p.signature()}}));
                for i in 1..=2 {
                    events.push(json!({"type":"response.output_item.added","output_index":i,"item":{"type":"function_call","id":format!("fc_{i}"),"call_id":format!("call_{i}"),"name":"inspect","arguments":""}}));
                }
                for i in [2, 1] {
                    let args = json!({"n":i}).to_string();
                    events.push(json!({"type":"response.function_call_arguments.delta","output_index":i,"delta":args}));
                    events.push(json!({"type":"response.output_item.done","output_index":i,"item":{"type":"function_call","id":format!("fc_{i}"),"call_id":format!("call_{i}"),"name":"inspect","arguments":args}}));
                }
            } else {
                events.push(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_eval","role":"assistant","content":[]}}));
                events.push(
                    json!({"type":"response.output_text.delta","output_index":0,"delta":"完成"}),
                );
                events.push(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg_eval","role":"assistant","content":[{"type":"output_text","text":"完成"}]}}));
            }
            events.push(json!({"type":"response.completed","response":{"id":"resp_eval","status":"completed","output":[],"usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":20},"output_tokens":10,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":110}}}));
        }
        Protocol::Completions => {
            if tools {
                events.push(json!({"id":"eval","choices":[{"delta":{"reasoning_content":"检查","reasoning_details":[{"type":"reasoning.encrypted","data":p.signature()}],"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"inspect","arguments":""}},{"index":1,"id":"call_2","type":"function","function":{"name":"inspect","arguments":""}}]}}]}));
                for (i, n) in [(1, 2), (0, 1)] {
                    events.push(json!({"choices":[{"delta":{"tool_calls":[{"index":i,"function":{"arguments":json!({"n":n}).to_string()}}]}}]}));
                }
            } else {
                events.push(json!({"choices":[{"delta":{"content":"完成"}}]}));
            }
            events.push(json!({"choices":[{"delta":{},"finish_reason":if tools {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":100,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":20,"cache_write_tokens":5},"total_tokens":110}}));
        }
        Protocol::Anthropic => {
            events.push(json!({"type":"message_start","message":{"id":"msg_eval","usage":{"input_tokens":80,"cache_read_input_tokens":20,"cache_creation_input_tokens":5}}}));
            if tools {
                events.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}));
                events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"检查"}}));
                events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":p.signature()}}));
                events.push(json!({"type":"content_block_stop","index":0}));
                for i in 1..=2 {
                    events.push(json!({"type":"content_block_start","index":i,"content_block":{"type":"tool_use","id":format!("call_{i}"),"name":"inspect","input":{}}}));
                    events.push(json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":json!({"n":i}).to_string()}}));
                    events.push(json!({"type":"content_block_stop","index":i}));
                }
            } else {
                events.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
                events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"完成"}}));
                events.push(json!({"type":"content_block_stop","index":0}));
            }
            events.push(json!({"type":"message_delta","delta":{"stop_reason":if tools {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":10}}));
            events.push(json!({"type":"message_stop"}));
        }
        Protocol::Google => {
            if tools {
                events.push(json!({"responseId":"eval","candidates":[{"content":{"parts":[{"thought":true,"text":"检查","thoughtSignature":p.signature()}]}}]}));
                events.push(json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"inspect","args":{"n":1}},"thoughtSignature":p.signature()},{"functionCall":{"id":"call_2","name":"inspect","args":{"n":2}},"thoughtSignature":p.signature()}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":20,"candidatesTokenCount":8,"thoughtsTokenCount":2,"totalTokenCount":110}}));
            } else {
                events.push(json!({"candidates":[{"content":{"parts":[{"text":"完成"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":20,"candidatesTokenCount":10,"totalTokenCount":110}}));
            }
        }
    }
    let mut raw = events.into_iter().map(frame).collect::<String>();
    if matches!(p, Protocol::Completions) {
        raw.push_str("data: [DONE]\n\n");
    }
    raw
}
async fn request(socket: &mut TcpStream) -> Value {
    let mut raw = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&chunk[..n]);
        assert!(raw.len() <= 64 * 1024, "mock request exceeded its budget");
        if let Some(end) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&raw[..end]);
            let len: usize = headers
                .lines()
                .find_map(|s| {
                    s.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|v| v.parse().unwrap())
                })
                .unwrap();
            if raw.len() >= end + 4 + len {
                return serde_json::from_slice(&raw[end + 4..end + 4 + len]).unwrap();
            }
        }
    }
}
async fn server(p: Protocol) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = vec![];
        for tools in [true, false] {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(request(&mut socket).await);
            let body = fixture(p, tools);
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    (format!("http://{addr}/v1"), task)
}
fn collection(p: Protocol, base: &str) -> (Models, Model) {
    let (provider, api, streams) = p.spec();
    let id = if matches!(p, Protocol::Google) {
        "gemini-3-contract"
    } else {
        "contract-model"
    };
    let model: Model = serde_json::from_value(json!({
        "id":id, "name":"Contract Model", "type":"chat",
        "api":api, "provider":provider, "baseUrl":base,
        "reasoning":true, "input":["text","image"],
        "cost":{"input":1.0,"output":2.0,"cacheRead":0.1,"cacheWrite":1.25},
        "contextWindow":128000, "maxTokens":4096
    }))
    .unwrap();
    let provider = create_provider(CreateProviderOptions {
        id: provider.to_owned(),
        name: None,
        base_url: Some(base.to_owned()),
        headers: None,
        auth: ProviderAuth::api_key(ApiKeyAuth::keyless("test", "isolated-mock-key")),
        models: vec![AnyModel::Chat(model.clone())],
        fetch_models: None,
        filter_models: None,
        api: Some(ProviderApiConfig::Single(streams)),
        classifiers: None,
    })
    .unwrap();
    let models = create_models(None);
    models.set_provider(provider);
    (models, model)
}
fn context() -> Context {
    let mut user = UserMessage::new("检查图片并并行调用两个inspect");
    user.content = UserContent::Blocks(vec![
        UserContentBlock::text("检查"),
        UserContentBlock::Image(ImageContent {
            data: "VEVTVElNQUdF".to_owned(),
            mime_type: "image/png".to_owned(),
        }),
    ]);
    Context {
        system_prompt: Some("Run inspections.".to_owned()),
        messages: vec![Message::User(user)],
        tools: Some(vec![Tool {
            name: "inspect".to_owned(),
            description: "inspect".to_owned(),
            parameters: json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"]}),
            constrained_sampling: None,
        }]),
    }
}
fn validate_events(events: &[AssistantMessageEvent], expected_tools: usize) {
    assert!(matches!(
        events.first(),
        Some(AssistantMessageEvent::Start { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::Start { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::Done { .. }))
            .count(),
        1
    );
    let mut blocks = BTreeMap::new();
    let mut tools = 0;
    for event in events {
        let (index, kind, phase) = match event {
            AssistantMessageEvent::Error { error, .. } => panic!("unexpected error: {error:?}"),
            AssistantMessageEvent::TextStart { content_index, .. } => (*content_index, "text", 0),
            AssistantMessageEvent::TextDelta { content_index, .. } => (*content_index, "text", 1),
            AssistantMessageEvent::TextEnd { content_index, .. } => (*content_index, "text", 2),
            AssistantMessageEvent::ThinkingStart { content_index, .. } => {
                (*content_index, "thinking", 0)
            }
            AssistantMessageEvent::ThinkingDelta { content_index, .. } => {
                (*content_index, "thinking", 1)
            }
            AssistantMessageEvent::ThinkingEnd { content_index, .. } => {
                (*content_index, "thinking", 2)
            }
            AssistantMessageEvent::ToolCallStart { content_index, .. } => {
                (*content_index, "tool", 0)
            }
            AssistantMessageEvent::ToolCallDelta { content_index, .. } => {
                (*content_index, "tool", 1)
            }
            AssistantMessageEvent::ToolCallEnd { content_index, .. } => {
                tools += 1;
                (*content_index, "tool", 2)
            }
            _ => continue,
        };
        match phase {
            0 => assert!(blocks.insert(index, (kind, false)).is_none()),
            1 => assert_eq!(blocks.get(&index), Some(&(kind, false))),
            _ => assert_eq!(blocks.insert(index, (kind, true)), Some((kind, false))),
        }
    }
    assert_eq!(tools, expected_tools);
    assert!(blocks.values().all(|(_, ended)| *ended));
}
fn count_tool_results(p: Protocol, req: &Value) -> usize {
    match p {
        Protocol::Responses => req["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["type"] == "function_call_output")
            .count(),
        Protocol::Completions => req["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .count(),
        Protocol::Anthropic => req["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().into_iter().flatten())
            .filter(|b| b["type"] == "tool_result")
            .count(),
        Protocol::Google => req["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["parts"].as_array().into_iter().flatten())
            .filter(|b| b.get("functionResponse").is_some())
            .count(),
    }
}
fn wire_tool_ids(protocol: Protocol, request: &Value) -> (Vec<String>, Vec<String>) {
    let mut calls = vec![];
    let mut results = vec![];
    match protocol {
        Protocol::Responses => {
            for item in request["input"].as_array().unwrap() {
                match item["type"].as_str() {
                    Some("function_call") => {
                        calls.push(item["call_id"].as_str().unwrap().to_owned())
                    }
                    Some("function_call_output") => {
                        results.push(item["call_id"].as_str().unwrap().to_owned())
                    }
                    _ => (),
                }
            }
        }
        Protocol::Completions => {
            for message in request["messages"].as_array().unwrap() {
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    calls.push(call["id"].as_str().unwrap().to_owned());
                }
                if message["role"] == "tool" {
                    results.push(message["tool_call_id"].as_str().unwrap().to_owned());
                }
            }
        }
        Protocol::Anthropic => {
            for block in request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|m| m["content"].as_array().into_iter().flatten())
            {
                match block["type"].as_str() {
                    Some("tool_use") => calls.push(block["id"].as_str().unwrap().to_owned()),
                    Some("tool_result") => {
                        results.push(block["tool_use_id"].as_str().unwrap().to_owned())
                    }
                    _ => (),
                }
            }
        }
        Protocol::Google => {
            for part in request["contents"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|c| c["parts"].as_array().into_iter().flatten())
            {
                if let Some(call) = part.get("functionCall") {
                    calls.push(call["id"].as_str().unwrap().to_owned());
                }
                if let Some(result) = part.get("functionResponse") {
                    results.push(result["id"].as_str().unwrap().to_owned());
                }
            }
        }
    }
    calls.sort();
    results.sort();
    (calls, results)
}

async fn roundtrip(p: Protocol) {
    let (base, server) = server(p).await;
    let (models, model) = collection(p, &base);
    let mut ctx = context();
    let opts = StreamOptions {
        timeout_ms: Some(3000),
        max_retries: Some(0),
        ..Default::default()
    };
    let mut stream = models.stream(&model, &ctx, Some(&opts));
    let mut events = vec![];
    while let Some(e) = stream.next().await {
        events.push(e);
    }
    validate_events(&events, 2);
    let msg = stream.result().await;
    assert_eq!(msg.stop_reason, StopReason::ToolUse, "{p:?}");
    assert_eq!(msg.tool_calls().len(), 2);
    assert_eq!(msg.thinking(), "检查");
    assert_eq!(msg.usage.cache_read, 20);
    assert_eq!(msg.usage.output, 10);
    assert_eq!(
        msg.usage.total_tokens,
        msg.usage.input + msg.usage.cache_read + msg.usage.cache_write + msg.usage.output
    );
    let expected_cost = (msg.usage.input as f64
        + 2.0 * msg.usage.output as f64
        + 0.1 * msg.usage.cache_read as f64
        + 1.25 * msg.usage.cache_write as f64)
        / 1e6;
    assert!((msg.usage.cost.total - expected_cost).abs() < 1e-12);
    let mut values: Vec<_> = msg
        .tool_calls()
        .iter()
        .map(|t| t.arguments["n"].as_u64().unwrap())
        .collect();
    values.sort();
    assert_eq!(values, vec![1, 2]);
    let results: Vec<_> = msg
        .tool_calls()
        .iter()
        .map(|t| {
            Message::ToolResult(ToolResultMessage {
                tool_call_id: t.id.clone(),
                tool_name: t.name.clone(),
                content: vec![UserContentBlock::text(format!(
                    "result-{}",
                    t.arguments["n"]
                ))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
                duration_ms: None,
            })
        })
        .collect();
    ctx.messages.push(Message::Assistant(msg));
    ctx.messages.extend(results);
    let mut final_stream = models.stream(&model, &ctx, Some(&opts));
    let mut final_events = vec![];
    while let Some(event) = final_stream.next().await {
        final_events.push(event);
    }
    validate_events(&final_events, 0);
    let final_msg = final_stream.result().await;
    assert_eq!(
        final_msg.stop_reason,
        StopReason::Stop,
        "{p:?}: {final_msg:?}"
    );
    assert_eq!(final_msg.text(), "完成");
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    let (calls, results) = wire_tool_ids(p, &requests[1]);
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0], calls[1]);
    assert_eq!(
        calls, results,
        "{p:?} tool result IDs must match their calls"
    );
    for output in ["result-1", "result-2"] {
        assert!(
            requests[1].to_string().contains(output),
            "{p:?} missing {output}"
        );
    }
    assert!(
        requests[0].to_string().contains("VEVTVElNQUdF"),
        "{p:?} missing image"
    );
    assert_eq!(
        count_tool_results(p, &requests[1]),
        2,
        "{p:?}: {}",
        requests[1]
    );
    assert!(
        requests[1].to_string().contains(p.signature()),
        "{p:?} missing replay signature: {}",
        requests[1]
    );
}
async fn bounded_roundtrip(protocol: Protocol) {
    tokio::time::timeout(Duration::from_secs(10), roundtrip(protocol))
        .await
        .expect("protocol roundtrip must finish");
}
#[tokio::test]
async fn responses_multitool_image_signature_event_roundtrip() {
    bounded_roundtrip(Protocol::Responses).await;
}
#[tokio::test]
async fn completions_multitool_image_signature_event_roundtrip() {
    bounded_roundtrip(Protocol::Completions).await;
}
#[tokio::test]
async fn anthropic_multitool_image_signature_event_roundtrip() {
    bounded_roundtrip(Protocol::Anthropic).await;
}
#[tokio::test]
async fn google_multitool_image_signature_event_roundtrip() {
    bounded_roundtrip(Protocol::Google).await;
}

#[tokio::test]
async fn tool_results_and_serialized_history_roundtrip_through_public_api() {
    use pi_ai::providers::faux::*;
    let models = create_models(None);
    let faux = register_faux_provider(&models);
    let first = faux_assistant_message(
        vec![
            faux_tool_call(
                "inspect",
                serde_json::from_value(json!({"n":1})).unwrap(),
                Some("one".to_owned()),
            ),
            faux_tool_call(
                "inspect",
                serde_json::from_value(json!({"n":2})).unwrap(),
                Some("two".to_owned()),
            ),
        ],
        Some(StopReason::ToolUse),
    );
    faux.set_responses(vec![
        first.into(),
        FauxResponseStep::Dynamic(std::sync::Arc::new(|ctx, _, _| {
            let results: Vec<_> = ctx
                .messages()
                .iter()
                .filter_map(|m| match m {
                    Message::ToolResult(r) => Some(r),
                    _ => None,
                })
                .collect();
            assert_eq!(results.len(), 2);
            assert_eq!(results[0].tool_call_id, "one");
            assert_eq!(results[1].tool_call_id, "two");
            assert!(!results[0].is_error);
            assert!(results[1].is_error);
            faux_text_message("工具结果已处理")
        })),
    ]);
    let mut ctx = context();
    let first = tokio::time::timeout(
        Duration::from_secs(3),
        models.complete(&faux.model, &ctx, None),
    )
    .await
    .expect("faux tool call round must finish");
    assert_eq!(first.stop_reason, StopReason::ToolUse);
    let results = first
        .tool_calls()
        .iter()
        .enumerate()
        .map(|(i, t)| {
            Message::ToolResult(ToolResultMessage {
                tool_call_id: t.id.clone(),
                tool_name: t.name.clone(),
                content: vec![UserContentBlock::text(if i == 0 {
                    "ok"
                } else {
                    "controlled failure"
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: i == 1,
                timestamp: 2,
                duration_ms: None,
            })
        })
        .collect::<Vec<_>>();
    ctx.messages.push(Message::Assistant(first));
    ctx.messages.extend(results);
    let serialized = serde_json::to_value(&ctx).unwrap();
    let restored: Context = serde_json::from_value(serialized.clone()).unwrap();
    assert_eq!(ctx, restored);
    assert_eq!(serialized["messages"][1]["role"], "assistant");
    assert_eq!(serialized["messages"][2]["role"], "toolResult");
    let last = tokio::time::timeout(
        Duration::from_secs(3),
        models.complete(&faux.model, &restored, None),
    )
    .await
    .expect("faux tool result round must finish");
    assert_eq!(last.stop_reason, StopReason::Stop);
    assert_eq!(last.text(), "工具结果已处理");
    assert_eq!(faux.call_count(), 2);
}

#[test]
fn cost_tier_and_long_cache_write_boundaries() {
    let cost:ModelCost=serde_json::from_value(json!({"input":1.0,"output":2.0,"cacheRead":0.1,"cacheWrite":1.25,"tiers":[{"inputTokensAbove":100,"input":3.0,"output":6.0,"cacheRead":0.3,"cacheWrite":3.75}]})).unwrap();
    let mut usage = Usage {
        input: 70,
        output: 10,
        cache_read: 20,
        cache_write: 10,
        cache_write_1h: Some(4),
        ..Default::default()
    };
    calculate_cost(&cost, &mut usage);
    assert!((usage.cost.input - 0.000070).abs() < 1e-12);
    assert!((usage.cost.cache_write - (6.0 * 1.25 + 4.0 * 2.0) / 1e6).abs() < 1e-12);
    usage.input += 1;
    calculate_cost(&cost, &mut usage);
    assert!((usage.cost.input - 71.0 * 3.0 / 1e6).abs() < 1e-12);
    usage.cost.total = 0.42;
    usage.cost.total_from_provider = Some(true);
    calculate_cost(&cost, &mut usage);
    assert_eq!(usage.cost.total, 0.42);
}

async fn hanging_server() -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 10000\r\n\r\n").await.unwrap();
        tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    (base, rx, server)
}
#[tokio::test]
async fn all_four_timeout_during_body_read() {
    for p in ALL {
        let (base, headers_sent, server) = hanging_server().await;
        let (models, model) = collection(p, &base);
        let stream = models.stream(
            &model,
            &context(),
            Some(&StreamOptions {
                timeout_ms: Some(1000),
                max_retries: Some(0),
                ..Default::default()
            }),
        );
        tokio::time::timeout(Duration::from_secs(3), headers_sent)
            .await
            .expect("mock must send headers")
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), stream.result())
            .await
            .expect("body timeout must complete");
        assert_eq!(result.stop_reason, StopReason::Error, "{p:?}: {result:?}");
        server.abort();
        let _ = server.await;
    }
}
#[tokio::test]
async fn all_four_cancel_during_body_read() {
    for p in ALL {
        let (base, headers_sent, server) = hanging_server().await;
        let (models, model) = collection(p, &base);
        let token = CancellationToken::new();
        let stream = models.stream(
            &model,
            &context(),
            Some(&StreamOptions {
                cancel_token: Some(token.clone()),
                timeout_ms: Some(5000),
                max_retries: Some(0),
                ..Default::default()
            }),
        );
        tokio::time::timeout(Duration::from_secs(3), headers_sent)
            .await
            .expect("mock must send headers")
            .unwrap();
        token.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), stream.result())
            .await
            .expect("cancellation must complete");
        assert_eq!(result.stop_reason, StopReason::Aborted, "{p:?}: {result:?}");
        server.abort();
        let _ = server.await;
    }
}

use std::collections::BTreeMap;

use futures_util::StreamExt;
use pi_ai::api::{
    SseParser, anthropic_messages, google_generative_ai, openai_completions, openai_responses,
    typesafe_system_one,
};
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, ClassifierAnswer, ClassifierBoolCriteria,
    ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierQuestion,
    ClassifierStopReason, ContentBlock, Context, ImageContent, Message, Model, ModelCost,
    ModelInputModality, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ThinkingLevel, Tool, ToolCall, ToolChoice, ToolResultMessage,
    UserContentBlock, UserMessage, builtin_models, create_assistant_message_event_stream,
    known_api, known_classifier_api, normalize_context,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn test_model(id: &str, provider: &str, api: &str, base_url: &str) -> Model {
    Model {
        id: id.to_owned(),
        name: id.to_owned(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        base_url: base_url.to_owned(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        input_limits: None,
        cost: ModelCost {
            input: 0.3,
            output: 1.2,
            cache_read: 0.006,
            cache_write: 0.375,
            tiers: None,
        },
        headers: None,
        model_type: Some("chat".to_owned()),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 1_000_000,
        max_tokens: 4096,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

#[test]
fn sse_parser_handles_split_chunks_and_multibyte_utf8() {
    let mut parser = SseParser::new();
    let e1 = parser.feed("event: response.created\r\ndata: {\"type\":\"response.");
    assert!(e1.is_empty());

    let e2 = parser.feed("created\",\"response\":{\"id\":\"resp_1\"}}\r\n\r\ndata: [DONE]\n\n");
    assert_eq!(e2.len(), 2);
    assert_eq!(e2[0].event.as_deref(), Some("response.created"));
    assert_eq!(
        e2[0].data,
        "{\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}"
    );
    assert_eq!(e2[1].data, "[DONE]");

    // 验证多字节 UTF-8（中文）跨字节块切分不会产生 U+FFFD 乱码
    let mut byte_parser = SseParser::new();
    let raw = "data: {\"text\":\"你好，世界\"}\n\n".as_bytes();
    let split_pos = raw.iter().position(|&b| b == 0xe4).unwrap() + 1;
    assert!(byte_parser.feed_bytes(&raw[..split_pos]).is_empty());
    let events = byte_parser.feed_bytes(&raw[split_pos..]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "{\"text\":\"你好，世界\"}");
}

#[test]
fn all_four_protocols_parse_sse_streams_and_preserve_signatures() {
    // 1. OpenAI Responses (DeepSeek UUID format + encrypted_content + tool_call)
    let ds_model = test_model(
        "deepseek-flash",
        "deepseek",
        known_api::OPENAI_RESPONSES,
        "https://api.deepseek.com",
    );
    let raw_responses_sse = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_ds_1\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"uuid-r1\",\"status\":\"in_progress\",\"content\":[],\"summary\":[],\"encrypted_content\":\"enc-sig-0\"}}\n\n",
        "event: response.reasoning_text.delta\n",
        "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"delta\":\"先查C盘\"}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"uuid-r1\",\"status\":\"completed\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"先查C盘\"}],\"summary\":[],\"encrypted_content\":\"enc-sig-0\"}}\n\n",
        "event: response.output_item.added\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"uuid-fc1\",\"call_id\":\"call_00_1\",\"name\":\"get_disk\",\"arguments\":\"\"}}\n\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"drive\\\":\\\"C\\\"}\"}\n\n",
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"uuid-fc1\",\"call_id\":\"call_00_1\",\"name\":\"get_disk\",\"arguments\":\"{\\\"drive\\\":\\\"C\\\"}\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ds_1\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":40},\"output_tokens\":30,\"output_tokens_details\":{\"reasoning_tokens\":10},\"total_tokens\":130}}}\n\n"
    );
    let mut parser = SseParser::new();
    let sse_events = parser.feed(raw_responses_sse);
    let mut out1 = AssistantMessage::new_pending(&ds_model);
    let stream1 = create_assistant_message_event_stream();
    openai_responses::process_responses_sse_events(&sse_events, &mut out1, &stream1, &ds_model)
        .expect("responses sse should succeed");

    assert_eq!(out1.stop_reason, StopReason::ToolUse);
    assert_eq!(out1.thinking(), "先查C盘");
    assert_eq!(out1.usage.input, 60);
    assert_eq!(out1.usage.cache_read, 40);
    assert_eq!(out1.usage.reasoning, Some(10));
    let tc = out1.tool_calls()[0];
    assert_eq!(tc.id, "call_00_1|uuid-fc1");
    assert_eq!(tc.arguments["drive"], json!("C"));

    // 验证第二轮回传构建时保留 reasoning item 并省略非 fc_ 的 UUID function_call.id
    let ctx_round2 = normalize_context(&Context {
        system_prompt: Some("Assistant".to_owned()),
        tools: None,
        messages: vec![
            Message::User(UserMessage::new("查C盘")),
            Message::Assistant(out1),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_00_1|uuid-fc1".to_owned(),
                tool_name: "get_disk".to_owned(),
                content: vec![UserContentBlock::text("128GB free")],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
        ],
    });
    let wire_msgs = openai_responses::convert_responses_messages(&ds_model, &ctx_round2);
    assert_eq!(wire_msgs[2]["type"], json!("reasoning"));
    assert_eq!(wire_msgs[2]["encrypted_content"], json!("enc-sig-0"));
    assert_eq!(wire_msgs[3]["type"], json!("function_call"));
    assert_eq!(wire_msgs[3]["call_id"], json!("call_00_1"));
    assert!(wire_msgs[3].get("id").is_none());
    assert_eq!(wire_msgs[4]["type"], json!("function_call_output"));
    assert_eq!(wire_msgs[4]["call_id"], json!("call_00_1"));

    // 2. OpenAI Completions (含 reasoning_details 加密签名与 cache_write_tokens 计费)
    let comp_model = test_model(
        "glm-5-turbo",
        "openrouter",
        known_api::OPENAI_COMPLETIONS,
        "https://openrouter.ai/api/v1",
    );
    let raw_comp_sse = concat!(
        "data: {\"id\":\"chat_1\",\"choices\":[{\"delta\":{\"reasoning_content\":\"思考中\",\"reasoning_details\":[{\"type\":\"reasoning.encrypted\",\"id\":\"re_1\",\"data\":\"cipher_abc\"}]}}]}\n\n",
        "data: {\"id\":\"chat_1\",\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"prompt_tokens_details\":{\"cached_tokens\":20,\"cache_write_tokens\":30},\"total_tokens\":110}}\n\n",
        "data: [DONE]\n\n"
    );
    let comp_events = SseParser::new().feed(raw_comp_sse);
    let mut out2 = AssistantMessage::new_pending(&comp_model);
    let stream2 = create_assistant_message_event_stream();
    openai_completions::process_completions_sse_events(
        &comp_events,
        &mut out2,
        &stream2,
        &comp_model,
    )
    .expect("completions sse should succeed");
    assert_eq!(out2.thinking(), "思考中");
    assert_eq!(out2.text(), "你好");
    assert_eq!(out2.usage.input, 50);
    assert_eq!(out2.usage.cache_read, 20);
    assert_eq!(out2.usage.cache_write, 30);
    assert_eq!(out2.usage.total_tokens, 110);

    // 验证 Completions 回放时携带 reasoning_details
    let comp_round2 = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::User(UserMessage::new("hi")),
            Message::Assistant(out2),
        ],
    });
    let comp_wire = openai_completions::convert_completions_messages(&comp_model, &comp_round2);
    assert_eq!(
        comp_wire[1]["reasoning_details"][0]["data"],
        json!("cipher_abc")
    );

    // 3. Anthropic Messages
    let claude_model = test_model(
        "claude-sonnet-4-6",
        "anthropic",
        known_api::ANTHROPIC_MESSAGES,
        "https://api.anthropic.com",
    );
    let raw_anth_sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_a1\",\"usage\":{\"input_tokens\":50,\"cache_read_input_tokens\":10,\"cache_creation_input_tokens\":5,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Step 1\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"anth-sig-99\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":25}}\n\n"
    );
    let anth_events = SseParser::new().feed(raw_anth_sse);
    let mut out3 = AssistantMessage::new_pending(&claude_model);
    let stream3 = create_assistant_message_event_stream();
    anthropic_messages::process_anthropic_sse_events(
        &anth_events,
        &mut out3,
        &stream3,
        &claude_model,
    )
    .expect("anthropic sse should succeed");
    assert_eq!(out3.thinking(), "Step 1");
    assert_eq!(
        out3.content[0]
            .as_thinking()
            .unwrap()
            .thinking_signature
            .as_deref(),
        Some("anth-sig-99")
    );
    assert_eq!(out3.text(), "Done.");
    assert_eq!(out3.usage.cache_read, 10);
    assert_eq!(out3.usage.cache_write, 5);
    assert_eq!(out3.usage.output, 25);

    // 验证 Anthropic 空文本但有有效签名的思考块在回放时保留
    let mut empty_signed_assistant = AssistantMessage::new_pending(&claude_model);
    empty_signed_assistant.stop_reason = StopReason::Stop;
    empty_signed_assistant.content = vec![
        ContentBlock::Thinking(ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some("valid-empty-sig".to_owned()),
            redacted: None,
        }),
        ContentBlock::Text(TextContent::new("ok")),
    ];
    let anth_wire = anthropic_messages::convert_anthropic_messages(
        &claude_model,
        &normalize_context(&Context {
            system_prompt: None,
            tools: None,
            messages: vec![Message::Assistant(empty_signed_assistant)],
        }),
    );
    assert_eq!(
        anth_wire[0]["content"][0]["signature"],
        json!("valid-empty-sig")
    );

    // 4. Google Generative AI
    let gemini_model = test_model(
        "gemini-3-flash",
        "google",
        known_api::GOOGLE_GENERATIVE_AI,
        "https://generativelanguage.googleapis.com/v1beta",
    );
    let raw_google_sse = concat!(
        "data: {\"responseId\":\"g_1\",\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"Checking\",\"thoughtSignature\":\"YWJjMTIz\"}]}}]}\n\n",
        "data: {\"responseId\":\"g_1\",\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"fc_g1\",\"name\":\"ping\",\"args\":{\"ip\":\"1.1.1.1\"}},\"thoughtSignature\":\"ZGVmNDU2\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":30,\"cachedContentTokenCount\":10,\"candidatesTokenCount\":12,\"thoughtsTokenCount\":8,\"totalTokenCount\":50}}\n\n"
    );
    let google_events = SseParser::new().feed(raw_google_sse);
    let mut out4 = AssistantMessage::new_pending(&gemini_model);
    let stream4 = create_assistant_message_event_stream();
    google_generative_ai::process_google_sse_events(
        &google_events,
        &mut out4,
        &stream4,
        &gemini_model,
    )
    .expect("google sse should succeed");
    assert_eq!(out4.stop_reason, StopReason::ToolUse);
    assert_eq!(out4.thinking(), "Checking");
    assert_eq!(out4.tool_calls()[0].name, "ping");
    assert_eq!(
        out4.tool_calls()[0].thought_signature.as_deref(),
        Some("ZGVmNDU2")
    );
    assert_eq!(out4.usage.input, 20);
    assert_eq!(out4.usage.cache_read, 10);
    assert_eq!(out4.usage.output, 20);
}

#[test]
fn truncated_streams_and_error_finish_reasons_are_rejected() {
    let comp_model = test_model(
        "deepseek-flash",
        "deepseek",
        known_api::OPENAI_COMPLETIONS,
        "https://api.deepseek.com",
    );
    // 1. 缺少 finish_reason 的截断 Completions 流必须报错
    let trunc_events =
        SseParser::new().feed("data: {\"choices\":[{\"delta\":{\"content\":\"half\"}}]}\n\n");
    let mut out_trunc = AssistantMessage::new_pending(&comp_model);
    let err = openai_completions::process_completions_sse_events(
        &trunc_events,
        &mut out_trunc,
        &create_assistant_message_event_stream(),
        &comp_model,
    );
    assert!(err.is_err());
    assert_eq!(out_trunc.stop_reason, StopReason::Error);

    // 2. content_filter 停止原因必须返回 Err 并标记 StopReason::Error
    let cf_events = SseParser::new().feed(
        "data: {\"choices\":[{\"delta\":{\"content\":\"blocked\"},\"finish_reason\":\"content_filter\"}]}\n\n",
    );
    let mut out_cf = AssistantMessage::new_pending(&comp_model);
    let cf_res = openai_completions::process_completions_sse_events(
        &cf_events,
        &mut out_cf,
        &create_assistant_message_event_stream(),
        &comp_model,
    );
    assert!(cf_res.is_err());
    assert_eq!(out_cf.stop_reason, StopReason::Error);

    // 3. Google 缺少 finishReason 或返回 SAFETY 必须报错
    let g_model = test_model(
        "gemini-3-flash",
        "google",
        known_api::GOOGLE_GENERATIVE_AI,
        "https://generativelanguage.googleapis.com/v1beta",
    );
    let g_trunc = SseParser::new()
        .feed("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n");
    let mut out_g = AssistantMessage::new_pending(&g_model);
    assert!(
        google_generative_ai::process_google_sse_events(
            &g_trunc,
            &mut out_g,
            &create_assistant_message_event_stream(),
            &g_model
        )
        .is_err()
    );

    let g_safety =
        SseParser::new().feed("data: {\"candidates\":[{\"finishReason\":\"SAFETY\"}]}\n\n");
    let mut out_gs = AssistantMessage::new_pending(&g_model);
    assert!(
        google_generative_ai::process_google_sse_events(
            &g_safety,
            &mut out_gs,
            &create_assistant_message_event_stream(),
            &g_model
        )
        .is_err()
    );

    // 4. Anthropic refusal 停止原因必须报错
    let a_model = test_model(
        "claude-sonnet-4-6",
        "anthropic",
        known_api::ANTHROPIC_MESSAGES,
        "https://api.anthropic.com",
    );
    let a_refusal = SseParser::new().feed(
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"}}\n\n",
    );
    let mut out_a = AssistantMessage::new_pending(&a_model);
    assert!(
        anthropic_messages::process_anthropic_sse_events(
            &a_refusal,
            &mut out_a,
            &create_assistant_message_event_stream(),
            &a_model
        )
        .is_err()
    );
}

#[test]
fn google_tool_images_and_off_null_thinking_config_are_handled() {
    let models = builtin_models(None);
    let gemini3 = models
        .get_model("google", "gemini-3.1-pro-preview")
        .expect("gemini-3.1-pro-preview must exist");

    // 1. off: null 的 Gemini 3.1 Pro 默认请求不应发送 thinkingBudget: 0，而应发送最低支持档位 LOW
    let empty_ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![],
        tools: None,
    });
    let params = google_generative_ai::build_google_params(&gemini3, &empty_ctx, None);
    assert_eq!(
        params["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        json!("LOW")
    );
    assert!(
        params["generationConfig"]["thinkingConfig"]
            .get("thinkingBudget")
            .is_none()
    );

    // 2. 工具结果中的截图在 Gemini 3+ 中嵌入 functionResponse.parts
    let mut assistant = AssistantMessage::new_pending(&gemini3);
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content.push(ContentBlock::ToolCall(ToolCall {
        id: "call_img_1".to_owned(),
        name: "take_screenshot".to_owned(),
        arguments: serde_json::Map::new(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }));
    let img = ImageContent {
        data: "c2NyZWVuc2hvdA==".to_owned(),
        mime_type: "image/png".to_owned(),
    };
    let ctx = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::Assistant(assistant),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_img_1".to_owned(),
                tool_name: "take_screenshot".to_owned(),
                content: vec![UserContentBlock::Image(img)],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
        ],
    });
    let wire = google_generative_ai::convert_google_messages(&gemini3, &ctx);
    let serialized = serde_json::to_string(&wire).unwrap();
    assert!(serialized.contains("c2NyZWVuc2hvdA=="));
    assert_eq!(
        wire[1]["parts"][0]["functionResponse"]["parts"][0]["inlineData"]["data"],
        json!("c2NyZWVuc2hvdA==")
    );
}

#[test]
fn openai_codex_endpoint_and_payload_adaptations_work() {
    let models = builtin_models(None);
    let codex_model = models
        .get_model("openai-codex", "gpt-5.6-luna")
        .expect("openai-codex model must exist");
    assert_eq!(
        openai_responses::resolve_responses_endpoint_url(&codex_model),
        "https://chatgpt.com/backend-api/codex/responses"
    );

    let ctx = normalize_context(&Context {
        system_prompt: Some("You are a Rust expert.".to_owned()),
        tools: None,
        messages: vec![Message::User(UserMessage::new("hello"))],
    });
    let payload = openai_responses::build_responses_params(&codex_model, &ctx, None);
    assert_eq!(payload["instructions"], json!("You are a Rust expert."));
    assert_eq!(payload["parallel_tool_calls"], json!(true));
    assert_eq!(payload["text"]["verbosity"], json!("low"));
    let input_arr = payload["input"].as_array().unwrap();
    assert_eq!(input_arr.len(), 1);
    assert_eq!(input_arr[0]["role"], json!("user"));

    // Codex response.done 终止事件与 Anthropic OAuth 信封和迟到 reasoning_details 回放校验
    let done_events = SseParser::new().feed(
        "data: {\"type\":\"response.done\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":1,\"total_tokens\":5}}}\n\n",
    );
    let mut codex_out = AssistantMessage::new_pending(&codex_model);
    openai_responses::process_responses_sse_events(
        &done_events,
        &mut codex_out,
        &create_assistant_message_event_stream(),
        &codex_model,
    )
    .expect("Codex response.done should finalize cleanly");
    assert_eq!(codex_out.stop_reason, StopReason::Stop);
    assert_eq!(codex_out.usage.total_tokens, 5);

    // Late reasoning_details after content
    let mut or_model = codex_model.clone();
    or_model.provider = "openrouter".to_owned();
    or_model.api = known_api::OPENAI_COMPLETIONS.to_owned();
    let late_sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"step1\",\"id\":\"r1\"}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.encrypted\",\"data\":\"late-cipher\",\"id\":\"r2\"}]},\"finish_reason\":\"stop\"}]}\n\n"
    );
    let mut or_out = AssistantMessage::new_pending(&or_model);
    openai_completions::process_completions_sse_events(
        &SseParser::new().feed(late_sse),
        &mut or_out,
        &create_assistant_message_event_stream(),
        &or_model,
    )
    .unwrap();
    let or_wire = openai_completions::convert_completions_messages(
        &or_model,
        &normalize_context(&Context {
            system_prompt: None,
            messages: vec![Message::Assistant(or_out)],
            tools: None,
        }),
    );
    assert!(
        serde_json::to_string(&or_wire)
            .unwrap()
            .contains("late-cipher")
    );

    // Anthropic OAuth system identity & tool casing
    let anth_model = models
        .get_models(Some("anthropic"))
        .into_iter()
        .next()
        .unwrap();
    let anth_oauth_params = anthropic_messages::build_anthropic_params(
        &anth_model,
        &ctx,
        Some(&StreamOptions {
            api_key: Some("sk-ant-oat-test-token".to_owned()),
            ..StreamOptions::default()
        }),
    );
    assert_eq!(
        anth_oauth_params["system"][0]["text"],
        json!("You are Claude Code, Anthropic's official CLI for Claude.")
    );
    assert_eq!(
        anth_oauth_params["system"][1]["text"],
        json!("You are a Rust expert.")
    );
}

#[tokio::test]
async fn unicode_survives_split_http_chunks_and_body_read_honors_cancellation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"你好，世界\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".as_bytes();
        let pos = body.iter().position(|&b| b == 0xe4).unwrap() + 1;
        for chunk in [&body[..pos], &body[pos..]] {
            socket
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await
                .unwrap();
            socket.write_all(chunk).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        socket.write_all(b"0\r\n\r\n").await.unwrap();
    });

    let comp_model = test_model(
        "deepseek-flash",
        "deepseek",
        known_api::OPENAI_COMPLETIONS,
        &format!("http://{addr}/v1"),
    );
    let ctx = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![Message::User(UserMessage::new("hi"))],
    });
    let res = openai_completions::stream(
        &comp_model,
        &ctx,
        Some(&StreamOptions {
            api_key: Some("test-key".to_owned()),
            ..StreamOptions::default()
        }),
    )
    .result()
    .await;
    assert_eq!(res.stop_reason, StopReason::Stop);
    assert_eq!(res.text(), "你好，世界");

    // 错误响应体挂起时取消立即生效
    let err_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let err_addr = err_listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = err_listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 1000\r\n\r\npartial")
            .await
            .unwrap();
        let _ = tx.send(());
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    });

    let resp_model = test_model(
        "deepseek-flash",
        "deepseek",
        known_api::OPENAI_RESPONSES,
        &format!("http://{err_addr}/v1"),
    );
    let token = CancellationToken::new();
    let s = openai_responses::stream(
        &resp_model,
        &ctx,
        Some(&StreamOptions {
            api_key: Some("test-key".to_owned()),
            cancel_token: Some(token.clone()),
            ..StreamOptions::default()
        }),
    );
    rx.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    token.cancel();
    let aborted = tokio::time::timeout(std::time::Duration::from_millis(300), s.result())
        .await
        .expect("cancellation must interrupt reading error body promptly");
    assert_eq!(aborted.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn system_one_classifier_works_and_honors_body_cancellation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let body = r#"{"answers":{"route":{"type":"choice","choice":"fast","probabilities":{"fast":0.9,"deep":0.1},"confidence":0.9},"safe":{"type":"noul","noul":0.98}},"usage":{"input_tokens":40,"output_tokens":5}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
    });

    let model = ClassifierModel {
        model_type: "classifier".to_owned(),
        id: "jev-latest".to_owned(),
        name: "Jev Latest".to_owned(),
        api: known_classifier_api::TYPESAFE_SYSTEM_ONE.to_owned(),
        provider: "typesafe".to_owned(),
        base_url: format!("http://{addr}/v1"),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        input_limits: None,
        cost: ModelCost {
            input: 0.1,
            output: 0.1,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
        context_window: 32000,
        compat: None,
    };

    let mut questions = BTreeMap::new();
    questions.insert(
        "route".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Select route".to_owned(),
            criteria: BTreeMap::from([
                ("fast".to_owned(), "Quick query".to_owned()),
                ("deep".to_owned(), "Complex query".to_owned()),
            ]),
        },
    );
    questions.insert(
        "safe".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Is request safe?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "No destructive action".to_owned(),
                r#false: "Destructive action".to_owned(),
            },
        },
    );

    let ctx = ClassifierContext {
        state: serde_json::from_value(json!({"prompt": "check disk"})).unwrap(),
        questions,
        images: None,
    };

    let res = typesafe_system_one::classify(
        &model,
        &ctx,
        Some(&ClassifierOptions {
            api_key: Some("test-key".to_owned()),
            ..ClassifierOptions::default()
        }),
    )
    .await;

    assert_eq!(res.stop_reason, ClassifierStopReason::Stop);
    match res.answers.get("route").unwrap() {
        ClassifierAnswer::Choice { choice, .. } => assert_eq!(choice, "fast"),
        _ => panic!("expected choice"),
    }
    match res.answers.get("safe").unwrap() {
        ClassifierAnswer::Bool { probability } => assert!((*probability - 0.98).abs() < 1e-6),
        _ => panic!("expected bool"),
    }
    assert_eq!(res.usage.unwrap().total_tokens, 45);

    // 验证分类器在收到 200 响应头但 JSON 正文挂起时支持取消
    let hang_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hang_addr = hang_listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = hang_listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 500\r\n\r\n{\"answers\":")
            .await
            .unwrap();
        let _ = tx.send(());
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    });

    let mut hang_model = model.clone();
    hang_model.base_url = format!("http://{hang_addr}/v1");
    let token = CancellationToken::new();
    let token_clone = token.clone();
    tokio::spawn(async move {
        rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token_clone.cancel();
    });
    let aborted_res = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        typesafe_system_one::classify(
            &hang_model,
            &ctx,
            Some(&ClassifierOptions {
                api_key: Some("test-key".to_owned()),
                cancel_token: Some(token),
                ..ClassifierOptions::default()
            }),
        ),
    )
    .await
    .expect("classifier body read must abort promptly");
    assert_eq!(aborted_res.stop_reason, ClassifierStopReason::Aborted);
}

#[tokio::test]
async fn cancellation_token_aborts_inflight_sse_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            let _ = socket.write_all(header.as_bytes()).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let model = test_model(
        "deepseek-flash",
        "deepseek",
        known_api::OPENAI_RESPONSES,
        &format!("http://{addr}/v1"),
    );
    let ctx = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![Message::User(UserMessage::new("hello"))],
    });

    let token = CancellationToken::new();
    let token_clone = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        token_clone.cancel();
    });

    let stream = openai_responses::stream(
        &model,
        &ctx,
        Some(&StreamOptions {
            api_key: Some("test-key".to_owned()),
            cancel_token: Some(token),
            ..StreamOptions::default()
        }),
    );
    let result = stream.result().await;
    assert_eq!(result.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn mock_deepseek_responses_roundtrip_with_tools() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut s1, _)) = listener.accept().await {
            let mut buf = vec![0u8; 8192];
            let n = s1.read(&mut buf).await.unwrap_or(0);
            let req_str = String::from_utf8_lossy(&buf[..n]);
            assert!(req_str.starts_with("POST /v1/responses "));
            let sse1 = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"uuid-r1\",\"status\":\"in_progress\",\"content\":[],\"summary\":[],\"encrypted_content\":\"enc-sig-round1\"}}\n\n",
                "event: response.reasoning_text.delta\n",
                "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"delta\":\"先查C盘剩余空间\"}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"uuid-r1\",\"status\":\"completed\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"先查C盘剩余空间\"}],\"summary\":[],\"encrypted_content\":\"enc-sig-round1\"}}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"uuid-fc-1\",\"call_id\":\"call_ds_1\",\"name\":\"get_disk_free_gb\",\"arguments\":\"\"}}\n\n",
                "event: response.function_call_arguments.delta\n",
                "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"drive\\\":\\\"C\\\"}\"}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"uuid-fc-1\",\"call_id\":\"call_ds_1\",\"name\":\"get_disk_free_gb\",\"arguments\":\"{\\\"drive\\\":\\\"C\\\"}\"}}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":80,\"input_tokens_details\":{\"cached_tokens\":20},\"output_tokens\":25,\"output_tokens_details\":{\"reasoning_tokens\":10},\"total_tokens\":105}}}\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                sse1.len(),
                sse1
            );
            let _ = s1.write_all(resp.as_bytes()).await;
        }

        if let Ok((mut s2, _)) = listener.accept().await {
            let mut buf = vec![0u8; 16384];
            let n = s2.read(&mut buf).await.unwrap_or(0);
            let req_str = String::from_utf8_lossy(&buf[..n]);
            if let Some(body_idx) = req_str.find("\r\n\r\n") {
                let body_json: Value =
                    serde_json::from_str(&req_str[body_idx + 4..]).unwrap_or_default();
                let body_serialized = serde_json::to_string(&body_json).unwrap();
                assert!(body_serialized.contains("enc-sig-round1"));
                assert!(body_serialized.contains("function_call_output"));
            }
            let sse2 = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_2\"}}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_2\",\"role\":\"assistant\",\"status\":\"in_progress\",\"content\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"72\"}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_2\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"72\"}]}}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":120,\"output_tokens\":5,\"total_tokens\":125}}}\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                sse2.len(),
                sse2
            );
            let _ = s2.write_all(resp.as_bytes()).await;
        }
    });

    let model = test_model(
        "deepseek-flash",
        "deepseek",
        known_api::OPENAI_RESPONSES,
        &format!("http://{addr}/v1"),
    );

    let tool = Tool {
        name: "get_disk_free_gb".to_owned(),
        description: "Get free space of a Windows drive in GB.".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "drive": { "type": "string" }
            },
            "required": ["drive"],
            "additionalProperties": false
        }),
        constrained_sampling: None,
    };

    let ctx1 = normalize_context(&Context {
        system_prompt: Some("You are a diagnostic assistant.".to_owned()),
        tools: Some(vec![tool.clone()]),
        messages: vec![Message::User(UserMessage::new("查一下C盘剩余空间"))],
    });

    let mut s1 = openai_responses::stream_simple(
        &model,
        &ctx1,
        Some(&SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("mock-deepseek-key".to_owned()),
                max_tokens: Some(512),
                ..StreamOptions::default()
            },
            reasoning: Some(ThinkingLevel::Low),
            ..SimpleStreamOptions::default()
        }),
    );

    let mut saw_thinking_delta = false;
    let mut saw_toolcall_end = false;
    while let Some(ev) = s1.next().await {
        match ev {
            AssistantMessageEvent::ThinkingDelta { .. } => saw_thinking_delta = true,
            AssistantMessageEvent::ToolCallEnd { .. } => saw_toolcall_end = true,
            _ => {}
        }
    }
    let msg1 = s1.result().await;
    assert_eq!(msg1.stop_reason, StopReason::ToolUse);
    assert!(saw_thinking_delta);
    assert!(saw_toolcall_end);
    let tc = msg1.tool_calls()[0].clone();

    let ctx2 = normalize_context(&Context {
        system_prompt: Some("You are a diagnostic assistant.".to_owned()),
        tools: Some(vec![tool]),
        messages: vec![
            Message::User(UserMessage::new("查一下C盘剩余空间")),
            Message::Assistant(msg1),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: tc.id,
                tool_name: tc.name,
                content: vec![UserContentBlock::text("{\"drive\":\"C\",\"free_gb\":72}")],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
                duration_ms: None,
            }),
        ],
    });

    let msg2 = openai_responses::stream_simple(
        &model,
        &ctx2,
        Some(&SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("mock-deepseek-key".to_owned()),
                max_tokens: Some(512),
                ..StreamOptions::default()
            },
            reasoning: Some(ThinkingLevel::Low),
            ..SimpleStreamOptions::default()
        }),
    )
    .result()
    .await;

    assert_eq!(msg2.stop_reason, StopReason::Stop);
    assert!(msg2.text().contains("72"));
}

#[tokio::test]
#[ignore = "requires PI_AI_LIVE_DEEPSEEK_KEY env var"]
async fn live_deepseek_responses_roundtrip_with_tools() {
    let api_key = std::env::var("PI_AI_LIVE_DEEPSEEK_KEY")
        .expect("set PI_AI_LIVE_DEEPSEEK_KEY to run live DeepSeek test");
    let models = builtin_models(None);
    let model = models.get_model("deepseek", "deepseek-flash").unwrap();
    assert_eq!(model.api, known_api::OPENAI_RESPONSES);
    let mut ctx = Context {
        system_prompt: Some("Call get_disk_free_gb exactly once for drive C. Use the tool result to answer. After the tool result, reply only with the free_gb number.".to_owned()),
        tools: Some(vec![Tool {
            name: "get_disk_free_gb".to_owned(),
            description: "Get free disk space in GB for the requested drive.".to_owned(),
            parameters: json!({"type":"object","properties":{"drive":{"type":"string"}},"required":["drive"],"additionalProperties":false}),
            constrained_sampling: None,
        }]),
        messages: vec![Message::User(UserMessage::new("Use get_disk_free_gb to check drive C. Do not guess the result."))],
    };
    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key),
            max_tokens: Some(1024),
            timeout_ms: Some(90_000),
            max_retries: Some(0),
            ..StreamOptions::default()
        },
        reasoning: Some(ThinkingLevel::Low),
        ..SimpleStreamOptions::default()
    };
    let mut stream = models.stream_simple(&model, &ctx, Some(&options));
    let mut saw_thinking_delta = false;
    let mut saw_tool_end = false;
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::ThinkingDelta { .. } => saw_thinking_delta = true,
            AssistantMessageEvent::ToolCallEnd { .. } => saw_tool_end = true,
            _ => (),
        }
    }
    let first = stream.result().await;
    assert_eq!(first.stop_reason, StopReason::ToolUse);
    assert!(saw_thinking_delta && saw_tool_end);
    assert_eq!(first.tool_calls().len(), 1);
    assert!(first.usage.input + first.usage.cache_read > 0);
    assert!(first.usage.output > 0);
    assert!(
        first
            .content
            .iter()
            .filter_map(ContentBlock::as_thinking)
            .any(|thinking| {
                thinking
                    .thinking_signature
                    .as_ref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .and_then(|s| {
                        s.get("encrypted_content")
                            .and_then(Value::as_str)
                            .map(|s| !s.is_empty())
                    })
                    == Some(true)
            }),
        "DeepSeek must return a replayable encrypted reasoning signature"
    );
    let call = first.tool_calls()[0].clone();
    assert_eq!(call.name, "get_disk_free_gb");
    assert_eq!(call.arguments["drive"], "C");
    ctx.messages.push(Message::Assistant(first));
    ctx.messages.push(Message::ToolResult(ToolResultMessage {
        tool_call_id: call.id,
        tool_name: call.name,
        content: vec![UserContentBlock::text("{\"drive\":\"C\",\"free_gb\":72}")],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 2,
        duration_ms: None,
    }));
    let final_options = SimpleStreamOptions {
        tool_choice: Some(ToolChoice::None),
        ..options
    };
    let final_message = models
        .complete_simple(&model, &ctx, Some(&final_options))
        .await;
    assert_eq!(final_message.stop_reason, StopReason::Stop);
    assert_eq!(final_message.text().trim(), "72");
    assert!(final_message.usage.input + final_message.usage.cache_read > 0);
    assert!(final_message.usage.output > 0);
}

use std::collections::BTreeMap;

use pi_ai::{
    AnyModel, AssistantMessage, AssistantMessageEvent, ContentBlock, Context, ImageContent,
    Message, Model, ModelCost, ModelInputModality, ModelThinkingLevel, StopReason, SystemContent,
    SystemMessage, TextContent, ThinkingContent, Tool, ToolCall, ToolCallArgumentsStream,
    ToolResultMessage, UserContent, UserContentBlock, UserMessage,
    create_assistant_message_event_stream, known_api, normalize_context, parse_streaming_json,
    parse_tool_call_arguments, repair_json, resolve_transcript, split_error_tool_result_images,
    transform_messages,
};
use serde_json::json;

fn sample_model(id: &str, provider: &str, api: &str, supports_image: bool) -> Model {
    Model {
        id: id.to_owned(),
        name: id.to_owned(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        base_url: "https://api.deepseek.com".to_owned(),
        input: if supports_image {
            vec![ModelInputModality::Text, ModelInputModality::Image]
        } else {
            vec![ModelInputModality::Text]
        },
        input_limits: None,
        cost: ModelCost {
            input: 0.3,
            output: 1.2,
            cache_read: 0.006,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
        model_type: Some("chat".to_owned()),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 1_000_000,
        max_tokens: 384_000,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

#[test]
fn deserializes_upstream_catalog_json_structures() {
    let deepseek_sample = r#"{
        "openai-completions": {
            "chat:deepseek-flash": {
                "id": "deepseek-flash",
                "name": "DeepSeek V4.1 Flash",
                "api": "openai-completions",
                "baseUrl": "https://api.deepseek.com",
                "provider": "deepseek",
                "reasoning": true,
                "thinkingLevelMap": {"minimal": null, "low": "low", "medium": null, "high": "high", "max": "max"},
                "input": ["text", "image"],
                "cost": {"input": 0.3, "output": 1.2, "cacheRead": 0.006, "cacheWrite": 0},
                "contextWindow": 1000000,
                "maxTokens": 384000,
                "compat": {
                    "supportsStore": false,
                    "supportsDeveloperRole": false,
                    "maxTokensField": "max_tokens",
                    "requiresReasoningContentOnAssistantMessages": true,
                    "thinkingFormat": "deepseek",
                    "supportsStrictMode": true
                },
                "inputLimits": {
                    "images": {
                        "resize": {"maxWidth": 2000, "maxHeight": 2000, "maxBytes": 4718592, "jpegQuality": 80}
                    }
                },
                "type": "chat"
            }
        },
        "typesafe-system-one": {
            "classifier:jev-latest": {
                "type": "classifier",
                "id": "jev-latest",
                "name": "Jev",
                "api": "typesafe-system-one",
                "provider": "typesafe",
                "baseUrl": "https://api.typesafe.ai/v1/",
                "input": ["text", "image"],
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                "contextWindow": 64000,
                "compat": {"imageInput": "assets", "sendImageOptInHeader": true}
            }
        }
    }"#;

    let parsed: BTreeMap<String, BTreeMap<String, AnyModel>> =
        serde_json::from_str(deepseek_sample).expect("should parse upstream catalog json");

    let flash = parsed["openai-completions"]["chat:deepseek-flash"]
        .as_chat()
        .expect("should be chat model");
    assert_eq!(flash.id, "deepseek-flash");
    assert!(flash.supports_image_input());
    let level_map = flash.thinking_level_map.as_ref().unwrap();
    assert_eq!(level_map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(
        level_map.get(&ModelThinkingLevel::High),
        Some(&Some("high".to_owned()))
    );
    assert_eq!(level_map.get(&ModelThinkingLevel::Off), None);

    let jev = parsed["typesafe-system-one"]["classifier:jev-latest"]
        .as_classifier()
        .expect("should be classifier model");
    assert_eq!(jev.id, "jev-latest");
    assert_eq!(
        jev.compat.as_ref().unwrap().image_input.as_deref(),
        Some("assets")
    );
}

#[test]
fn streaming_json_and_repair_handle_partial_and_malformed_chunks() {
    let malformed = "{\"cmd\": \"C:\\Windows\\System32\nping\"}";
    let repaired = repair_json(malformed);
    let parsed = parse_tool_call_arguments(Some(&repaired));
    assert!(parsed.arguments_error.is_none());
    assert_eq!(
        parsed.arguments.get("cmd").and_then(|v| v.as_str()),
        Some("C:\\Windows\\System32\nping")
    );

    let mut stream = ToolCallArgumentsStream::new();
    let p1 = stream.append("{\"drive\": \"C");
    assert_eq!(p1.get("drive").and_then(|v| v.as_str()), Some("C"));

    let p2 = stream.append(":\", \"deep\": [1, 2, {\"ok\": tru");
    assert_eq!(p2.get("drive").and_then(|v| v.as_str()), Some("C:"));
    assert_eq!(p2["deep"][2]["ok"], json!(true));

    let p3 = stream.append("e}]}");
    assert_eq!(p3["deep"][2]["ok"], json!(true));

    let done = stream.finish();
    assert!(done.arguments_error.is_none());
    assert_eq!(done.arguments["drive"], json!("C:"));

    // 针对不合法的最终文本应返回空对象与详细错误
    let bad = parse_tool_call_arguments(Some("[1, 2, 3]"));
    assert!(bad.arguments.is_empty());
    assert!(
        bad.arguments_error
            .unwrap()
            .contains("Tool arguments must be a JSON object")
    );

    let partial_direct = parse_streaming_json(Some("{\"path\": \"/var/log"));
    assert_eq!(partial_direct["path"], json!("/var/log"));
}

#[test]
fn transform_messages_handles_cross_model_orphans_and_images() {
    let source_model = sample_model(
        "claude-sonnet-4-5",
        "anthropic",
        known_api::ANTHROPIC_MESSAGES,
        true,
    );
    let target_model = sample_model(
        "deepseek-v4-pro",
        "deepseek",
        known_api::OPENAI_RESPONSES,
        false,
    );

    let mut assistant1 = AssistantMessage::new_pending(&source_model);
    assistant1.stop_reason = StopReason::ToolUse;
    assistant1.content = vec![
        ContentBlock::Thinking(ThinkingContent {
            thinking: "let me check disk".to_owned(),
            thinking_signature: Some("sig_anthropic_123".to_owned()),
            redacted: None,
        }),
        ContentBlock::Text(TextContent {
            text: "Checking...".to_owned(),
            text_signature: Some("txt_sig_1".to_owned()),
        }),
        ContentBlock::ToolCall(ToolCall {
            id: "toolu_01|extra".to_owned(),
            name: "check_disk".to_owned(),
            arguments: serde_json::from_value(json!({"drive": "C"})).unwrap(),
            thought_signature: Some("google_thought_sig".to_owned()),
            namespace: None,
            arguments_error: None,
        }),
        ContentBlock::ToolCall(ToolCall {
            id: "toolu_02_orphan".to_owned(),
            name: "check_mem".to_owned(),
            arguments: serde_json::Map::new(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }),
    ];

    let mut errored_assistant = AssistantMessage::new_pending(&source_model);
    errored_assistant.stop_reason = StopReason::Error;
    errored_assistant.error_message = Some("network reset".to_owned());

    let messages = vec![
        Message::User(UserMessage {
            content: UserContent::Blocks(vec![
                UserContentBlock::text("请看截图"),
                UserContentBlock::Image(ImageContent {
                    data: "aGVsbG8=".to_owned(),
                    mime_type: "image/png".to_owned(),
                }),
            ]),
            timestamp: 100,
        }),
        Message::Assistant(assistant1),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "toolu_01|extra".to_owned(),
            tool_name: "check_disk".to_owned(),
            content: vec![UserContentBlock::text("free: 120GB")],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 101,
            duration_ms: None,
        }),
        // 下一个 User turn 打断了 toolu_02_orphan，且后面跟着一条报错的 AssistantMessage
        Message::User(UserMessage::new("继续")),
        Message::Assistant(errored_assistant),
    ];

    let cleaned = transform_messages(
        &messages,
        &target_model,
        Some(|id: &str, _m: &Model, _a: &AssistantMessage| id.replace('|', "_")),
    );

    // 1. User 消息图片应被降级为文本占位符
    let Message::User(u0) = &cleaned[0] else {
        panic!("expected user message");
    };
    let UserContent::Blocks(b0) = &u0.content else {
        panic!("expected blocks");
    };
    assert_eq!(b0.len(), 2);
    assert_eq!(
        b0[1],
        UserContentBlock::text("(image omitted: model does not support images)")
    );

    // 2. 异构模型的 Thinking 被剥离，Text 的 text_signature 被剥离，ToolCall ID 被规范化
    let Message::Assistant(a1) = &cleaned[1] else {
        panic!("expected assistant message");
    };
    assert_eq!(a1.content.len(), 3); // 1 Text + 2 ToolCall
    assert_eq!(a1.content[0].as_text().unwrap().text_signature, None);
    let tc1 = a1.content[1].as_tool_call().unwrap();
    assert_eq!(tc1.id, "toolu_01_extra");
    assert_eq!(tc1.thought_signature, None);

    // 3. 已提供的 ToolResult ID 同步更新为规范化后的 ID
    let Message::ToolResult(tr1) = &cleaned[2] else {
        panic!("expected first tool result");
    };
    assert_eq!(tr1.tool_call_id, "toolu_01_extra");
    assert!(!tr1.is_error);

    // 4. 孤儿 ToolCall (toolu_02_orphan) 在新 User turn 前自动补齐合成错误结果
    let Message::ToolResult(tr2) = &cleaned[3] else {
        panic!("expected synthetic tool result");
    };
    assert_eq!(tr2.tool_call_id, "toolu_02_orphan");
    assert!(tr2.is_error);
    assert_eq!(
        tr2.content,
        vec![UserContentBlock::text("No result provided")]
    );

    // 5. 后续 User turn 保留，而 StopReason::Error 的 AssistantMessage 被跳过
    assert_eq!(cleaned.len(), 5);
    assert!(matches!(cleaned[4], Message::User(_)));
}

#[test]
fn transcript_normalization_and_error_image_split_work() {
    let ctx = Context {
        system_prompt: Some("Base prompt".to_owned()),
        tools: Some(vec![Tool {
            name: "read".to_owned(),
            description: "Read file".to_owned(),
            parameters: json!({"type": "object"}),
            constrained_sampling: None,
        }]),
        messages: vec![
            Message::System(SystemMessage {
                content: SystemContent::Text("Later rule".to_owned()),
                sections: Some(BTreeMap::from([(
                    "env".to_owned(),
                    Some("Windows".to_owned()),
                )])),
                tools_added: None,
                tools_removed: None,
                timestamp: 10,
            }),
            Message::User(UserMessage::new("hi")),
        ],
    };

    let norm = normalize_context(&ctx);
    assert_eq!(norm.messages().len(), 3);

    let collapsed = resolve_transcript(&norm, false);
    assert_eq!(collapsed.messages().len(), 2);
    let Message::System(head) = &collapsed.messages()[0] else {
        panic!("expected leading system message");
    };
    assert_eq!(
        head.content.as_plain_text(),
        "Base prompt

Later rule"
    );

    // 测试错误工具结果中的图片拆分
    let err_tr = ToolResultMessage {
        tool_call_id: "call_1".to_owned(),
        tool_name: "screenshot".to_owned(),
        content: vec![
            UserContentBlock::text("Failed after capture"),
            UserContentBlock::Image(ImageContent {
                data: "abc".to_owned(),
                mime_type: "image/png".to_owned(),
            }),
        ],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: true,
        timestamp: 1,
        duration_ms: None,
    };
    let split = split_error_tool_result_images(&err_tr).expect("should split error images");
    assert_eq!(split.images.len(), 1);
    assert_eq!(split.content.len(), 2);
    assert_eq!(
        split.content[1].text,
        "(image attached after the tool results)"
    );
}

#[tokio::test]
async fn event_stream_delivers_events_and_final_result() {
    let model = sample_model(
        "deepseek-flash",
        "deepseek",
        known_api::OPENAI_RESPONSES,
        true,
    );
    let stream = create_assistant_message_event_stream();
    let writer = stream.clone();

    tokio::spawn(async move {
        let mut msg = AssistantMessage::new_pending(&model);
        writer.push(AssistantMessageEvent::Start {
            partial: msg.clone(),
        });
        msg.content.push(ContentBlock::Text(TextContent::new("2")));
        msg.stop_reason = StopReason::Stop;
        writer.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: msg,
        });
    });

    let final_msg = stream.result().await;
    assert_eq!(final_msg.stop_reason, StopReason::Stop);
    assert_eq!(final_msg.text(), "2");
    assert!(final_msg.duration_ms.is_some());
}

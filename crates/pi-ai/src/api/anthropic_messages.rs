use std::collections::BTreeMap;

use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use crate::api::{
    SseEvent, SseParser, adjust_max_tokens_for_thinking, build_base_options, build_header_map,
    calculate_cost, clamp_max_tokens_to_context, clamp_thinking_level,
    inject_copilot_dynamic_headers, invoke_on_payload, invoke_on_provider_stream_event,
    invoke_on_response, require_client_api_key, resolve_cache_retention, send_with_retry,
};
use crate::streaming_json::ToolCallArgumentsStream;
use crate::transform_messages::{
    collapse_system_messages, get_current_tools, get_initial_system_message,
    get_system_message_text, split_error_tool_result_images, transform_messages,
    without_initial_system_message,
};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention,
    ContentBlock, Message, Model, ModelThinkingLevel, ProviderCompat, ProviderHeaders,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, Tool, ToolCall,
    ToolChoice, TranscriptContext, UserContent, UserContentBlock,
    create_assistant_message_event_stream,
};

const CLAUDE_CODE_VERSION: &str = "2.1.280";

const CLAUDE_CODE_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

pub fn to_claude_code_name(name: &str) -> String {
    for &canonical in CLAUDE_CODE_TOOLS {
        if canonical.eq_ignore_ascii_case(name) {
            return canonical.to_owned();
        }
    }
    name.to_owned()
}

pub fn from_claude_code_name(name: &str, tools: &[Tool]) -> String {
    for tool in tools {
        if tool.name.eq_ignore_ascii_case(name) {
            return tool.name.clone();
        }
    }
    name.to_owned()
}

fn is_anthropic_oauth_token(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|k| k.contains("sk-ant-oat"))
}

fn normalize_anthropic_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if sanitized.is_empty() {
        "toolu_0".to_owned()
    } else {
        sanitized
    }
}

fn cache_control_value(model: &Model, retention: CacheRetention) -> Option<Value> {
    if retention == CacheRetention::None {
        return None;
    }
    let supports_long = model
        .compat
        .as_ref()
        .and_then(|c| c.supports_long_cache_retention)
        .unwrap_or(true);
    if retention == CacheRetention::Long && supports_long {
        Some(json!({ "type": "ephemeral", "ttl": "1h" }))
    } else {
        Some(json!({ "type": "ephemeral" }))
    }
}

pub fn convert_anthropic_tools(tools: &[Tool], cache_control: Option<&Value>) -> Vec<Value> {
    convert_anthropic_tools_with_oauth(tools, cache_control, false)
}

pub fn convert_anthropic_tools_with_oauth(
    tools: &[Tool],
    cache_control: Option<&Value>,
    is_oauth: bool,
) -> Vec<Value> {
    let len = tools.len();
    tools
        .iter()
        .enumerate()
        .map(|(idx, t)| {
            let mut obj = Map::new();
            let tool_name = if is_oauth {
                to_claude_code_name(&t.name)
            } else {
                t.name.clone()
            };
            obj.insert("name".to_owned(), Value::String(tool_name));
            obj.insert(
                "description".to_owned(),
                Value::String(t.description.clone()),
            );
            obj.insert("input_schema".to_owned(), t.parameters.clone());
            if idx + 1 == len
                && let Some(cc) = cache_control
            {
                obj.insert("cache_control".to_owned(), cc.clone());
            }
            Value::Object(obj)
        })
        .collect()
}

pub fn convert_anthropic_messages(model: &Model, context: &TranscriptContext) -> Vec<Value> {
    convert_anthropic_messages_with_oauth(model, context, false)
}

pub fn convert_anthropic_messages_with_oauth(
    model: &Model,
    context: &TranscriptContext,
    is_oauth: bool,
) -> Vec<Value> {
    let collapsed = collapse_system_messages(context);
    let conversation = without_initial_system_message(collapsed.messages());
    let transformed = transform_messages(
        &conversation,
        model,
        Some(|id: &str, _m: &Model, _a: &AssistantMessage| normalize_anthropic_tool_call_id(id)),
    );
    let allow_empty_sig = model
        .compat
        .as_ref()
        .and_then(|c| c.allow_empty_signature)
        .unwrap_or(false);

    let mut messages: Vec<Value> = Vec::new();
    let mut i = 0usize;

    while i < transformed.len() {
        match &transformed[i] {
            Message::System(_) => {}
            Message::User(user) => match &user.content {
                UserContent::Text(t) => {
                    if !t.trim().is_empty() {
                        messages.push(json!({
                            "role": "user",
                            "content": [{ "type": "text", "text": t }],
                        }));
                    }
                }
                UserContent::Blocks(blocks) => {
                    let parts: Vec<Value> = blocks
                        .iter()
                        .filter_map(|b| match b {
                            UserContentBlock::Text(t) if !t.text.trim().is_empty() => Some(json!({
                                "type": "text",
                                "text": t.text,
                            })),
                            UserContentBlock::Image(img) => Some(json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": img.mime_type,
                                    "data": img.data,
                                }
                            })),
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        messages.push(json!({
                            "role": "user",
                            "content": parts,
                        }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                if let Some(sig) = &thinking.thinking_signature {
                                    blocks.push(json!({
                                        "type": "redacted_thinking",
                                        "data": sig,
                                    }));
                                }
                                continue;
                            }
                            let sig = thinking.thinking_signature.as_deref().unwrap_or("");
                            if thinking.thinking.trim().is_empty() && sig.trim().is_empty() {
                                continue;
                            }
                            if !sig.is_empty() || allow_empty_sig {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": thinking.thinking,
                                    "signature": sig,
                                }));
                            } else {
                                blocks.push(json!({
                                    "type": "text",
                                    "text": thinking.thinking,
                                }));
                            }
                        }
                        ContentBlock::Text(text) => {
                            if !text.text.trim().is_empty() {
                                blocks.push(json!({
                                    "type": "text",
                                    "text": text.text,
                                }));
                            }
                        }
                        ContentBlock::ToolCall(tc) => {
                            let wire_name = if is_oauth {
                                to_claude_code_name(&tc.name)
                            } else {
                                tc.name.clone()
                            };
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": normalize_anthropic_tool_call_id(&tc.id),
                                "name": wire_name,
                                "input": tc.arguments,
                            }));
                        }
                    }
                }
                if !blocks.is_empty() {
                    messages.push(json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
            }
            Message::ToolResult(_) => {
                let mut user_blocks: Vec<Value> = Vec::new();
                let mut trailing_error_images: Vec<Value> = Vec::new();
                let mut j = i;
                while j < transformed.len() {
                    let Message::ToolResult(tr) = &transformed[j] else {
                        break;
                    };
                    if let Some(split) = split_error_tool_result_images(tr) {
                        let content_blocks: Vec<Value> = split
                            .content
                            .iter()
                            .map(|t| json!({ "type": "text", "text": t.text }))
                            .collect();
                        user_blocks.push(json!({
                            "type": "tool_result",
                            "tool_use_id": normalize_anthropic_tool_call_id(&tr.tool_call_id),
                            "content": content_blocks,
                            "is_error": true,
                        }));
                        trailing_error_images.push(json!({
                            "type": "text",
                            "text": split.caption,
                        }));
                        for img in split.images {
                            trailing_error_images.push(json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": img.mime_type,
                                    "data": img.data,
                                }
                            }));
                        }
                    } else {
                        let content_blocks: Vec<Value> = tr
                            .content
                            .iter()
                            .map(|b| match b {
                                UserContentBlock::Text(t) => json!({
                                    "type": "text",
                                    "text": t.text,
                                }),
                                UserContentBlock::Image(img) => json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": img.mime_type,
                                        "data": img.data,
                                    }
                                }),
                            })
                            .collect();
                        user_blocks.push(json!({
                            "type": "tool_result",
                            "tool_use_id": normalize_anthropic_tool_call_id(&tr.tool_call_id),
                            "content": content_blocks,
                            "is_error": tr.is_error,
                        }));
                    }
                    j += 1;
                }
                i = j - 1;
                user_blocks.extend(trailing_error_images);
                if !user_blocks.is_empty() {
                    messages.push(json!({
                        "role": "user",
                        "content": user_blocks,
                    }));
                }
            }
        }
        i += 1;
    }

    messages
}

pub fn build_anthropic_params(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&StreamOptions>,
) -> Value {
    let is_oauth = is_anthropic_oauth_token(options.and_then(|o| o.api_key.as_deref()));
    let compat: ProviderCompat = model.compat.clone().unwrap_or_default();
    let retention = resolve_cache_retention(
        options.and_then(|o| o.cache_retention),
        options.and_then(|o| o.env.as_ref()),
    );
    let cache_ctrl = cache_control_value(model, retention);

    let collapsed = collapse_system_messages(context);
    let initial_sys = get_initial_system_message(collapsed.messages());
    let initial_sys_text = initial_sys.map(get_system_message_text).unwrap_or_default();
    let messages = convert_anthropic_messages_with_oauth(model, &collapsed, is_oauth);

    let mut max_tokens = options
        .and_then(|o| o.max_tokens)
        .unwrap_or(model.max_tokens);
    let req_effort = options.and_then(|o| o.reasoning_effort);
    let force_adaptive = compat.force_adaptive_thinking.unwrap_or(false);

    let mut thinking_param: Option<Value> = None;
    let mut output_config_param: Option<Value> = None;

    if model.reasoning {
        if let Some(level) = req_effort {
            if force_adaptive {
                thinking_param = Some(json!({
                    "type": "adaptive",
                    "display": "summarized",
                }));
                let model_level = ModelThinkingLevel::from(level);
                let effort_str = model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|m| m.get(&model_level))
                    .and_then(|v| v.clone())
                    .unwrap_or_else(|| match level {
                        crate::types::ThinkingLevel::Minimal | crate::types::ThinkingLevel::Low => {
                            "low".to_owned()
                        }
                        crate::types::ThinkingLevel::Medium => "medium".to_owned(),
                        _ => "high".to_owned(),
                    });
                output_config_param = Some(json!({ "effort": effort_str }));
            } else {
                let (adj_max, budget) = adjust_max_tokens_for_thinking(
                    options.and_then(|o| o.max_tokens),
                    model.max_tokens,
                    level,
                    options.and_then(|o| o.thinking_budgets.as_ref()),
                );
                max_tokens = clamp_max_tokens_to_context(model, context, adj_max);
                let clamped_budget = budget.min(max_tokens.saturating_sub(1024));
                if clamped_budget >= 1024 {
                    thinking_param = Some(json!({
                        "type": "enabled",
                        "budget_tokens": clamped_budget,
                    }));
                }
            }
        } else if model
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&ModelThinkingLevel::Off))
            != Some(&None)
        {
            thinking_param = Some(json!({ "type": "disabled" }));
        }
    }

    let mut params = Map::new();
    params.insert("model".to_owned(), Value::String(model.id.clone()));
    params.insert("messages".to_owned(), Value::Array(messages));
    params.insert("max_tokens".to_owned(), Value::from(max_tokens));
    params.insert("stream".to_owned(), Value::Bool(true));

    if is_oauth {
        let mut sys_blocks = Vec::new();
        let mut cc_block = Map::new();
        cc_block.insert("type".to_owned(), Value::String("text".to_owned()));
        cc_block.insert(
            "text".to_owned(),
            Value::String("You are Claude Code, Anthropic's official CLI for Claude.".to_owned()),
        );
        if let Some(cc) = &cache_ctrl {
            cc_block.insert("cache_control".to_owned(), cc.clone());
        }
        sys_blocks.push(Value::Object(cc_block));

        if !initial_sys_text.is_empty() {
            let mut user_sys = Map::new();
            user_sys.insert("type".to_owned(), Value::String("text".to_owned()));
            user_sys.insert("text".to_owned(), Value::String(initial_sys_text));
            if let Some(cc) = &cache_ctrl {
                user_sys.insert("cache_control".to_owned(), cc.clone());
            }
            sys_blocks.push(Value::Object(user_sys));
        }
        params.insert("system".to_owned(), Value::Array(sys_blocks));
    } else if !initial_sys_text.is_empty() {
        let mut sys_block = Map::new();
        sys_block.insert("type".to_owned(), Value::String("text".to_owned()));
        sys_block.insert("text".to_owned(), Value::String(initial_sys_text));
        if let Some(cc) = &cache_ctrl {
            sys_block.insert("cache_control".to_owned(), cc.clone());
        }
        params.insert(
            "system".to_owned(),
            Value::Array(vec![Value::Object(sys_block)]),
        );
    }

    if let Some(temp) = options.and_then(|o| o.temperature)
        && req_effort.is_none()
        && compat.supports_temperature.unwrap_or(true)
    {
        params.insert("temperature".to_owned(), json!(temp));
    }

    let tools = get_current_tools(collapsed.messages());
    if !tools.is_empty() {
        let tool_cc = if compat.supports_cache_control_on_tools.unwrap_or(true) {
            cache_ctrl.as_ref()
        } else {
            None
        };
        params.insert(
            "tools".to_owned(),
            Value::Array(convert_anthropic_tools_with_oauth(
                &tools, tool_cc, is_oauth,
            )),
        );
    }

    if let Some(tc) = options.and_then(|o| o.tool_choice) {
        let tc_val = match tc {
            ToolChoice::Auto => json!({ "type": "auto" }),
            ToolChoice::None => json!({ "type": "none" }),
        };
        params.insert("tool_choice".to_owned(), tc_val);
    }

    if let Some(tp) = thinking_param {
        params.insert("thinking".to_owned(), tp);
    }
    if let Some(oc) = output_config_param {
        params.insert("output_config".to_owned(), oc);
    }

    Value::Object(params)
}

// =============================================================================
// Anthropic Stream Processing
// =============================================================================

#[allow(clippy::large_enum_variant)]
enum AnthropicSlot {
    Text {
        content_index: usize,
    },
    Thinking {
        content_index: usize,
    },
    ToolUse {
        content_index: usize,
        args_stream: ToolCallArgumentsStream,
    },
}

pub struct AnthropicStreamProcessor {
    slots: BTreeMap<u64, AnthropicSlot>,
    is_oauth: bool,
    declared_tools: Vec<Tool>,
}

impl Default for AnthropicStreamProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicStreamProcessor {
    pub fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            is_oauth: false,
            declared_tools: Vec::new(),
        }
    }

    pub fn with_oauth_tools(is_oauth: bool, declared_tools: Vec<Tool>) -> Self {
        Self {
            slots: BTreeMap::new(),
            is_oauth,
            declared_tools,
        }
    }

    pub fn handle_sse(
        &mut self,
        sse: &SseEvent,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
        model: &Model,
    ) -> Result<(), String> {
        let data = sse.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }
        let event: Value =
            serde_json::from_str(data).map_err(|e| format!("Invalid Anthropic SSE JSON: {e}"))?;
        let event_type = event
            .get("type")
            .and_then(|v| v.as_str())
            .or(sse.event.as_deref())
            .unwrap_or("");

        match event_type {
            "message_start" => {
                if let Some(msg) = event.get("message") {
                    if let Some(id) = msg.get("id").and_then(|v| v.as_str()) {
                        output.response_id = Some(id.to_owned());
                    }
                    if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                        output.response_model = Some(m.to_owned());
                    }
                    if let Some(usage) = msg.get("usage") {
                        self.apply_usage(usage, output, model);
                    }
                }
            }
            "content_block_start" => {
                let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(cb) = event.get("content_block") {
                    let cb_type = cb.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match cb_type {
                        "text" => {
                            output
                                .content
                                .push(ContentBlock::Text(TextContent::new("")));
                            let c_idx = output.content.len() - 1;
                            self.slots.insert(
                                index,
                                AnthropicSlot::Text {
                                    content_index: c_idx,
                                },
                            );
                            stream.push(AssistantMessageEvent::TextStart {
                                content_index: c_idx,
                                partial: output.clone(),
                            });
                        }
                        "thinking" => {
                            output
                                .content
                                .push(ContentBlock::Thinking(ThinkingContent::new("")));
                            let c_idx = output.content.len() - 1;
                            self.slots.insert(
                                index,
                                AnthropicSlot::Thinking {
                                    content_index: c_idx,
                                },
                            );
                            stream.push(AssistantMessageEvent::ThinkingStart {
                                content_index: c_idx,
                                partial: output.clone(),
                            });
                        }
                        "redacted_thinking" => {
                            let data_sig = cb
                                .get("data")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned();
                            output.content.push(ContentBlock::Thinking(ThinkingContent {
                                thinking: String::new(),
                                thinking_signature: Some(data_sig),
                                redacted: Some(true),
                            }));
                            let c_idx = output.content.len() - 1;
                            self.slots.insert(
                                index,
                                AnthropicSlot::Thinking {
                                    content_index: c_idx,
                                },
                            );
                            stream.push(AssistantMessageEvent::ThinkingStart {
                                content_index: c_idx,
                                partial: output.clone(),
                            });
                        }
                        "tool_use" => {
                            let id = cb.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let raw_name = cb.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let name = if self.is_oauth {
                                from_claude_code_name(raw_name, &self.declared_tools)
                            } else {
                                raw_name.to_owned()
                            };
                            output.content.push(ContentBlock::ToolCall(ToolCall {
                                id: id.to_owned(),
                                name,
                                arguments: Map::new(),
                                thought_signature: None,
                                namespace: None,
                                arguments_error: None,
                            }));
                            let c_idx = output.content.len() - 1;
                            self.slots.insert(
                                index,
                                AnthropicSlot::ToolUse {
                                    content_index: c_idx,
                                    args_stream: ToolCallArgumentsStream::new(),
                                },
                            );
                            stream.push(AssistantMessageEvent::ToolCallStart {
                                content_index: c_idx,
                                partial: output.clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(delta) = event.get("delta") {
                    let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match (delta_type, self.slots.get_mut(&index)) {
                        ("text_delta", Some(AnthropicSlot::Text { content_index })) => {
                            let text_piece =
                                delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let c_idx = *content_index;
                            if let Some(ContentBlock::Text(t)) = output.content.get_mut(c_idx) {
                                t.text.push_str(text_piece);
                            }
                            stream.push(AssistantMessageEvent::TextDelta {
                                content_index: c_idx,
                                delta: text_piece.to_owned(),
                                partial: output.clone(),
                            });
                        }
                        ("thinking_delta", Some(AnthropicSlot::Thinking { content_index })) => {
                            let think_piece =
                                delta.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                            let c_idx = *content_index;
                            if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(c_idx) {
                                t.thinking.push_str(think_piece);
                            }
                            stream.push(AssistantMessageEvent::ThinkingDelta {
                                content_index: c_idx,
                                delta: think_piece.to_owned(),
                                partial: output.clone(),
                            });
                        }
                        ("signature_delta", Some(AnthropicSlot::Thinking { content_index })) => {
                            let sig_piece = delta
                                .get("signature")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let c_idx = *content_index;
                            if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(c_idx) {
                                t.thinking_signature
                                    .get_or_insert_with(String::new)
                                    .push_str(sig_piece);
                            }
                        }
                        (
                            "input_json_delta",
                            Some(AnthropicSlot::ToolUse {
                                content_index,
                                args_stream,
                            }),
                        ) => {
                            let json_piece = delta
                                .get("partial_json")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let c_idx = *content_index;
                            let (refreshed, partial_args) =
                                args_stream.append_with_change(json_piece);
                            if refreshed
                                && let Some(ContentBlock::ToolCall(tc)) =
                                    output.content.get_mut(c_idx)
                            {
                                tc.arguments = partial_args.clone();
                            }
                            stream.push(AssistantMessageEvent::ToolCallDelta {
                                content_index: c_idx,
                                delta: json_piece.to_owned(),
                                partial: output.clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(slot) = self.slots.remove(&index) {
                    match slot {
                        AnthropicSlot::Text { content_index } => {
                            let content = output
                                .content
                                .get(content_index)
                                .and_then(|b| b.as_text())
                                .map(|t| t.text.clone())
                                .unwrap_or_default();
                            stream.push(AssistantMessageEvent::TextEnd {
                                content_index,
                                content,
                                partial: output.clone(),
                            });
                        }
                        AnthropicSlot::Thinking { content_index } => {
                            let content = output
                                .content
                                .get(content_index)
                                .and_then(|b| b.as_thinking())
                                .map(|t| t.thinking.clone())
                                .unwrap_or_default();
                            stream.push(AssistantMessageEvent::ThinkingEnd {
                                content_index,
                                content,
                                partial: output.clone(),
                            });
                        }
                        AnthropicSlot::ToolUse {
                            content_index,
                            args_stream,
                        } => {
                            let parsed = args_stream.finish();
                            let mut finished_tc = None;
                            if let Some(ContentBlock::ToolCall(tc)) =
                                output.content.get_mut(content_index)
                            {
                                parsed.apply_to(tc);
                                finished_tc = Some(tc.clone());
                            }
                            if let Some(tc) = finished_tc {
                                stream.push(AssistantMessageEvent::ToolCallEnd {
                                    content_index,
                                    tool_call: tc,
                                    partial: output.clone(),
                                });
                            }
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = event.get("delta")
                    && let Some(sr) = delta.get("stop_reason").and_then(|v| v.as_str())
                {
                    output.raw_stop_reason = Some(sr.to_owned());
                    match sr {
                        "end_turn" | "stop_sequence" | "pause_turn" => {
                            output.stop_reason = StopReason::Stop;
                        }
                        "max_tokens" | "model_context_window_exceeded" => {
                            output.stop_reason = StopReason::Length;
                        }
                        "tool_use" => {
                            output.stop_reason = StopReason::ToolUse;
                        }
                        "refusal" => {
                            output.stop_reason = StopReason::Error;
                            output.error_message =
                                Some("Anthropic stop_reason: refusal".to_owned());
                        }
                        other => {
                            output.stop_reason = StopReason::Error;
                            output.error_message = Some(format!("Anthropic stop_reason: {other}"));
                        }
                    }
                }
                if let Some(usage) = event.get("usage") {
                    self.apply_usage(usage, output, model);
                }
            }
            "error" => {
                let msg = event
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Anthropic stream error");
                return Err(msg.to_owned());
            }
            _ => {}
        }

        Ok(())
    }

    fn apply_usage(&self, usage: &Value, output: &mut AssistantMessage, model: &Model) {
        if let Some(in_tok) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
            output.usage.input = in_tok;
        }
        if let Some(out_tok) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
            output.usage.output = out_tok;
        }
        if let Some(cr) = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
        {
            output.usage.cache_read = cr;
        }
        if let Some(cw) = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
        {
            output.usage.cache_write = cw;
        }
        if let Some(cw1h) = usage
            .get("cache_creation")
            .and_then(|c| c.get("ephemeral_1h_input_tokens"))
            .and_then(|v| v.as_u64())
        {
            output.usage.cache_write_1h = Some(cw1h);
        }
        if let Some(think_tok) = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("thinking_tokens"))
            .and_then(|v| v.as_u64())
        {
            output.usage.reasoning = Some(think_tok);
        }
        output.usage.total_tokens = output.usage.input
            + output.usage.output
            + output.usage.cache_read
            + output.usage.cache_write;
        calculate_cost(&model.cost, &mut output.usage);
    }
}

pub fn process_anthropic_sse_events(
    events: &[SseEvent],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
) -> Result<(), String> {
    let mut processor = AnthropicStreamProcessor::new();
    for sse in events {
        processor.handle_sse(sse, output, stream, model)?;
    }
    if output.stop_reason == StopReason::Pending {
        output.stop_reason = StopReason::Error;
        output.error_message = Some("Anthropic stream ended without a stop reason".to_owned());
        return Err("Anthropic stream ended without a stop reason".to_owned());
    }
    if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
        return Err(output
            .error_message
            .clone()
            .unwrap_or_else(|| "Anthropic stream returned an error stop reason".to_owned()));
    }
    Ok(())
}

fn anthropic_endpoint_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    }
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&StreamOptions>,
) -> AssistantMessageEventStream {
    let event_stream = create_assistant_message_event_stream();
    let writer = event_stream.clone();
    let model = model.clone();
    let context = context.clone();
    let options = options.cloned();

    tokio::spawn(async move {
        let mut output = AssistantMessage::new_pending(&model);
        let cancel_token = options.as_ref().and_then(|o| o.cancel_token.clone());

        let run: Result<(), String> = async {
            let api_key = require_client_api_key(
                &model.provider,
                options.as_ref().and_then(|o| o.api_key.as_deref()),
                options.as_ref().and_then(|o| o.headers.as_ref()),
            )?;

            let is_oauth = is_anthropic_oauth_token(Some(api_key.as_str()));
            let is_bearer = is_oauth || model.provider == "github-copilot";
            let bearer_val = format!("Bearer {api_key}");
            let claude_cli_ua = format!("claude-cli/{CLAUDE_CODE_VERSION}");

            let mut default_headers = vec![
                ("anthropic-version", "2023-06-01"),
                ("content-type", "application/json"),
                ("accept", "text/event-stream"),
                ("anthropic-dangerous-direct-browser-access", "true"),
            ];
            if is_bearer {
                default_headers.push(("authorization", &bearer_val));
            } else {
                default_headers.push(("x-api-key", &api_key));
            }
            if is_oauth {
                default_headers.push(("user-agent", &claude_cli_ua));
                default_headers.push(("x-app", "cli"));
                default_headers.push(("anthropic-beta", "claude-code-20250219,oauth-2025-04-20"));
            }

            let mut opt_headers: ProviderHeaders = options
                .as_ref()
                .and_then(|o| o.headers.clone())
                .unwrap_or_default();
            inject_copilot_dynamic_headers(&model, context.messages(), &mut opt_headers);

            let headers = build_header_map(
                options.as_ref().and_then(|o| o.identity.as_ref()),
                &default_headers,
                model.headers.as_ref(),
                Some(&opt_headers),
            );

            let headers = super::invoke_on_headers(options.as_ref(), headers, &model).await?;

            let mut effective_options = options.unwrap_or_default();
            if effective_options.api_key.is_none() {
                effective_options.api_key = Some(api_key);
            }
            let raw_payload = build_anthropic_params(&model, &context, Some(&effective_options));
            let payload = invoke_on_payload(Some(&effective_options), raw_payload, &model).await?;
            let url = anthropic_endpoint_url(&model.base_url);
            let client = reqwest::Client::new();

            let resp = send_with_retry(
                &client,
                &url,
                headers,
                &payload,
                effective_options.timeout_ms,
                effective_options.max_retries.unwrap_or(0),
                effective_options.max_retry_delay_ms,
                cancel_token.as_ref(),
            )
            .await?;

            invoke_on_response(Some(&effective_options), &resp, &model).await?;

            writer.push(AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            let declared_tools = get_current_tools(context.messages());
            let mut byte_stream = resp.bytes_stream();
            let mut sse_parser = SseParser::new();
            let mut processor =
                AnthropicStreamProcessor::with_oauth_tools(is_oauth, declared_tools);

            loop {
                let next_chunk = if let Some(token) = &cancel_token {
                    tokio::select! {
                        _ = token.cancelled() => return Err("Request was aborted".to_owned()),
                        chunk = byte_stream.next() => chunk,
                    }
                } else {
                    byte_stream.next().await
                };

                let Some(chunk_res) = next_chunk else {
                    break;
                };
                let bytes = chunk_res.map_err(|e| format!("Stream read error: {e}"))?;
                for sse in sse_parser.feed_bytes(&bytes) {
                    invoke_on_provider_stream_event(Some(&effective_options), &sse, &model).await?;
                    processor.handle_sse(&sse, &mut output, &writer, &model)?;
                }
            }

            for sse in sse_parser.finish() {
                invoke_on_provider_stream_event(Some(&effective_options), &sse, &model).await?;
                processor.handle_sse(&sse, &mut output, &writer, &model)?;
            }

            if output.stop_reason == StopReason::Pending {
                return Err("Anthropic stream ended without a stop reason".to_owned());
            }
            if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
                return Err(output.error_message.clone().unwrap_or_else(|| {
                    "Anthropic stream returned an error stop reason".to_owned()
                }));
            }

            Ok(())
        }
        .await;

        match run {
            Ok(()) => {
                writer.push(AssistantMessageEvent::Done {
                    reason: output.stop_reason,
                    message: output,
                });
                writer.end(None);
            }
            Err(err) => {
                let is_aborted = cancel_token.is_some_and(|t| t.is_cancelled());
                output.stop_reason = if is_aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                output.error_message = Some(err);
                writer.push(AssistantMessageEvent::Error {
                    reason: output.stop_reason,
                    error: output,
                });
                writer.end(None);
            }
        }
    });

    event_stream
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let mut base = build_base_options(model, context, options);
    if let Some(reasoning) = options.and_then(|o| o.reasoning) {
        let clamped = clamp_thinking_level(model, ModelThinkingLevel::from(reasoning));
        base.reasoning_effort = match clamped {
            ModelThinkingLevel::Off => None,
            ModelThinkingLevel::Minimal => Some(crate::types::ThinkingLevel::Minimal),
            ModelThinkingLevel::Low => Some(crate::types::ThinkingLevel::Low),
            ModelThinkingLevel::Medium => Some(crate::types::ThinkingLevel::Medium),
            ModelThinkingLevel::High => Some(crate::types::ThinkingLevel::High),
            ModelThinkingLevel::XHigh => Some(crate::types::ThinkingLevel::XHigh),
            ModelThinkingLevel::Max => Some(crate::types::ThinkingLevel::Max),
        };
    }
    stream(model, context, Some(&base))
}

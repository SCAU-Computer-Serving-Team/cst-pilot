use std::collections::BTreeMap;

use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use crate::api::{
    SseEvent, SseParser, build_base_options, build_header_map, calculate_cost,
    clamp_openai_prompt_cache_key, clamp_thinking_level, inject_copilot_dynamic_headers,
    invoke_on_payload, invoke_on_provider_stream_event, invoke_on_response, require_client_api_key,
    resolve_cache_retention, resolve_sampling_params, send_with_retry, short_hash,
};
use crate::streaming_json::ToolCallArgumentsStream;
use crate::transform_messages::{
    TranscriptToolSupport, get_system_message_text, render_system_message_update,
    resolve_transcript, resolve_transcript_tools, transform_messages,
};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention,
    ContentBlock, Message, Model, ModelThinkingLevel, ProviderCompat, ProviderHeaders,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, Tool, ToolCall,
    ToolChoice, TranscriptContext, Usage, UsageCost, UserContent, UserContentBlock,
    create_assistant_message_event_stream,
};

fn normalize_completions_tool_call_id(id: &str, model: &Model) -> String {
    if id.contains('|') {
        let (call_id, item_id) = id.split_once('|').unwrap_or((id, ""));
        let combined = if item_id.is_empty() {
            call_id.to_owned()
        } else {
            format!("{call_id}_{item_id}")
        };
        if combined.len() <= 40 {
            return combined;
        }
        let hash = &short_hash(id)[..8];
        let max_prefix = 40usize.saturating_sub(hash.len() + 1).max(1);
        let prefix: String = call_id.chars().take(max_prefix).collect();
        return format!("{prefix}_{hash}");
    }
    if model.provider == "openai" && id.len() > 40 {
        return id.chars().take(40).collect();
    }
    id.to_owned()
}

pub fn convert_completions_tools(tools: &[Tool], supports_strict_mode: bool) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let mut fn_obj = Map::new();
            fn_obj.insert("name".to_owned(), Value::String(t.name.clone()));
            fn_obj.insert(
                "description".to_owned(),
                Value::String(t.description.clone()),
            );
            fn_obj.insert("parameters".to_owned(), t.parameters.clone());
            if supports_strict_mode {
                fn_obj.insert("strict".to_owned(), Value::Bool(false));
            }
            json!({
                "type": "function",
                "function": Value::Object(fn_obj),
            })
        })
        .collect()
}

pub fn convert_completions_messages(model: &Model, context: &TranscriptContext) -> Vec<Value> {
    let compat: ProviderCompat = model.compat.clone().unwrap_or_default();
    let supports_mid_convo = compat.supports_mid_convo_system_messages.unwrap_or(false);
    let supports_developer_role = compat
        .supports_developer_role
        .unwrap_or(model.provider == "openai");
    let requires_assistant_after_tool =
        compat.requires_assistant_after_tool_result.unwrap_or(false);
    let requires_thinking_as_text = compat.requires_thinking_as_text.unwrap_or(false);
    let requires_reasoning_content = compat
        .requires_reasoning_content_on_assistant_messages
        .unwrap_or(model.provider == "deepseek" || model.provider.starts_with("moonshotai"));
    let requires_tool_result_name = compat.requires_tool_result_name.unwrap_or(false);

    let normalized_context = resolve_transcript(context, supports_mid_convo);
    let transformed = transform_messages(
        normalized_context.messages(),
        model,
        Some(|id: &str, m: &Model, _a: &AssistantMessage| {
            normalize_completions_tool_call_id(id, m)
        }),
    );

    let instruction_role = if model.reasoning && supports_developer_role {
        "developer"
    } else {
        "system"
    };

    let mut params: Vec<Value> = Vec::new();
    let mut last_role: Option<&str> = None;
    let mut idx = 0usize;

    while idx < transformed.len() {
        let msg = &transformed[idx];
        if requires_assistant_after_tool
            && last_role == Some("toolResult")
            && matches!(msg, Message::User(_))
        {
            params.push(json!({
                "role": "assistant",
                "content": "I have processed the tool results.",
            }));
        }

        match msg {
            Message::System(sys) => {
                let text = if idx == 0 {
                    get_system_message_text(sys)
                } else {
                    render_system_message_update(sys)
                };
                if !text.is_empty() {
                    params.push(json!({
                        "role": instruction_role,
                        "content": text,
                    }));
                }
                last_role = Some("system");
            }
            Message::User(user) => {
                match &user.content {
                    UserContent::Text(t) => {
                        params.push(json!({
                            "role": "user",
                            "content": t,
                        }));
                    }
                    UserContent::Blocks(blocks) => {
                        let parts: Vec<Value> = blocks
                            .iter()
                            .filter(|b| match b {
                                UserContentBlock::Text(t) => !t.text.is_empty(),
                                UserContentBlock::Image(_) => true,
                            })
                            .map(|b| match b {
                                UserContentBlock::Text(t) => json!({
                                    "type": "text",
                                    "text": t.text,
                                }),
                                UserContentBlock::Image(img) => json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", img.mime_type, img.data)
                                    }
                                }),
                            })
                            .collect();
                        if !parts.is_empty() {
                            params.push(json!({
                                "role": "user",
                                "content": parts,
                            }));
                        }
                    }
                }
                last_role = Some("user");
            }
            Message::Assistant(assistant) => {
                let mut assistant_obj = Map::new();
                assistant_obj.insert("role".to_owned(), Value::String("assistant".to_owned()));
                assistant_obj.insert(
                    "content".to_owned(),
                    if requires_assistant_after_tool {
                        Value::String(String::new())
                    } else {
                        Value::Null
                    },
                );

                let text_joined: String = assistant
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text(t) if !t.text.trim().is_empty() => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let all_thinking_blocks: Vec<&ThinkingContent> = assistant
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Thinking(t) => Some(t),
                        _ => None,
                    })
                    .collect();

                let thinking_blocks: Vec<&ThinkingContent> = all_thinking_blocks
                    .iter()
                    .copied()
                    .filter(|t| !t.thinking.trim().is_empty())
                    .collect();

                let tool_calls: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolCall(tc) => Some(tc),
                        _ => None,
                    })
                    .collect();

                let preserved_reasoning_details = all_thinking_blocks
                    .iter()
                    .rev()
                    .find_map(|b| parse_openai_reasoning_details(b.thinking_signature.as_deref()))
                    .or_else(|| {
                        let legacy: Vec<Value> = tool_calls
                            .iter()
                            .filter_map(|tc| {
                                parse_legacy_encrypted_reasoning_detail(
                                    tc.thought_signature.as_deref(),
                                )
                            })
                            .collect();
                        if legacy.is_empty() {
                            None
                        } else {
                            Some(legacy)
                        }
                    });

                if !thinking_blocks.is_empty() {
                    if requires_thinking_as_text {
                        let thinking_text = thinking_blocks
                            .iter()
                            .map(|b| b.thinking.as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let combined = if text_joined.is_empty() {
                            thinking_text
                        } else {
                            format!("{thinking_text}\n\n{text_joined}")
                        };
                        assistant_obj.insert("content".to_owned(), Value::String(combined));
                    } else {
                        if !text_joined.is_empty() {
                            assistant_obj.insert("content".to_owned(), Value::String(text_joined));
                        }
                        if preserved_reasoning_details.is_none() {
                            let mut field_name = thinking_blocks[0]
                                .thinking_signature
                                .as_deref()
                                .filter(|s| {
                                    matches!(
                                        *s,
                                        "reasoning_content"
                                            | "reasoning"
                                            | "reasoning_text"
                                            | "thinking"
                                    )
                                })
                                .unwrap_or("reasoning_content");
                            if model.provider == "opencode-go" && field_name == "reasoning" {
                                field_name = "reasoning_content";
                            }
                            let joined_thinking = thinking_blocks
                                .iter()
                                .map(|b| b.thinking.as_str())
                                .collect::<Vec<_>>()
                                .join("\n");
                            assistant_obj
                                .insert(field_name.to_owned(), Value::String(joined_thinking));
                        }
                    }
                } else if !text_joined.is_empty() {
                    assistant_obj.insert("content".to_owned(), Value::String(text_joined));
                }

                if !tool_calls.is_empty() {
                    let tc_arr: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_owned()),
                                }
                            })
                        })
                        .collect();
                    assistant_obj.insert("tool_calls".to_owned(), Value::Array(tc_arr));
                }

                if let Some(details) = preserved_reasoning_details {
                    assistant_obj.insert("reasoning_details".to_owned(), Value::Array(details));
                }

                if requires_reasoning_content
                    && model.reasoning
                    && !assistant_obj.contains_key("reasoning_content")
                {
                    assistant_obj
                        .insert("reasoning_content".to_owned(), Value::String(String::new()));
                }

                let has_content = assistant_obj
                    .get("content")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let has_tools = assistant_obj.contains_key("tool_calls");
                if has_content || has_tools {
                    params.push(Value::Object(assistant_obj));
                    last_role = Some("assistant");
                }
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut j = idx;
                while j < transformed.len() {
                    let Message::ToolResult(tr) = &transformed[j] else {
                        break;
                    };
                    let text_res = tr
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            UserContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tr
                        .content
                        .iter()
                        .any(|b| matches!(b, UserContentBlock::Image(_)));
                    let tool_text = if !text_res.trim().is_empty() {
                        text_res
                    } else if has_images {
                        "(see attached image)".to_owned()
                    } else {
                        "(no tool output)".to_owned()
                    };

                    let mut tr_obj = Map::new();
                    tr_obj.insert("role".to_owned(), Value::String("tool".to_owned()));
                    tr_obj.insert("content".to_owned(), Value::String(tool_text));
                    tr_obj.insert(
                        "tool_call_id".to_owned(),
                        Value::String(tr.tool_call_id.clone()),
                    );
                    if requires_tool_result_name && !tr.tool_name.is_empty() {
                        tr_obj.insert("name".to_owned(), Value::String(tr.tool_name.clone()));
                    }
                    params.push(Value::Object(tr_obj));

                    if has_images && model.supports_image_input() {
                        for b in &tr.content {
                            if let UserContentBlock::Image(img) = b {
                                image_blocks.push(json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", img.mime_type, img.data)
                                    }
                                }));
                            }
                        }
                    }
                    j += 1;
                }
                idx = j - 1;

                if !image_blocks.is_empty() {
                    if requires_assistant_after_tool {
                        params.push(json!({
                            "role": "assistant",
                            "content": "I have processed the tool results.",
                        }));
                    }
                    let mut user_parts = vec![json!({
                        "type": "text",
                        "text": "Attached image(s) from tool result:",
                    })];
                    user_parts.extend(image_blocks);
                    params.push(json!({
                        "role": "user",
                        "content": user_parts,
                    }));
                    last_role = Some("user");
                } else {
                    last_role = Some("toolResult");
                }
            }
        }
        idx += 1;
    }

    params
}

pub fn build_completions_params(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&StreamOptions>,
) -> Value {
    let compat: ProviderCompat = model.compat.clone().unwrap_or_default();
    let supports_strict_mode = compat.supports_strict_mode.unwrap_or(false);
    let supports_usage_in_streaming = compat.supports_usage_in_streaming.unwrap_or(true);
    let supports_store = compat.supports_store.unwrap_or(model.provider == "openai");
    let max_tokens_field =
        compat
            .max_tokens_field
            .as_deref()
            .unwrap_or(if model.provider == "openai" {
                "max_completion_tokens"
            } else {
                "max_tokens"
            });

    let messages = convert_completions_messages(model, context);
    let transcript_tools =
        resolve_transcript_tools(context.messages(), TranscriptToolSupport::None);

    let mut params = Map::new();
    params.insert("model".to_owned(), Value::String(model.id.clone()));
    params.insert("messages".to_owned(), Value::Array(messages));
    params.insert("stream".to_owned(), Value::Bool(true));

    if supports_usage_in_streaming {
        params.insert(
            "stream_options".to_owned(),
            json!({ "include_usage": true }),
        );
    }
    if supports_store {
        params.insert("store".to_owned(), Value::Bool(false));
    }

    let cache_retention = resolve_cache_retention(
        options.and_then(|o| o.cache_retention),
        options.and_then(|o| o.env.as_ref()),
    );
    if cache_retention != CacheRetention::None
        && model.provider == "openai"
        && let Some(key) =
            clamp_openai_prompt_cache_key(options.and_then(|o| o.session_id.as_deref()))
    {
        params.insert("prompt_cache_key".to_owned(), Value::String(key));
    }

    if let Some(max_tokens) = options.and_then(|o| o.max_tokens)
        && max_tokens > 0
    {
        params.insert(max_tokens_field.to_owned(), Value::from(max_tokens));
    }

    if let Some(temp) = options.and_then(|o| o.temperature) {
        params.insert("temperature".to_owned(), json!(temp));
    }

    if !transcript_tools.request_tools.is_empty() {
        params.insert(
            "tools".to_owned(),
            Value::Array(convert_completions_tools(
                &transcript_tools.request_tools,
                supports_strict_mode,
            )),
        );
        if compat.zai_tool_stream == Some(true) {
            params.insert("tool_stream".to_owned(), Value::Bool(true));
        }
    }

    if let Some(tc) = options.and_then(|o| o.tool_choice) {
        let tc_str = match tc {
            ToolChoice::Auto => "auto",
            ToolChoice::None => "none",
        };
        params.insert("tool_choice".to_owned(), Value::String(tc_str.to_owned()));
    }

    let req_effort = options.and_then(|o| o.reasoning_effort);
    if model.reasoning {
        let thinking_format =
            compat
                .thinking_format
                .as_deref()
                .unwrap_or(match model.provider.as_str() {
                    "deepseek" => "deepseek",
                    "zai" | "zai-coding-cn" => "zai",
                    "qwen-token-plan-cn" | "qwen-token-plan" | "qwen-token-plan-individual" => {
                        "qwen"
                    }
                    "openrouter" => "openrouter",
                    "ant-ling" => "ant-ling",
                    _ => "openai",
                });

        if let Some(level) = req_effort {
            let model_level = ModelThinkingLevel::from(level);
            let mapped_effort = model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&model_level))
                .and_then(|v| v.clone())
                .unwrap_or_else(|| level.as_str().to_owned());

            match thinking_format {
                "deepseek" => {
                    params.insert("thinking".to_owned(), json!({ "type": "enabled" }));
                    params.insert("reasoning_effort".to_owned(), Value::String(mapped_effort));
                }
                "zai" => {
                    params.insert("thinking".to_owned(), json!({ "type": "enabled" }));
                }
                "qwen" => {
                    params.insert("enable_thinking".to_owned(), Value::Bool(true));
                }
                "openrouter" | "ant-ling" => {
                    params.insert("reasoning".to_owned(), json!({ "effort": mapped_effort }));
                }
                _ => {
                    params.insert("reasoning_effort".to_owned(), Value::String(mapped_effort));
                }
            }
        } else {
            match thinking_format {
                "deepseek" | "zai" => {
                    params.insert("thinking".to_owned(), json!({ "type": "disabled" }));
                }
                "qwen" => {
                    params.insert("enable_thinking".to_owned(), Value::Bool(false));
                }
                _ => {}
            }
        }
    }

    let thinking_level = req_effort
        .map(ModelThinkingLevel::from)
        .unwrap_or(ModelThinkingLevel::Off);
    if let Some(sampling) = resolve_sampling_params(
        model,
        thinking_level,
        options.and_then(|o| o.sampling_params.as_ref()),
    ) {
        for (k, v) in sampling {
            params.insert(k, v);
        }
    }

    Value::Object(params)
}

fn is_openai_reasoning_detail(val: &Value) -> bool {
    let Some(obj) = val.as_object() else {
        return false;
    };
    let valid_id = obj.get("id").is_none_or(|v| v.is_null() || v.is_string());
    let valid_format = obj.get("format").is_none_or(|v| v.is_string());
    let valid_index = obj.get("index").is_none_or(|v| v.is_number());
    if !valid_id || !valid_format || !valid_index {
        return false;
    }
    match obj.get("type").and_then(|v| v.as_str()) {
        Some("reasoning.summary") => obj.get("summary").is_some_and(|v| v.is_string()),
        Some("reasoning.encrypted") => obj.get("data").is_some_and(|v| v.is_string()),
        Some("reasoning.text") => {
            obj.get("text").is_some_and(|v| v.is_string())
                && obj
                    .get("signature")
                    .is_none_or(|v| v.is_null() || v.is_string())
        }
        _ => false,
    }
}

fn parse_openai_reasoning_details(signature: Option<&str>) -> Option<Vec<Value>> {
    let sig = signature?;
    let parsed: Value = serde_json::from_str(sig).ok()?;
    let arr = parsed.as_array()?;
    if arr.is_empty() || !arr.iter().all(is_openai_reasoning_detail) {
        return None;
    }
    Some(arr.clone())
}

fn parse_legacy_encrypted_reasoning_detail(signature: Option<&str>) -> Option<Value> {
    let sig = signature?;
    let parsed: Value = serde_json::from_str(sig).ok()?;
    if is_openai_reasoning_detail(&parsed)
        && parsed.get("type").and_then(|v| v.as_str()) == Some("reasoning.encrypted")
        && parsed
            .get("id")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
        && parsed
            .get("data")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    {
        Some(parsed)
    } else {
        None
    }
}

fn append_openai_reasoning_detail(details: &mut Vec<Value>, detail: &Value) {
    let detail_type = detail.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(last) = details.last_mut()
        && let Some(last_obj) = last.as_object_mut()
    {
        let last_type = last_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if detail_type == "reasoning.text" && last_type == "reasoning.text" {
            let add_text = detail.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let cur_text = last_obj
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            last_obj.insert(
                "text".to_owned(),
                Value::String(format!("{cur_text}{add_text}")),
            );
            if last_obj
                .get("signature")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .is_empty()
                && let Some(sig) = detail.get("signature")
            {
                last_obj.insert("signature".to_owned(), sig.clone());
            }
            for key in ["id", "format", "index"] {
                if last_obj.get(key).is_none_or(|v| v.is_null())
                    && let Some(v) = detail.get(key)
                {
                    last_obj.insert(key.to_owned(), v.clone());
                }
            }
            return;
        }
        if detail_type == "reasoning.summary" && last_type == "reasoning.summary" {
            let add_sum = detail.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            let cur_sum = last_obj
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            last_obj.insert(
                "summary".to_owned(),
                Value::String(format!("{cur_sum}{add_sum}")),
            );
            for key in ["id", "format", "index"] {
                if last_obj.get(key).is_none_or(|v| v.is_null())
                    && let Some(v) = detail.get(key)
                {
                    last_obj.insert(key.to_owned(), v.clone());
                }
            }
            return;
        }
    }
    details.push(detail.clone());
}

// =============================================================================
// Completions Stream Processing
// =============================================================================

struct ToolSlot {
    content_index: usize,
    args_stream: ToolCallArgumentsStream,
}

pub struct CompletionsStreamProcessor {
    thinking_index: Option<usize>,
    text_index: Option<usize>,
    tool_slots: BTreeMap<u64, ToolSlot>,
    streamed_reasoning_details: Vec<Value>,
    has_finish_reason: bool,
}

impl Default for CompletionsStreamProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionsStreamProcessor {
    pub fn new() -> Self {
        Self {
            thinking_index: None,
            text_index: None,
            tool_slots: BTreeMap::new(),
            streamed_reasoning_details: Vec::new(),
            has_finish_reason: false,
        }
    }

    fn ensure_thinking_block(
        &mut self,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
        signature: &str,
    ) -> usize {
        if let Some(idx) = self.thinking_index {
            return idx;
        }
        let mut tc = ThinkingContent::new("");
        tc.thinking_signature = Some(signature.to_owned());
        output.content.push(ContentBlock::Thinking(tc));
        let new_idx = output.content.len() - 1;
        self.thinking_index = Some(new_idx);
        stream.push(AssistantMessageEvent::ThinkingStart {
            content_index: new_idx,
            partial: output.clone(),
        });
        new_idx
    }

    fn close_thinking_block(
        &mut self,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) {
        if let Some(t_idx) = self.thinking_index.take() {
            let mut content = String::new();
            if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(t_idx) {
                if !self.streamed_reasoning_details.is_empty()
                    && let Ok(serialized) = serde_json::to_string(&self.streamed_reasoning_details)
                {
                    t.thinking_signature = Some(serialized);
                }
                content = t.thinking.clone();
            }
            stream.push(AssistantMessageEvent::ThinkingEnd {
                content_index: t_idx,
                content,
                partial: output.clone(),
            });
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
        let chunk: Value =
            serde_json::from_str(data).map_err(|e| format!("Invalid Completions SSE JSON: {e}"))?;

        if let Some(err) = chunk.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(msg.to_owned());
        }

        if let Some(id) = chunk.get("id").and_then(|v| v.as_str())
            && !id.is_empty()
        {
            output.response_id = Some(id.to_owned());
        }
        if let Some(resp_model) = chunk.get("model").and_then(|v| v.as_str())
            && !resp_model.is_empty()
        {
            output.response_model = Some(resp_model.to_owned());
        }

        if let Some(choices) = chunk.get("choices").and_then(|v| v.as_array())
            && let Some(choice) = choices.first()
        {
            if let Some(delta) = choice.get("delta") {
                for field in [
                    "reasoning_content",
                    "reasoning",
                    "reasoning_text",
                    "thinking",
                ] {
                    if let Some(r_delta) = delta.get(field).and_then(|v| v.as_str())
                        && !r_delta.is_empty()
                    {
                        let sig = if model.provider == "opencode-go" && field == "reasoning" {
                            "reasoning_content"
                        } else {
                            field
                        };
                        let idx = self.ensure_thinking_block(output, stream, sig);
                        if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(idx) {
                            t.thinking.push_str(r_delta);
                        }
                        stream.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: idx,
                            delta: r_delta.to_owned(),
                            partial: output.clone(),
                        });
                        break;
                    }
                }

                if let Some(details) = delta.get("reasoning_details").and_then(|v| v.as_array()) {
                    for detail in details {
                        if !is_openai_reasoning_detail(detail) {
                            continue;
                        }
                        self.ensure_thinking_block(output, stream, "");
                        append_openai_reasoning_detail(
                            &mut self.streamed_reasoning_details,
                            detail,
                        );
                    }
                }

                if let Some(c_delta) = delta.get("content").and_then(|v| v.as_str())
                    && !c_delta.is_empty()
                {
                    self.close_thinking_block(output, stream);

                    let idx = match self.text_index {
                        Some(i) => i,
                        None => {
                            output
                                .content
                                .push(ContentBlock::Text(TextContent::new("")));
                            let new_idx = output.content.len() - 1;
                            self.text_index = Some(new_idx);
                            stream.push(AssistantMessageEvent::TextStart {
                                content_index: new_idx,
                                partial: output.clone(),
                            });
                            new_idx
                        }
                    };
                    if let Some(ContentBlock::Text(t)) = output.content.get_mut(idx) {
                        t.text.push_str(c_delta);
                    }
                    stream.push(AssistantMessageEvent::TextDelta {
                        content_index: idx,
                        delta: c_delta.to_owned(),
                        partial: output.clone(),
                    });
                }

                if let Some(tc_deltas) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc_delta in tc_deltas {
                        let tc_index = tc_delta.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        let id_part = tc_delta.get("id").and_then(|v| v.as_str());
                        let fn_obj = tc_delta.get("function");
                        let name_part = fn_obj.and_then(|f| f.get("name")).and_then(|v| v.as_str());
                        let args_part = fn_obj
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str());

                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            self.tool_slots.entry(tc_index)
                        {
                            let tc = ToolCall {
                                id: id_part
                                    .filter(|s| !s.is_empty())
                                    .map(|s| s.to_owned())
                                    .unwrap_or_else(|| format!("call_{tc_index}")),
                                name: name_part.unwrap_or("").to_owned(),
                                arguments: Map::new(),
                                thought_signature: None,
                                namespace: None,
                                arguments_error: None,
                            };
                            output.content.push(ContentBlock::ToolCall(tc));
                            let content_index = output.content.len() - 1;
                            entry.insert(ToolSlot {
                                content_index,
                                args_stream: ToolCallArgumentsStream::new(),
                            });
                            stream.push(AssistantMessageEvent::ToolCallStart {
                                content_index,
                                partial: output.clone(),
                            });
                        }

                        if let Some(slot) = self.tool_slots.get_mut(&tc_index) {
                            let c_idx = slot.content_index;
                            if let Some(ContentBlock::ToolCall(tc)) = output.content.get_mut(c_idx)
                            {
                                if let Some(id_str) = id_part
                                    && !id_str.is_empty()
                                {
                                    tc.id = id_str.to_owned();
                                }
                                if let Some(name_str) = name_part
                                    && !name_str.is_empty()
                                {
                                    if tc.name.is_empty() {
                                        tc.name = name_str.to_owned();
                                    } else if tc.name != name_str {
                                        tc.name.push_str(name_str);
                                    }
                                }
                            }
                            if let Some(arg_delta) = args_part
                                && !arg_delta.is_empty()
                            {
                                let (refreshed, partial_args) =
                                    slot.args_stream.append_with_change(arg_delta);
                                if refreshed
                                    && let Some(ContentBlock::ToolCall(tc)) =
                                        output.content.get_mut(c_idx)
                                {
                                    tc.arguments = partial_args.clone();
                                }
                                stream.push(AssistantMessageEvent::ToolCallDelta {
                                    content_index: c_idx,
                                    delta: arg_delta.to_owned(),
                                    partial: output.clone(),
                                });
                            }
                        }
                    }
                }
            }

            if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                self.has_finish_reason = true;
                output.raw_stop_reason = Some(finish_reason.to_owned());
                match finish_reason {
                    "stop" | "end" => {
                        output.stop_reason = StopReason::Stop;
                    }
                    "length" | "max_tokens" => {
                        output.stop_reason = StopReason::Length;
                    }
                    "tool_calls" | "function_call" => {
                        output.stop_reason = StopReason::ToolUse;
                    }
                    "content_filter" => {
                        output.stop_reason = StopReason::Error;
                        output.error_message =
                            Some("Provider finish_reason: content_filter".to_owned());
                    }
                    other => {
                        output.stop_reason = StopReason::Error;
                        output.error_message = Some(format!("Provider finish_reason: {other}"));
                    }
                }
            }
        }

        let usage_obj = chunk.get("usage").filter(|u| !u.is_null()).or_else(|| {
            chunk
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|c| c.first())
                .and_then(|c| c.get("usage"))
                .filter(|u| !u.is_null())
        });

        if let Some(usage) = usage_obj {
            let prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion_tokens = usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_read_tokens = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .or_else(|| usage.get("prompt_cache_hit_tokens"))
                .or_else(|| usage.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_write_tokens = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cache_write_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let reasoning_tokens = usage
                .get("completion_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|v| v.as_u64());
            let input_tokens = prompt_tokens.saturating_sub(cache_read_tokens + cache_write_tokens);
            let total_tokens =
                input_tokens + completion_tokens + cache_read_tokens + cache_write_tokens;

            output.usage = Usage {
                input: input_tokens,
                output: completion_tokens,
                cache_read: cache_read_tokens,
                cache_write: cache_write_tokens,
                cache_write_1h: None,
                reasoning: reasoning_tokens,
                total_tokens,
                cost: UsageCost::default(),
            };
            calculate_cost(&model.cost, &mut output.usage);
        }

        Ok(())
    }

    pub fn finish(
        &mut self,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
        model: &Model,
    ) -> Result<(), String> {
        self.close_thinking_block(output, stream);
        if !self.streamed_reasoning_details.is_empty()
            && let Ok(serialized) = serde_json::to_string(&self.streamed_reasoning_details)
        {
            for block in &mut output.content {
                if let ContentBlock::Thinking(t) = block {
                    t.thinking_signature = Some(serialized.clone());
                }
            }
        }
        if let Some(txt_idx) = self.text_index.take() {
            let content = output
                .content
                .get(txt_idx)
                .and_then(|b| b.as_text())
                .map(|t| t.text.clone())
                .unwrap_or_default();
            stream.push(AssistantMessageEvent::TextEnd {
                content_index: txt_idx,
                content,
                partial: output.clone(),
            });
        }

        let slots = std::mem::take(&mut self.tool_slots);
        for (_, slot) in slots {
            let parsed = slot.args_stream.finish();
            let mut finished_tc = None;
            if let Some(ContentBlock::ToolCall(tc)) = output.content.get_mut(slot.content_index) {
                parsed.apply_to(tc);
                finished_tc = Some(tc.clone());
            }
            if let Some(tc) = finished_tc {
                stream.push(AssistantMessageEvent::ToolCallEnd {
                    content_index: slot.content_index,
                    tool_call: tc,
                    partial: output.clone(),
                });
            }
        }

        let supports_finish_reason = model
            .compat
            .as_ref()
            .and_then(|c| c.supports_finish_reason)
            .unwrap_or(true);
        let has_tool_calls = output
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolCall(_)));

        if !self.has_finish_reason && !supports_finish_reason {
            output.stop_reason = if has_tool_calls {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            };
        } else if has_tool_calls && output.stop_reason == StopReason::Stop {
            output.stop_reason = StopReason::ToolUse;
        }

        if output.stop_reason == StopReason::Error {
            return Err(output
                .error_message
                .clone()
                .unwrap_or_else(|| "Provider returned an error stop reason".to_owned()));
        }
        if (supports_finish_reason && !self.has_finish_reason)
            || output.stop_reason == StopReason::Pending
        {
            output.stop_reason = StopReason::Error;
            output.error_message = Some("Stream ended without finish_reason".to_owned());
            return Err("Stream ended without finish_reason".to_owned());
        }
        Ok(())
    }
}

pub fn process_completions_sse_events(
    events: &[SseEvent],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
) -> Result<(), String> {
    let mut processor = CompletionsStreamProcessor::new();
    for sse in events {
        processor.handle_sse(sse, output, stream, model)?;
    }
    processor.finish(output, stream, model)
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

            let mut opt_headers: ProviderHeaders = options
                .as_ref()
                .and_then(|o| o.headers.clone())
                .unwrap_or_default();
            inject_copilot_dynamic_headers(&model, context.messages(), &mut opt_headers);

            let auth_val = format!("Bearer {api_key}");
            let headers = build_header_map(
                options.as_ref().and_then(|o| o.identity.as_ref()),
                &[
                    ("authorization", &auth_val),
                    ("content-type", "application/json"),
                    ("accept", "text/event-stream"),
                ],
                model.headers.as_ref(),
                Some(&opt_headers),
            );

            let headers = super::invoke_on_headers(options.as_ref(), headers, &model).await?;

            let raw_payload = build_completions_params(&model, &context, options.as_ref());
            let payload = invoke_on_payload(options.as_ref(), raw_payload, &model).await?;
            let base = model.base_url.trim_end_matches('/');
            let url = format!("{base}/chat/completions");
            let client = reqwest::Client::new();

            let resp = send_with_retry(
                &client,
                &url,
                headers,
                &payload,
                options.as_ref().and_then(|o| o.timeout_ms),
                options.as_ref().and_then(|o| o.max_retries).unwrap_or(0),
                options.as_ref().and_then(|o| o.max_retry_delay_ms),
                cancel_token.as_ref(),
            )
            .await?;

            invoke_on_response(options.as_ref(), &resp, &model).await?;

            writer.push(AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            let mut byte_stream = resp.bytes_stream();
            let mut sse_parser = SseParser::new();
            let mut processor = CompletionsStreamProcessor::new();

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
                    invoke_on_provider_stream_event(options.as_ref(), &sse, &model).await?;
                    processor.handle_sse(&sse, &mut output, &writer, &model)?;
                }
            }

            for sse in sse_parser.finish() {
                invoke_on_provider_stream_event(options.as_ref(), &sse, &model).await?;
                processor.handle_sse(&sse, &mut output, &writer, &model)?;
            }
            processor.finish(&mut output, &writer, &model)?;

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

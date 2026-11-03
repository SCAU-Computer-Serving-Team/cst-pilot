use std::collections::{BTreeMap, BTreeSet};

use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use crate::api::{
    SseEvent, SseParser, build_base_options, build_header_map, calculate_cost,
    clamp_openai_prompt_cache_key, clamp_thinking_level, inject_copilot_dynamic_headers,
    require_client_api_key, resolve_cache_retention, resolve_sampling_params, send_with_retry,
    short_hash,
};
use crate::streaming_json::ToolCallArgumentsStream;
use crate::transform_messages::{
    TranscriptToolSupport, get_initial_system_message, get_system_message_text,
    render_system_message_update, render_tool_removal_notice, resolve_transcript,
    resolve_transcript_tools, transform_messages,
};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention,
    ContentBlock, Message, Model, ModelThinkingLevel, ProviderCompat, ProviderHeaders,
    SessionAffinityFormat, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    TextSignatureV1, ThinkingContent, Tool, ToolCall, ToolChoice, TranscriptContext, Usage,
    UsageCost, UserContent, UserContentBlock, create_assistant_message_event_stream,
};

const OPENAI_TOOL_CALL_PROVIDERS: &[&str] = &["openai", "openai-codex", "opencode", "deepseek"];
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

fn is_chatgpt_sign_in(model: &Model, api_key: Option<&str>) -> bool {
    model.provider == "openai"
        && model.base_url == "https://api.openai.com/v1"
        && api_key.is_some_and(|k| !k.starts_with("sk-"))
}

pub fn is_codex_model(model: &Model) -> bool {
    model.provider == "openai-codex"
        || model.api == "openai-codex-responses"
        || model.base_url.contains("chatgpt.com/backend-api")
}

pub fn extract_chatgpt_account_id(jwt_token: &str) -> Option<String> {
    use base64::Engine;
    let payload_b64 = jwt_token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload_b64))
        .ok()?;
    let json_val: Value = serde_json::from_slice(&decoded).ok()?;
    json_val
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|id| id.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
}

fn detect_session_affinity_format(model: &Model) -> SessionAffinityFormat {
    if let Some(fmt) = model
        .compat
        .as_ref()
        .and_then(|c| c.session_affinity_format)
    {
        return fmt;
    }
    if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
        SessionAffinityFormat::OpenRouter
    } else {
        SessionAffinityFormat::OpenAi
    }
}

fn encode_text_signature_v1(id: &str, phase: Option<&str>) -> Option<String> {
    if id.is_empty() && phase.is_none() {
        return None;
    }
    let sig = TextSignatureV1 {
        v: 1,
        id: id.to_owned(),
        phase: phase.map(|s| s.to_owned()),
    };
    serde_json::to_string(&sig).ok()
}

fn parse_text_signature(sig: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(raw) = sig else {
        return (None, None);
    };
    if raw.is_empty() {
        return (None, None);
    }
    if let Ok(parsed) = serde_json::from_str::<TextSignatureV1>(raw)
        && parsed.v == 1
    {
        return (
            if parsed.id.is_empty() {
                None
            } else {
                Some(parsed.id)
            },
            parsed.phase,
        );
    }
    (Some(raw.to_owned()), None)
}

fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = if sanitized.len() > 64 {
        &sanitized[..64]
    } else {
        &sanitized
    };
    trimmed.trim_end_matches('_').to_owned()
}

fn convert_tool_result_output(model: &Model, content: &[UserContentBlock]) -> Value {
    let has_images = content
        .iter()
        .any(|b| matches!(b, UserContentBlock::Image(_)));
    if !has_images || !model.supports_image_input() {
        let text = content
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Value::String(if text.is_empty() {
            "(no tool output)".to_owned()
        } else {
            text
        });
    }

    let parts: Vec<Value> = content
        .iter()
        .map(|b| match b {
            UserContentBlock::Text(t) => json!({
                "type": "input_text",
                "text": t.text,
            }),
            UserContentBlock::Image(img) => json!({
                "type": "input_image",
                "detail": "auto",
                "image_url": format!("data:{};base64,{}", img.mime_type, img.data),
            }),
        })
        .collect();
    Value::Array(parts)
}

pub fn convert_responses_tools(tools: &[Tool], supports_strict_mode: bool) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut obj = Map::new();
            obj.insert("type".to_owned(), Value::String("function".to_owned()));
            obj.insert("name".to_owned(), Value::String(tool.name.clone()));
            obj.insert(
                "description".to_owned(),
                Value::String(tool.description.clone()),
            );
            obj.insert("parameters".to_owned(), tool.parameters.clone());
            if supports_strict_mode {
                obj.insert("strict".to_owned(), Value::Bool(false));
            }
            Value::Object(obj)
        })
        .collect()
}

pub fn convert_responses_messages(model: &Model, context: &TranscriptContext) -> Vec<Value> {
    let compat = model.compat.as_ref();
    let supports_mid_convo = compat
        .and_then(|c| c.supports_mid_convo_system_messages)
        .unwrap_or(false);
    let supports_developer_role = compat
        .and_then(|c| c.supports_developer_role)
        .unwrap_or(true);
    let instruction_role = if model.reasoning && supports_developer_role {
        "developer"
    } else {
        "system"
    };

    let normalized_context = resolve_transcript(context, supports_mid_convo);
    let normalize_tool_call_id = |id: &str, target_model: &Model, source: &AssistantMessage| {
        if !OPENAI_TOOL_CALL_PROVIDERS.contains(&target_model.provider.as_str()) {
            return normalize_id_part(id);
        }
        if !id.contains('|') {
            return normalize_id_part(id);
        }
        let (call_id, item_id) = id.split_once('|').unwrap_or((id, ""));
        let norm_call = normalize_id_part(call_id);
        let foreign = source.provider != target_model.provider || source.api != target_model.api;
        let mut norm_item = if foreign {
            format!("fc_{}", short_hash(item_id))
        } else {
            normalize_id_part(item_id)
        };
        if !norm_item.starts_with("fc_") && !norm_item.starts_with("ctc_") {
            norm_item = normalize_id_part(&format!("fc_{norm_item}"));
        }
        format!("{norm_call}|{norm_item}")
    };

    let transformed = transform_messages(
        normalized_context.messages(),
        model,
        Some(normalize_tool_call_id),
    );
    let transcript_tools =
        resolve_transcript_tools(normalized_context.messages(), TranscriptToolSupport::None);

    let is_codex = is_codex_model(model);
    let mut messages: Vec<Value> = Vec::new();
    let mut msg_index = 0usize;
    let mut system_index = 0usize;
    let mut unavailable_tools: BTreeSet<String> = BTreeSet::new();

    for (source_index, msg) in transformed.iter().enumerate() {
        let is_leading_system = source_index == 0 && matches!(msg, Message::System(_));

        match msg {
            Message::System(sys) => {
                let anchors_tool_changes = system_index >= transcript_tools.folded_system_messages;
                system_index += 1;
                if let Some(added) = &sys.tools_added {
                    for t in added {
                        unavailable_tools.remove(&t.name);
                    }
                }
                let mut parts = if is_leading_system && is_codex {
                    Vec::new()
                } else {
                    vec![if is_leading_system {
                        get_system_message_text(sys)
                    } else {
                        render_system_message_update(sys)
                    }]
                };

                if anchors_tool_changes {
                    let added_names: BTreeSet<&str> = sys
                        .tools_added
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect();
                    let removed: Vec<String> = sys
                        .tools_removed
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .filter(|r| !added_names.contains(r.name.as_str()))
                        .map(|r| r.name.clone())
                        .collect();
                    if !removed.is_empty() {
                        parts.push(render_tool_removal_notice(&removed));
                    }
                    for name in removed {
                        unavailable_tools.insert(name);
                    }
                }

                let text = parts
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !text.is_empty() {
                    messages.push(json!({
                        "role": instruction_role,
                        "content": text,
                    }));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    messages.push(json!({
                        "role": "user",
                        "content": [{ "type": "input_text", "text": text }],
                    }));
                }
                UserContent::Blocks(blocks) => {
                    if blocks.is_empty() {
                        continue;
                    }
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|b| match b {
                            UserContentBlock::Text(t) => json!({
                                "type": "input_text",
                                "text": t.text,
                            }),
                            UserContentBlock::Image(img) => json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{};base64,{}", img.mime_type, img.data),
                            }),
                        })
                        .collect();
                    messages.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            },
            Message::Assistant(assistant) => {
                let is_same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api;
                let is_different_model = is_same_provider_and_api && assistant.model != model.id;
                let mut text_block_index = 0usize;
                let mut output_items: Vec<Value> = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking(thinking) => {
                            if let Some(sig) = &thinking.thinking_signature
                                && let Ok(item) = serde_json::from_str::<Value>(sig)
                            {
                                output_items.push(item);
                            }
                        }
                        ContentBlock::Text(text_block) => {
                            let (sig_id, sig_phase) =
                                parse_text_signature(text_block.text_signature.as_deref());
                            let fallback_id = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;

                            let mut msg_id = sig_id.unwrap_or(fallback_id);
                            if msg_id.len() > 64 {
                                msg_id = format!("msg_{}", short_hash(&msg_id));
                            }
                            let mut item = json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": text_block.text,
                                    "annotations": []
                                }],
                                "status": "completed",
                                "id": msg_id,
                            });
                            if let Some(phase) = sig_phase
                                && let Some(obj) = item.as_object_mut()
                            {
                                obj.insert("phase".to_owned(), Value::String(phase));
                            }
                            output_items.push(item);
                        }
                        ContentBlock::ToolCall(tc) => {
                            let (call_id, item_id_raw) =
                                tc.id.split_once('|').unwrap_or((tc.id.as_str(), ""));
                            let item_id = if !is_different_model && item_id_raw.starts_with("fc_") {
                                Some(item_id_raw)
                            } else {
                                None
                            };
                            let mut fc_obj = Map::new();
                            fc_obj.insert(
                                "type".to_owned(),
                                Value::String("function_call".to_owned()),
                            );
                            if let Some(id_val) = item_id {
                                fc_obj.insert("id".to_owned(), Value::String(id_val.to_owned()));
                            }
                            fc_obj.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
                            fc_obj.insert("name".to_owned(), Value::String(tc.name.clone()));
                            fc_obj.insert(
                                "arguments".to_owned(),
                                Value::String(
                                    serde_json::to_string(&tc.arguments)
                                        .unwrap_or_else(|_| "{}".to_owned()),
                                ),
                            );
                            output_items.push(Value::Object(fc_obj));
                        }
                    }
                }
                messages.extend(output_items);
            }
            Message::ToolResult(tr) => {
                let (call_id, _) = tr
                    .tool_call_id
                    .split_once('|')
                    .unwrap_or((tr.tool_call_id.as_str(), ""));
                let output = convert_tool_result_output(model, &tr.content);
                messages.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
        }

        if !is_leading_system {
            msg_index += 1;
        }
    }

    messages
}

pub fn build_responses_params(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&StreamOptions>,
) -> Value {
    let compat: ProviderCompat = model.compat.clone().unwrap_or_default();
    let supports_strict_mode = compat.supports_strict_mode.unwrap_or(false);
    let supports_max_output_tokens = compat.supports_max_output_tokens.unwrap_or(true);
    let supports_long_cache = compat.supports_long_cache_retention.unwrap_or(true);
    let supports_explicit_cache = compat.supports_explicit_prompt_cache_mode.unwrap_or(false);

    let messages = convert_responses_messages(model, context);
    let transcript_tools =
        resolve_transcript_tools(context.messages(), TranscriptToolSupport::None);
    let cache_retention = resolve_cache_retention(
        options.and_then(|o| o.cache_retention),
        options.and_then(|o| o.env.as_ref()),
    );
    let is_codex = is_codex_model(model);
    let omit_unsupported =
        is_codex || is_chatgpt_sign_in(model, options.and_then(|o| o.api_key.as_deref()));

    let mut params = Map::new();
    params.insert("model".to_owned(), Value::String(model.id.clone()));
    params.insert("input".to_owned(), Value::Array(messages));
    params.insert("stream".to_owned(), Value::Bool(true));
    params.insert("store".to_owned(), Value::Bool(false));

    if is_codex {
        let initial_sys = get_initial_system_message(context.messages())
            .map(get_system_message_text)
            .unwrap_or_default();
        let instructions = if initial_sys.trim().is_empty() {
            "You are a helpful assistant.".to_owned()
        } else {
            initial_sys
        };
        params.insert("instructions".to_owned(), Value::String(instructions));
        params.insert("text".to_owned(), json!({ "verbosity": "low" }));
        params.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
        params.insert("parallel_tool_calls".to_owned(), Value::Bool(true));
        params.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
    }

    if cache_retention != CacheRetention::None
        && let Some(key) =
            clamp_openai_prompt_cache_key(options.and_then(|o| o.session_id.as_deref()))
    {
        params.insert("prompt_cache_key".to_owned(), Value::String(key));
    }

    if !omit_unsupported {
        if cache_retention == CacheRetention::Long
            && supports_long_cache
            && !supports_explicit_cache
        {
            params.insert(
                "prompt_cache_retention".to_owned(),
                Value::String("24h".to_owned()),
            );
        }
        if supports_explicit_cache {
            if cache_retention == CacheRetention::None {
                params.insert(
                    "prompt_cache_options".to_owned(),
                    json!({ "mode": "explicit" }),
                );
            } else if cache_retention == CacheRetention::Long && supports_long_cache {
                params.insert("prompt_cache_options".to_owned(), json!({ "ttl": "30m" }));
            }
        }
    }

    if let Some(max_tokens) = options.and_then(|o| o.max_tokens)
        && max_tokens > 0
        && supports_max_output_tokens
        && !omit_unsupported
    {
        params.insert(
            "max_output_tokens".to_owned(),
            Value::from(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }

    if let Some(temp) = options.and_then(|o| o.temperature)
        && !omit_unsupported
    {
        params.insert("temperature".to_owned(), json!(temp));
    }

    if let Some(tier) = options.and_then(|o| o.service_tier.as_ref()) {
        params.insert("service_tier".to_owned(), Value::String(tier.clone()));
    }

    if !transcript_tools.request_tools.is_empty() {
        params.insert(
            "tools".to_owned(),
            Value::Array(convert_responses_tools(
                &transcript_tools.request_tools,
                supports_strict_mode,
            )),
        );
    }

    if let Some(tc) = options.and_then(|o| o.tool_choice) {
        let tc_str = match tc {
            ToolChoice::Auto => "auto",
            ToolChoice::None => "none",
        };
        params.insert("tool_choice".to_owned(), Value::String(tc_str.to_owned()));
    }

    let req_effort = options.and_then(|o| o.reasoning_effort);
    let req_summary = options.and_then(|o| o.reasoning_summary.as_deref());

    if model.reasoning {
        let effective_effort = req_effort.or(if req_summary.is_some() {
            Some(crate::types::ThinkingLevel::Medium)
        } else {
            None
        });
        if let Some(level) = effective_effort {
            let model_level = ModelThinkingLevel::from(level);
            let mapped = model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&model_level))
                .and_then(|v| v.clone())
                .unwrap_or_else(|| level.as_str().to_owned());
            params.insert(
                "reasoning".to_owned(),
                json!({
                    "effort": mapped,
                    "summary": req_summary.unwrap_or("auto"),
                }),
            );
            params.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
        } else if model.provider != "github-copilot"
            && model.provider != "deepseek"
            && model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&ModelThinkingLevel::Off))
                != Some(&None)
        {
            let off_val = model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&ModelThinkingLevel::Off))
                .and_then(|v| v.clone())
                .unwrap_or_else(|| "none".to_owned());
            params.insert("reasoning".to_owned(), json!({ "effort": off_val }));
        }
        if model.provider == "xai" || model.provider == "deepseek" {
            params.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
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

// =============================================================================
// Responses Stream Processing
// =============================================================================

enum SlotState {
    Thinking {
        content_index: usize,
    },
    Text {
        content_index: usize,
    },
    ToolCall {
        content_index: usize,
        args_stream: ToolCallArgumentsStream,
    },
}

fn map_responses_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> (StopReason, Option<String>) {
    match status {
        None | Some("completed") | Some("in_progress") | Some("queued") => (StopReason::Stop, None),
        Some("incomplete") => {
            if incomplete_reason == Some("max_output_tokens") {
                (StopReason::Length, None)
            } else {
                let msg = match incomplete_reason {
                    Some(r) => format!("Response incomplete: {r}"),
                    None => "Response incomplete without a provider reason".to_owned(),
                };
                (StopReason::Error, Some(msg))
            }
        }
        Some("failed") | Some("cancelled") => (StopReason::Error, None),
        Some(other) => (
            StopReason::Error,
            Some(format!("Unhandled stop reason: {other}")),
        ),
    }
}

/// 处理已解析的 OpenAI Responses SSE 事件序列（供 HTTP 流与离线单元测试共用）。
pub fn process_responses_sse_events(
    events: &[SseEvent],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
) -> Result<(), String> {
    let mut state = ResponsesStreamProcessor::new();
    for sse in events {
        state.handle_sse(sse, output, stream, model)?;
    }
    state.finish(output)
}

pub(crate) struct ResponsesStreamProcessor {
    saw_terminal: bool,
    slots: BTreeMap<u64, SlotState>,
    unfinished_tool_calls: BTreeSet<usize>,
    reasoning_indices_by_id: BTreeMap<String, usize>,
}

impl ResponsesStreamProcessor {
    pub fn new() -> Self {
        Self {
            saw_terminal: false,
            slots: BTreeMap::new(),
            unfinished_tool_calls: BTreeSet::new(),
            reasoning_indices_by_id: BTreeMap::new(),
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
            serde_json::from_str(data).map_err(|e| format!("Invalid Responses SSE JSON: {e}"))?;
        let event_type = event
            .get("type")
            .and_then(|v| v.as_str())
            .or(sse.event.as_deref())
            .unwrap_or("");

        match event_type {
            "response.created" => {
                if let Some(id) = event
                    .get("response")
                    .and_then(|r| r.get("id"))
                    .and_then(|v| v.as_str())
                {
                    output.response_id = Some(id.to_owned());
                }
            }
            "response.output_item.added" => {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Some(item) = event.get("item") {
                    self.create_slot(output_index, item, output, stream);
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let delta = event.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(SlotState::Thinking { content_index }) = self.slots.get(&output_index) {
                    let idx = *content_index;
                    if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(idx) {
                        t.thinking.push_str(delta);
                    }
                    stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: idx,
                        delta: delta.to_owned(),
                        partial: output.clone(),
                    });
                }
            }
            "response.reasoning_summary_part.done" => {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Some(SlotState::Thinking { content_index }) = self.slots.get(&output_index) {
                    let idx = *content_index;
                    if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(idx) {
                        t.thinking.push_str("\n\n");
                    }
                    stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: idx,
                        delta: "\n\n".to_owned(),
                        partial: output.clone(),
                    });
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let delta = event.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(SlotState::Text { content_index }) = self.slots.get(&output_index) {
                    let idx = *content_index;
                    if let Some(ContentBlock::Text(t)) = output.content.get_mut(idx) {
                        t.text.push_str(delta);
                    }
                    stream.push(AssistantMessageEvent::TextDelta {
                        content_index: idx,
                        delta: delta.to_owned(),
                        partial: output.clone(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let delta = event.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(SlotState::ToolCall {
                    content_index,
                    args_stream,
                }) = self.slots.get_mut(&output_index)
                {
                    let idx = *content_index;
                    let partial_args = args_stream.append(delta);
                    if let Some(ContentBlock::ToolCall(tc)) = output.content.get_mut(idx) {
                        tc.arguments = partial_args;
                    }
                    stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: idx,
                        delta: delta.to_owned(),
                        partial: output.clone(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let full_args = event
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if let Some(SlotState::ToolCall {
                    content_index,
                    args_stream,
                }) = self.slots.get_mut(&output_index)
                {
                    let idx = *content_index;
                    let replaced = args_stream.replace(full_args);
                    if let Some(ContentBlock::ToolCall(tc)) = output.content.get_mut(idx) {
                        tc.arguments = replaced.arguments;
                    }
                    if let Some(delta) = replaced.delta {
                        stream.push(AssistantMessageEvent::ToolCallDelta {
                            content_index: idx,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            "response.output_item.done" => {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if let Some(item) = event.get("item") {
                    if !self.slots.contains_key(&output_index) {
                        self.create_slot(output_index, item, output, stream);
                    }
                    self.finish_slot(output_index, item, output, stream);
                }
            }
            "response.done" | "response.completed" | "response.incomplete" => {
                if let Some(resp) = event.get("response") {
                    self.finalize_response(resp, output, model);
                } else {
                    self.saw_terminal = true;
                    if output.stop_reason == StopReason::Pending {
                        output.stop_reason = if output
                            .content
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolCall(_)))
                        {
                            StopReason::ToolUse
                        } else {
                            StopReason::Stop
                        };
                    }
                }
            }
            "response.failed" => {
                self.saw_terminal = true;
                let resp = event.get("response");
                output.raw_stop_reason = resp
                    .and_then(|r| r.get("status"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_owned());
                let err_msg = resp
                    .and_then(|r| r.get("error"))
                    .map(|e| {
                        let code = e.get("code").and_then(|c| c.as_str()).unwrap_or("unknown");
                        let msg = e
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("no message");
                        format!("{code}: {msg}")
                    })
                    .unwrap_or_else(|| "Unknown error (no error details in response)".to_owned());
                return Err(err_msg);
            }
            "error" => {
                let code = event
                    .get("code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("unknown");
                let msg = event
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error");
                return Err(format!("Error Code {code}: {msg}"));
            }
            _ => {}
        }

        Ok(())
    }

    fn create_slot(
        &mut self,
        output_index: u64,
        item: &Value,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match item_type {
            "reasoning" => {
                output
                    .content
                    .push(ContentBlock::Thinking(ThinkingContent::new("")));
                let idx = output.content.len() - 1;
                self.slots
                    .insert(output_index, SlotState::Thinking { content_index: idx });
                stream.push(AssistantMessageEvent::ThinkingStart {
                    content_index: idx,
                    partial: output.clone(),
                });
            }
            "message" => {
                if item.get("phase").and_then(|v| v.as_str()) == Some("final_answer") {
                    output.stop_reason = StopReason::Stop;
                }
                output
                    .content
                    .push(ContentBlock::Text(TextContent::new("")));
                let idx = output.content.len() - 1;
                self.slots
                    .insert(output_index, SlotState::Text { content_index: idx });
                stream.push(AssistantMessageEvent::TextStart {
                    content_index: idx,
                    partial: output.clone(),
                });
            }
            "function_call" => {
                let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let init_args = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("");

                let mut args_stream = ToolCallArgumentsStream::new();
                let arguments = if init_args.is_empty() {
                    Map::new()
                } else {
                    args_stream.append(init_args)
                };

                let tc = ToolCall {
                    id: format!("{call_id}|{id}"),
                    name: name.to_owned(),
                    arguments,
                    thought_signature: None,
                    namespace: item
                        .get("namespace")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_owned()),
                    arguments_error: None,
                };
                output.content.push(ContentBlock::ToolCall(tc));
                let idx = output.content.len() - 1;
                self.unfinished_tool_calls.insert(idx);
                self.slots.insert(
                    output_index,
                    SlotState::ToolCall {
                        content_index: idx,
                        args_stream,
                    },
                );
                stream.push(AssistantMessageEvent::ToolCallStart {
                    content_index: idx,
                    partial: output.clone(),
                });
            }
            _ => {}
        }
    }

    fn finish_slot(
        &mut self,
        output_index: u64,
        item: &Value,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) {
        let Some(slot) = self.slots.remove(&output_index) else {
            return;
        };
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match (item_type, slot) {
            ("reasoning", SlotState::Thinking { content_index }) => {
                let summary_text = item
                    .get("summary")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                let content_text = item
                    .get("content")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();

                let mut final_thinking = String::new();
                if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(content_index) {
                    if !summary_text.is_empty() {
                        t.thinking = summary_text;
                    } else if !content_text.is_empty() {
                        t.thinking = content_text;
                    }
                    t.thinking_signature = serde_json::to_string(item).ok();
                    final_thinking = t.thinking.clone();
                }
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    self.reasoning_indices_by_id
                        .insert(id.to_owned(), content_index);
                }
                stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content: final_thinking,
                    partial: output.clone(),
                });
            }
            ("message", SlotState::Text { content_index }) => {
                if item.get("phase").and_then(|v| v.as_str()) == Some("final_answer") {
                    output.stop_reason = StopReason::Stop;
                }
                let full_text = item
                    .get("content")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|c| {
                                c.get("text")
                                    .or_else(|| c.get("refusal"))
                                    .and_then(|t| t.as_str())
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let phase = item.get("phase").and_then(|v| v.as_str());

                let mut final_text = full_text;
                if let Some(ContentBlock::Text(t)) = output.content.get_mut(content_index) {
                    if !final_text.is_empty() {
                        t.text = final_text.clone();
                    } else {
                        final_text = t.text.clone();
                    }
                    t.text_signature = encode_text_signature_v1(item_id, phase);
                }
                stream.push(AssistantMessageEvent::TextEnd {
                    content_index,
                    content: final_text,
                    partial: output.clone(),
                });
            }
            (
                "function_call",
                SlotState::ToolCall {
                    content_index,
                    mut args_stream,
                },
            ) => {
                if let Some(item_args) = item.get("arguments").and_then(|v| v.as_str())
                    && !item_args.is_empty()
                {
                    args_stream.replace(item_args);
                }
                let parsed = args_stream.finish();
                let mut finished_tc: Option<ToolCall> = None;
                if let Some(ContentBlock::ToolCall(tc)) = output.content.get_mut(content_index) {
                    parsed.apply_to(tc);
                    if let Some(ns) = item.get("namespace").and_then(|v| v.as_str()) {
                        tc.namespace = Some(ns.to_owned());
                    }
                    finished_tc = Some(tc.clone());
                }
                self.unfinished_tool_calls.remove(&content_index);
                if let Some(tc) = finished_tc {
                    stream.push(AssistantMessageEvent::ToolCallEnd {
                        content_index,
                        tool_call: tc,
                        partial: output.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn finalize_response(&mut self, resp: &Value, output: &mut AssistantMessage, model: &Model) {
        self.saw_terminal = true;

        // 回填 reasoning.encrypted_content
        if let Some(items) = resp.get("output").and_then(|v| v.as_array()) {
            for item in items {
                if item.get("type").and_then(|v| v.as_str()) != Some("reasoning") {
                    continue;
                }
                let Some(enc) = item.get("encrypted_content").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(&idx) = self.reasoning_indices_by_id.get(id) else {
                    continue;
                };
                if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(idx)
                    && let Some(sig) = &t.thinking_signature
                    && let Ok(mut stored) = serde_json::from_str::<Value>(sig)
                    && stored.get("encrypted_content").is_none_or(|v| v.is_null())
                {
                    if let Some(obj) = stored.as_object_mut() {
                        obj.insert(
                            "encrypted_content".to_owned(),
                            Value::String(enc.to_owned()),
                        );
                    }
                    t.thinking_signature = serde_json::to_string(&stored).ok();
                }
            }
        }

        if let Some(id) = resp.get("id").and_then(|v| v.as_str()) {
            output.response_id = Some(id.to_owned());
        }

        if let Some(usage) = resp.get("usage") {
            let input_tokens = usage
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output_tokens = usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cached_tokens = usage
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_write_tokens = usage
                .get("input_tokens_details")
                .and_then(|d| d.get("cache_write_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let reasoning_tokens = usage
                .get("output_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|v| v.as_u64());
            let total_tokens = usage
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(input_tokens + output_tokens);

            output.usage = Usage {
                input: input_tokens.saturating_sub(cached_tokens + cache_write_tokens),
                output: output_tokens,
                cache_read: cached_tokens,
                cache_write: cache_write_tokens,
                cache_write_1h: None,
                reasoning: reasoning_tokens,
                total_tokens,
                cost: UsageCost::default(),
            };
        }
        calculate_cost(&model.cost, &mut output.usage);

        let status = resp.get("status").and_then(|v| v.as_str());
        let incomplete_reason = resp
            .get("incomplete_details")
            .and_then(|d| d.get("reason"))
            .and_then(|v| v.as_str());
        output.raw_stop_reason = match (status, incomplete_reason) {
            (Some(s), Some(r)) => Some(format!("{s}.{r}")),
            (Some(s), None) => Some(s.to_owned()),
            _ => None,
        };

        let (mapped_stop, err_msg) = map_responses_stop_reason(status, incomplete_reason);
        output.stop_reason = mapped_stop;
        output.error_message = err_msg;

        if output
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolCall(_)))
            && output.stop_reason == StopReason::Stop
        {
            output.stop_reason = StopReason::ToolUse;
        }
    }

    pub fn finish(&self, output: &AssistantMessage) -> Result<(), String> {
        if !self.saw_terminal {
            return Err(
                "OpenAI Responses stream ended before a terminal response event".to_owned(),
            );
        }
        if output.stop_reason == StopReason::ToolUse && !self.unfinished_tool_calls.is_empty() {
            return Err(
                "OpenAI Responses stream completed with an unfinished tool call".to_owned(),
            );
        }
        Ok(())
    }
}

pub fn resolve_responses_endpoint_url(model: &Model) -> String {
    let raw = if model.base_url.trim().is_empty() {
        if is_codex_model(model) {
            "https://chatgpt.com/backend-api"
        } else {
            "https://api.openai.com/v1"
        }
    } else {
        model.base_url.trim()
    };
    let trimmed = raw.trim_end_matches('/');
    if is_codex_model(model)
        || trimmed.contains("chatgpt.com/backend-api")
        || trimmed.ends_with("/codex")
        || trimmed.ends_with("/codex/responses")
    {
        if trimmed.ends_with("/codex/responses") {
            trimmed.to_owned()
        } else if trimmed.ends_with("/codex") {
            format!("{trimmed}/responses")
        } else {
            format!("{trimmed}/codex/responses")
        }
    } else if trimmed == "https://api.deepseek.com" {
        "https://api.deepseek.com/v1/responses".to_owned()
    } else if trimmed.ends_with("/responses") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/responses")
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

            let cache_retention = resolve_cache_retention(
                options.as_ref().and_then(|o| o.cache_retention),
                options.as_ref().and_then(|o| o.env.as_ref()),
            );
            let cache_session_id = if cache_retention == CacheRetention::None {
                None
            } else {
                options.as_ref().and_then(|o| o.session_id.as_deref())
            };

            let mut opt_headers: ProviderHeaders = options
                .as_ref()
                .and_then(|o| o.headers.clone())
                .unwrap_or_default();
            if let Some(sid) = cache_session_id {
                match detect_session_affinity_format(&model) {
                    SessionAffinityFormat::OpenRouter => {
                        opt_headers
                            .entry("x-session-id".to_owned())
                            .or_insert_with(|| Some(sid.to_owned()));
                    }
                    SessionAffinityFormat::OpenAi => {
                        opt_headers
                            .entry("session_id".to_owned())
                            .or_insert_with(|| Some(sid.to_owned()));
                        opt_headers
                            .entry("x-client-request-id".to_owned())
                            .or_insert_with(|| Some(sid.to_owned()));
                    }
                    SessionAffinityFormat::OpenAiNoSession => {
                        opt_headers
                            .entry("x-client-request-id".to_owned())
                            .or_insert_with(|| Some(sid.to_owned()));
                    }
                }
            }

            if is_codex_model(&model)
                && !opt_headers
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case("chatgpt-account-id"))
                && let Some(account_id) = extract_chatgpt_account_id(&api_key)
            {
                opt_headers.insert("ChatGPT-Account-Id".to_owned(), Some(account_id));
            }
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

            let payload = build_responses_params(&model, &context, options.as_ref());
            let url = resolve_responses_endpoint_url(&model);
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

            writer.push(AssistantMessageEvent::Start {
                partial: output.clone(),
            });

            let mut byte_stream = resp.bytes_stream();
            let mut sse_parser = SseParser::new();
            let mut processor = ResponsesStreamProcessor::new();

            'read_stream: loop {
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
                    processor.handle_sse(&sse, &mut output, &writer, &model)?;
                    if processor.saw_terminal {
                        break 'read_stream;
                    }
                }
            }

            if !processor.saw_terminal {
                for sse in sse_parser.finish() {
                    processor.handle_sse(&sse, &mut output, &writer, &model)?;
                    if processor.saw_terminal {
                        break;
                    }
                }
            }
            processor.finish(&output)?;

            if output.stop_reason == StopReason::Pending {
                return Err("OpenAI Responses stream ended without a stop reason".to_owned());
            }
            if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
                return Err(output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "An unknown error occurred".to_owned()));
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

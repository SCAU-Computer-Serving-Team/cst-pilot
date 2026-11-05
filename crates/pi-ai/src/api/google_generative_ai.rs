use futures_util::StreamExt;
use serde_json::{Map, Value, json};

use crate::api::{
    SseEvent, SseParser, build_base_options, build_header_map, calculate_cost,
    clamp_thinking_level, invoke_on_payload, invoke_on_provider_stream_event, invoke_on_response,
    require_client_api_key, send_with_retry, thinking_budget_for_level,
};
use crate::transform_messages::{
    collapse_system_messages, get_current_tools, get_initial_system_message,
    get_system_message_text, transform_messages, without_initial_system_message,
};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, ContentBlock, Message,
    Model, ModelThinkingLevel, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, Tool, ToolCall, ToolChoice, TranscriptContext, Usage, UsageCost, UserContent,
    UserContentBlock, create_assistant_message_event_stream,
};

fn is_valid_base64_signature(sig: Option<&str>) -> bool {
    let Some(s) = sig else {
        return false;
    };
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
}

fn uses_google_thinking_level(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    lower.starts_with("gemini-3")
        || lower.starts_with("gemini-4")
        || lower.starts_with("gemini-5")
        || lower == "gemini-flash-latest"
        || lower == "gemini-flash-lite-latest"
        || lower.contains("gemma-4")
        || lower.contains("gemma4")
}

fn get_gemini_major_version(model_id: &str) -> Option<u32> {
    let lower = model_id.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("gemini-live-")
        .or_else(|| lower.strip_prefix("gemini-"))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u32>().ok()
}

fn supports_multimodal_function_response(model_id: &str) -> bool {
    match get_gemini_major_version(model_id) {
        Some(v) => v >= 3,
        None => true,
    }
}

fn requires_google_tool_call_id(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    lower.starts_with("claude-")
        || lower.starts_with("gpt-oss-")
        || get_gemini_major_version(model_id).is_some_and(|v| v >= 3)
}

fn rewrite_google_schema(schema: &Value, name_map: bool) -> Value {
    match schema {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| rewrite_google_schema(v, false))
                .collect(),
        ),
        Value::Object(object) => {
            let mut result: Map<String, Value> = object
                .iter()
                .map(|(key, value)| {
                    let map = matches!(
                        key.as_str(),
                        "properties"
                            | "patternProperties"
                            | "$defs"
                            | "definitions"
                            | "dependentSchemas"
                            | "dependencies"
                    );
                    let subschema = matches!(
                        key.as_str(),
                        "additionalProperties"
                            | "unevaluatedProperties"
                            | "propertyNames"
                            | "items"
                            | "prefixItems"
                            | "additionalItems"
                            | "unevaluatedItems"
                            | "contains"
                            | "allOf"
                            | "anyOf"
                            | "oneOf"
                            | "not"
                            | "if"
                            | "then"
                            | "else"
                            | "contentSchema"
                    );
                    (
                        key.clone(),
                        if name_map || subschema {
                            rewrite_google_schema(value, false)
                        } else if map {
                            rewrite_google_schema(value, true)
                        } else {
                            value.clone()
                        },
                    )
                })
                .collect();
            if !name_map
                && !result.contains_key("enum")
                && let Some(value @ (Value::String(_) | Value::Number(_))) =
                    result.get("const").cloned()
            {
                result.remove("const");
                result.insert("enum".to_owned(), json!([value]));
            }
            Value::Object(result)
        }
        _ => schema.clone(),
    }
}

pub fn convert_google_tools(tools: &[Tool]) -> Vec<Value> {
    if tools.is_empty() {
        return Vec::new();
    }
    let decls: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "parametersJsonSchema": rewrite_google_schema(&t.parameters, false),
            })
        })
        .collect();
    vec![json!({ "functionDeclarations": decls })]
}

pub fn convert_google_messages(model: &Model, context: &TranscriptContext) -> Vec<Value> {
    let collapsed = collapse_system_messages(context);
    let conversation = without_initial_system_message(collapsed.messages());
    let transformed = transform_messages(
        &conversation,
        model,
        Some(|id: &str, m: &Model, _a: &AssistantMessage| {
            if !requires_google_tool_call_id(&m.id) {
                return id.to_owned();
            }
            id.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .take(64)
                .collect()
        }),
    );

    let mut contents: Vec<Value> = Vec::new();
    let mut i = 0usize;

    while i < transformed.len() {
        match &transformed[i] {
            Message::System(_) => {}
            Message::User(user) => match &user.content {
                UserContent::Text(t) => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{ "text": t }],
                    }));
                }
                UserContent::Blocks(blocks) => {
                    let parts: Vec<Value> = blocks
                        .iter()
                        .map(|b| match b {
                            UserContentBlock::Text(t) => json!({ "text": t.text }),
                            UserContentBlock::Image(img) => json!({
                                "inlineData": {
                                    "mimeType": img.mime_type,
                                    "data": img.data,
                                }
                            }),
                        })
                        .collect();
                    if !parts.is_empty() {
                        contents.push(json!({
                            "role": "user",
                            "parts": parts,
                        }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let is_same = assistant.provider == model.provider && assistant.model == model.id;
                let mut parts: Vec<Value> = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking(thinking) => {
                            if !is_same {
                                continue;
                            }
                            let sig = thinking
                                .thinking_signature
                                .as_deref()
                                .filter(|s| is_valid_base64_signature(Some(s)));
                            if thinking.thinking.trim().is_empty() && sig.is_none() {
                                continue;
                            }
                            let mut part = Map::new();
                            part.insert("thought".to_owned(), Value::Bool(true));
                            part.insert(
                                "text".to_owned(),
                                Value::String(thinking.thinking.clone()),
                            );
                            if let Some(s) = sig {
                                part.insert(
                                    "thoughtSignature".to_owned(),
                                    Value::String(s.to_owned()),
                                );
                            }
                            parts.push(Value::Object(part));
                        }
                        ContentBlock::Text(text) => {
                            let sig = text
                                .text_signature
                                .as_deref()
                                .filter(|s| is_same && is_valid_base64_signature(Some(s)));
                            if text.text.trim().is_empty() && sig.is_none() {
                                continue;
                            }
                            let mut part = Map::new();
                            part.insert("text".to_owned(), Value::String(text.text.clone()));
                            if let Some(s) = sig {
                                part.insert(
                                    "thoughtSignature".to_owned(),
                                    Value::String(s.to_owned()),
                                );
                            }
                            parts.push(Value::Object(part));
                        }
                        ContentBlock::ToolCall(tc) => {
                            let sig = tc
                                .thought_signature
                                .as_deref()
                                .filter(|s| is_same && is_valid_base64_signature(Some(s)));
                            let mut fc = Map::new();
                            fc.insert("name".to_owned(), Value::String(tc.name.clone()));
                            fc.insert("args".to_owned(), Value::Object(tc.arguments.clone()));
                            if requires_google_tool_call_id(&model.id) && !tc.id.is_empty() {
                                fc.insert("id".to_owned(), Value::String(tc.id.clone()));
                            }
                            let mut part = Map::new();
                            part.insert("functionCall".to_owned(), Value::Object(fc));
                            if let Some(s) = sig {
                                part.insert(
                                    "thoughtSignature".to_owned(),
                                    Value::String(s.to_owned()),
                                );
                            }
                            parts.push(Value::Object(part));
                        }
                    }
                }

                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "model",
                        "parts": parts,
                    }));
                }
            }
            Message::ToolResult(_) => {
                let mut parts: Vec<Value> = Vec::new();
                let mut legacy_image_parts: Vec<Value> = Vec::new();
                let multimodal_fn_resp = supports_multimodal_function_response(&model.id);
                let mut j = i;
                while j < transformed.len() {
                    let Message::ToolResult(tr) = &transformed[j] else {
                        break;
                    };
                    let text_value = tr
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            UserContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let image_parts: Vec<Value> = if model.supports_image_input() {
                        tr.content
                            .iter()
                            .filter_map(|b| match b {
                                UserContentBlock::Image(img) => Some(json!({
                                    "inlineData": {
                                        "mimeType": img.mime_type,
                                        "data": img.data,
                                    }
                                })),
                                _ => None,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let has_text = !text_value.trim().is_empty();
                    let has_images = !image_parts.is_empty();
                    let response_value = if has_text {
                        text_value
                    } else if has_images {
                        "(see attached image)".to_owned()
                    } else if tr.is_error {
                        "Tool execution failed".to_owned()
                    } else {
                        String::new()
                    };
                    let resp_obj = if tr.is_error {
                        json!({ "error": response_value })
                    } else {
                        json!({ "output": response_value })
                    };
                    let mut fr = Map::new();
                    fr.insert("name".to_owned(), Value::String(tr.tool_name.clone()));
                    fr.insert("response".to_owned(), resp_obj);
                    if has_images && multimodal_fn_resp {
                        fr.insert("parts".to_owned(), Value::Array(image_parts));
                    } else if has_images {
                        legacy_image_parts.extend(image_parts);
                    }
                    if requires_google_tool_call_id(&model.id) && !tr.tool_call_id.is_empty() {
                        fr.insert("id".to_owned(), Value::String(tr.tool_call_id.clone()));
                    }
                    parts.push(json!({ "functionResponse": Value::Object(fr) }));
                    j += 1;
                }
                i = j - 1;
                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "user",
                        "parts": parts,
                    }));
                }
                if !legacy_image_parts.is_empty() {
                    let mut user_img_parts = vec![json!({ "text": "Tool result image:" })];
                    user_img_parts.extend(legacy_image_parts);
                    contents.push(json!({
                        "role": "user",
                        "parts": user_img_parts,
                    }));
                }
            }
        }
        i += 1;
    }

    contents
}

pub fn build_google_params(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&StreamOptions>,
) -> Value {
    let collapsed = collapse_system_messages(context);
    let initial_sys = get_initial_system_message(collapsed.messages());
    let sys_text = initial_sys.map(get_system_message_text).unwrap_or_default();
    let contents = convert_google_messages(model, &collapsed);
    let tools = get_current_tools(collapsed.messages());

    let mut params = Map::new();
    params.insert("contents".to_owned(), Value::Array(contents));

    if !sys_text.is_empty() {
        params.insert(
            "systemInstruction".to_owned(),
            json!({
                "parts": [{ "text": sys_text }]
            }),
        );
    }

    if !tools.is_empty() {
        params.insert(
            "tools".to_owned(),
            Value::Array(convert_google_tools(&tools)),
        );
        if let Some(tc) = options.and_then(|o| o.tool_choice) {
            let mode = match tc {
                ToolChoice::Auto => "AUTO",
                ToolChoice::None => "NONE",
            };
            params.insert(
                "toolConfig".to_owned(),
                json!({
                    "functionCallingConfig": { "mode": mode }
                }),
            );
        }
    }

    let mut gen_config = Map::new();
    if let Some(max_tok) = options.and_then(|o| o.max_tokens)
        && max_tok > 0
    {
        gen_config.insert("maxOutputTokens".to_owned(), Value::from(max_tok));
    }
    if let Some(temp) = options.and_then(|o| o.temperature) {
        gen_config.insert("temperature".to_owned(), json!(temp));
    }

    if model.reasoning {
        if let Some(level) = options.and_then(|o| o.reasoning_effort) {
            if uses_google_thinking_level(&model.id) {
                let lvl_str = match level {
                    crate::types::ThinkingLevel::Minimal => "MINIMAL",
                    crate::types::ThinkingLevel::Low => "LOW",
                    crate::types::ThinkingLevel::Medium => "MEDIUM",
                    _ => "HIGH",
                };
                gen_config.insert(
                    "thinkingConfig".to_owned(),
                    json!({
                        "includeThoughts": true,
                        "thinkingLevel": lvl_str,
                    }),
                );
            } else {
                let budget = thinking_budget_for_level(
                    level,
                    options.and_then(|o| o.thinking_budgets.as_ref()),
                );
                gen_config.insert(
                    "thinkingConfig".to_owned(),
                    json!({
                        "includeThoughts": true,
                        "thinkingBudget": budget,
                    }),
                );
            }
        } else if uses_google_thinking_level(&model.id) {
            let fallback = clamp_thinking_level(model, ModelThinkingLevel::Off);
            if fallback == ModelThinkingLevel::Off {
                gen_config.insert("thinkingConfig".to_owned(), json!({ "thinkingBudget": 0 }));
            } else {
                let lvl_str = match fallback {
                    ModelThinkingLevel::Minimal => "MINIMAL",
                    ModelThinkingLevel::Low => "LOW",
                    ModelThinkingLevel::Medium => "MEDIUM",
                    _ => "HIGH",
                };
                gen_config.insert(
                    "thinkingConfig".to_owned(),
                    json!({ "thinkingLevel": lvl_str }),
                );
            }
        } else {
            gen_config.insert("thinkingConfig".to_owned(), json!({ "thinkingBudget": 0 }));
        }
    }

    if !gen_config.is_empty() {
        params.insert("generationConfig".to_owned(), Value::Object(gen_config));
    }

    Value::Object(params)
}

// =============================================================================
// Google Stream Processing
// =============================================================================

pub struct GoogleStreamProcessor {
    current_thinking_index: Option<usize>,
    current_text_index: Option<usize>,
    tool_counter: usize,
}

impl Default for GoogleStreamProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleStreamProcessor {
    pub fn new() -> Self {
        Self {
            current_thinking_index: None,
            current_text_index: None,
            tool_counter: 0,
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
            serde_json::from_str(data).map_err(|e| format!("Invalid Google SSE JSON: {e}"))?;

        if let Some(err) = chunk.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Google API error");
            return Err(msg.to_owned());
        }

        if let Some(id) = chunk.get("responseId").and_then(|v| v.as_str()) {
            output.response_id = Some(id.to_owned());
        }
        if let Some(ver) = chunk.get("modelVersion").and_then(|v| v.as_str()) {
            output.response_model = Some(ver.to_owned());
        }

        if let Some(candidates) = chunk.get("candidates").and_then(|v| v.as_array())
            && let Some(candidate) = candidates.first()
        {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    let is_thought = part.get("thought").and_then(|v| v.as_bool()) == Some(true);
                    let thought_sig = part
                        .get("thoughtSignature")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());

                    if is_thought {
                        self.close_text_block(output, stream);
                        let delta = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        let idx = match self.current_thinking_index {
                            Some(i) => i,
                            None => {
                                output
                                    .content
                                    .push(ContentBlock::Thinking(ThinkingContent::new("")));
                                let new_idx = output.content.len() - 1;
                                self.current_thinking_index = Some(new_idx);
                                stream.push(AssistantMessageEvent::ThinkingStart {
                                    content_index: new_idx,
                                    partial: output.clone(),
                                });
                                new_idx
                            }
                        };
                        if let Some(ContentBlock::Thinking(t)) = output.content.get_mut(idx) {
                            t.thinking.push_str(delta);
                            if let Some(sig) = thought_sig {
                                t.thinking_signature = Some(sig.to_owned());
                            }
                        }
                        if !delta.is_empty() {
                            stream.push(AssistantMessageEvent::ThinkingDelta {
                                content_index: idx,
                                delta: delta.to_owned(),
                                partial: output.clone(),
                            });
                        }
                    } else if let Some(text_delta) = part.get("text").and_then(|v| v.as_str()) {
                        self.close_thinking_block(output, stream);
                        let idx = match self.current_text_index {
                            Some(i) => i,
                            None => {
                                output
                                    .content
                                    .push(ContentBlock::Text(TextContent::new("")));
                                let new_idx = output.content.len() - 1;
                                self.current_text_index = Some(new_idx);
                                stream.push(AssistantMessageEvent::TextStart {
                                    content_index: new_idx,
                                    partial: output.clone(),
                                });
                                new_idx
                            }
                        };
                        if let Some(ContentBlock::Text(t)) = output.content.get_mut(idx) {
                            t.text.push_str(text_delta);
                            if let Some(sig) = thought_sig {
                                t.text_signature = Some(sig.to_owned());
                            }
                        }
                        if !text_delta.is_empty() {
                            stream.push(AssistantMessageEvent::TextDelta {
                                content_index: idx,
                                delta: text_delta.to_owned(),
                                partial: output.clone(),
                            });
                        }
                    } else if let Some(fc) = part.get("functionCall") {
                        self.close_thinking_block(output, stream);
                        self.close_text_block(output, stream);
                        self.tool_counter += 1;

                        let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let provided_id = fc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty());
                        let is_duplicate = provided_id.is_some_and(|pid| {
                            output
                                .content
                                .iter()
                                .any(|b| matches!(b, ContentBlock::ToolCall(tc) if tc.id == pid))
                        });
                        let id = match provided_id {
                            Some(pid) if !is_duplicate => pid.to_owned(),
                            _ => format!("{name}_{}_{}", crate::types::now_ms(), self.tool_counter),
                        };
                        let args = fc
                            .get("args")
                            .and_then(|v| v.as_object())
                            .cloned()
                            .unwrap_or_default();

                        let tc = ToolCall {
                            id,
                            name: name.to_owned(),
                            arguments: args.clone(),
                            thought_signature: thought_sig.map(|s| s.to_owned()),
                            namespace: None,
                            arguments_error: None,
                        };
                        output.content.push(ContentBlock::ToolCall(tc.clone()));
                        let c_idx = output.content.len() - 1;
                        let args_json =
                            serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_owned());
                        stream.push(AssistantMessageEvent::ToolCallStart {
                            content_index: c_idx,
                            partial: output.clone(),
                        });
                        stream.push(AssistantMessageEvent::ToolCallDelta {
                            content_index: c_idx,
                            delta: args_json,
                            partial: output.clone(),
                        });
                        stream.push(AssistantMessageEvent::ToolCallEnd {
                            content_index: c_idx,
                            tool_call: tc,
                            partial: output.clone(),
                        });
                    }
                }
            }

            if let Some(fr) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                output.raw_stop_reason = Some(fr.to_owned());
                match fr {
                    "STOP" => {
                        output.stop_reason = StopReason::Stop;
                    }
                    "MAX_TOKENS" => {
                        output.stop_reason = StopReason::Length;
                    }
                    other => {
                        output.stop_reason = StopReason::Error;
                        output.error_message = Some(format!("Google finishReason: {other}"));
                    }
                }
            }
        }

        if let Some(um) = chunk.get("usageMetadata") {
            let prompt_tokens = um
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cached_tokens = um
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let candidates_tokens = um
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let thoughts_tokens = um
                .get("thoughtsTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total_tokens = um
                .get("totalTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(prompt_tokens + candidates_tokens + thoughts_tokens);

            output.usage = Usage {
                input: prompt_tokens.saturating_sub(cached_tokens),
                output: candidates_tokens + thoughts_tokens,
                cache_read: cached_tokens,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: Some(thoughts_tokens),
                total_tokens,
                cost: UsageCost::default(),
            };
            calculate_cost(&model.cost, &mut output.usage);
        }

        Ok(())
    }

    fn close_thinking_block(
        &mut self,
        output: &AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) {
        if let Some(idx) = self.current_thinking_index.take() {
            let content = output
                .content
                .get(idx)
                .and_then(|b| b.as_thinking())
                .map(|t| t.thinking.clone())
                .unwrap_or_default();
            stream.push(AssistantMessageEvent::ThinkingEnd {
                content_index: idx,
                content,
                partial: output.clone(),
            });
        }
    }

    fn close_text_block(
        &mut self,
        output: &AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) {
        if let Some(idx) = self.current_text_index.take() {
            let content = output
                .content
                .get(idx)
                .and_then(|b| b.as_text())
                .map(|t| t.text.clone())
                .unwrap_or_default();
            stream.push(AssistantMessageEvent::TextEnd {
                content_index: idx,
                content,
                partial: output.clone(),
            });
        }
    }

    pub fn finish(
        &mut self,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) -> Result<(), String> {
        self.close_thinking_block(output, stream);
        self.close_text_block(output, stream);

        let has_tools = output
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolCall(_)));
        if has_tools && output.stop_reason == StopReason::Stop {
            output.stop_reason = StopReason::ToolUse;
        }
        if output.stop_reason == StopReason::Pending {
            output.stop_reason = StopReason::Error;
            output.error_message = Some("Google stream ended without a finish reason".to_owned());
            return Err("Google stream ended without a finish reason".to_owned());
        }
        if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
            return Err(output
                .error_message
                .clone()
                .unwrap_or_else(|| "Google stream returned an error finish reason".to_owned()));
        }
        Ok(())
    }
}

pub fn process_google_sse_events(
    events: &[SseEvent],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
) -> Result<(), String> {
    let mut processor = GoogleStreamProcessor::new();
    for sse in events {
        processor.handle_sse(sse, output, stream, model)?;
    }
    processor.finish(output, stream)
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

            let headers = build_header_map(
                options.as_ref().and_then(|o| o.identity.as_ref()),
                &[
                    ("x-goog-api-key", &api_key),
                    ("content-type", "application/json"),
                    ("accept", "text/event-stream"),
                ],
                model.headers.as_ref(),
                options.as_ref().and_then(|o| o.headers.as_ref()),
            );

            let headers = super::invoke_on_headers(options.as_ref(), headers, &model).await?;

            let raw_payload = build_google_params(&model, &context, options.as_ref());
            let payload = invoke_on_payload(options.as_ref(), raw_payload, &model).await?;
            let base = model.base_url.trim_end_matches('/');
            let url = format!("{base}/models/{}:streamGenerateContent?alt=sse", model.id);
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
            let mut processor = GoogleStreamProcessor::new();

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
            processor.finish(&mut output, &writer)?;

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

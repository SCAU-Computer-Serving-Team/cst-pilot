use std::collections::BTreeMap;

use futures_util::StreamExt;
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention,
    ContentBlock, JsonValue, Model, ProviderHeaders, SamplingParams, SseParser, StopReason,
    TextContent, ThinkingBudgets, ThinkingContent, ThinkingLevel, ToolCall,
    ToolCallArgumentsStream, TranscriptContext, Transport, Usage,
    create_assistant_message_event_stream,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use tokio_util::sync::CancellationToken;

/// 代理服务端剥离了 `partial` 字段后的精简流式事件（对照 `ProxyAssistantMessageEvent`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProxyAssistantMessageEvent {
    Start,
    TextStart {
        content_index: usize,
    },
    TextDelta {
        content_index: usize,
        delta: String,
    },
    TextEnd {
        content_index: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    ThinkingStart {
        content_index: usize,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        content_index: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        content_index: usize,
        id: String,
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        content_index: usize,
        tool_call: ToolCall,
    },
    Done {
        reason: StopReason,
        usage: Usage,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
    Error {
        reason: StopReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        #[serde(default)]
        usage: Usage,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySerializableStreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<SamplingParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, JsonValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<ThinkingBudgets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ProxyStreamOptions {
    pub cancel_token: Option<CancellationToken>,
    pub auth_token: String,
    pub proxy_url: String,
    pub stream_options: ProxySerializableStreamOptions,
}

/// 处理单条 `ProxyAssistantMessageEvent` 并原地更新 `partial: AssistantMessage` 与工具参数增量流。
pub fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    tool_arguments: &mut BTreeMap<usize, ToolCallArgumentsStream>,
) -> Result<Option<AssistantMessageEvent>, String> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Ok(Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        })),
        ProxyAssistantMessageEvent::TextStart { content_index } => {
            set_content_slot(
                &mut partial.content,
                content_index,
                ContentBlock::Text(TextContent::new("")),
            )?;
            Ok(Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            if let Some(ContentBlock::Text(t)) = partial.content.get_mut(content_index) {
                t.text.push_str(&delta);
                Ok(Some(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            } else {
                Err("Received text_delta for non-text content".to_owned())
            }
        }
        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            if let Some(ContentBlock::Text(t)) = partial.content.get_mut(content_index) {
                t.text_signature = content_signature;
                let content = t.text.clone();
                Ok(Some(AssistantMessageEvent::TextEnd {
                    content_index,
                    content,
                    partial: partial.clone(),
                }))
            } else {
                Err("Received text_end for non-text content".to_owned())
            }
        }
        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            set_content_slot(
                &mut partial.content,
                content_index,
                ContentBlock::Thinking(ThinkingContent::new("")),
            )?;
            Ok(Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            if let Some(ContentBlock::Thinking(t)) = partial.content.get_mut(content_index) {
                t.thinking.push_str(&delta);
                Ok(Some(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            } else {
                Err("Received thinking_delta for non-thinking content".to_owned())
            }
        }
        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            if let Some(ContentBlock::Thinking(t)) = partial.content.get_mut(content_index) {
                t.thinking_signature = content_signature;
                let content = t.thinking.clone();
                Ok(Some(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial: partial.clone(),
                }))
            } else {
                Err("Received thinking_end for non-thinking content".to_owned())
            }
        }
        ProxyAssistantMessageEvent::ToolCallStart {
            content_index,
            id,
            tool_name,
        } => {
            set_content_slot(
                &mut partial.content,
                content_index,
                ContentBlock::ToolCall(ToolCall {
                    id,
                    name: tool_name,
                    arguments: Map::new(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                }),
            )?;
            tool_arguments.insert(content_index, ToolCallArgumentsStream::new());
            Ok(Some(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
        } => {
            let Some(args_stream) = tool_arguments.get_mut(&content_index) else {
                return Err("Received toolcall_delta for non-toolCall content".to_owned());
            };
            let (refreshed, args_ref) = args_stream.append_with_change(&delta);
            if let Some(ContentBlock::ToolCall(tc)) = partial.content.get_mut(content_index) {
                if refreshed {
                    tc.arguments = args_ref.clone();
                }
                Ok(Some(AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            } else {
                Err("Received toolcall_delta for non-toolCall content".to_owned())
            }
        }
        ProxyAssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
        } => {
            if let Some(ContentBlock::ToolCall(tc)) = partial.content.get_mut(content_index) {
                *tc = tool_call.clone();
                tool_arguments.remove(&content_index);
                Ok(Some(AssistantMessageEvent::ToolCallEnd {
                    content_index,
                    tool_call,
                    partial: partial.clone(),
                }))
            } else {
                Ok(None)
            }
        }
        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Ok(Some(AssistantMessageEvent::Done {
                reason,
                message: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.error_message = error_message;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Ok(Some(AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            }))
        }
    }
}

fn set_content_slot(
    content: &mut Vec<ContentBlock>,
    index: usize,
    block: ContentBlock,
) -> Result<(), String> {
    if index > content.len() {
        return Err(format!("Received non-contiguous content index: {index}"));
    }
    if index == content.len() {
        content.push(block);
    } else {
        content[index] = block;
    }
    Ok(())
}

/// 通过远端代理服务端发起模型流式请求并在客户端原地重建 `AssistantMessageEventStream`（对照 `streamProxy`）。
pub fn stream_proxy(
    model: &Model,
    context: &TranscriptContext,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let writer = stream.clone();
    let model = model.clone();
    let context = context.clone();

    tokio::spawn(async move {
        let mut partial = AssistantMessage::new_pending(&model);
        let cancel_token = options.cancel_token.clone();

        let run_res: Result<bool, String> = async {
            if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                return Err("Request aborted by user".to_owned());
            }

            let url = format!("{}/api/stream", options.proxy_url.trim_end_matches('/'));
            let body = json!({
                "model": model,
                "context": {
                    "messages": context.messages(),
                },
                "options": options.stream_options,
            });

            let client = reqwest::Client::new();
            let req_fut = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", options.auth_token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send();

            let response = if let Some(token) = &cancel_token {
                tokio::select! {
                    _ = token.cancelled() => return Err("Request aborted by user".to_owned()),
                    res = req_fut => res.map_err(|e| e.to_string())?,
                }
            } else {
                req_fut.await.map_err(|e| e.to_string())?
            };

            if !response.status().is_success() {
                let status = response.status();
                let mut error_message = format!(
                    "Proxy error: {} {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("")
                )
                .trim_end()
                .to_owned();
                if let Ok(text) = response.text().await
                    && let Ok(val) = serde_json::from_str::<JsonValue>(&text)
                    && let Some(err_str) = val.get("error").and_then(|v| v.as_str())
                    && !err_str.is_empty()
                {
                    error_message = format!("Proxy error: {err_str}");
                }
                return Err(error_message);
            }

            let mut byte_stream = response.bytes_stream();
            let mut sse_parser = SseParser::new();
            let mut tool_arguments: BTreeMap<usize, ToolCallArgumentsStream> = BTreeMap::new();
            let mut saw_terminal_event = false;

            loop {
                let next_chunk = if let Some(token) = &cancel_token {
                    tokio::select! {
                        _ = token.cancelled() => return Err("Request aborted by user".to_owned()),
                        chunk = byte_stream.next() => chunk,
                    }
                } else {
                    byte_stream.next().await
                };

                let Some(chunk_res) = next_chunk else {
                    break;
                };
                if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                    return Err("Request aborted by user".to_owned());
                }

                let bytes = chunk_res.map_err(|e| e.to_string())?;
                for sse in sse_parser.feed_bytes(&bytes) {
                    let data = sse.data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    let proxy_ev: ProxyAssistantMessageEvent =
                        serde_json::from_str(data).map_err(|e| e.to_string())?;
                    if let Some(ev) =
                        process_proxy_event(proxy_ev, &mut partial, &mut tool_arguments)?
                    {
                        if matches!(
                            ev,
                            AssistantMessageEvent::Done { .. }
                                | AssistantMessageEvent::Error { .. }
                        ) {
                            saw_terminal_event = true;
                        }
                        writer.push(ev);
                    }
                }
            }

            if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                return Err("Request aborted by user".to_owned());
            }

            for sse in sse_parser.finish() {
                let data = sse.data.trim();
                if data.is_empty() {
                    continue;
                }
                let proxy_ev: ProxyAssistantMessageEvent =
                    serde_json::from_str(data).map_err(|e| e.to_string())?;
                if let Some(ev) = process_proxy_event(proxy_ev, &mut partial, &mut tool_arguments)?
                {
                    if matches!(
                        ev,
                        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                    ) {
                        saw_terminal_event = true;
                    }
                    writer.push(ev);
                }
            }

            Ok(saw_terminal_event)
        }
        .await;

        match run_res {
            Ok(true) => {
                writer.end(None);
            }
            Ok(false) => {
                partial.stop_reason = StopReason::Error;
                partial.error_message = Some(
                    "Connection closed by proxy server before the response completed".to_owned(),
                );
                writer.push(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: partial,
                });
                writer.end(None);
            }
            Err(err_msg) => {
                let is_aborted = cancel_token.as_ref().is_some_and(|t| t.is_cancelled());
                let reason = if is_aborted {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                partial.stop_reason = reason;
                partial.error_message = Some(err_msg);
                writer.push(AssistantMessageEvent::Error {
                    reason,
                    error: partial,
                });
                writer.end(None);
            }
        }
    });

    stream
}

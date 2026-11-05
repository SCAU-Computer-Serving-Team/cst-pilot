use pi_agent::AgentMessage;
use pi_ai::{AssistantMessageEvent, ContentBlock};
use pi_session::AgentSessionEvent;
use serde_json::{Value, json};

/// 将 `AgentSessionEvent` 转换为面向 JSON 流与 RPC `stdout` 的线协议事件（对照 `toJsonEvent`）：
/// 剥离 `message_update` 中的累积 `partial` 快照，保留常量大小的 `usage` 以及 `toolcall_start` 的 `id` 与 `toolName`。
pub fn to_json_event(event: &AgentSessionEvent) -> Result<Value, String> {
    match event {
        AgentSessionEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            let AgentMessage::Assistant(asst) = message else {
                return Err("message_update message is not an assistant message".to_owned());
            };
            // Select fields before serialization: cumulative partial snapshots may be huge.
            let sub_val = match assistant_message_event {
                AssistantMessageEvent::Start { .. } => json!({"type":"start"}),
                AssistantMessageEvent::TextStart { content_index, .. } => {
                    json!({"type":"text_start", "contentIndex":content_index})
                }
                AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    ..
                } => json!({"type":"text_delta", "contentIndex":content_index, "delta":delta}),
                AssistantMessageEvent::TextEnd {
                    content_index,
                    content,
                    ..
                } => json!({"type":"text_end", "contentIndex":content_index, "content":content}),
                AssistantMessageEvent::ThinkingStart { content_index, .. } => {
                    json!({"type":"thinking_start", "contentIndex":content_index})
                }
                AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    ..
                } => json!({"type":"thinking_delta", "contentIndex":content_index, "delta":delta}),
                AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    ..
                } => {
                    json!({"type":"thinking_end", "contentIndex":content_index, "content":content})
                }
                AssistantMessageEvent::ToolCallStart {
                    content_index,
                    partial,
                } => {
                    let Some(ContentBlock::ToolCall(tc)) = partial.content.get(*content_index)
                    else {
                        return Err(format!(
                            "toolcall_start content at index {content_index} is not a tool call"
                        ));
                    };
                    json!({"type":"toolcall_start", "contentIndex":content_index, "id":tc.id, "toolName":tc.name})
                }
                AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    ..
                } => json!({"type":"toolcall_delta", "contentIndex":content_index, "delta":delta}),
                AssistantMessageEvent::ToolCallEnd {
                    content_index,
                    tool_call,
                    ..
                } => {
                    json!({"type":"toolcall_end", "contentIndex":content_index, "toolCall":tool_call})
                }
                AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                    serde_json::to_value(assistant_message_event)
                        .map_err(|_| "Failed to serialize terminal assistant event".to_owned())?
                }
            };
            Ok(json!({
                "type": "message_update",
                "usage": asst.usage,
                "assistantMessageEvent": sub_val,
            }))
        }
        other => serde_json::to_value(other)
            .map_err(|e| format!("Failed to serialize session event: {e}")),
    }
}

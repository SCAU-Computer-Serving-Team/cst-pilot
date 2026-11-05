use std::collections::{BTreeMap, BTreeSet};

use crate::types::{
    AssistantMessage, ContentBlock, Context, ImageContent, Message, Model, StopReason,
    SystemContent, SystemMessage, TextContent, Tool, ToolCall, ToolReference, ToolResultMessage,
    TranscriptContext, UserContent, UserContentBlock, UserMessage, now_ms,
};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";
const ERROR_TOOL_IMAGE_PLACEHOLDER: &str = "(image attached after the tool results)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitErrorToolResult {
    pub content: Vec<TextContent>,
    pub caption: String,
    pub images: Vec<ImageContent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptToolSupport {
    None,
    Additions,
    Changes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptTools {
    pub request_tools: Vec<Tool>,
    pub folded_system_messages: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolStateChanges {
    pub tools_added: Vec<Tool>,
    pub tools_removed: Vec<ToolReference>,
}

// =============================================================================
// System Message & Transcript Helpers (对照 utils/text.ts, utils/transcript.ts)
// =============================================================================

pub fn get_system_message_text(message: &SystemMessage) -> String {
    let mut parts = Vec::new();
    let main = message.content.as_plain_text();
    if !main.is_empty() {
        parts.push(main);
    }
    if let Some(sections) = &message.sections {
        // Keep stable instructions before volatile project/skill/cwd sections.
        const ORDER: &[&str] = &[
            "preamble",
            "tools",
            "rules",
            "docs",
            "addendum",
            "project_context",
            "skills",
            "cwd",
        ];
        let ordered = ORDER.iter().filter_map(|key| sections.get(*key));
        let custom = sections
            .iter()
            .filter(|(key, _)| !ORDER.contains(&key.as_str()))
            .map(|(_, value)| value);
        for text in ordered.chain(custom).flatten() {
            if !text.is_empty() {
                parts.push(text.clone());
            }
        }
    }
    parts.join("\n\n")
}

pub fn render_system_message_update(message: &SystemMessage) -> String {
    let mut parts = Vec::new();
    let text = message.content.as_plain_text();
    if !text.is_empty() {
        parts.push(text);
    }
    if let Some(sections) = &message.sections {
        for (name, value) in sections {
            match value {
                None => parts.push(format!("Removed system prompt section \"{name}\".")),
                Some(v) => parts.push(format!("Updated system prompt section \"{name}\":\n\n{v}")),
            }
        }
    }
    parts.join("\n\n")
}

pub fn render_tool_removal_notice(removed: &[String]) -> String {
    if removed.is_empty() {
        return String::new();
    }
    let list = removed
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if removed.len() == 1 {
        format!("Tool {list} is no longer available. Do not call it.")
    } else {
        format!("Tools {list} are no longer available. Do not call them.")
    }
}

pub fn render_incremental_tools_notice(unavailable: &BTreeSet<String>) -> String {
    let base =
        "Additional tools are declared below. Earlier tools remain available unless removed.";
    if unavailable.is_empty() {
        return base.to_owned();
    }
    let removed: Vec<String> = unavailable.iter().cloned().collect();
    format!("{base} {}", render_tool_removal_notice(&removed))
}

pub fn create_initial_system_message(
    system_prompt: Option<&str>,
    tools: Option<&[Tool]>,
) -> Option<SystemMessage> {
    let has_prompt = system_prompt.is_some_and(|s| !s.is_empty());
    let has_tools = tools.is_some_and(|t| !t.is_empty());
    if !has_prompt && !has_tools {
        return None;
    }
    Some(SystemMessage {
        content: SystemContent::Text(system_prompt.unwrap_or("").to_owned()),
        sections: None,
        tools_added: if has_tools {
            tools.map(|t| t.to_vec())
        } else {
            None
        },
        tools_removed: None,
        timestamp: 0,
    })
}

/// 将 `Context.system_prompt` 与 `Context.tools` 折叠为首条 `SystemMessage`。
pub fn normalize_context(context: &Context) -> TranscriptContext {
    let initial =
        create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
    let mut messages = Vec::with_capacity(context.messages.len() + usize::from(initial.is_some()));
    if let Some(sys) = initial {
        messages.push(Message::System(sys));
    }
    messages.extend(context.messages.iter().cloned());
    TranscriptContext { messages }
}

pub fn get_initial_system_message(messages: &[Message]) -> Option<&SystemMessage> {
    match messages.first() {
        Some(Message::System(sys)) => Some(sys),
        _ => None,
    }
}

pub fn without_initial_system_message(messages: &[Message]) -> Vec<Message> {
    if get_initial_system_message(messages).is_some() {
        messages[1..].to_vec()
    } else {
        messages.to_vec()
    }
}

pub fn get_current_tools(messages: &[Message]) -> Vec<Tool> {
    let mut order: Vec<String> = Vec::new();
    let mut tools: BTreeMap<String, Tool> = BTreeMap::new();
    for message in messages {
        let Message::System(sys) = message else {
            continue;
        };
        if let Some(removed) = &sys.tools_removed {
            for r in removed {
                tools.remove(&r.name);
                order.retain(|n| n != &r.name);
            }
        }
        if let Some(added) = &sys.tools_added {
            for tool in added {
                if !tools.contains_key(&tool.name) {
                    order.push(tool.name.clone());
                }
                tools.insert(tool.name.clone(), tool.clone());
            }
        }
    }
    order
        .into_iter()
        .filter_map(|name| tools.remove(&name))
        .collect()
}

pub fn get_current_system_message(messages: &[Message]) -> Option<SystemMessage> {
    let mut content_parts = Vec::new();
    let mut sections: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut timestamp: Option<u64> = None;

    for message in messages {
        let Message::System(sys) = message else {
            continue;
        };
        if timestamp.is_none() {
            timestamp = Some(sys.timestamp);
        }
        let text = sys.content.as_plain_text();
        if !text.is_empty() {
            content_parts.push(text);
        }
        if let Some(sys_sections) = &sys.sections {
            for (name, value) in sys_sections {
                if value.is_none() {
                    sections.remove(name);
                } else {
                    sections.insert(name.clone(), value.clone());
                }
            }
        }
    }

    let tools = get_current_tools(messages);
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }

    Some(SystemMessage {
        content: SystemContent::Text(content_parts.join("\n\n")),
        sections: if sections.is_empty() {
            None
        } else {
            Some(sections)
        },
        tools_added: if tools.is_empty() { None } else { Some(tools) },
        tools_removed: None,
        timestamp: timestamp.unwrap_or(0),
    })
}

pub fn get_current_system_prompt(messages: &[Message]) -> String {
    get_current_system_message(messages)
        .map(|msg| get_system_message_text(&msg))
        .unwrap_or_default()
}

pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
    let head = get_current_system_message(&context.messages);
    let mut messages = Vec::with_capacity(context.messages.len());
    if let Some(sys) = head {
        messages.push(Message::System(sys));
    }
    for msg in &context.messages {
        if !matches!(msg, Message::System(_)) {
            messages.push(msg.clone());
        }
    }
    TranscriptContext { messages }
}

pub fn resolve_transcript(
    context: &TranscriptContext,
    supports_mid_convo_system_messages: bool,
) -> TranscriptContext {
    if supports_mid_convo_system_messages {
        context.clone()
    } else {
        collapse_system_messages(context)
    }
}

pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
    left.name == right.name
        && left.description == right.description
        && left.parameters == right.parameters
        && left.constrained_sampling == right.constrained_sampling
}

pub fn get_tool_state_changes(previous: &[Tool], current: &[Tool]) -> ToolStateChanges {
    let prev_map: BTreeMap<&str, &Tool> = previous.iter().map(|t| (t.name.as_str(), t)).collect();
    let curr_map: BTreeMap<&str, &Tool> = current.iter().map(|t| (t.name.as_str(), t)).collect();

    let tools_added = current
        .iter()
        .filter(|tool| match prev_map.get(tool.name.as_str()) {
            None => true,
            Some(prev) => !declarations_equal(prev, tool),
        })
        .cloned()
        .collect();

    let tools_removed = previous
        .iter()
        .filter(|tool| match curr_map.get(tool.name.as_str()) {
            None => true,
            Some(curr) => !declarations_equal(tool, curr),
        })
        .map(|tool| ToolReference {
            name: tool.name.clone(),
        })
        .collect();

    ToolStateChanges {
        tools_added,
        tools_removed,
    }
}

pub fn get_declared_tools(messages: &[Message]) -> Vec<Tool> {
    let mut order: Vec<String> = Vec::new();
    let mut definitions: BTreeMap<String, Tool> = BTreeMap::new();
    for message in messages {
        let Message::System(sys) = message else {
            continue;
        };
        if let Some(added) = &sys.tools_added {
            for tool in added {
                if !definitions.contains_key(&tool.name) {
                    order.push(tool.name.clone());
                }
                definitions.insert(tool.name.clone(), tool.clone());
            }
        }
    }
    order
        .into_iter()
        .filter_map(|name| definitions.remove(&name))
        .collect()
}

fn find_last_non_additive_tool_change(system_messages: &[&SystemMessage]) -> isize {
    let mut declared = BTreeSet::new();
    let mut last: isize = -1;
    for (idx, msg) in system_messages.iter().enumerate() {
        if msg.tools_removed.as_ref().is_some_and(|r| !r.is_empty()) {
            last = idx as isize;
        }
        if let Some(added) = &msg.tools_added {
            for tool in added {
                if !declared.insert(tool.name.clone()) {
                    last = idx as isize;
                }
            }
        }
    }
    last
}

pub fn resolve_transcript_tools(
    messages: &[Message],
    support: TranscriptToolSupport,
) -> TranscriptTools {
    let system_messages: Vec<&SystemMessage> = messages
        .iter()
        .filter_map(|m| match m {
            Message::System(s) => Some(s),
            _ => None,
        })
        .collect();

    match support {
        TranscriptToolSupport::None => TranscriptTools {
            request_tools: get_current_tools(messages),
            folded_system_messages: system_messages.len(),
        },
        TranscriptToolSupport::Changes => TranscriptTools {
            request_tools: Vec::new(),
            folded_system_messages: 0,
        },
        TranscriptToolSupport::Additions => {
            let initial_count = usize::from(get_initial_system_message(messages).is_some());
            let last_change =
                (find_last_non_additive_tool_change(&system_messages) + 1).max(0) as usize;
            let folded = initial_count.max(last_change);
            let folded_msgs: Vec<Message> = system_messages
                .iter()
                .take(folded)
                .map(|s| Message::System((*s).clone()))
                .collect();
            TranscriptTools {
                request_tools: get_current_tools(&folded_msgs),
                folded_system_messages: folded,
            }
        }
    }
}

// =============================================================================
// Message Transformation (对照 api/transform-messages.ts)
// =============================================================================

fn replace_images_with_placeholder(
    content: &[UserContentBlock],
    placeholder: &str,
) -> Vec<TextContent> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(TextContent::new(placeholder));
                }
                previous_was_placeholder = true;
            }
            UserContentBlock::Text(text) => {
                result.push(text.clone());
                previous_was_placeholder = text.text == placeholder;
            }
        }
    }

    result
}

/// 将错误工具结果（`is_error == true`）中的图片拆分出来，供拒绝错误结果带图的协议（如 Anthropic）使用。
pub fn split_error_tool_result_images(msg: &ToolResultMessage) -> Option<SplitErrorToolResult> {
    if !msg.is_error {
        return None;
    }
    let images: Vec<ImageContent> = msg
        .content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Image(img) => Some(img.clone()),
            _ => None,
        })
        .collect();
    if images.is_empty() {
        return None;
    }
    Some(SplitErrorToolResult {
        content: replace_images_with_placeholder(&msg.content, ERROR_TOOL_IMAGE_PLACEHOLDER),
        caption: format!(
            "Images from the error result of tool call {} ({}):",
            msg.tool_call_id, msg.tool_name
        ),
        images,
    })
}

fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    if model.supports_image_input() {
        return messages.to_vec();
    }

    messages
        .iter()
        .map(|msg| match msg {
            Message::User(user) => match &user.content {
                UserContent::Text(_) => Message::User(user.clone()),
                UserContent::Blocks(blocks) => {
                    let downgraded =
                        replace_images_with_placeholder(blocks, NON_VISION_USER_IMAGE_PLACEHOLDER)
                            .into_iter()
                            .map(UserContentBlock::Text)
                            .collect();
                    Message::User(UserMessage {
                        content: UserContent::Blocks(downgraded),
                        timestamp: user.timestamp,
                    })
                }
            },
            Message::ToolResult(tr) => {
                let downgraded =
                    replace_images_with_placeholder(&tr.content, NON_VISION_TOOL_IMAGE_PLACEHOLDER)
                        .into_iter()
                        .map(UserContentBlock::Text)
                        .collect();
                let mut next = tr.clone();
                next.content = downgraded;
                Message::ToolResult(next)
            }
            other => other.clone(),
        })
        .collect()
}

/// 跨模型历史消息清洗：
/// 1. 若目标模型不支持图片，自动将用户消息与工具结果中的图片替换为文本占位符；
/// 2. 跨模型切换时丢弃异构模型的 `Thinking` 块、`text_signature` 与 `thought_signature`，并按需规范化 `ToolCall.id`；
/// 3. 跳过 `stop_reason` 为 `Error` 或 `Aborted` 的残缺助手消息；
/// 4. 为没有对应 `ToolResult` 的孤儿 `ToolCall` 自动补齐合成的错误 `ToolResultMessage`（`No result provided`）。
pub fn transform_messages<F>(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<F>,
) -> Vec<Message>
where
    F: Fn(&str, &Model, &AssistantMessage) -> String,
{
    let mut tool_call_id_map: BTreeMap<String, String> = BTreeMap::new();
    let image_aware = downgrade_unsupported_images(messages, model);

    // 第一遍：转换思考块与工具调用 ID
    let transformed: Vec<Message> = image_aware
        .into_iter()
        .map(|msg| match msg {
            Message::System(_) | Message::User(_) => msg,
            Message::ToolResult(mut tr) => {
                if let Some(normalized_id) = tool_call_id_map.get(&tr.tool_call_id)
                    && normalized_id != &tr.tool_call_id
                {
                    tr.tool_call_id = normalized_id.clone();
                }
                Message::ToolResult(tr)
            }
            Message::Assistant(assistant_msg) => {
                let is_same_model = assistant_msg.provider == model.provider
                    && assistant_msg.api == model.api
                    && assistant_msg.model == model.id;

                let mut transformed_content = Vec::with_capacity(assistant_msg.content.len());
                for block in &assistant_msg.content {
                    match block {
                        ContentBlock::Thinking(thinking) => {
                            if !is_same_model {
                                continue;
                            }
                            if thinking.redacted == Some(true)
                                || thinking.thinking_signature.is_some()
                            {
                                transformed_content.push(ContentBlock::Thinking(thinking.clone()));
                                continue;
                            }
                            if !thinking.thinking.trim().is_empty() {
                                transformed_content.push(ContentBlock::Thinking(thinking.clone()));
                            }
                        }
                        ContentBlock::Text(text) => {
                            if is_same_model {
                                transformed_content.push(ContentBlock::Text(text.clone()));
                            } else {
                                transformed_content.push(ContentBlock::Text(TextContent {
                                    text: text.text.clone(),
                                    text_signature: None,
                                }));
                            }
                        }
                        ContentBlock::ToolCall(tc) => {
                            let mut next_tc = tc.clone();
                            if !is_same_model {
                                next_tc.thought_signature = None;
                                if let Some(ref norm_fn) = normalize_tool_call_id {
                                    let normalized_id = norm_fn(&tc.id, model, &assistant_msg);
                                    if normalized_id != tc.id {
                                        tool_call_id_map
                                            .insert(tc.id.clone(), normalized_id.clone());
                                        next_tc.id = normalized_id;
                                    }
                                }
                            }
                            transformed_content.push(ContentBlock::ToolCall(next_tc));
                        }
                    }
                }

                let mut next_assistant = assistant_msg;
                next_assistant.content = transformed_content;
                Message::Assistant(next_assistant)
            }
        })
        .collect();

    // 第二遍：过滤错误/中断的助手消息，并为孤儿 ToolCall 补齐合成 ToolResultMessage
    let mut result: Vec<Message> = Vec::with_capacity(transformed.len());
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: BTreeSet<String> = BTreeSet::new();
    let mut held_system_messages: Vec<Message> = Vec::new();

    let close_pending = |result: &mut Vec<Message>,
                         pending_tool_calls: &mut Vec<ToolCall>,
                         existing_tool_result_ids: &mut BTreeSet<String>,
                         held_system_messages: &mut Vec<Message>| {
        if !pending_tool_calls.is_empty() {
            for tc in pending_tool_calls.drain(..) {
                if !existing_tool_result_ids.contains(&tc.id) {
                    result.push(Message::ToolResult(ToolResultMessage {
                        tool_call_id: tc.id,
                        tool_name: tc.name,
                        content: vec![UserContentBlock::text("No result provided")],
                        details: None,
                        usage: None,
                        nested_calls: None,
                        is_error: true,
                        timestamp: now_ms(),
                        duration_ms: None,
                    }));
                }
            }
            existing_tool_result_ids.clear();
        }
        result.append(held_system_messages);
    };

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant_msg) => {
                close_pending(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );

                if matches!(
                    assistant_msg.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    continue;
                }

                let tool_calls: Vec<ToolCall> = assistant_msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolCall(tc) => Some(tc.clone()),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }

                result.push(msg);
            }
            Message::ToolResult(tr) => {
                existing_tool_result_ids.insert(tr.tool_call_id.clone());
                result.push(msg);
            }
            Message::System(_) => {
                if !pending_tool_calls.is_empty() {
                    held_system_messages.push(msg);
                } else {
                    result.push(msg);
                }
            }
            Message::User(_) => {
                close_pending(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );
                result.push(msg);
            }
        }
    }

    close_pending(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
        &mut held_system_messages,
    );

    result
}

use std::collections::BTreeSet;

use pi_agent::{AgentMessage, StreamFn, get_agent_current_system_message};
use pi_ai::{
    AssistantMessage, CacheRetention, ContentBlock, Context, JsonObject, JsonValue, Message, Model,
    ProviderHeaders, RetryCallbacks, RetryPolicy, SimpleStreamOptions, StopReason, StreamOptions,
    SystemContent, SystemTextBlock, TextContent, ThinkingLevel, TranscriptContext, Usage,
    UserContent, UserContentBlock, UserMessage, normalize_context, now_ms, retry_assistant_call,
    uuid_v7,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::messages::{
    BashExecutionMessage, BranchSummaryMessage, CompactionSummaryMessage, CustomMessage,
    CustomMessageContent, convert_to_llm_with_options,
};
use crate::session_manager::{
    ProjectedSessionEntry, SessionEntry, SessionProjection, build_session_projection,
    session_entry_to_context_messages,
};
use crate::usage_totals::combine_usage;

pub const ESTIMATED_IMAGE_CHARS: usize = 4800;
pub const TOOL_RESULT_MAX_CHARS: usize = 2000;

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.\n\nUse this EXACT format:\n\n## Goal\n[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned by user]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [Ordered list of what should happen next]\n\n## Critical Context\n- [Any data, examples, or references needed to continue]\n- [Or \"(none)\" if not applicable]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

pub const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\nUpdate the existing structured summary with new information. RULES:\n- PRESERVE all existing information from the previous summary\n- ADD new progress, decisions, and context from the new messages\n- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed\n- UPDATE \"Next Steps\" based on what was accomplished\n- PRESERVE exact file paths, function names, and error messages\n- If something is no longer relevant, you may remove it\n\nUse this EXACT format:\n\n## Goal\n[Preserve existing goals, add new ones if the task expanded]\n\n## Constraints & Preferences\n- [Preserve existing, add new ones discovered]\n\n## Progress\n### Done\n- [x] [Include previously done items AND newly completed items]\n\n### In Progress\n- [ ] [Current work - update based on progress]\n\n### Blocked\n- [Current blockers - remove if resolved]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale] (preserve all previous, add new)\n\n## Next Steps\n1. [Update based on current state]\n\n## Critical Context\n- [Preserve important context, add new if needed]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

pub const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.\n\nCreate a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.\n\n## Original Request\n[What did the user ask for?]\n\n## Progress So Far\n- [Key decisions and work completed in these messages]\n\n## Context Needed to Continue\n- [Information from these messages needed to understand the later work]\n\nOnly summarize information explicitly present above. Do not infer or recreate later messages.";

// =============================================================================
// 1. File Operation Tracking & Conversation Serialization
// =============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    pub read: BTreeSet<String>,
    pub written: BTreeSet<String>,
    pub edited: BTreeSet<String>,
}

pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

fn add_file_op(tool_name: &str, args: Option<&JsonObject>, file_ops: &mut FileOperations) {
    let Some(path) = args
        .and_then(|a| a.get("path"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    match tool_name {
        "read" => {
            file_ops.read.insert(path.to_owned());
        }
        "write" => {
            file_ops.written.insert(path.to_owned());
        }
        "edit" => {
            file_ops.edited.insert(path.to_owned());
        }
        _ => {}
    }
}

/// 从助手工具调用以及 `ToolResultMessage.nested_calls` 中提取文件读写修改记录（对照 `extractFileOpsFromMessage`）。
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    match message {
        AgentMessage::ToolResult(tr) => {
            if let Some(nested) = &tr.nested_calls {
                for call in &nested.calls {
                    add_file_op(&call.name, call.arguments.as_ref(), file_ops);
                }
            }
        }
        AgentMessage::Assistant(a) => {
            for block in &a.content {
                if let ContentBlock::ToolCall(tc) = block {
                    add_file_op(&tc.name, Some(&tc.arguments), file_ops);
                }
            }
        }
        _ => {}
    }
}

pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let mut modified: BTreeSet<String> = file_ops.edited.clone();
    modified.extend(file_ops.written.iter().cloned());

    let read_files: Vec<String> = file_ops
        .read
        .iter()
        .filter(|f| !modified.contains(*f))
        .cloned()
        .collect();
    let modified_files: Vec<String> = modified.into_iter().collect();
    (read_files, modified_files)
}

pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_owned();
    }
    let truncated_chars = char_count - max_chars;
    let prefix: String = text.chars().take(max_chars).collect();
    format!("{prefix}\n\n[... {truncated_chars} more characters truncated]")
}

fn user_content_text(content: &UserContent, sep: &str) -> String {
    match content {
        UserContent::Text(s) => s.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(sep),
    }
}

/// 将标准 LLM 消息序列序列化为纯文本对话摘要输入（省略 `thinking` 块并截断过长工具结果，对照 `serializeConversation`）。
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(u) => {
                let text = user_content_text(&u.content, "");
                if !text.is_empty() {
                    parts.push(format!("[User]: {text}"));
                }
            }
            Message::Assistant(a) => {
                let mut tool_calls = Vec::new();
                let mut text_blocks = Vec::new();
                for block in &a.content {
                    match block {
                        ContentBlock::Text(t) => text_blocks.push(t.text.as_str()),
                        ContentBlock::ToolCall(tc) => {
                            let args_str = tc
                                .arguments
                                .iter()
                                .map(|(k, v)| {
                                    format!("{k}={}", serde_json::to_string(v).unwrap_or_default())
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            tool_calls.push(format!("{}({args_str})", tc.name));
                        }
                        ContentBlock::Thinking(_) => {}
                    }
                }
                if !text_blocks.is_empty() {
                    parts.push(format!("[Assistant]: {}", text_blocks.join("\n")));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(tr) => {
                let text = tr
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&text, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }

    parts.join("\n\n")
}

// =============================================================================
// 2. Token Calculation & Projection-Aware Context Estimation
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16_384,
    keep_recent_tokens: 20_000,
};

impl Default for CompactionSettings {
    fn default() -> Self {
        DEFAULT_COMPACTION_SETTINGS
    }
}

pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write)
    }
}

fn get_assistant_usage(msg: &AgentMessage) -> Option<&Usage> {
    let AgentMessage::Assistant(a) = msg else {
        return None;
    };
    if matches!(a.stop_reason, StopReason::Aborted | StopReason::Error) {
        return None;
    }
    (calculate_context_tokens(&a.usage) > 0).then_some(&a.usage)
}

pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(m) = entry
            && let Some(u) = get_assistant_usage(&m.message)
        {
            return Some(u.clone());
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    pub last_usage_index: Option<usize>,
}

fn estimate_blocks_chars(blocks: &[UserContentBlock]) -> usize {
    let mut chars = 0usize;
    for b in blocks {
        match b {
            UserContentBlock::Text(t) => chars += t.text.len(),
            UserContentBlock::Image(_) => chars += ESTIMATED_IMAGE_CHARS,
        }
    }
    chars
}

fn estimate_user_content_chars(content: &UserContent) -> usize {
    match content {
        UserContent::Text(s) => s.len(),
        UserContent::Blocks(blocks) => estimate_blocks_chars(blocks),
    }
}

pub fn estimate_system_message_tokens(sys: &pi_ai::SystemMessage) -> u64 {
    let mut chars = match &sys.content {
        SystemContent::Text(s) => s.len(),
        SystemContent::Blocks(blocks) => blocks
            .iter()
            .map(|SystemTextBlock::Text(t)| t.text.len())
            .sum(),
    };
    if let Some(sections) = &sys.sections {
        for val in sections.values().flatten() {
            chars += val.len();
        }
    }
    if let Some(tools_added) = &sys.tools_added {
        chars += serde_json::to_string(tools_added).map_or(0, |s| s.len());
    }
    (chars as u64).div_ceil(4)
}

/// 使用 `ceil(chars / 4)` 启发式估算单条消息的 Token 数量（对照 `estimateTokens`）。
pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    match message {
        AgentMessage::System(sys) => estimate_system_message_tokens(sys),
        AgentMessage::User(u) => (estimate_user_content_chars(&u.content) as u64).div_ceil(4),
        AgentMessage::Assistant(a) => {
            let mut chars = 0usize;
            for block in &a.content {
                match block {
                    ContentBlock::Text(t) => chars += t.text.len(),
                    ContentBlock::Thinking(th) => chars += th.thinking.len(),
                    ContentBlock::ToolCall(tc) => {
                        chars += tc.name.len()
                            + serde_json::to_string(&tc.arguments).map_or(0, |s| s.len());
                    }
                }
            }
            (chars as u64).div_ceil(4)
        }
        AgentMessage::ToolResult(tr) => (estimate_blocks_chars(&tr.content) as u64).div_ceil(4),
        AgentMessage::Custom(c) => match c.role.as_str() {
            "custom" => {
                if let Some(cm) = CustomMessage::from_agent_message(message) {
                    let chars = match &cm.content {
                        CustomMessageContent::Text(s) => s.len(),
                        CustomMessageContent::Blocks(b) => estimate_blocks_chars(b),
                    };
                    (chars as u64).div_ceil(4)
                } else {
                    0
                }
            }
            "bashExecution" => {
                if let Some(bm) = BashExecutionMessage::from_agent_message(message) {
                    ((bm.command.len() + bm.output.len()) as u64).div_ceil(4)
                } else {
                    0
                }
            }
            "branchSummary" => {
                if let Some(bs) = BranchSummaryMessage::from_agent_message(message) {
                    (bs.summary.len() as u64).div_ceil(4)
                } else {
                    0
                }
            }
            "compactionSummary" => {
                if let Some(cs) = CompactionSummaryMessage::from_agent_message(message) {
                    (cs.summary.len() as u64).div_ceil(4)
                } else {
                    0
                }
            }
            _ => 0,
        },
    }
}

pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let usage_info = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, m)| get_assistant_usage(m).map(|u| (i, u)));

    let Some((usage_idx, usage)) = usage_info else {
        let estimated: u64 = messages.iter().map(estimate_tokens).sum();
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(usage);
    let trailing_tokens: u64 = messages[usage_idx + 1..].iter().map(estimate_tokens).sum();
    ContextUsageEstimate {
        tokens: usage_tokens.saturating_add(trailing_tokens),
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(usage_idx),
    }
}

/// 估算投影后的上下文 Token 数；若最后一条 `usage` 之前/之后发生了更晚的 `context_edit` 或 `compaction`，
/// 自动作废旧 `usage` 并回退到纯消息估算（对照 `estimateProjectedContextTokens`）。
pub fn estimate_projected_context_tokens(
    projection: &SessionProjection,
    branch_entries: &[SessionEntry],
) -> ContextUsageEstimate {
    let estimate = estimate_context_tokens(&projection.messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        let mut projected_msg_idx = 0usize;
        let mut usage_entry_id: Option<&str> = None;
        for entry in &projection.entries {
            let next_msg_idx = projected_msg_idx + entry.messages.len();
            if last_usage_index < next_msg_idx {
                usage_entry_id = Some(entry.source_entry.id());
                break;
            }
            projected_msg_idx = next_msg_idx;
        }

        let usage_entry_index =
            usage_entry_id.and_then(|id| branch_entries.iter().position(|e| e.id() == id));
        let latest_invalidating_index = branch_entries.iter().rposition(|e| {
            matches!(
                e,
                SessionEntry::ContextEdit(_) | SessionEntry::Compaction(_)
            )
        });

        let valid = match (usage_entry_index, latest_invalidating_index) {
            (Some(u_idx), Some(inv_idx)) => u_idx > inv_idx,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if valid {
            return estimate;
        }
    }

    let current_system = get_agent_current_system_message(&projection.messages);
    let mut tokens = current_system
        .as_ref()
        .map_or(0, estimate_system_message_tokens);
    for msg in &projection.messages {
        if msg.role() != "system" {
            tokens = tokens.saturating_add(estimate_tokens(msg));
        }
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled || context_window == 0 {
        return false;
    }
    context_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

// =============================================================================
// 3. Cut Point Detection & Split-Turn Handling
// =============================================================================

fn is_cut_point_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "assistant" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

fn is_turn_start_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return false;
    }
    session_entry_to_context_messages(entry)
        .iter()
        .any(is_turn_start_message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPointResult {
    pub first_kept_entry_index: usize,
    pub turn_start_index: Option<usize>,
    pub is_split_turn: bool,
}

pub fn find_turn_start_index(
    entries: &[SessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    if entry_index < start_index || entries.is_empty() {
        return None;
    }
    let upper = entry_index.min(entries.len() - 1);
    (start_index..=upper)
        .rev()
        .find(|&i| is_turn_start_entry(&entries[i]))
}

pub fn find_cut_point(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let end = end_index.min(entries.len());
    let mut cut_points = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end).skip(start_index) {
        if matches!(entry, SessionEntry::Compaction(_)) {
            continue;
        }
        if session_entry_to_context_messages(entry)
            .iter()
            .any(is_cut_point_message)
        {
            cut_points.push(i);
        }
    }

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0u64;
    let mut cut_index = cut_points[0];

    for i in (start_index..end).rev() {
        let entry = &entries[i];
        let msg_tokens: u64 = session_entry_to_context_messages(entry)
            .iter()
            .map(estimate_tokens)
            .sum();
        if msg_tokens == 0 {
            continue;
        }
        accumulated_tokens = accumulated_tokens.saturating_add(msg_tokens);
        if accumulated_tokens >= keep_recent_tokens {
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&c| c >= i)
                .unwrap_or(*cut_points.last().expect("cut_points non-empty"));
            break;
        }
    }

    while cut_index > start_index {
        let prev = &entries[cut_index - 1];
        if matches!(prev, SessionEntry::Compaction(_))
            || !session_entry_to_context_messages(prev).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_turn_start_entry(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

fn is_projected_turn_start(entry: &ProjectedSessionEntry) -> bool {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return false;
    }
    entry.messages.iter().any(is_turn_start_message)
}

fn find_projected_turn_start_index(
    entries: &[ProjectedSessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    if entry_index < start_index || entries.is_empty() {
        return None;
    }
    let upper = entry_index.min(entries.len() - 1);
    (start_index..=upper)
        .rev()
        .find(|&i| is_projected_turn_start(&entries[i]))
}

pub fn find_projected_cut_point(
    entries: &[ProjectedSessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let end = end_index.min(entries.len());
    let mut cut_points = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end).skip(start_index) {
        if !matches!(entry.source_entry, SessionEntry::Compaction(_))
            && entry.messages.iter().any(is_cut_point_message)
        {
            cut_points.push(i);
        }
    }

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0u64;
    let mut exceeded_budget = false;
    let mut cut_index = cut_points[0];

    for i in (start_index..end).rev() {
        let msg_tokens: u64 = entries[i].messages.iter().map(estimate_tokens).sum();
        if msg_tokens == 0 {
            continue;
        }
        accumulated_tokens = accumulated_tokens.saturating_add(msg_tokens);
        if accumulated_tokens >= keep_recent_tokens {
            exceeded_budget = true;
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&c| c >= i)
                .unwrap_or(*cut_points.last().expect("cut_points non-empty"));
            break;
        }
    }

    // 处理被 context_edit(null) 隐去的失败恢复尝试后缀（isRecoveryOmissionSuffix）
    let suffix = if cut_index + 1 < end {
        &entries[cut_index + 1..end]
    } else {
        &[]
    };
    let is_intrinsically_visible = |e: &ProjectedSessionEntry| -> bool {
        !matches!(e.source_entry, SessionEntry::ContextEdit(_))
            && !session_entry_to_context_messages(&e.source_entry).is_empty()
    };
    let is_omitted = |e: &ProjectedSessionEntry| -> bool {
        is_intrinsically_visible(e) && e.messages.is_empty()
    };
    let omitted_suffix_ids: BTreeSet<String> = suffix
        .iter()
        .filter(|e| is_omitted(e))
        .map(|e| e.source_entry.id().to_owned())
        .collect();
    let has_external_replacement = suffix.iter().any(|e| {
        matches!(
            &e.source_entry,
            SessionEntry::ContextEdit(ce)
                if ce.replacement.is_some() && !omitted_suffix_ids.contains(&ce.target_id)
        )
    });
    let is_recovery_omission_suffix = exceeded_budget
        && !has_external_replacement
        && suffix.iter().any(|e| {
            matches!(
                &e.source_entry,
                SessionEntry::Message(m) if m.message.role() == "assistant"
            ) && is_omitted(e)
        })
        && suffix.iter().all(|e| {
            !matches!(e.source_entry, SessionEntry::Compaction(_))
                && (!is_intrinsically_visible(e) || is_omitted(e))
        });

    if is_recovery_omission_suffix && cut_index + 1 < end {
        cut_index += 1;
    }

    while cut_index > start_index {
        let prev = &entries[cut_index - 1];
        if matches!(prev.source_entry, SessionEntry::Compaction(_)) || !prev.messages.is_empty() {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_projected_turn_start(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_projected_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

// =============================================================================
// 4. Compaction Preparation & Summarization Execution
// =============================================================================

#[derive(Debug, Clone)]
pub struct CompactionPreparation {
    pub first_kept_entry_id: String,
    pub messages_to_summarize: Vec<AgentMessage>,
    pub turn_prefix_messages: Vec<AgentMessage>,
    pub is_split_turn: bool,
    pub tokens_before: u64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionEntry],
    prev_compaction_index: Option<usize>,
) -> FileOperations {
    let mut file_ops = create_file_ops();

    if let Some(idx) = prev_compaction_index
        && let Some(SessionEntry::Compaction(prev_comp)) = entries.get(idx)
        && prev_comp.from_hook != Some(true)
        && let Some(details) = &prev_comp.details
        && let Ok(parsed) = serde_json::from_value::<CompactionDetails>(details.clone())
    {
        for f in parsed.read_files {
            file_ops.read.insert(f);
        }
        for f in parsed.modified_files {
            file_ops.edited.insert(f);
        }
    }

    for msg in messages {
        extract_file_ops_from_message(msg, &mut file_ops);
    }

    file_ops
}

fn get_messages_from_projected_entry_for_compaction(
    entry: &ProjectedSessionEntry,
) -> Vec<AgentMessage> {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return Vec::new();
    }
    entry
        .messages
        .iter()
        .filter(|m| m.role() != "system")
        .cloned()
        .collect()
}

/// 根据会话分支路径与压缩设置计算压缩切点与待总结消息（对照 `prepareCompaction`）。
pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: CompactionSettings,
) -> Option<CompactionPreparation> {
    if matches!(path_entries.last(), Some(SessionEntry::Compaction(_))) {
        return None;
    }

    let projection = build_session_projection(path_entries, None, None);
    let projected_entries = &projection.entries;
    let source_entries: Vec<SessionEntry> = projected_entries
        .iter()
        .map(|e| e.source_entry.clone())
        .collect();

    let prev_compaction_index = projected_entries.iter().position(|e| {
        matches!(e.source_entry, SessionEntry::Compaction(_)) && !e.messages.is_empty()
    });

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if let Some(idx) = prev_compaction_index
        && let SessionEntry::Compaction(comp) = &projected_entries[idx].source_entry
    {
        previous_summary = Some(comp.summary.clone());
        boundary_start = idx + 1;
    }

    let boundary_end = projected_entries.len();
    let tokens_before = estimate_projected_context_tokens(&projection, path_entries).tokens;
    let cut_point = find_projected_cut_point(
        projected_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );

    let first_kept_entry = projected_entries
        .get(cut_point.first_kept_entry_index)
        .map(|e| &e.source_entry)?;
    if first_kept_entry.id().is_empty() {
        return None;
    }
    let first_kept_entry_id = first_kept_entry.id().to_owned();

    let history_end = if cut_point.is_split_turn {
        cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index)
    } else {
        cut_point.first_kept_entry_index
    };

    let messages_to_summarize: Vec<AgentMessage> = projected_entries[boundary_start..history_end]
        .iter()
        .flat_map(get_messages_from_projected_entry_for_compaction)
        .collect();

    let turn_prefix_messages: Vec<AgentMessage> = if cut_point.is_split_turn {
        let ts_idx = cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index);
        projected_entries[ts_idx..cut_point.first_kept_entry_index]
            .iter()
            .flat_map(get_messages_from_projected_entry_for_compaction)
            .collect()
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let mut file_ops = extract_file_operations(
        &messages_to_summarize,
        &source_entries,
        prev_compaction_index,
    );
    if cut_point.is_split_turn {
        for msg in &turn_prefix_messages {
            extract_file_ops_from_message(msg, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings,
    })
}

/// 面向手动压缩（`/compact`）的自适应预算准备：若默认 `keep_recent_tokens` 覆盖了全部消息导致无需切分，
/// 则在不修改全局设置的前提下从当前非系统消息估算 Token 的一半开始逐步折半尝试切分。
pub fn prepare_compaction_with_adaptive_budget(
    path_entries: &[SessionEntry],
    settings: CompactionSettings,
) -> Option<CompactionPreparation> {
    if let Some(prep) = prepare_compaction(path_entries, settings) {
        return Some(prep);
    }

    let projection = build_session_projection(path_entries, None, None);
    let non_system_tokens: u64 = projection
        .messages
        .iter()
        .filter(|m| m.role() != "system")
        .map(estimate_tokens)
        .sum();
    if non_system_tokens <= 1 {
        return None;
    }

    let mut candidate_keep = settings
        .keep_recent_tokens
        .min(non_system_tokens / 2)
        .max(1);
    loop {
        let mut try_settings = settings;
        try_settings.keep_recent_tokens = candidate_keep;
        if let Some(prep) = prepare_compaction(path_entries, try_settings) {
            return Some(prep);
        }
        if candidate_keep <= 1 {
            break;
        }
        candidate_keep /= 2;
    }
    None
}

/// 校验总结响应是否可安全作为检查点保存（`Error` 或 `Length` 截断均视为失败，对照 `getSummarizationFailure`）。
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    if response.stop_reason == StopReason::Error {
        let err = response
            .error_message
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown error");
        return Some(format!("{label} failed: {err}"));
    }
    if response.stop_reason == StopReason::Length {
        return Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        ));
    }
    None
}

pub fn build_summarization_context(prompt_text: &str) -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                prompt_text,
            ))]),
            timestamp: now_ms(),
        })],
        tools: None,
    })
}

/// 共享的总结生成入口：强制 `cache_retention: CacheRetention::None`，分配路由 `session_id` 并包裹 `retry_assistant_call`。
pub async fn complete_summarization(
    model: &Model,
    context: TranscriptContext,
    mut options: SimpleStreamOptions,
    stream_fn: Option<&StreamFn>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks>,
) -> AssistantMessage {
    if model.provider == "pi-unconfigured" || model.api == "pi-unconfigured" {
        return pi_agent::create_error_assistant_message(
            model,
            "No models available for summarization",
            false,
        );
    }
    options.base.cache_retention = Some(CacheRetention::None);
    if options.base.session_id.is_none() {
        options.base.session_id = Some(uuid_v7());
    }
    let cancel_token = options.base.cancel_token.clone();

    retry_assistant_call(
        || {
            let model = model.clone();
            let context = context.clone();
            let options = options.clone();
            let s_fn = stream_fn.cloned();
            async move {
                if let Some(f) = s_fn {
                    f(model, context, options).await.result().await
                } else {
                    pi_agent::create_error_assistant_message(
                        &model,
                        "No stream_fn provided for summarization",
                        false,
                    )
                }
            }
        },
        retry,
        cancel_token,
        callbacks,
    )
    .await
}

#[derive(Clone, Default)]
pub struct SummarizationCallOptions {
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub cancel_token: Option<CancellationToken>,
    pub custom_instructions: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub stream_fn: Option<StreamFn>,
    pub retry: Option<RetryPolicy>,
    pub callbacks: Option<RetryCallbacks>,
    pub session_id: Option<String>,
    pub block_images: bool,
}

fn create_simple_stream_options(
    model: &Model,
    opts: &SummarizationCallOptions,
) -> SimpleStreamOptions {
    let reasoning = if model.reasoning {
        opts.thinking_level
    } else {
        None
    };
    SimpleStreamOptions {
        base: StreamOptions {
            cancel_token: opts.cancel_token.clone(),
            api_key: opts.api_key.clone(),
            headers: opts.headers.clone(),
            session_id: opts.session_id.clone(),
            reasoning_effort: reasoning,
            ..StreamOptions::default()
        },
        reasoning,
        ..SimpleStreamOptions::default()
    }
}

pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    model: &Model,
    previous_summary: Option<&str>,
    opts: &SummarizationCallOptions,
) -> Result<(String, Usage), String> {
    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_owned()
    } else {
        SUMMARIZATION_PROMPT.to_owned()
    };
    if let Some(custom) = opts.custom_instructions.as_deref()
        && !custom.trim().is_empty()
    {
        base_prompt.push_str(&format!("\n\nAdditional focus: {custom}"));
    }

    let llm_messages = convert_to_llm_with_options(current_messages, opts.block_images);
    let conversation_text = serialize_conversation(&llm_messages);

    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(prev) = previous_summary {
        prompt_text.push_str(&format!(
            "<previous-summary>\n{prev}\n</previous-summary>\n\n"
        ));
    }
    prompt_text.push_str(&base_prompt);

    let stream_opts = create_simple_stream_options(model, opts);
    let response = complete_summarization(
        model,
        build_summarization_context(&prompt_text),
        stream_opts,
        opts.stream_fn.as_ref(),
        opts.retry.as_ref(),
        opts.callbacks.as_ref(),
    )
    .await;

    if response.stop_reason == StopReason::Aborted {
        return Err("Compaction cancelled".to_owned());
    }
    if let Some(failure) = get_summarization_failure(&response, "Summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall(_)))
    {
        return Err("Summarization attempted to call a tool".to_owned());
    }

    Ok((response.text(), response.usage))
}

async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    model: &Model,
    opts: &SummarizationCallOptions,
) -> Result<(String, Usage), String> {
    let llm_messages = convert_to_llm_with_options(messages, opts.block_images);
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text = format!(
        "# Conversation\n{conversation_text}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    if let Some(custom) = opts.custom_instructions.as_deref()
        && !custom.trim().is_empty()
    {
        prompt_text.push_str(&format!("\n\nAdditional focus: {custom}"));
    }

    let stream_opts = create_simple_stream_options(model, opts);
    let response = complete_summarization(
        model,
        build_summarization_context(&prompt_text),
        stream_opts,
        opts.stream_fn.as_ref(),
        opts.retry.as_ref(),
        opts.callbacks.as_ref(),
    )
    .await;

    if response.stop_reason == StopReason::Aborted {
        return Err("Compaction cancelled".to_owned());
    }
    if let Some(failure) = get_summarization_failure(&response, "Turn prefix summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall(_)))
    {
        return Err("Turn prefix summarization attempted to call a tool".to_owned());
    }

    Ok((response.text(), response.usage))
}

/// 根据 `CompactionPreparation` 生成完整的上下文压缩总结（含分裂轮次双总结拼接与累积文件操作列表，对照 `compact`）。
pub async fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    opts: &SummarizationCallOptions,
) -> Result<CompactionResult, String> {
    if preparation.first_kept_entry_id.is_empty() {
        return Err("First kept entry has no UUID - session may need migration".to_owned());
    }

    let (mut summary, summary_usage) = if preparation.is_split_turn
        && !preparation.turn_prefix_messages.is_empty()
    {
        let mut history_text = preparation
            .previous_summary
            .clone()
            .unwrap_or_else(|| "No prior history.".to_owned());
        let mut history_usage: Option<Usage> = None;

        if !preparation.messages_to_summarize.is_empty() {
            let (h_text, h_usage) = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                model,
                preparation.previous_summary.as_deref(),
                opts,
            )
            .await?;
            history_text = h_text;
            history_usage = Some(h_usage);
        }

        let (tp_text, tp_usage) =
            generate_turn_prefix_summary(&preparation.turn_prefix_messages, model, opts).await?;
        let merged_text =
            format!("{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{tp_text}");
        let merged_usage = match history_usage {
            Some(hu) => combine_usage(&hu, &tp_usage),
            None => tp_usage,
        };
        (merged_text, merged_usage)
    } else {
        generate_summary_with_usage(
            &preparation.messages_to_summarize,
            model,
            preparation.previous_summary.as_deref(),
            opts,
        )
        .await?
    };

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    let details = serde_json::to_value(CompactionDetails {
        read_files,
        modified_files,
    })
    .ok();

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details,
    })
}

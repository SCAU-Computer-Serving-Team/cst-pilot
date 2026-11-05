use std::collections::{BTreeMap, BTreeSet};

use pi_agent::{AgentMessage, StreamFn};
use pi_ai::{
    ContentBlock, Model, ProviderHeaders, RetryCallbacks, RetryPolicy, SimpleStreamOptions,
    StopReason, StreamOptions, ThinkingLevel, Usage,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::compaction::{
    FileOperations, build_summarization_context, complete_summarization, compute_file_lists,
    create_file_ops, estimate_tokens, extract_file_ops_from_message, format_file_operations,
    get_summarization_failure, serialize_conversation,
};
use crate::messages::{
    BranchSummaryMessage, CompactionSummaryMessage, CustomMessage, convert_to_llm_with_options,
};
use crate::session_manager::{
    ContextEditEntry, SessionEntry, SessionManager, parse_iso_timestamp_ms, project_context_entry,
};

pub const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

pub const BRANCH_SUMMARY_PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.\n\nUse this EXACT format:\n\n## Goal\n[What was the user trying to accomplish in this branch?]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Work that was started but not finished]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [What should happen next to continue this work]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aborted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BranchPreparation {
    pub messages: Vec<AgentMessage>,
    pub file_ops: FileOperations,
    pub total_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct CollectEntriesResult {
    pub entries: Vec<SessionEntry>,
    pub common_ancestor_id: Option<String>,
}

#[derive(Clone)]
pub struct GenerateBranchSummaryOptions {
    pub model: Model,
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub cancel_token: Option<CancellationToken>,
    pub thinking_level: Option<ThinkingLevel>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
    pub reserve_tokens: u64,
    pub stream_fn: Option<StreamFn>,
    pub retry: Option<RetryPolicy>,
    pub callbacks: Option<RetryCallbacks>,
    pub block_images: bool,
}

impl GenerateBranchSummaryOptions {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            api_key: None,
            headers: None,
            cancel_token: None,
            thinking_level: None,
            custom_instructions: None,
            replace_instructions: false,
            reserve_tokens: 16_384,
            stream_fn: None,
            retry: None,
            callbacks: None,
            block_images: false,
        }
    }
}

/// 从 `old_leaf_id` 向回收集直到与 `target_id` 的最深公共祖先为止的全部会话条目（对照 `collectEntriesForBranchSummary`）。
pub fn collect_entries_for_branch_summary(
    session: &SessionManager,
    old_leaf_id: Option<&str>,
    target_id: Option<&str>,
) -> CollectEntriesResult {
    let Some(old_id) = old_leaf_id else {
        return CollectEntriesResult {
            entries: Vec::new(),
            common_ancestor_id: None,
        };
    };

    let old_path_ids: BTreeSet<String> = session
        .get_branch(Some(old_id))
        .into_iter()
        .map(|e| e.id().to_owned())
        .collect();

    let target_path = match target_id {
        Some(tid) => session.get_branch(Some(tid)),
        None => Vec::new(),
    };

    let mut common_ancestor_id: Option<String> = None;
    for entry in target_path.iter().rev() {
        if old_path_ids.contains(entry.id()) {
            common_ancestor_id = Some(entry.id().to_owned());
            break;
        }
    }

    let mut entries = Vec::new();
    let mut current: Option<String> = Some(old_id.to_owned());
    let mut visited = BTreeSet::new();
    while let Some(cid) = current {
        if Some(cid.as_str()) == common_ancestor_id.as_deref() || !visited.insert(cid.clone()) {
            break;
        }
        let Some(entry) = session.get_entry(&cid) else {
            break;
        };
        current = entry.parent_id().map(ToOwned::to_owned);
        entries.push(entry.clone());
    }
    entries.reverse();

    CollectEntriesResult {
        entries,
        common_ancestor_id,
    }
}

fn get_message_from_entry(
    entry: &SessionEntry,
    edit: Option<&ContextEditEntry>,
) -> Option<AgentMessage> {
    match entry {
        SessionEntry::Message(m) => {
            if m.message.role() == "toolResult" {
                return None;
            }
            if edit.is_some() {
                project_context_entry(entry, edit).into_iter().next()
            } else {
                Some(m.message.clone())
            }
        }
        SessionEntry::CustomMessage(cm) => {
            if edit.is_some() {
                project_context_entry(entry, edit).into_iter().next()
            } else {
                let ts = parse_iso_timestamp_ms(&cm.timestamp).unwrap_or(0);
                Some(
                    CustomMessage {
                        custom_type: cm.custom_type.clone(),
                        content: cm.content.clone(),
                        display: cm.display,
                        details: cm.details.clone(),
                        timestamp: ts,
                    }
                    .to_agent_message(),
                )
            }
        }
        SessionEntry::BranchSummary(bs) => {
            let ts = parse_iso_timestamp_ms(&bs.timestamp).unwrap_or(0);
            Some(
                BranchSummaryMessage {
                    summary: bs.summary.clone(),
                    from_id: Some(bs.from_id.clone()),
                    timestamp: ts,
                }
                .to_agent_message(),
            )
        }
        SessionEntry::Compaction(c) => {
            let ts = parse_iso_timestamp_ms(&c.timestamp).unwrap_or(0);
            Some(
                CompactionSummaryMessage {
                    summary: c.summary.clone(),
                    tokens_before: c.tokens_before,
                    timestamp: ts,
                }
                .to_agent_message(),
            )
        }
        _ => None,
    }
}

/// 按 Token 预算从最新到最旧挑选分支条目并聚合文件操作记录（对照 `prepareBranchEntries`）。
pub fn prepare_branch_entries(entries: &[SessionEntry], token_budget: u64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens = 0u64;

    for entry in entries {
        if let SessionEntry::BranchSummary(bs) = entry
            && bs.from_hook != Some(true)
            && let Some(details) = &bs.details
            && let Ok(parsed) = serde_json::from_value::<BranchSummaryDetails>(details.clone())
        {
            for f in parsed.read_files {
                file_ops.read.insert(f);
            }
            for f in parsed.modified_files {
                file_ops.edited.insert(f);
            }
        }
    }

    let mut edits: BTreeMap<String, ContextEditEntry> = BTreeMap::new();
    for entry in entries {
        if let SessionEntry::ContextEdit(ce) = entry {
            edits.insert(ce.target_id.clone(), ce.clone());
        }
    }

    for entry in entries.iter().rev() {
        let Some(message) = get_message_from_entry(entry, edits.get(entry.id())) else {
            continue;
        };
        extract_file_ops_from_message(&message, &mut file_ops);
        let tokens = estimate_tokens(&message);

        if token_budget > 0 && total_tokens.saturating_add(tokens) > token_budget {
            if matches!(
                entry,
                SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_)
            ) && (total_tokens as f64) < (token_budget as f64) * 0.9
            {
                messages.insert(0, message);
                total_tokens = total_tokens.saturating_add(tokens);
            }
            break;
        }

        messages.insert(0, message);
        total_tokens = total_tokens.saturating_add(tokens);
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

/// 调用大模型生成被离开分支的结构化总结（对照 `generateBranchSummary`）。
pub async fn generate_branch_summary(
    entries: &[SessionEntry],
    options: &GenerateBranchSummaryOptions,
) -> BranchSummaryResult {
    let context_window = if options.model.context_window > 0 {
        options.model.context_window
    } else {
        128_000
    };
    let token_budget = context_window.saturating_sub(options.reserve_tokens);
    let prep = prepare_branch_entries(entries, token_budget);

    if prep.messages.is_empty() {
        return BranchSummaryResult {
            summary: Some("No content to summarize".to_owned()),
            ..BranchSummaryResult::default()
        };
    }

    let llm_messages = convert_to_llm_with_options(&prep.messages, options.block_images);
    let conversation_text = serialize_conversation(&llm_messages);

    let instructions = match (
        options.replace_instructions,
        options.custom_instructions.as_deref(),
    ) {
        (true, Some(custom)) if !custom.trim().is_empty() => custom.to_owned(),
        (false, Some(custom)) if !custom.trim().is_empty() => {
            format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom}")
        }
        _ => BRANCH_SUMMARY_PROMPT.to_owned(),
    };

    let prompt_text =
        format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}");
    let reasoning = if options.model.reasoning {
        options.thinking_level
    } else {
        None
    };
    let request_options = SimpleStreamOptions {
        base: StreamOptions {
            cancel_token: options.cancel_token.clone(),
            api_key: options.api_key.clone(),
            headers: options.headers.clone(),
            reasoning_effort: reasoning,
            ..StreamOptions::default()
        },
        reasoning,
        ..SimpleStreamOptions::default()
    };

    let response = complete_summarization(
        &options.model,
        build_summarization_context(&prompt_text),
        request_options,
        options.stream_fn.as_ref(),
        options.retry.as_ref(),
        options.callbacks.as_ref(),
    )
    .await;

    if response.stop_reason == StopReason::Aborted {
        return BranchSummaryResult {
            aborted: Some(true),
            ..BranchSummaryResult::default()
        };
    }
    if let Some(failure) = get_summarization_failure(&response, "Branch summarization") {
        return BranchSummaryResult {
            error: Some(failure),
            ..BranchSummaryResult::default()
        };
    }
    if response
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall(_)))
    {
        return BranchSummaryResult {
            error: Some("Branch summarization attempted to call a tool".to_owned()),
            ..BranchSummaryResult::default()
        };
    }

    let mut summary = format!("{BRANCH_SUMMARY_PREAMBLE}{}", response.text());
    let (read_files, modified_files) = compute_file_lists(&prep.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    BranchSummaryResult {
        summary: Some(if summary.is_empty() {
            "No summary generated".to_owned()
        } else {
            summary
        }),
        usage: Some(response.usage),
        read_files: Some(read_files),
        modified_files: Some(modified_files),
        aborted: None,
        error: None,
    }
}

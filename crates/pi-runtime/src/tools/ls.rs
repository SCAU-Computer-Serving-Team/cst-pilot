use std::collections::BinaryHeap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use pi_agent::{AgentTool, AgentToolResult, BoxFuture, ToolReplayPolicy};
use pi_ai::{TextContent, UserContentBlock};
use pi_session::{ExtensionToolContext, NoOpUiContext, ToolDefinition, ToolExposure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::tools::read::resolve_to_cwd;
use crate::tools::truncate::{
    DEFAULT_MAX_BYTES, TruncationOptions, TruncationResult, format_size, truncate_head,
};

pub const DEFAULT_LS_LIMIT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LsToolInput {
    /// Directory to list (default: current directory)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Maximum number of entries to return (default: 500)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_limit_reached: Option<usize>,
}

pub trait LsOperations: Send + Sync {
    fn exists(&self, absolute_path: &Path) -> bool;
    fn is_directory(&self, absolute_path: &Path) -> Result<bool, String>;
    fn readdir(&self, absolute_path: &Path) -> Result<Vec<String>, String>;
    fn readdir_limited(
        &self,
        absolute_path: &Path,
        _limit: usize,
        _cancel: &CancellationToken,
    ) -> Result<Vec<String>, String> {
        self.readdir(absolute_path)
    }
}

#[derive(Default, Clone)]
pub struct LocalLsOperations;

impl LsOperations for LocalLsOperations {
    fn exists(&self, absolute_path: &Path) -> bool {
        absolute_path.exists()
    }

    fn is_directory(&self, absolute_path: &Path) -> Result<bool, String> {
        fs::metadata(absolute_path)
            .map(|m| m.is_dir())
            .map_err(|e| e.to_string())
    }

    fn readdir(&self, absolute_path: &Path) -> Result<Vec<String>, String> {
        let rd = fs::read_dir(absolute_path).map_err(|e| e.to_string())?;
        let mut names = Vec::new();
        for entry in rd.flatten() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn readdir_limited(
        &self,
        absolute_path: &Path,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>, String> {
        let mut smallest = BinaryHeap::new();
        for entry in fs::read_dir(absolute_path).map_err(|e| e.to_string())? {
            if cancel.is_cancelled() {
                return Err("Operation aborted".to_owned());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            smallest.push((name.to_lowercase(), name));
            if smallest.len() > limit {
                smallest.pop();
            }
        }
        Ok(smallest
            .into_sorted_vec()
            .into_iter()
            .map(|(_, name)| name)
            .collect())
    }
}

#[derive(Clone, Default)]
pub struct LsToolOptions {
    pub operations: Option<Arc<dyn LsOperations>>,
}

pub fn create_ls_tool_definition(
    cwd: impl Into<String>,
    options: Option<LsToolOptions>,
) -> ToolDefinition {
    let default_cwd = cwd.into();
    let ops: Arc<dyn LsOperations> = options
        .and_then(|o| o.operations)
        .unwrap_or_else(|| Arc::new(LocalLsOperations));

    ToolDefinition::new_typed_with_context::<LsToolInput, _, _>(
        "ls",
        "ls",
        format!(
            "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to {DEFAULT_LS_LIMIT} entries or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        ),
        move |_tool_call_id, input: LsToolInput, _on_update, cancel: CancellationToken, ctx: ExtensionToolContext| {
            let ops = ops.clone();
            let base_cwd = default_cwd.clone();
            Box::pin(async move {
                if cancel.is_cancelled() {
                    return Err("Operation aborted".to_owned());
                }
                let effective_cwd = if ctx.cwd().is_empty() {
                    base_cwd
                } else {
                    ctx.cwd()
                };
                let dir_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &effective_cwd);
                let effective_limit = input.limit.unwrap_or(DEFAULT_LS_LIMIT).clamp(1, DEFAULT_MAX_BYTES);

                tokio::task::spawn_blocking(move || -> Result<AgentToolResult, String> {
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }
                    if !ops.exists(&dir_path) {
                        return Err(format!("Path not found: {}", dir_path.display()));
                    }
                    let is_dir = ops.is_directory(&dir_path)?;
                    if !is_dir {
                        return Err(format!("Not a directory: {}", dir_path.display()));
                    }

                    let mut entries = ops
                        .readdir_limited(&dir_path, effective_limit.saturating_add(1), &cancel)
                        .map_err(|e| format!("Cannot read directory: {e}"))?;
                    entries.sort_by(|a, b| {
                        a.to_lowercase()
                            .cmp(&b.to_lowercase())
                            .then_with(|| a.cmp(b))
                    });

                    let mut results = Vec::new();
                    let mut entry_limit_reached = false;
                    for entry in entries {
                        if cancel.is_cancelled() {
                            return Err("Operation aborted".to_owned());
                        }
                        if results.len() >= effective_limit {
                            entry_limit_reached = true;
                            break;
                        }
                        let full = dir_path.join(&entry);
                        let Ok(entry_is_dir) = ops.is_directory(&full) else {
                            continue;
                        };
                        if entry_is_dir {
                            results.push(format!("{entry}/"));
                        } else {
                            results.push(entry);
                        }
                    }

                    if results.is_empty() {
                        return Ok(AgentToolResult::text("(empty directory)"));
                    }

                    let raw_output = results.join("\n");
                    let truncation = truncate_head(
                        &raw_output,
                        TruncationOptions {
                            max_lines: Some(usize::MAX),
                            max_bytes: Some(DEFAULT_MAX_BYTES),
                        },
                    );
                    let mut output = truncation.content.clone();
                    let mut notices = Vec::new();
                    let mut details = LsToolDetails {
                        truncation: None,
                        entry_limit_reached: None,
                    };

                    if entry_limit_reached {
                        notices.push(format!(
                            "{effective_limit} entries limit reached. Use limit={} for more",
                            effective_limit.saturating_mul(2)
                        ));
                        details.entry_limit_reached = Some(effective_limit);
                    }
                    if truncation.truncated {
                        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                        details.truncation = Some(truncation);
                    }
                    if !notices.is_empty() {
                        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
                    }

                    let details_val = if details.truncation.is_some()
                        || details.entry_limit_reached.is_some()
                    {
                        serde_json::to_value(details).unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    };

                    Ok(AgentToolResult {
                            is_error: false,
                            terminate: None,
                        content: vec![UserContentBlock::Text(TextContent::new(output))],
                        details: details_val,
                        structured_content: None,
                        usage: None,
                    })
                })
                .await
                .map_err(|e| format!("Ls task panicked: {e}"))?
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prompt_snippet("List directory contents")
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Safe)
}

pub fn create_ls_tool(cwd: impl Into<String>, options: Option<LsToolOptions>) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_ls_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

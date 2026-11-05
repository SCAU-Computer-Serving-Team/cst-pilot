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

use crate::tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, split_bom,
};
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::read::resolve_to_cwd;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceEdit {
    /// Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call.
    pub old_text: String,
    /// Replacement text for this targeted edit.
    pub new_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    /// Path to the file to edit (relative or absolute)
    pub path: String,
    /// One or more targeted replacements. Each edit is matched against the original file, not incrementally.
    pub edits: Vec<ReplaceEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    pub diff: String,
    pub patch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

pub trait EditOperations: Send + Sync {
    fn access(&self, absolute_path: &Path) -> Result<(), String>;
    fn read_file(&self, absolute_path: &Path) -> Result<String, String>;
    fn write_file(&self, absolute_path: &Path, content: &str) -> Result<(), String>;
}

#[derive(Default, Clone)]
pub struct LocalEditOperations;

impl EditOperations for LocalEditOperations {
    fn access(&self, absolute_path: &Path) -> Result<(), String> {
        if !absolute_path.exists() {
            return Err("Error code: ENOENT".to_owned());
        }
        if absolute_path.is_dir() {
            return Err("Error code: EISDIR".to_owned());
        }
        Ok(())
    }

    fn read_file(&self, absolute_path: &Path) -> Result<String, String> {
        fs::read_to_string(absolute_path)
            .map_err(|e| format!("Failed to read '{}': {e}", absolute_path.display()))
    }

    fn write_file(&self, absolute_path: &Path, content: &str) -> Result<(), String> {
        fs::write(absolute_path, content.as_bytes())
            .map_err(|e| format!("Failed to write '{}': {e}", absolute_path.display()))
    }
}

#[derive(Clone, Default)]
pub struct EditToolOptions {
    pub operations: Option<Arc<dyn EditOperations>>,
}

fn is_single_edit_object(val: &Value) -> bool {
    let Some(obj) = val.as_object() else {
        return false;
    };
    obj.get("oldText").is_some_and(Value::is_string)
        && obj.get("newText").is_some_and(Value::is_string)
}

/// 参数预处理：兼容部分模型将 `edits` 传成 JSON 字符串、单对象或顶层 `oldText`+`newText`（对照 `prepareEditArguments`）。
pub fn prepare_edit_arguments(mut input: Value) -> Value {
    let Some(obj) = input.as_object_mut() else {
        return input;
    };

    if let Some(Value::String(edits_str)) = obj.get("edits").cloned() {
        if let Ok(parsed) = serde_json::from_str::<Value>(&edits_str) {
            if parsed.is_array() {
                obj.insert("edits".to_owned(), parsed);
            } else if is_single_edit_object(&parsed) {
                obj.insert("edits".to_owned(), Value::Array(vec![parsed]));
            }
        }
    } else if let Some(single) = obj.get("edits").cloned()
        && is_single_edit_object(&single)
    {
        obj.insert("edits".to_owned(), Value::Array(vec![single]));
    }

    let legacy_old = obj.remove("oldText");
    let legacy_new = obj.remove("newText");
    if let (Some(Value::String(old_text)), Some(Value::String(new_text))) = (legacy_old, legacy_new)
    {
        let mut arr = match obj.remove("edits") {
            Some(Value::Array(existing)) => existing,
            _ => Vec::new(),
        };
        arr.push(serde_json::json!({
            "oldText": old_text,
            "newText": new_text,
        }));
        obj.insert("edits".to_owned(), Value::Array(arr));
    }

    input
}

pub fn create_edit_tool_definition(
    cwd: impl Into<String>,
    options: Option<EditToolOptions>,
) -> ToolDefinition {
    let default_cwd = cwd.into();
    let ops: Arc<dyn EditOperations> = options
        .and_then(|o| o.operations)
        .unwrap_or_else(|| Arc::new(LocalEditOperations));

    ToolDefinition::new_typed_with_context::<EditToolInput, _, _>(
        "edit",
        "edit",
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        move |_tool_call_id, input: EditToolInput, _on_update, cancel: CancellationToken, ctx: ExtensionToolContext| {
            let ops = ops.clone();
            let base_cwd = default_cwd.clone();
            Box::pin(async move {
                if input.edits.is_empty() {
                    return Err(
                        "Edit tool input is invalid. edits must contain at least one replacement."
                            .to_owned(),
                    );
                }
                let effective_cwd = if ctx.cwd().is_empty() {
                    base_cwd
                } else {
                    ctx.cwd()
                };
                let abs_path = resolve_to_cwd(&input.path, &effective_cwd);
                let display_path = input.path.clone();
                let edits: Vec<Edit> = input
                    .edits
                    .into_iter()
                    .map(|e| Edit {
                        old_text: e.old_text,
                        new_text: e.new_text,
                    })
                    .collect();

                with_file_mutation_queue(abs_path.clone(), move || async move {
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }

                    let abs_for_read = abs_path.clone();
                    let ops_for_read = ops.clone();
                    let dp_for_read = display_path.clone();
                    let raw_content = tokio::task::spawn_blocking(move || -> Result<String, String> {
                        if let Err(err) = ops_for_read.access(&abs_for_read) {
                            return Err(format!("Could not edit file: {dp_for_read}. {err}."));
                        }
                        ops_for_read.read_file(&abs_for_read)
                    })
                    .await
                    .map_err(|e| format!("Edit read task panicked: {e}"))??;

                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }

                    let (bom, content) = split_bom(&raw_content);
                    let original_ending = detect_line_ending(content);
                    let normalized_content = normalize_to_lf(content);
                    let applied = apply_edits_to_normalized_content(
                        &normalized_content,
                        &edits,
                        &display_path,
                    )?;

                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }

                    let final_content = format!(
                        "{bom}{}",
                        restore_line_endings(&applied.new_content, original_ending)
                    );
                    let abs_for_write = abs_path.clone();
                    let ops_for_write = ops.clone();
                    tokio::task::spawn_blocking(move || -> Result<(), String> {
                        ops_for_write.write_file(&abs_for_write, &final_content)
                    })
                    .await
                    .map_err(|e| format!("Edit write task panicked: {e}"))??;

                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }

                    let diff_res = generate_diff_string(&applied.base_content, &applied.new_content);
                    let patch = generate_unified_patch(
                        &display_path,
                        &applied.base_content,
                        &applied.new_content,
                    );
                    let details = EditToolDetails {
                        diff: diff_res.diff,
                        patch,
                        first_changed_line: diff_res.first_changed_line,
                    };

                    Ok(AgentToolResult {
                            is_error: false,
                            terminate: None,
                        content: vec![UserContentBlock::Text(TextContent::new(format!(
                            "Successfully replaced {} block(s) in {display_path}.",
                            edits.len()
                        )))],
                        details: serde_json::to_value(details).unwrap_or(Value::Null),
                        structured_content: None,
                        usage: None,
                    })
                })
                .await
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prepare_arguments(Arc::new(|input| prepare_edit_arguments(input.clone())))
    .with_prompt_snippet(
        "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
    )
    .with_prompt_guidelines(vec![
        "Use edit for targeted changes. Batch changes to one file in a single call.".to_owned(),
        "Each edits[].oldText must exactly and uniquely match the original file. Keep matches short and non-overlapping; merge nearby changes without including large unchanged regions.".to_owned(),
    ])
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Never)
}

pub fn create_edit_tool(cwd: impl Into<String>, options: Option<EditToolOptions>) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_edit_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

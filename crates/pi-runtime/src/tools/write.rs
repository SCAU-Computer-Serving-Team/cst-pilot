use std::fs;
use std::path::Path;
use std::sync::Arc;

use pi_agent::{AgentTool, AgentToolResult, BoxFuture, ToolReplayPolicy};
use pi_session::{ExtensionToolContext, NoOpUiContext, ToolDefinition, ToolExposure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::read::resolve_to_cwd;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolInput {
    /// Path to the file to write (relative or absolute)
    pub path: String,
    /// Content to write to the file
    pub content: String,
}

pub trait WriteOperations: Send + Sync {
    fn mkdir(&self, dir: &Path) -> Result<(), String>;
    fn write_file(&self, absolute_path: &Path, content: &str) -> Result<(), String>;
}

#[derive(Default, Clone)]
pub struct LocalWriteOperations;

impl WriteOperations for LocalWriteOperations {
    fn mkdir(&self, dir: &Path) -> Result<(), String> {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create directory '{}': {e}", dir.display()))?;
        }
        Ok(())
    }

    fn write_file(&self, absolute_path: &Path, content: &str) -> Result<(), String> {
        fs::write(absolute_path, content.as_bytes())
            .map_err(|e| format!("Failed to write '{}': {e}", absolute_path.display()))
    }
}

#[derive(Clone, Default)]
pub struct WriteToolOptions {
    pub operations: Option<Arc<dyn WriteOperations>>,
}

pub fn create_write_tool_definition(
    cwd: impl Into<String>,
    options: Option<WriteToolOptions>,
) -> ToolDefinition {
    let default_cwd = cwd.into();
    let ops: Arc<dyn WriteOperations> = options
        .and_then(|o| o.operations)
        .unwrap_or_else(|| Arc::new(LocalWriteOperations));

    ToolDefinition::new_typed_with_context::<WriteToolInput, _, _>(
        "write",
        "write",
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.",
        move |_tool_call_id, input: WriteToolInput, _on_update, cancel: CancellationToken, ctx: ExtensionToolContext| {
            let ops = ops.clone();
            let base_cwd = default_cwd.clone();
            Box::pin(async move {
                let effective_cwd = if ctx.cwd().is_empty() {
                    base_cwd
                } else {
                    ctx.cwd()
                };
                let abs_path = resolve_to_cwd(&input.path, &effective_cwd);
                let display_path = input.path.clone();
                let content = input.content;

                with_file_mutation_queue(abs_path.clone(), move || async move {
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }
                    let abs_for_mkdir = abs_path.clone();
                    let ops_for_mkdir = ops.clone();
                    tokio::task::spawn_blocking(move || -> Result<(), String> {
                        if let Some(dir) = abs_for_mkdir.parent() {
                            ops_for_mkdir.mkdir(dir)?;
                        }
                        Ok(())
                    })
                    .await
                    .map_err(|e| format!("Write mkdir task panicked: {e}"))??;

                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }

                    let abs_for_write = abs_path.clone();
                    let ops_for_write = ops.clone();
                    tokio::task::spawn_blocking(move || -> Result<(), String> {
                        ops_for_write.write_file(&abs_for_write, &content)
                    })
                    .await
                    .map_err(|e| format!("Write file task panicked: {e}"))??;

                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }

                    Ok(AgentToolResult::text(format!(
                        "Successfully wrote to {display_path}"
                    )))
                })
                .await
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prompt_snippet("Create or overwrite files")
    .with_prompt_guidelines(vec![
        "Use write only for new files or complete rewrites.".to_owned(),
    ])
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Never)
}

pub fn create_write_tool(cwd: impl Into<String>, options: Option<WriteToolOptions>) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_write_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

use std::path::Path;
use std::sync::Arc;

use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use pi_agent::{AgentTool, AgentToolResult, BoxFuture, ToolReplayPolicy};
use pi_ai::{TextContent, UserContentBlock};
use pi_session::{ExtensionToolContext, NoOpUiContext, ToolDefinition, ToolExposure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::skills::to_posix_path;
use crate::tools::read::resolve_to_cwd;
use crate::tools::truncate::{
    DEFAULT_MAX_BYTES, TruncationOptions, TruncationResult, format_size, truncate_head,
};

pub const DEFAULT_FIND_LIMIT: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindToolInput {
    /// Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'
    pub pattern: String,
    /// Directory to search in (default: current directory)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Maximum number of results (default: 1000)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_limit_reached: Option<usize>,
}

pub trait FindOperations: Send + Sync {
    fn exists(&self, absolute_path: &Path) -> bool;
    fn glob(
        &self,
        pattern: &str,
        search_path: &Path,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>, String>;
}

pub fn relativize_find_result_path(result_path: &str, search_path: &Path) -> String {
    let had_trailing = result_path.ends_with('/') || result_path.ends_with('\\');
    let p = Path::new(result_path);
    let rel = if p.is_absolute() {
        p.strip_prefix(search_path).unwrap_or(p)
    } else {
        p
    };
    let posix = to_posix_path(rel);
    let mut posix = posix.strip_prefix("./").unwrap_or(&posix).to_owned();
    if had_trailing && !posix.ends_with('/') {
        posix.push('/');
    }
    posix
}

pub(crate) fn compile_find_glob_matcher(pattern: &str) -> Result<(GlobMatcher, bool), String> {
    let clean = pattern.trim().replace('\\', "/");
    let clean = clean.strip_prefix("./").unwrap_or(&clean);
    let match_full_path = clean.contains('/');
    let matcher = GlobBuilder::new(clean.trim_end_matches('/'))
        .literal_separator(true)
        .case_insensitive(true)
        .build()
        .map_err(|_| "Invalid glob pattern".to_owned())?
        .compile_matcher();
    Ok((matcher, match_full_path))
}

/// Shared native traversal: ignore semantics include nested negations and anchored patterns.
/// Symlinks are never followed; cancellation is checked by the consumer for each entry.
pub(crate) fn native_walk(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_global(false)
        .add_custom_ignore_filename(".fdignore")
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(entry.file_name().to_str(), Some(".git" | "node_modules"))
        })
        .sort_by_file_name(|a, b| a.cmp(b))
        .build()
}

#[derive(Default, Clone)]
pub struct LocalFindOperations;

impl FindOperations for LocalFindOperations {
    fn exists(&self, absolute_path: &Path) -> bool {
        absolute_path.exists()
    }

    fn glob(
        &self,
        pattern: &str,
        search_path: &Path,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>, String> {
        let (matcher, full_path) = compile_find_glob_matcher(pattern)?;
        let mut results = Vec::new();
        let mut bytes = 0usize;
        for entry in native_walk(search_path) {
            if cancel.is_cancelled() {
                return Err("Operation aborted".to_owned());
            }
            let Ok(entry) = entry else {
                continue;
            };
            if entry.depth() == 0 || entry.file_type().is_none_or(|t| t.is_symlink()) {
                continue;
            }
            let rel = to_posix_path(
                entry
                    .path()
                    .strip_prefix(search_path)
                    .unwrap_or(entry.path()),
            );
            let name = entry.file_name().to_string_lossy();
            if !matcher.is_match(if full_path {
                rel.as_str()
            } else {
                name.as_ref()
            }) {
                continue;
            }
            let result = if entry.file_type().is_some_and(|t| t.is_dir()) {
                format!("{rel}/")
            } else {
                rel
            };
            bytes = bytes.saturating_add(result.len() + 1);
            results.push(result);
            // Retain at most one extra record to make byte truncation visible to the caller.
            if results.len() >= limit || bytes > DEFAULT_MAX_BYTES {
                break;
            }
        }
        Ok(results)
    }
}

struct DefaultFindOperations {
    backend: super::SearchBackend,
}

impl FindOperations for DefaultFindOperations {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn glob(
        &self,
        pattern: &str,
        root: &Path,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>, String> {
        if self.backend != super::SearchBackend::Native {
            let mut results = Vec::new();
            let mut bytes = 0usize;
            let args = [
                "--glob",
                pattern,
                ".",
                "--hidden",
                "--ignore-case",
                "--exclude",
                ".git",
                "--exclude",
                "node_modules",
                "--color",
                "never",
                "--print0",
                "--type",
                "f",
                "--type",
                "d",
                "--no-require-git",
                "--path-separator",
                "/",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            let result = super::search_process::run_search(
                "fd",
                &args,
                root,
                0,
                DEFAULT_MAX_BYTES,
                cancel,
                |line| {
                    let path = String::from_utf8_lossy(line).into_owned();
                    let path = relativize_find_result_path(&path, root);
                    bytes = bytes.saturating_add(path.len() + 1);
                    results.push(path);
                    results.len() < limit && bytes <= DEFAULT_MAX_BYTES
                },
            );
            match result {
                Ok(Some(())) => return Ok(results),
                _ if cancel.is_cancelled() => return Err("Operation aborted".to_owned()),
                Err(error) if self.backend == super::SearchBackend::External => return Err(error),
                Ok(None) if self.backend == super::SearchBackend::External => {
                    return Err(
                        "External find is unavailable or exceeded its record limit".to_owned()
                    );
                }
                _ => {}
            }
        }
        LocalFindOperations.glob(pattern, root, limit, cancel)
    }
}

#[derive(Clone, Default)]
pub struct FindToolOptions {
    pub operations: Option<Arc<dyn FindOperations>>,
    pub backend: super::SearchBackend,
}

pub fn create_find_tool_definition(
    cwd: impl Into<String>,
    options: Option<FindToolOptions>,
) -> ToolDefinition {
    let default_cwd = cwd.into();
    let options = options.unwrap_or_default();
    let ops: Arc<dyn FindOperations> = options.operations.unwrap_or_else(|| {
        Arc::new(DefaultFindOperations {
            backend: options.backend,
        })
    });

    ToolDefinition::new_typed_with_context::<FindToolInput, _, _>(
        "find",
        "find",
        format!(
            "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to {DEFAULT_FIND_LIMIT} results or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        ),
        move |_tool_call_id, input: FindToolInput, _on_update, cancel: CancellationToken, ctx: ExtensionToolContext| {
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
                let search_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &effective_cwd);
                let effective_limit = input.limit.unwrap_or(DEFAULT_FIND_LIMIT).max(1);
                let pattern = input.pattern;

                tokio::task::spawn_blocking(move || -> Result<AgentToolResult, String> {
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }
                    if !ops.exists(&search_path) {
                        return Err(format!("Path not found: {}", search_path.display()));
                    }
                    let raw_results = ops.glob(&pattern, &search_path, effective_limit, &cancel)?;
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }
                    if raw_results.is_empty() {
                        return Ok(AgentToolResult::text("No files found matching pattern"));
                    }

                    let relativized: Vec<String> = raw_results
                        .into_iter()
                        .take(effective_limit)
                        .map(|p| relativize_find_result_path(&p, &search_path))
                        .collect();
                    let result_limit_reached = relativized.len() >= effective_limit;
                    let raw_output = relativized.join("\n");
                    let truncation = truncate_head(
                        &raw_output,
                        TruncationOptions {
                            max_lines: Some(usize::MAX),
                            max_bytes: Some(DEFAULT_MAX_BYTES),
                        },
                    );

                    let mut output = truncation.content.clone();
                    let mut notices = Vec::new();
                    let mut details = FindToolDetails {
                        truncation: None,
                        result_limit_reached: None,
                    };
                    if result_limit_reached {
                        notices.push(format!("{effective_limit} results limit reached"));
                        details.result_limit_reached = Some(effective_limit);
                    }
                    if truncation.truncated {
                        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                        details.truncation = Some(truncation);
                    }
                    if !notices.is_empty() {
                        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
                    }

                    let details_val = if details.truncation.is_some()
                        || details.result_limit_reached.is_some()
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
                .map_err(|e| format!("Find task panicked: {e}"))?
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prompt_snippet("Find files by glob pattern (respects .gitignore)")
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Safe)
}

pub fn create_find_tool(cwd: impl Into<String>, options: Option<FindToolOptions>) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_find_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

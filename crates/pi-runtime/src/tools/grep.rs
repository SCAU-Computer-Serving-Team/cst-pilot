use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Cursor};
use std::path::Path;
use std::sync::Arc;

use pi_agent::{AgentTool, AgentToolResult, BoxFuture, ToolReplayPolicy};
use pi_ai::{TextContent, UserContentBlock};
use pi_session::{ExtensionToolContext, NoOpUiContext, ToolDefinition, ToolExposure};
use regex::Regex;
use regex_automata::{
    Input,
    dfa::{Automaton, dense},
    util::primitives::StateID,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::skills::to_posix_path;
use crate::tools::find::{compile_find_glob_matcher, native_walk};
use crate::tools::read::resolve_to_cwd;
use crate::tools::truncate::{
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, TruncationOptions, TruncationResult, format_size,
    truncate_head, truncate_line,
};

pub const DEFAULT_GREP_LIMIT: usize = 100;
const MAX_GREP_SCAN_LINE_BYTES: usize = GREP_MAX_LINE_LENGTH * 8;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolInput {
    /// Search pattern (regex or literal string)
    pub pattern: String,
    /// Directory or file to search (default: current directory)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    /// Case-insensitive search (default: false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_case: Option<bool>,
    /// Treat pattern as literal string instead of regex (default: false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub literal: Option<bool>,
    /// Number of lines to show before and after each match (default: 0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    /// Maximum number of matches to return (default: 100)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_limit_reached: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_truncated: Option<bool>,
}

pub trait GrepOperations: Send + Sync {
    fn is_directory(&self, absolute_path: &Path) -> Result<bool, String>;
    fn read_file(&self, absolute_path: &Path) -> Result<String, String>;
    fn visit_files(
        &self,
        root: &Path,
        cancel: &CancellationToken,
        visitor: &mut dyn FnMut(&Path) -> Result<bool, String>,
    ) -> Result<(), String> {
        for entry in native_walk(root) {
            if cancel.is_cancelled() {
                return Err("Operation aborted".to_owned());
            }
            let Ok(entry) = entry else {
                continue;
            };
            if entry.file_type().is_some_and(|t| t.is_file()) && !visitor(entry.path())? {
                break;
            }
        }
        Ok(())
    }
    fn open_reader(&self, absolute_path: &Path) -> Result<Box<dyn BufRead + Send>, String> {
        let text = self.read_file(absolute_path)?;
        Ok(Box::new(BufReader::with_capacity(
            8192,
            Cursor::new(text.into_bytes()),
        )))
    }
}

#[derive(Default, Clone)]
pub struct LocalGrepOperations;

impl GrepOperations for LocalGrepOperations {
    fn is_directory(&self, absolute_path: &Path) -> Result<bool, String> {
        fs::metadata(absolute_path)
            .map(|m| m.is_dir())
            .map_err(|e| e.to_string())
    }

    fn read_file(&self, absolute_path: &Path) -> Result<String, String> {
        fs::read_to_string(absolute_path).map_err(|e| e.to_string())
    }

    fn open_reader(&self, absolute_path: &Path) -> Result<Box<dyn BufRead + Send>, String> {
        let file = File::open(absolute_path).map_err(|e| e.to_string())?;
        Ok(Box::new(BufReader::with_capacity(8192, file)))
    }
}

#[derive(Clone, Default)]
pub struct GrepToolOptions {
    pub operations: Option<Arc<dyn GrepOperations>>,
    pub backend: super::SearchBackend,
}

struct GrepState {
    output_lines: Vec<String>,
    output_bytes: usize,
    byte_limit_reached: bool,
    match_count: usize,
    match_limit_reached: bool,
    lines_truncated: bool,
    effective_limit: usize,
    context_lines: usize,
}

impl GrepState {
    fn emit(&mut self, line: String) {
        if self.byte_limit_reached {
            return;
        }
        self.output_bytes = self
            .output_bytes
            .saturating_add(line.len() + usize::from(!self.output_lines.is_empty()));
        self.output_lines.push(line);
        self.byte_limit_reached = self.output_bytes > DEFAULT_MAX_BYTES;
    }
}

struct StreamingLineSearch {
    dfa: Arc<dense::DFA<Vec<u32>>>,
    state: StateID,
    matched: bool,
    quit: bool,
    pending_cr: usize,
}
impl StreamingLineSearch {
    fn new(dfa: Arc<dense::DFA<Vec<u32>>>) -> Result<Self, String> {
        let state = dfa
            .start_state_forward(&Input::new(b""))
            .map_err(|_| "Cannot start bounded long-line search".to_owned())?;
        Ok(Self {
            dfa,
            state,
            matched: false,
            quit: false,
            pending_cr: 0,
        })
    }
    fn byte(&mut self, byte: u8) {
        if self.matched || self.quit {
            return;
        }
        self.state = self.dfa.next_state(self.state, byte);
        self.matched |= self.dfa.is_match_state(self.state);
        self.quit |= self.dfa.is_quit_state(self.state);
    }
    fn push(&mut self, byte: u8) {
        if byte == b'\r' {
            self.pending_cr += 1;
            return;
        }
        let cr = std::mem::take(&mut self.pending_cr);
        for _ in 0..cr {
            self.byte(b'\r');
        }
        self.byte(byte);
    }
    fn finish(mut self) -> Result<bool, String> {
        if self.quit {
            return Err("Pattern cannot be evaluated on a bounded long line".to_owned());
        }
        self.state = self.dfa.next_eoi_state(self.state);
        Ok(self.matched || self.dfa.is_match_state(self.state))
    }
}

fn grep_single_file_bounded(
    file_path: &Path,
    display_path: &str,
    ops: &dyn GrepOperations,
    matcher: &Regex,
    state: &mut GrepState,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let Ok(mut reader) = ops.open_reader(file_path) else {
        return Ok(());
    };
    let mut before: VecDeque<(usize, String, bool)> = VecDeque::new();
    let mut before_bytes = 0usize;
    let mut after = 0usize;
    let mut last_emitted = 0usize;
    let mut line_num = 1usize;
    let mut line = Vec::with_capacity(MAX_GREP_SCAN_LINE_BYTES);
    let mut capped = false;
    let mut streaming_dfa = None;
    let mut streaming: Option<StreamingLineSearch> = None;

    let mut process = |ln: usize,
                       raw: &[u8],
                       capped: bool,
                       full_match: Option<bool>,
                       state: &mut GrepState,
                       after: &mut usize| {
        if state.byte_limit_reached {
            return;
        }
        let decoded = String::from_utf8_lossy(raw);
        let clean = decoded.trim_end_matches('\r');
        let is_match = state.match_count < state.effective_limit
            && full_match.unwrap_or_else(|| matcher.is_match(clean));
        let (mut shown, truncated) = truncate_line(clean, Some(GREP_MAX_LINE_LENGTH));
        if capped && !truncated {
            shown.push_str("... [truncated]");
        }
        let truncated = truncated || capped;
        if is_match {
            state.match_count += 1;
            state.match_limit_reached = state.match_count >= state.effective_limit;
            for (n, t, was_capped) in before.drain(..) {
                if n > last_emitted {
                    state.lines_truncated |= was_capped;
                    state.emit(format!("{display_path}-{n}- {t}"));
                    last_emitted = n;
                }
                if state.byte_limit_reached {
                    break;
                }
            }
            before_bytes = 0;
            state.lines_truncated |= truncated;
            state.emit(format!("{display_path}:{ln}: {shown}"));
            last_emitted = ln;
            *after = state.context_lines;
        } else if *after > 0 {
            state.lines_truncated |= truncated;
            state.emit(format!("{display_path}-{ln}- {shown}"));
            last_emitted = ln;
            *after -= 1;
        } else if state.context_lines > 0 {
            before_bytes += shown.len() + 1;
            before.push_back((ln, shown, truncated));
            while before.len() > state.context_lines || before_bytes > DEFAULT_MAX_BYTES {
                if let Some((_, old, _)) = before.pop_front() {
                    before_bytes -= old.len() + 1;
                }
            }
        }
    };

    loop {
        if cancel.is_cancelled() {
            return Err("Operation aborted".to_owned());
        }
        if state.byte_limit_reached || (state.match_limit_reached && after == 0) {
            break;
        }
        let buf = reader
            .fill_buf()
            .map_err(|_| "Failed to read search input".to_owned())?;
        if buf.is_empty() {
            break;
        }
        if buf.contains(&0) {
            break;
        }
        let mut consumed = 0;
        for &byte in buf {
            consumed += 1;
            if byte == b'\n' {
                let full_match = streaming
                    .take()
                    .map(StreamingLineSearch::finish)
                    .transpose()?;
                process(line_num, &line, capped, full_match, state, &mut after);
                line.clear();
                capped = false;
                line_num += 1;
                if state.byte_limit_reached || (state.match_limit_reached && after == 0) {
                    break;
                }
            } else if line.len() < MAX_GREP_SCAN_LINE_BYTES {
                line.push(byte);
            } else {
                capped = true;
                if streaming.is_none() {
                    if streaming_dfa.is_none() {
                        let dfa = dense::Builder::new()
                            .configure(
                                dense::Config::new()
                                    .dfa_size_limit(Some(4 * 1024 * 1024))
                                    .determinize_size_limit(Some(8 * 1024 * 1024))
                                    .unicode_word_boundary(true),
                            )
                            .build(matcher.as_str())
                            .map_err(|_| {
                                "Pattern exceeds bounded long-line search capacity".to_owned()
                            })?;
                        streaming_dfa = Some(Arc::new(dfa));
                    }
                    let mut search =
                        StreamingLineSearch::new(streaming_dfa.as_ref().unwrap().clone())?;
                    for &byte in &line {
                        search.push(byte);
                    }
                    streaming = Some(search);
                }
                streaming.as_mut().unwrap().push(byte);
            }
        }
        reader.consume(consumed);
    }
    if !line.is_empty() && !state.byte_limit_reached && (!state.match_limit_reached || after > 0) {
        let full_match = streaming
            .take()
            .map(StreamingLineSearch::finish)
            .transpose()?;
        process(line_num, &line, capped, full_match, state, &mut after);
    }
    Ok(())
}

fn walk_grep_dir(
    root: &Path,
    ops: &dyn GrepOperations,
    matcher: &Regex,
    glob_filter: Option<&(globset::GlobMatcher, bool)>,
    state: &mut GrepState,
    cancel: &CancellationToken,
) -> Result<(), String> {
    ops.visit_files(root, cancel, &mut |path| {
        if cancel.is_cancelled() {
            return Err("Operation aborted".to_owned());
        }
        if state.match_limit_reached || state.byte_limit_reached {
            return Ok(false);
        }
        let rel = to_posix_path(path.strip_prefix(root).unwrap_or(path));
        if let Some((glob, full)) = glob_filter {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !glob.is_match(if *full { rel.as_str() } else { name.as_ref() }) {
                return Ok(true);
            }
        }
        grep_single_file_bounded(path, &rel, ops, matcher, state, cancel)?;
        Ok(!state.match_limit_reached && !state.byte_limit_reached)
    })?;
    Ok(())
}

fn grep_external(
    path: &Path,
    is_dir: bool,
    input: &GrepToolInput,
    state: &mut GrepState,
    cancel: &CancellationToken,
) -> Result<bool, String> {
    let cwd = if is_dir {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    let target = if is_dir {
        ".".to_owned()
    } else {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    let mut args = vec![
        "--json",
        "--crlf",
        "--hidden",
        "--no-require-git",
        "--color",
        "never",
        "--glob",
        "!.git",
        "--glob",
        "!node_modules",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if input.ignore_case.unwrap_or(false) {
        args.push("--ignore-case".to_owned());
    }
    if input.literal.unwrap_or(false) {
        args.push("--fixed-strings".to_owned());
    }
    // rg include globs override ignore files. Apply the user's glob after rg's ignore filtering.
    let glob_filter = input
        .glob
        .as_deref()
        .map(compile_find_glob_matcher)
        .transpose()?;
    args.extend([
        "--context".to_owned(),
        input.context.unwrap_or(0).to_string(),
        "--regexp".to_owned(),
        input.pattern.clone(),
        "--".to_owned(),
        target,
    ]);
    let mut after = 0usize;
    let mut malformed = false;
    let result =
        super::search_process::run_search("rg", &args, cwd, b'\n', 1024 * 1024, cancel, |line| {
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                malformed = true;
                return false;
            };
            let kind = value["type"].as_str().unwrap_or("");
            if kind != "match" && kind != "context" {
                return !state.match_limit_reached
                    || (after > 0 && kind != "end" && kind != "begin");
            }
            let data = &value["data"];
            let (Some(path), Some(line), Some(number)) = (
                data["path"]["text"].as_str(),
                data["lines"]["text"].as_str(),
                data["line_number"].as_u64(),
            ) else {
                malformed = true;
                return false;
            };
            let path = path.replace('\\', "/");
            let path = path.strip_prefix("./").unwrap_or(&path);
            if let Some((filter, full)) = &glob_filter {
                let name = path.rsplit('/').next().unwrap_or(path);
                if !filter.is_match(if *full { path } else { name }) {
                    return true;
                }
            }
            let (shown, truncated) = truncate_line(
                line.trim_end_matches(['\r', '\n']),
                Some(GREP_MAX_LINE_LENGTH),
            );
            state.lines_truncated |= truncated;
            if kind == "match" && !state.match_limit_reached {
                state.match_count += 1;
                state.match_limit_reached = state.match_count >= state.effective_limit;
                state.emit(format!("{path}:{number}: {shown}"));
                after = state.context_lines;
            } else {
                state.emit(format!("{path}-{number}- {shown}"));
                after = after.saturating_sub(1);
            }
            // Before-context may fill the display budget before the first actual match.
            (!state.byte_limit_reached || state.match_count == 0)
                && (!state.match_limit_reached || after > 0)
        })?;
    Ok(result.is_some() && !malformed)
}

pub fn create_grep_tool_definition(
    cwd: impl Into<String>,
    options: Option<GrepToolOptions>,
) -> ToolDefinition {
    let default_cwd = cwd.into();
    let options = options.unwrap_or_default();
    let backend = if options.operations.is_some() {
        super::SearchBackend::Native
    } else {
        options.backend
    };
    let ops: Arc<dyn GrepOperations> = options
        .operations
        .unwrap_or_else(|| Arc::new(LocalGrepOperations));

    ToolDefinition::new_typed_with_context::<GrepToolInput, _, _>(
        "grep",
        "grep",
        format!(
            "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {DEFAULT_GREP_LIMIT} matches or {}KB (whichever is hit first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
            DEFAULT_MAX_BYTES / 1024
        ),
        move |_tool_call_id, input: GrepToolInput, _on_update, cancel: CancellationToken, ctx: ExtensionToolContext| {
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
                let effective_limit = input.limit.unwrap_or(DEFAULT_GREP_LIMIT).max(1);
                let context_lines = input.context.unwrap_or(0);
                let external_input = input.clone();
                let raw_pattern = if input.literal.unwrap_or(false) {
                    regex::escape(&input.pattern)
                } else {
                    input.pattern.clone()
                };
                let raw_pattern = if input.ignore_case.unwrap_or(false) { format!("(?i:{raw_pattern})") } else { raw_pattern };
                let matcher = Regex::new(&raw_pattern)
                    .map_err(|e| format!("Invalid search pattern: {e}"))?;

                let glob_filter = match &input.glob {
                    Some(g) if !g.trim().is_empty() => Some(compile_find_glob_matcher(g)?),
                    _ => None,
                };

                tokio::task::spawn_blocking(move || -> Result<AgentToolResult, String> {
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }
                    let is_dir = ops
                        .is_directory(&search_path)
                        .map_err(|_| format!("Path not found: {}", search_path.display()))?;

                    let mut state = GrepState {
                        output_lines: Vec::new(),
                        output_bytes: 0,
                        byte_limit_reached: false,
                        match_count: 0,
                        match_limit_reached: false,
                        lines_truncated: false,
                        effective_limit,
                        context_lines,
                    };

                    let external_done = if backend != super::SearchBackend::Native {
                        match grep_external(&search_path, is_dir, &external_input, &mut state, &cancel) {
                            Ok(true) => true,
                            _ if cancel.is_cancelled() => return Err("Operation aborted".to_owned()),
                            Err(error) if backend == super::SearchBackend::External => return Err(error),
                            _ if backend == super::SearchBackend::External => return Err("External grep unavailable or record limit exceeded".to_owned()),
                            _ => {
                                state.output_lines.clear(); state.output_bytes = 0; state.byte_limit_reached = false;
                                state.match_count = 0; state.match_limit_reached = false; state.lines_truncated = false;
                                false
                            }
                        }
                    } else { false };
                    if external_done {
                        // External records have already passed the shared output bounds.
                    } else if is_dir {
                        walk_grep_dir(
                            &search_path,
                            ops.as_ref(),
                            &matcher,
                            glob_filter.as_ref(),
                            &mut state,
                            &cancel,
                        )?;
                    } else {
                        let basename = search_path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| search_path.to_string_lossy().into_owned());
                        grep_single_file_bounded(
                            &search_path,
                            &basename,
                            ops.as_ref(),
                            &matcher,
                            &mut state,
                            &cancel,
                        )?;
                    }

                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }
                    if state.match_count == 0 {
                        return Ok(AgentToolResult::text("No matches found"));
                    }

                    let raw_output = state.output_lines.join("\n");
                    let truncation = truncate_head(
                        &raw_output,
                        TruncationOptions {
                            max_lines: Some(usize::MAX),
                            max_bytes: Some(DEFAULT_MAX_BYTES),
                        },
                    );
                    let mut output = truncation.content.clone();
                    let mut notices = Vec::new();
                    let mut details = GrepToolDetails {
                        truncation: None,
                        match_limit_reached: None,
                        lines_truncated: None,
                    };

                    if state.match_limit_reached {
                        notices.push(format!(
                            "{effective_limit} matches limit reached. Use limit={} for more, or refine pattern",
                            effective_limit.saturating_mul(2)
                        ));
                        details.match_limit_reached = Some(effective_limit);
                    }
                    if truncation.truncated {
                        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                        details.truncation = Some(truncation);
                    }
                    if state.lines_truncated {
                        notices.push(format!(
                            "Some lines were truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
                        ));
                        details.lines_truncated = Some(true);
                    }
                    if !notices.is_empty() {
                        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
                    }

                    let details_val = if details.truncation.is_some()
                        || details.match_limit_reached.is_some()
                        || details.lines_truncated.is_some()
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
                .map_err(|e| format!("Grep task panicked: {e}"))?
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prompt_snippet("Search file contents for patterns (respects .gitignore)")
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Safe)
}

pub fn create_grep_tool(cwd: impl Into<String>, options: Option<GrepToolOptions>) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_grep_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent::{AgentTool, AgentToolResult, BoxFuture, ToolReplayPolicy};
use pi_ai::{ImageContent, Model, ModelInputModality, TextContent, UserContentBlock};
use pi_session::{ExtensionToolContext, NoOpUiContext, ToolDefinition, ToolExposure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::image_process::{ImageResizeOptions, ProcessImageOptions, process_image_sync};
use crate::tools::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationResult, format_size,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    /// Path to the file to read (relative or absolute)
    pub path: String,
    /// Line number to start reading from (1-indexed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    /// Maximum number of lines to read
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

pub trait ReadOperations: Send + Sync {
    fn access(&self, absolute_path: &Path) -> Result<(), String>;
    fn detect_image_mime_type(&self, absolute_path: &Path) -> Result<Option<String>, String>;
    fn read_bytes(&self, absolute_path: &Path) -> Result<Vec<u8>, String>;
    fn open_reader(&self, absolute_path: &Path) -> Result<Box<dyn BufRead + Send>, String> {
        let bytes = self.read_bytes(absolute_path)?;
        Ok(Box::new(BufReader::with_capacity(8192, Cursor::new(bytes))))
    }
}

#[derive(Default, Clone)]
pub struct LocalReadOperations;

impl ReadOperations for LocalReadOperations {
    fn access(&self, absolute_path: &Path) -> Result<(), String> {
        if !absolute_path.exists() {
            return Err(format!(
                "ENOENT: no such file or directory, access '{}'",
                absolute_path.display()
            ));
        }
        if absolute_path.is_dir() {
            return Err(format!(
                "EISDIR: illegal operation on a directory, read '{}'",
                absolute_path.display()
            ));
        }
        File::open(absolute_path)
            .map(|_| ())
            .map_err(|e| format!("Failed to access '{}': {e}", absolute_path.display()))
    }

    fn detect_image_mime_type(&self, absolute_path: &Path) -> Result<Option<String>, String> {
        detect_supported_image_mime_type(absolute_path)
    }

    fn read_bytes(&self, absolute_path: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(absolute_path)
            .map_err(|e| format!("Failed to read '{}': {e}", absolute_path.display()))
    }

    fn open_reader(&self, absolute_path: &Path) -> Result<Box<dyn BufRead + Send>, String> {
        let file = File::open(absolute_path)
            .map_err(|e| format!("Failed to open '{}': {e}", absolute_path.display()))?;
        Ok(Box::new(BufReader::with_capacity(8192, file)))
    }
}

#[derive(Clone, Default)]
pub struct ReadToolOptions {
    pub auto_resize_images: Option<bool>,
    pub resize_options: Option<ImageResizeOptions>,
    pub operations: Option<Arc<dyn ReadOperations>>,
}

pub fn resolve_to_cwd(raw_path: &str, cwd: &str) -> PathBuf {
    let trimmed = raw_path.trim();
    let p = Path::new(trimmed);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd).join(p)
    }
}

pub fn detect_supported_image_mime_type(path: &Path) -> Result<Option<String>, String> {
    if let Ok(mut file) = File::open(path) {
        use std::io::Read;
        let mut magic = [0u8; 16];
        let n = file.read(&mut magic).unwrap_or(0);
        let head = &magic[..n];
        if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Ok(Some("image/png".to_owned()));
        }
        if head.starts_with(b"\xff\xd8\xff") {
            return Ok(Some("image/jpeg".to_owned()));
        }
        if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            return Ok(Some("image/gif".to_owned()));
        }
        if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
            return Ok(Some("image/webp".to_owned()));
        }
        if head.starts_with(b"BM")
            && let Some(ext) = path.extension().and_then(|e| e.to_str())
            && ext.eq_ignore_ascii_case("bmp")
        {
            return Ok(Some("image/bmp".to_owned()));
        }
    }
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return Ok(None);
    };
    Ok(match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png".to_owned()),
        "jpg" | "jpeg" => Some("image/jpeg".to_owned()),
        "gif" => Some("image/gif".to_owned()),
        "webp" => Some("image/webp".to_owned()),
        "bmp" => Some("image/bmp".to_owned()),
        _ => None,
    })
}

fn non_vision_image_note(model: Option<&Model>) -> Option<&'static str> {
    let m = model?;
    if m.input.contains(&ModelInputModality::Image) {
        None
    } else {
        Some(
            "[Current model does not support images. The image will be omitted from this request.]",
        )
    }
}

struct BoundedScanOutput {
    total_file_lines: usize,
    selected_total_lines: usize,
    selected_total_bytes: usize,
    output_lines: Vec<String>,
    first_line_bytes: usize,
    first_line_exceeds_limit: bool,
    truncated: bool,
    truncated_by: Option<TruncatedBy>,
}

/// 使用固定块 `fill_buf/consume` 流式扫描文件，在扫描过程中直接限制单行缓冲大小（上限 `max_bytes + 1`），
/// 即使遇到百兆级无换行单行大文件也不会在内存中分配超长字符串。
fn scan_text_reader_bounded(
    mut reader: Box<dyn BufRead + Send>,
    start_line_0: usize,
    user_limit: Option<usize>,
    max_lines: usize,
    max_bytes: usize,
    cancel: &CancellationToken,
) -> Result<BoundedScanOutput, String> {
    let mut current_line_idx = 0usize; // 0-indexed
    let mut current_line_buf: Vec<u8> = Vec::new();
    let mut current_line_total_bytes = 0usize;
    let mut output_lines: Vec<String> = Vec::new();
    let mut output_bytes = 0usize;
    let mut first_line_bytes = 0usize;
    let mut first_line_exceeds_limit = false;
    let mut truncated = false;
    let mut truncated_by: Option<TruncatedBy> = None;
    let mut selected_total_lines = 0usize;
    let mut selected_total_bytes = 0usize;
    let mut stop_collecting_output = false;
    let mut saw_any_byte = false;

    let end_line_exclusive = user_limit.map(|l| start_line_0.saturating_add(l));

    let mut finish_line = |line_idx: usize,
                           line_buf: &mut Vec<u8>,
                           line_total_bytes: usize,
                           stop_collecting_output: &mut bool| {
        let in_user_window =
            line_idx >= start_line_0 && end_line_exclusive.is_none_or(|end| line_idx < end);
        if in_user_window {
            if selected_total_lines > 0 {
                selected_total_bytes = selected_total_bytes.saturating_add(1);
            }
            selected_total_bytes = selected_total_bytes.saturating_add(line_total_bytes);
            selected_total_lines += 1;

            if line_idx == start_line_0 {
                first_line_bytes = line_total_bytes;
                if line_total_bytes > max_bytes {
                    first_line_exceeds_limit = true;
                    truncated = true;
                    truncated_by = Some(TruncatedBy::Bytes);
                    *stop_collecting_output = true;
                }
            }

            if !*stop_collecting_output {
                if output_lines.len() >= max_lines {
                    truncated = true;
                    truncated_by = Some(TruncatedBy::Lines);
                    *stop_collecting_output = true;
                } else {
                    let add_bytes = line_total_bytes + usize::from(!output_lines.is_empty());
                    if output_bytes + add_bytes > max_bytes {
                        truncated = true;
                        truncated_by = Some(TruncatedBy::Bytes);
                        *stop_collecting_output = true;
                    } else {
                        let s = String::from_utf8_lossy(line_buf).into_owned();
                        let decoded_bytes = s.len() + usize::from(!output_lines.is_empty());
                        if output_bytes.saturating_add(decoded_bytes) > max_bytes {
                            if line_idx == start_line_0 {
                                first_line_exceeds_limit = true;
                                first_line_bytes = s.len();
                            }
                            truncated = true;
                            truncated_by = Some(TruncatedBy::Bytes);
                            *stop_collecting_output = true;
                        } else {
                            output_lines.push(s);
                            output_bytes += decoded_bytes;
                        }
                    }
                }
            }
        }
        line_buf.clear();
    };

    loop {
        if cancel.is_cancelled() {
            return Err("Operation aborted".to_owned());
        }
        let buf = reader
            .fill_buf()
            .map_err(|e| format!("Failed to read file stream: {e}"))?;
        if buf.is_empty() {
            break;
        }
        saw_any_byte = true;
        let len = buf.len();
        let mut start = 0usize;

        while start < len {
            if cancel.is_cancelled() {
                return Err("Operation aborted".to_owned());
            }
            let slice = &buf[start..len];
            if let Some(nl_rel) = slice.iter().position(|&b| b == b'\n') {
                let segment = &slice[..nl_rel];
                current_line_total_bytes = current_line_total_bytes.saturating_add(segment.len());
                if !stop_collecting_output
                    && current_line_idx >= start_line_0
                    && end_line_exclusive.is_none_or(|end| current_line_idx < end)
                    && current_line_buf.len() <= max_bytes
                {
                    let remain = (max_bytes + 1).saturating_sub(current_line_buf.len());
                    let take = segment.len().min(remain);
                    current_line_buf.extend_from_slice(&segment[..take]);
                }
                finish_line(
                    current_line_idx,
                    &mut current_line_buf,
                    current_line_total_bytes,
                    &mut stop_collecting_output,
                );
                current_line_idx += 1;
                current_line_total_bytes = 0;
                start += nl_rel + 1;
            } else {
                let segment = slice;
                current_line_total_bytes = current_line_total_bytes.saturating_add(segment.len());
                if !stop_collecting_output
                    && current_line_idx >= start_line_0
                    && end_line_exclusive.is_none_or(|end| current_line_idx < end)
                    && current_line_buf.len() <= max_bytes
                {
                    let remain = (max_bytes + 1).saturating_sub(current_line_buf.len());
                    let take = segment.len().min(remain);
                    current_line_buf.extend_from_slice(&segment[..take]);
                }
                start = len;
            }
        }

        reader.consume(len);
    }

    // 与 JS `textContent.split("\n")` 对齐：空文件也有 1 个空行；以 `\n` 结尾的文件在末尾 `\n` 后还有 1 个空行
    if !saw_any_byte || current_line_total_bytes > 0 || current_line_idx > 0 {
        finish_line(
            current_line_idx,
            &mut current_line_buf,
            current_line_total_bytes,
            &mut stop_collecting_output,
        );
        current_line_idx += 1;
    }

    Ok(BoundedScanOutput {
        total_file_lines: current_line_idx,
        selected_total_lines,
        selected_total_bytes,
        output_lines,
        first_line_bytes,
        first_line_exceeds_limit,
        truncated,
        truncated_by,
    })
}

pub fn create_read_tool_definition(
    cwd: impl Into<String>,
    options: Option<ReadToolOptions>,
) -> ToolDefinition {
    let default_cwd = cwd.into();
    let opts = options.unwrap_or_default();
    let auto_resize_images = opts.auto_resize_images.unwrap_or(true);
    let fallback_resize_options = opts.resize_options;
    let ops: Arc<dyn ReadOperations> = opts
        .operations
        .unwrap_or_else(|| Arc::new(LocalReadOperations));

    ToolDefinition::new_typed_with_context::<ReadToolInput, _, _>(
        "read",
        "read",
        format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_BYTES / 1024
        ),
        move |_tool_call_id, input: ReadToolInput, _on_update, cancel: CancellationToken, ctx: ExtensionToolContext| {
            let ops = ops.clone();
            let base_cwd = default_cwd.clone();
            let fallback_resize = fallback_resize_options.clone();
            Box::pin(async move {
                if input.offset == Some(0) || input.limit == Some(0) {
                    return Err("offset and limit must be positive integers".to_owned());
                }
                if cancel.is_cancelled() {
                    return Err("Operation aborted".to_owned());
                }
                let effective_cwd = if ctx.cwd().is_empty() {
                    base_cwd
                } else {
                    ctx.cwd()
                };
                let current_model = ctx.model();
                let abs_path = resolve_to_cwd(&input.path, &effective_cwd);
                let display_path = input.path.clone();

                tokio::task::spawn_blocking(move || -> Result<AgentToolResult, String> {
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }
                    ops.access(&abs_path)?;
                    if cancel.is_cancelled() {
                        return Err("Operation aborted".to_owned());
                    }

                    if let Some(mime_type) = ops.detect_image_mime_type(&abs_path)? {
                        let raw_bytes = ops.read_bytes(&abs_path)?;
                        if cancel.is_cancelled() {
                            return Err("Operation aborted".to_owned());
                        }
                        let processed = process_image_sync(
                            &raw_bytes,
                            &mime_type,
                            &ProcessImageOptions {
                                auto_resize_images: Some(auto_resize_images),
                                resize_options: fallback_resize,
                            },
                        );
                        if !processed.ok {
                            let mut text_note = format!(
                                "Read image file [{mime_type}]\n{}",
                                processed.message.unwrap_or_default()
                            );
                            if let Some(note) = non_vision_image_note(current_model.as_ref()) {
                                text_note.push('\n');
                                text_note.push_str(note);
                            }
                            return Ok(AgentToolResult {
                                is_error: false,
                                terminate: None,
                                content: vec![UserContentBlock::Text(TextContent::new(
                                    text_note.clone(),
                                ))],
                                details: Value::Null,
                                structured_content: Some(Value::String(text_note)),
                                usage: None,
                            });
                        }

                        let mut text_note = format!("Read image file [{}]", processed.mime_type);
                        if !processed.hints.is_empty() {
                            text_note.push('\n');
                            text_note.push_str(&processed.hints.join("\n"));
                        }
                        if let Some(note) = non_vision_image_note(current_model.as_ref()) {
                            text_note.push('\n');
                            text_note.push_str(note);
                        }
                        let content = vec![
                            UserContentBlock::Text(TextContent::new(text_note.clone())),
                            UserContentBlock::Image(ImageContent {
                                data: processed.data.clone(),
                                mime_type: processed.mime_type.clone(),
                            }),
                        ];
                        return Ok(AgentToolResult {
                            is_error: false,
                            terminate: None,
                            content,
                            details: Value::Null,
                            structured_content: Some(json!({
                                "type": "image",
                                "data": processed.data,
                                "mimeType": processed.mime_type,
                                "note": text_note,
                            })),
                            usage: None,
                        });
                    }

                    let start_line_0 = input.offset.map_or(0, |o| o.saturating_sub(1));
                    let start_line_display = start_line_0 + 1;
                    let reader = ops.open_reader(&abs_path)?;
                    let scanned = scan_text_reader_bounded(
                        reader,
                        start_line_0,
                        input.limit,
                        DEFAULT_MAX_LINES,
                        DEFAULT_MAX_BYTES,
                        &cancel,
                    )?;

                    if start_line_0 >= scanned.total_file_lines {
                        return Err(format!(
                            "Offset {} is beyond end of file ({} lines total)",
                            input.offset.unwrap_or(1),
                            scanned.total_file_lines
                        ));
                    }

                    let joined_output = scanned.output_lines.join("\n");
                    let final_output_bytes = joined_output.len();
                    let truncation = TruncationResult {
                        content: joined_output.clone(),
                        truncated: scanned.truncated,
                        truncated_by: scanned.truncated_by,
                        total_lines: scanned.selected_total_lines,
                        total_bytes: scanned.selected_total_bytes,
                        output_lines: scanned.output_lines.len(),
                        output_bytes: final_output_bytes,
                        last_line_partial: false,
                        first_line_exceeds_limit: scanned.first_line_exceeds_limit,
                        max_lines: DEFAULT_MAX_LINES,
                        max_bytes: DEFAULT_MAX_BYTES,
                    };

                    let mut details_val = Value::Null;
                    let output_text = if truncation.first_line_exceeds_limit {
                        let first_size = format_size(scanned.first_line_bytes);
                        details_val = serde_json::to_value(ReadToolDetails {
                            truncation: Some(truncation),
                        })
                        .unwrap_or(Value::Null);
                        format!(
                            "[Line {start_line_display} is {first_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {display_path} | head -c {DEFAULT_MAX_BYTES}]",
                            format_size(DEFAULT_MAX_BYTES)
                        )
                    } else if truncation.truncated {
                        let end_line_display =
                            start_line_display + truncation.output_lines.saturating_sub(1);
                        let next_offset = end_line_display + 1;
                        let mut text = truncation.content.clone();
                        if truncation.truncated_by == Some(TruncatedBy::Lines) {
                            text.push_str(&format!(
                                "\n\n[Showing lines {start_line_display}-{end_line_display} of {}. Use offset={next_offset} to continue.]",
                                scanned.total_file_lines
                            ));
                        } else {
                            text.push_str(&format!(
                                "\n\n[Showing lines {start_line_display}-{end_line_display} of {} ({} limit). Use offset={next_offset} to continue.]",
                                scanned.total_file_lines,
                                format_size(DEFAULT_MAX_BYTES)
                            ));
                        }
                        details_val = serde_json::to_value(ReadToolDetails {
                            truncation: Some(truncation),
                        })
                        .unwrap_or(Value::Null);
                        text
                    } else if let Some(ul) = input.limit
                        && start_line_0.saturating_add(ul) < scanned.total_file_lines
                    {
                        let remaining = scanned.total_file_lines - start_line_0.saturating_add(ul);
                        let next_offset = start_line_0.saturating_add(ul).saturating_add(1);
                        format!(
                            "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                            truncation.content
                        )
                    } else {
                        truncation.content
                    };

                    Ok(AgentToolResult {
                            is_error: false,
                            terminate: None,
                        content: vec![UserContentBlock::Text(TextContent::new(output_text.clone()))],
                        details: details_val,
                        structured_content: Some(Value::String(output_text)),
                        usage: None,
                    })
                })
                .await
                .map_err(|e| format!("Read task panicked: {e}"))?
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prompt_snippet("Read file contents")
    .with_prompt_guidelines(vec![
        "Use read to examine files instead of cat or sed.".to_owned(),
    ])
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Safe)
}

pub fn create_read_tool(cwd: impl Into<String>, options: Option<ReadToolOptions>) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_read_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use cst_os::strip_windows_verbatim_prefix;
use pi_agent::{AgentToolResult, BoxFuture, ToolReplayPolicy};
use pi_ai::{ImageContent, Model, ModelInputModality, TextContent, UserContentBlock};
use pi_runtime::image_process::{ProcessImageOptions, process_image_sync, resize_image_sync};
use pi_runtime::tools::read::{ReadToolInput, resolve_to_cwd};
use pi_runtime::tools::truncate::{DEFAULT_MAX_LINES, format_size};
use pi_session::{ExtensionToolContext, ToolDefinition, ToolExposure};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::diagnostics::result::diagnostic_result;
use crate::tools::MAX_OUTPUT_BYTES;
use crate::tools::path_guard::PathGuard;

const JSON_META_RESERVE_BYTES: usize = 1024;
const MAX_IMAGE_FILE_BYTES: usize = 20 * 1024 * 1024;
const IMAGE_CHUNK_BYTES: usize = 64 * 1024;

fn json_escaped_len(s: &str) -> usize {
    let mut len = 0usize;
    for b in s.bytes() {
        len += match b {
            b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        };
    }
    len
}

fn detect_image_mime(head: &[u8], path: &Path) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.starts_with(b"BM")
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("bmp"))
    {
        Some("image/bmp")
    } else {
        None
    }
}

fn non_vision_note(model: Option<&Model>) -> Option<&'static str> {
    let m = model?;
    if m.input.contains(&ModelInputModality::Image) {
        None
    } else {
        Some("当前模型不支持视觉输入，图片将在发送给模型时省略。")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TruncReason {
    Lines,
    Bytes,
    Limit,
}

impl TruncReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lines => "lines",
            Self::Bytes => "bytes",
            Self::Limit => "limit",
        }
    }
}

struct TextScanResult {
    total_lines: usize,
    output_lines: Vec<String>,
    first_line_bytes: usize,
    first_line_exceeds_limit: bool,
    truncated_by: Option<TruncReason>,
    encoding_degraded: bool,
}

fn scan_text_bounded(
    mut reader: impl BufRead,
    start_line_0: usize,
    user_limit: Option<usize>,
    max_lines: usize,
    max_json_bytes: usize,
    cancel: &CancellationToken,
) -> Result<TextScanResult, String> {
    let mut current_line_idx = 0usize;
    let mut current_line_buf: Vec<u8> = Vec::new();
    let mut current_line_total_bytes = 0usize;
    let mut output_lines: Vec<String> = Vec::new();
    let mut output_json_bytes = 0usize;
    let mut first_line_bytes = 0usize;
    let mut first_line_exceeds_limit = false;
    let mut truncated_by: Option<TruncReason> = None;
    let mut encoding_degraded = false;
    let mut stop_collecting = false;
    let mut saw_any_byte = false;

    let end_line_exclusive = user_limit.map(|l| start_line_0.saturating_add(l));
    let raw_cap = max_json_bytes.saturating_add(1);

    let mut finish_line = |line_idx: usize,
                           line_buf: &mut Vec<u8>,
                           line_total_bytes: usize,
                           stop_collecting: &mut bool| {
        let in_window =
            line_idx >= start_line_0 && end_line_exclusive.is_none_or(|end| line_idx < end);
        if in_window {
            if line_idx == start_line_0 {
                first_line_bytes = line_total_bytes;
                if line_total_bytes > max_json_bytes {
                    first_line_exceeds_limit = true;
                    truncated_by = Some(TruncReason::Bytes);
                    *stop_collecting = true;
                }
            }
            if !*stop_collecting {
                if output_lines.len() >= max_lines {
                    truncated_by = Some(TruncReason::Lines);
                    *stop_collecting = true;
                } else {
                    let s = match std::str::from_utf8(line_buf) {
                        Ok(valid) => valid.to_owned(),
                        Err(_) => {
                            encoding_degraded = true;
                            String::from_utf8_lossy(line_buf).into_owned()
                        }
                    };
                    let sep_json = if output_lines.is_empty() { 0 } else { 2 }; // `\n` -> `\\n`
                    let line_json = json_escaped_len(&s);
                    if output_json_bytes.saturating_add(sep_json + line_json) > max_json_bytes {
                        if line_idx == start_line_0 {
                            first_line_exceeds_limit = true;
                            first_line_bytes = s.len();
                        }
                        truncated_by = Some(TruncReason::Bytes);
                        *stop_collecting = true;
                    } else {
                        output_lines.push(s);
                        output_json_bytes += sep_json + line_json;
                    }
                }
            }
        }
        line_buf.clear();
    };

    loop {
        if cancel.is_cancelled() {
            return Err("文件读取已取消。".into());
        }
        let buf = reader.fill_buf().map_err(|_| "文件读取失败。")?;
        if buf.is_empty() {
            break;
        }
        if memchr::memchr(0, buf).is_some() {
            return Err("目标文件包含二进制空字节，不支持按文本读取。".into());
        }
        saw_any_byte = true;
        let len = buf.len();
        let mut start = 0usize;
        while start < len {
            if cancel.is_cancelled() {
                return Err("文件读取已取消。".into());
            }
            let slice = &buf[start..len];
            if let Some(nl_rel) = slice.iter().position(|&b| b == b'\n') {
                let segment = &slice[..nl_rel];
                current_line_total_bytes = current_line_total_bytes.saturating_add(segment.len());
                if !stop_collecting
                    && current_line_idx >= start_line_0
                    && end_line_exclusive.is_none_or(|end| current_line_idx < end)
                    && current_line_buf.len() < raw_cap
                {
                    let remain = raw_cap.saturating_sub(current_line_buf.len());
                    let take = segment.len().min(remain);
                    current_line_buf.extend_from_slice(&segment[..take]);
                }
                finish_line(
                    current_line_idx,
                    &mut current_line_buf,
                    current_line_total_bytes,
                    &mut stop_collecting,
                );
                current_line_idx += 1;
                current_line_total_bytes = 0;
                start += nl_rel + 1;
            } else {
                let segment = slice;
                current_line_total_bytes = current_line_total_bytes.saturating_add(segment.len());
                if !stop_collecting
                    && current_line_idx >= start_line_0
                    && end_line_exclusive.is_none_or(|end| current_line_idx < end)
                    && current_line_buf.len() < raw_cap
                {
                    let remain = raw_cap.saturating_sub(current_line_buf.len());
                    let take = segment.len().min(remain);
                    current_line_buf.extend_from_slice(&segment[..take]);
                }
                start = len;
            }
        }
        reader.consume(len);
    }

    if saw_any_byte && (current_line_total_bytes > 0 || current_line_idx > 0) {
        finish_line(
            current_line_idx,
            &mut current_line_buf,
            current_line_total_bytes,
            &mut stop_collecting,
        );
        current_line_idx += 1;
    }

    if truncated_by.is_none()
        && let Some(ul) = user_limit
        && start_line_0.saturating_add(ul) < current_line_idx
    {
        truncated_by = Some(TruncReason::Limit);
    }

    Ok(TextScanResult {
        total_lines: current_line_idx,
        output_lines,
        first_line_bytes,
        first_line_exceeds_limit,
        truncated_by,
        encoding_degraded,
    })
}

fn read_image_bounded(
    file: &mut std::fs::File,
    file_size: u64,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    if file_size > MAX_IMAGE_FILE_BYTES as u64 {
        return Err(format!(
            "图片文件大小 {} 超过 20 MiB 上限，已拒绝读取。",
            format_size(file_size as usize)
        ));
    }
    let mut raw_bytes = Vec::with_capacity((file_size as usize).min(MAX_IMAGE_FILE_BYTES));
    let mut chunk = [0u8; IMAGE_CHUNK_BYTES];
    loop {
        if cancel.is_cancelled() {
            return Err("文件读取已取消。".into());
        }
        let n = file
            .read(&mut chunk)
            .map_err(|_| "图片文件读取失败。".to_owned())?;
        if n == 0 {
            break;
        }
        if raw_bytes.len().saturating_add(n) > MAX_IMAGE_FILE_BYTES {
            return Err("图片文件超过 20 MiB 上限，已中止读取。".into());
        }
        raw_bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(raw_bytes)
}

fn normalized_display_path(abs_path: &Path) -> String {
    let resolved = std::fs::canonicalize(abs_path).unwrap_or_else(|_| abs_path.to_path_buf());
    strip_windows_verbatim_prefix(&resolved)
        .to_string_lossy()
        .into_owned()
}

pub fn definition(default_root: &Path, guard: PathGuard) -> ToolDefinition {
    let default_cwd = default_root.to_string_lossy().into_owned();
    ToolDefinition::new_typed_with_context::<ReadToolInput, _, _>(
        "read",
        "read",
        format!(
            "读取本地文本文件或图片（png/jpg/gif/webp/bmp），统一返回 JSON 元信息；图片以多模态附件返回。文本单次最多 {DEFAULT_MAX_LINES} 行且整体受 50 KiB JSON 预算限制，可用 offset/limit 分段续读。禁止读取凭据与运行态目录。"
        ),
        move |_tool_call_id,
              input: ReadToolInput,
              _on_update,
              cancel: CancellationToken,
              ctx: ExtensionToolContext| {
            let guard = guard.clone();
            let base_cwd = default_cwd.clone();
            Box::pin(async move {
                if input.offset == Some(0) || input.limit == Some(0) {
                    return Err("offset 和 limit 必须为正整数。".into());
                }
                if cancel.is_cancelled() {
                    return Err("文件读取已取消。".into());
                }
                let effective_cwd = if ctx.cwd().is_empty() {
                    base_cwd
                } else {
                    ctx.cwd()
                };
                let current_model = ctx.model();
                let abs_path: PathBuf = resolve_to_cwd(&input.path, &effective_cwd);

                tokio::task::spawn_blocking(move || -> Result<AgentToolResult, String> {
                    if cancel.is_cancelled() {
                        return Err("文件读取已取消。".into());
                    }
                    let (mut file, file_size) = guard.session().open_file(&abs_path)?;
                    let display = normalized_display_path(&abs_path);
                    let mut head = [0u8; 16];
                    let head_len = file
                        .read(&mut head)
                        .map_err(|_| "文件类型无法读取。".to_owned())?;
                    file.seek(SeekFrom::Start(0))
                        .map_err(|_| "文件无法重置读取位置。".to_owned())?;

                    if let Some(mime_type) = detect_image_mime(&head[..head_len], &abs_path) {
                        let raw_bytes = read_image_bounded(&mut file, file_size, &cancel)?;
                        if cancel.is_cancelled() {
                            return Err("文件读取已取消。".into());
                        }
                        let vision_note = non_vision_note(current_model.as_ref());
                        let (ok, data, resolved_mime, resized, hints, fail_reason) =
                            if mime_type == "image/bmp" {
                                let processed = process_image_sync(
                                    &raw_bytes,
                                    mime_type,
                                    &ProcessImageOptions {
                                        auto_resize_images: Some(true),
                                        resize_options: None,
                                    },
                                );
                                (
                                    processed.ok,
                                    processed.data,
                                    processed.mime_type,
                                    processed.dimension_note.is_some(),
                                    processed.hints,
                                    processed.message,
                                )
                            } else {
                                let encoded = BASE64_STANDARD.encode(&raw_bytes);
                                let img = resize_image_sync(
                                    &ImageContent {
                                        data: encoded,
                                        mime_type: mime_type.to_owned(),
                                    },
                                    None,
                                );
                                if img.width == 0 || img.height == 0 {
                                    (
                                        false,
                                        String::new(),
                                        mime_type.to_owned(),
                                        false,
                                        Vec::new(),
                                        Some("图片文件已损坏或无法解码。".to_owned()),
                                    )
                                } else {
                                    let mut hints = Vec::new();
                                    if img.was_resized {
                                        hints.push(format!(
                                            "[Image: original {}x{}, scaled to {}x{}.]",
                                            img.original_width,
                                            img.original_height,
                                            img.width,
                                            img.height
                                        ));
                                    }
                                    (
                                        true,
                                        img.data,
                                        img.mime_type,
                                        img.was_resized,
                                        hints,
                                        None,
                                    )
                                }
                            };
                        if !ok {
                            let reason =
                                fail_reason.unwrap_or_else(|| "图片解码或缩放失败。".into());
                            let mut notice = format!("图片处理失败，未附带图片：{reason}");
                            if let Some(vn) = vision_note {
                                notice.push(' ');
                                notice.push_str(vn);
                            }
                            let meta = json!({
                                "path": display,
                                "kind": "image",
                                "mimeType": mime_type,
                                "bytes": raw_bytes.len(),
                                "resized": false,
                                "hints": [],
                                "imageAttached": false,
                                "degraded": true,
                                "collectionErrors": [{"source": "image", "message": reason}],
                                "notice": notice,
                            });
                            return diagnostic_result(meta);
                        }
                        let mut notice = "图片已附带于本条工具结果中。".to_owned();
                        if let Some(vn) = vision_note {
                            notice.push(' ');
                            notice.push_str(vn);
                        }
                        let meta = json!({
                            "path": display,
                            "kind": "image",
                            "mimeType": resolved_mime,
                            "bytes": raw_bytes.len(),
                            "resized": resized,
                            "hints": hints,
                            "imageAttached": true,
                            "notice": notice,
                        });
                        let meta_text = serde_json::to_string(&meta)
                            .map_err(|_| "图片元信息编码失败。".to_owned())?;
                        return Ok(AgentToolResult {
                            content: vec![
                                UserContentBlock::Text(TextContent::new(meta_text)),
                                UserContentBlock::Image(ImageContent {
                                    data,
                                    mime_type: resolved_mime,
                                }),
                            ],
                            details: meta,
                            structured_content: None,
                            usage: None,
                            is_error: false,
                            terminate: None,
                        });
                    }

                    if head_len >= 2
                        && (head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]))
                    {
                        return Err("暂不支持直接读取 UTF-16 编码文件。".into());
                    }
                    if memchr::memchr(0, &head[..head_len]).is_some() {
                        return Err("目标文件包含二进制空字节，不支持按文本读取。".into());
                    }

                    let start_line_0 = input.offset.map_or(0, |o| o.saturating_sub(1));
                    let start_line_display = start_line_0 + 1;
                    if (file_size == 0 && start_line_0 > 0) || (start_line_0 as u64) > file_size {
                        return Err(format!(
                            "起始行 offset={} 超出文件总行数范围。",
                            input.offset.unwrap_or(1)
                        ));
                    }
                    let effective_limit = input.limit.unwrap_or(DEFAULT_MAX_LINES);
                    let max_json_content_bytes = MAX_OUTPUT_BYTES
                        .saturating_sub(JSON_META_RESERVE_BYTES + json_escaped_len(&display));

                    let scanned = scan_text_bounded(
                        BufReader::with_capacity(16 * 1024, file),
                        start_line_0,
                        input.limit,
                        DEFAULT_MAX_LINES,
                        max_json_content_bytes,
                        &cancel,
                    )?;

                    if scanned.total_lines == 0 {
                        if start_line_0 > 0 {
                            return Err(format!(
                                "起始行 offset={} 超出文件总行数（共 0 行）。",
                                input.offset.unwrap_or(1)
                            ));
                        }
                    } else if start_line_0 >= scanned.total_lines {
                        return Err(format!(
                            "起始行 offset={} 超出文件总行数（共 {} 行）。",
                            input.offset.unwrap_or(1),
                            scanned.total_lines
                        ));
                    }

                    let content = if scanned.first_line_exceeds_limit {
                        String::new()
                    } else {
                        scanned.output_lines.join("\n")
                    };
                    let output_lines = if scanned.first_line_exceeds_limit {
                        0
                    } else {
                        scanned.output_lines.len()
                    };
                    let output_bytes = content.len();
                    let truncated = scanned.truncated_by.is_some();

                    let mut obj = Map::new();
                    obj.insert("path".into(), json!(display));
                    obj.insert("kind".into(), json!("text"));
                    obj.insert("offset".into(), json!(start_line_display));
                    obj.insert("limit".into(), json!(effective_limit));
                    obj.insert("outputLines".into(), json!(output_lines));
                    obj.insert("totalLines".into(), json!(scanned.total_lines));
                    obj.insert("outputBytes".into(), json!(output_bytes));
                    obj.insert("truncated".into(), json!(truncated));

                    if let Some(reason) = scanned.truncated_by {
                        obj.insert("truncatedBy".into(), json!(reason.as_str()));
                    }
                    if scanned.encoding_degraded {
                        obj.insert("encodingDegraded".into(), json!(true));
                    }
                    let mut notice_parts = Vec::new();
                    if scanned.first_line_exceeds_limit {
                        obj.insert("firstLineExceedsLimit".into(), json!(true));
                        obj.insert("firstLineBytes".into(), json!(scanned.first_line_bytes));
                        let next_offset = start_line_display + 1;
                        if next_offset <= scanned.total_lines {
                            obj.insert("nextOffset".into(), json!(next_offset));
                        }
                        notice_parts.push(format!(
                            "第 {start_line_display} 行长度 {} 超过单次读取预算，未返回该行正文；可使用 ffgrep 按关键字检索该文件，或传 offset={next_offset} 跳过该行继续读取。",
                            format_size(scanned.first_line_bytes)
                        ));
                    } else if truncated {
                        let next_offset = start_line_display + output_lines;
                        obj.insert("nextOffset".into(), json!(next_offset));
                        let reason_label = match scanned.truncated_by {
                            Some(TruncReason::Lines) => "行数上限",
                            Some(TruncReason::Bytes) => "字节预算",
                            Some(TruncReason::Limit) => "limit 上限",
                            None => "上限",
                        };
                        notice_parts.push(format!(
                            "已按{reason_label}截断（第 {start_line_display}-{} 行，共 {} 行），可传 offset={next_offset} 继续读取。",
                            start_line_display + output_lines.saturating_sub(1),
                            scanned.total_lines
                        ));
                    }
                    if scanned.encoding_degraded {
                        notice_parts.push("文件包含非 UTF-8 字节，已按替换字符降级解码。".into());
                    }
                    if !notice_parts.is_empty() {
                        obj.insert("notice".into(), json!(notice_parts.join(" ")));
                    }
                    obj.insert("content".into(), json!(content));

                    diagnostic_result(Value::Object(obj))
                })
                .await
                .map_err(|e| format!("文件读取任务异常中断: {e}"))?
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prompt_snippet("读取文本或图片，统一返回 JSON 元信息；禁止读取凭据文件和运行态目录。")
    .with_prompt_guidelines(vec![
        "读取大文本文件时按返回中的 nextOffset 分段续读。".to_owned(),
    ])
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Safe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_session::{ExtensionRunner, NoOpUiContext};
    use std::fs;
    use std::sync::Arc;

    fn make_runner(root: &Path) -> ExtensionRunner {
        let runner = ExtensionRunner::empty(root.to_string_lossy().into_owned());
        runner.set_ui_context(
            Some(Arc::new(NoOpUiContext::new())),
            pi_session::ExtensionMode::Print,
        );
        runner
    }

    #[tokio::test]
    async fn read_text_returns_json_and_respects_50kib_escaped_budget() {
        let root = std::env::temp_dir().join(format!("cst-read-json-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("app")).unwrap();
        let file = root.join("sample.log");
        let line = "quote=\"hello\"\\path\\next";
        let big = (0..3000)
            .map(|i| format!("{i:04}: {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&file, big).unwrap();

        let guard = PathGuard::new(&root).unwrap();
        let def = definition(&root, guard);
        let runner = make_runner(&root);
        let ctx = runner.create_tool_context("tc-1", Some(CancellationToken::new()));
        let res = (def.execute)(
            "tc-1".into(),
            json!({"path": "sample.log"}),
            None,
            None,
            ctx,
        )
        .await
        .unwrap();
        assert!(!res.is_error);
        let UserContentBlock::Text(text) = &res.content[0] else {
            panic!("expected text block");
        };
        assert!(text.text.len() <= MAX_OUTPUT_BYTES);
        let parsed: Value = serde_json::from_str(&text.text).unwrap();
        assert_eq!(parsed["kind"], "text");
        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["truncatedBy"], "bytes");
        assert!(parsed["nextOffset"].as_u64().unwrap() > 1);
        assert!(parsed["outputTruncated"].is_null());

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn read_image_returns_dual_blocks_without_base64_in_details() {
        // Minimal valid 1x1 red PNG
        const TINY_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D,
            0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let root = std::env::temp_dir().join(format!("cst-read-img-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("app")).unwrap();
        let img_path = root.join("dot.png");
        fs::write(&img_path, TINY_PNG).unwrap();

        let guard = PathGuard::new(&root).unwrap();
        let def = definition(&root, guard);
        let runner = make_runner(&root);
        let ctx = runner.create_tool_context("tc-2", Some(CancellationToken::new()));
        let res = (def.execute)("tc-2".into(), json!({"path": "dot.png"}), None, None, ctx)
            .await
            .unwrap();
        assert!(!res.is_error);
        assert_eq!(res.content.len(), 2);
        let UserContentBlock::Text(meta) = &res.content[0] else {
            panic!("expected text");
        };
        assert!(matches!(res.content[1], UserContentBlock::Image(_)));
        let parsed: Value = serde_json::from_str(&meta.text).unwrap();
        assert_eq!(parsed["kind"], "image");
        assert_eq!(parsed["mimeType"], "image/png");
        assert_eq!(parsed["imageAttached"], true);
        assert!(res.details.get("data").is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn read_first_line_exceeds_limit_has_no_bash_sed_hint() {
        let root = std::env::temp_dir().join(format!("cst-read-long-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("one.txt"), "A".repeat(60_000)).unwrap();

        let guard = PathGuard::new(&root).unwrap();
        let def = definition(&root, guard);
        let runner = make_runner(&root);
        let ctx = runner.create_tool_context("tc-3", Some(CancellationToken::new()));
        let res = (def.execute)("tc-3".into(), json!({"path": "one.txt"}), None, None, ctx)
            .await
            .unwrap();
        assert_eq!(res.details["firstLineExceedsLimit"], true);
        assert_eq!(res.details["content"], "");
        let notice = res.details["notice"].as_str().unwrap();
        assert!(!notice.contains("sed"));
        assert!(!notice.contains("bash"));

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn read_handles_empty_binary_non_utf8_and_corrupt_image_cleanly() {
        let root = std::env::temp_dir().join(format!("cst-read-edge-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("empty.txt"), b"").unwrap();
        fs::write(root.join("bin.dat"), b"abc\0def").unwrap();
        fs::write(root.join("gbk.log"), b"ok\n\xB2\xE2\xCA\xD4\n").unwrap();
        // PNG signature followed by garbage bytes
        fs::write(root.join("broken.png"), b"\x89PNG\r\n\x1a\nnot-a-valid-png").unwrap();

        let guard = PathGuard::new(&root).unwrap();
        let def = definition(&root, guard);
        let runner = make_runner(&root);

        // 0-byte empty file: offset=1 succeeds with totalLines=0; offset=2 errors.
        let ctx = runner.create_tool_context("tc-e1", Some(CancellationToken::new()));
        let empty = (def.execute)(
            "tc-e1".into(),
            json!({"path": "empty.txt"}),
            None,
            None,
            ctx,
        )
        .await
        .unwrap();
        assert_eq!(empty.details["totalLines"], 0);
        assert_eq!(empty.details["outputLines"], 0);
        assert_eq!(empty.details["content"], "");

        let ctx = runner.create_tool_context("tc-e2", Some(CancellationToken::new()));
        assert!(
            (def.execute)(
                "tc-e2".into(),
                json!({"path": "empty.txt", "offset": 2}),
                None,
                None,
                ctx,
            )
            .await
            .is_err()
        );

        // Binary NUL file is rejected.
        let ctx = runner.create_tool_context("tc-e3", Some(CancellationToken::new()));
        assert!(
            (def.execute)("tc-e3".into(), json!({"path": "bin.dat"}), None, None, ctx)
                .await
                .is_err()
        );

        // Non-UTF-8 text sets encodingDegraded=true.
        let ctx = runner.create_tool_context("tc-e4", Some(CancellationToken::new()));
        let non_utf8 = (def.execute)("tc-e4".into(), json!({"path": "gbk.log"}), None, None, ctx)
            .await
            .unwrap();
        assert_eq!(non_utf8.details["encodingDegraded"], true);

        // Corrupt PNG does not attach garbage image; returns degraded single text block.
        let ctx = runner.create_tool_context("tc-e5", Some(CancellationToken::new()));
        let corrupt = (def.execute)(
            "tc-e5".into(),
            json!({"path": "broken.png"}),
            None,
            None,
            ctx,
        )
        .await
        .unwrap();
        assert_eq!(corrupt.content.len(), 1);
        assert_eq!(corrupt.details["imageAttached"], false);
        assert_eq!(corrupt.details["degraded"], true);

        let _ = fs::remove_dir_all(&root);
    }
}

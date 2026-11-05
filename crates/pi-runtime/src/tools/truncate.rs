use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024; // 50KB
pub const GREP_MAX_LINE_LENGTH: usize = 500;
pub const STRUCTURED_OUTPUT_MAX_BYTES: usize = 1024 * 1024; // 1MB

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TruncationOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// 头部截断（保留前 N 行 / 字节，绝不返回残缺行；若首行单行超限则返回空内容且 `first_line_exceeds_limit = true`）。
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    if let Some(first) = lines.first()
        && first.len() > max_bytes
    {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines_vec = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, &line) in lines.iter().enumerate() {
        if i >= max_lines {
            truncated_by = TruncatedBy::Lines;
            break;
        }
        let line_bytes = line.len() + usize::from(i > 0);
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_vec.push(line);
        output_bytes_count += line_bytes;
    }

    let output_content = output_lines_vec.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_vec.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

fn truncate_string_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    let bytes = s.as_bytes();
    if bytes.len() <= max_bytes {
        return s.to_owned();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// 尾部截断（保留末尾 N 行 / 字节；若最后一行单行超出 `max_bytes`，按 UTF-8 字符边界截取该行末尾并置 `last_line_partial = true`）。
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines_vec: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for &line in lines.iter().rev() {
        if output_lines_vec.len() >= max_lines {
            truncated_by = TruncatedBy::Lines;
            break;
        }
        let line_bytes = line.len() + usize::from(!output_lines_vec.is_empty());
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            if output_lines_vec.is_empty() {
                let partial = truncate_string_to_bytes_from_end(line, max_bytes);
                output_lines_vec.push(partial);
                last_line_partial = true;
            }
            break;
        }
        output_lines_vec.push(line.to_owned());
        output_bytes_count += line_bytes;
    }

    output_lines_vec.reverse();
    let output_content = output_lines_vec.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_vec.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// 单行字符长度截断（用于 `grep` 匹配行，默认上限 500 字符）。
pub fn truncate_line(line: &str, max_chars: Option<usize>) -> (String, bool) {
    let limit = max_chars.unwrap_or(GREP_MAX_LINE_LENGTH);
    let mut char_indices = line.char_indices();
    if let Some((byte_idx, _)) = char_indices.nth(limit) {
        (format!("{}... [truncated]", &line[..byte_idx]), true)
    } else {
        (line.to_owned(), false)
    }
}

#[derive(Debug, Clone, Default)]
pub struct OutputAccumulatorOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    pub temp_file_prefix: Option<String>,
    pub temp_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct OutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<String>,
    pub temp_file_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FullOutput {
    pub content: String,
    pub truncated: bool,
}

fn bounded_full_output(mut content: String, max_bytes: usize, mut truncated: bool) -> FullOutput {
    if content.len() > max_bytes {
        let mut end = max_bytes;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
        truncated = true;
    }
    FullOutput { content, truncated }
}

/// 从首字节开始的有界流式输出累积器（对照 `OutputAccumulator`）：
/// 1. 使用增量 UTF-8 解码器处理跨分片多字节字符。
/// 2. 内存中仅维护 `max_rolling_bytes`（`2 * max_bytes`）的滚动尾部。
/// 3. 一旦超过 `max_bytes` 或 `max_lines`，增量流式写入临时文件；若临时文件创建或写入失败，
///    自动清除 `full_output_path`（绝不谎称完整输出已保存）并继续维护有界内存尾部。
pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_file_prefix: String,
    temp_dir: Option<PathBuf>,
    incomplete_utf8: Vec<u8>,
    raw_chunks: Vec<Vec<u8>>,
    tail_text: String,
    tail_bytes: usize,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    finished: bool,
    temp_file_path: Option<PathBuf>,
    temp_file: Option<File>,
    temp_file_error: Option<String>,
}

impl OutputAccumulator {
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        let max_rolling_bytes = (max_bytes.saturating_mul(2)).max(1);
        Self {
            max_lines,
            max_bytes,
            max_rolling_bytes,
            temp_file_prefix: options
                .temp_file_prefix
                .unwrap_or_else(|| "pi-output".to_owned()),
            temp_dir: options.temp_dir,
            incomplete_utf8: Vec::with_capacity(4),
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_bytes: 0,
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,
            temp_file_path: None,
            temp_file: None,
            temp_file_error: None,
        }
    }

    pub fn append(&mut self, data: &[u8]) {
        if self.finished || data.is_empty() {
            return;
        }
        // Bound temporary decoding/tail allocations even for a custom producer's giant chunk.
        for data in data.chunks(8192) {
            self.total_raw_bytes = self.total_raw_bytes.saturating_add(data.len());
            let decoded = self.decode_streaming_utf8(data, false);
            self.append_decoded_text(&decoded);
            if self.temp_file.is_some() || self.should_use_temp_file() {
                self.ensure_temp_file();
                self.write_to_temp_file(data);
            } else if self.temp_file_error.is_none() {
                self.raw_chunks.push(data.to_vec());
            }
        }
    }

    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let remaining = self.decode_streaming_utf8(&[], true);
        if !remaining.is_empty() {
            self.append_decoded_text(&remaining);
        }
        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
        self.close_temp_file();
    }

    pub fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let snap_text = self.get_snapshot_text().to_owned();
        let mut tail_trunc = truncate_tail(
            &snap_text,
            TruncationOptions {
                max_lines: Some(self.max_lines),
                max_bytes: Some(self.max_bytes),
            },
        );
        let truncated =
            self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            tail_trunc
                .truncated_by
                .or(Some(if self.total_decoded_bytes > self.max_bytes {
                    TruncatedBy::Bytes
                } else {
                    TruncatedBy::Lines
                }))
        } else {
            None
        };
        tail_trunc.truncated = truncated;
        tail_trunc.truncated_by = truncated_by;
        tail_trunc.total_lines = self.total_lines;
        tail_trunc.total_bytes = self.total_decoded_bytes;
        tail_trunc.max_lines = self.max_lines;
        tail_trunc.max_bytes = self.max_bytes;

        if persist_if_truncated && tail_trunc.truncated {
            self.ensure_temp_file();
            // Live snapshots must not close the file: later chunks still belong to it.
            if let Some(file) = self.temp_file.as_mut()
                && let Err(err) = file.flush()
            {
                self.fail_temp_file(format!("Failed to flush temp output file: {err}"));
            }
        }

        OutputSnapshot {
            content: tail_trunc.content.clone(),
            truncation: tail_trunc,
            full_output_path: self
                .temp_file_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            temp_file_error: self.temp_file_error.clone(),
        }
    }

    pub fn read_full_output(&self, max_bytes: usize) -> FullOutput {
        let fallback = || {
            bounded_full_output(
                self.get_snapshot_text().to_owned(),
                max_bytes,
                self.temp_file_path.is_some() || self.total_decoded_bytes > self.tail_bytes,
            )
        };
        let Some(ref path) = self.temp_file_path else {
            if !self.raw_chunks.is_empty() {
                let bytes = self.raw_chunks.concat();
                return bounded_full_output(
                    String::from_utf8_lossy(&bytes).into_owned(),
                    max_bytes,
                    false,
                );
            }
            return fallback();
        };
        let Ok(mut file) = File::open(path) else {
            return fallback();
        };
        let Ok(metadata) = file.metadata() else {
            return fallback();
        };
        let size = metadata.len();
        if size <= max_bytes as u64 {
            let mut buf = Vec::new();
            if Read::by_ref(&mut file)
                .take(max_bytes.saturating_add(1) as u64)
                .read_to_end(&mut buf)
                .is_ok()
            {
                return bounded_full_output(
                    String::from_utf8_lossy(&buf).into_owned(),
                    max_bytes,
                    false,
                );
            }
            return fallback();
        }
        let marker = "\n[... output omitted ...]\n";
        let budget = max_bytes.saturating_sub(marker.len());
        let head_len = budget / 2;
        let tail_len = budget - head_len;
        let mut head = vec![0u8; head_len];
        let mut tail = vec![0u8; tail_len];
        if file.read_exact(&mut head).is_err()
            || file
                .seek(SeekFrom::Start(size.saturating_sub(tail_len as u64)))
                .is_err()
            || file.read_exact(&mut tail).is_err()
        {
            return fallback();
        }
        let mut tail_start = 0;
        while tail_start < tail.len() && (tail[tail_start] & 0xc0) == 0x80 {
            tail_start += 1;
        }
        let head = String::from_utf8_lossy(&head);
        let tail = String::from_utf8_lossy(&tail[tail_start..]);
        bounded_full_output(format!("{head}{marker}{tail}"), max_bytes, true)
    }

    pub fn last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    fn decode_streaming_utf8(&mut self, chunk: &[u8], flush: bool) -> String {
        let mut buf = std::mem::take(&mut self.incomplete_utf8);
        buf.extend_from_slice(chunk);

        let mut out = String::new();
        let mut slice: &[u8] = &buf;
        loop {
            match std::str::from_utf8(slice) {
                Ok(valid) => {
                    out.push_str(valid);
                    slice = &[];
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        out.push_str(
                            std::str::from_utf8(&slice[..valid_up_to]).unwrap_or_default(),
                        );
                    }
                    slice = &slice[valid_up_to..];
                    match err.error_len() {
                        Some(invalid_len) => {
                            out.push('\u{FFFD}');
                            slice = &slice[invalid_len..];
                        }
                        None => {
                            if flush {
                                out.push('\u{FFFD}');
                                slice = &[];
                            }
                            break;
                        }
                    }
                }
            }
        }

        if !slice.is_empty() {
            self.incomplete_utf8.extend_from_slice(slice);
        }
        out
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        self.total_decoded_bytes = self.total_decoded_bytes.saturating_add(bytes);
        self.tail_text.push_str(text);
        self.tail_bytes = self.tail_bytes.saturating_add(bytes);
        if self.tail_bytes > self.max_rolling_bytes.saturating_mul(2) {
            self.trim_tail();
        }

        let mut newlines = 0usize;
        let mut last_newline = None;
        for (idx, b) in text.bytes().enumerate() {
            if b == b'\n' {
                newlines += 1;
                last_newline = Some(idx);
            }
        }
        if let Some(last_nl) = last_newline {
            self.completed_lines = self.completed_lines.saturating_add(newlines);
            let tail = &text[last_nl + 1..];
            self.current_line_bytes = tail.len();
            self.has_open_line = !tail.is_empty();
        } else {
            self.current_line_bytes = self.current_line_bytes.saturating_add(bytes);
            self.has_open_line = true;
        }
        self.total_lines = self.completed_lines + usize::from(self.has_open_line);
    }

    fn trim_tail(&mut self) {
        let bytes = self.tail_text.as_bytes();
        if bytes.len() <= self.max_rolling_bytes {
            self.tail_bytes = bytes.len();
            return;
        }
        let mut start = bytes.len() - self.max_rolling_bytes;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
        }
        self.tail_starts_at_line_boundary = if start == 0 {
            self.tail_starts_at_line_boundary
        } else {
            bytes[start - 1] == b'\n'
        };
        self.tail_text = self.tail_text[start..].to_owned();
        self.tail_bytes = self.tail_text.len();
    }

    fn get_snapshot_text(&self) -> &str {
        if self.tail_starts_at_line_boundary {
            return &self.tail_text;
        }
        match self.tail_text.find('\n') {
            Some(idx) => &self.tail_text[idx + 1..],
            None => &self.tail_text,
        }
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.temp_file_path.is_some() || self.temp_file_error.is_some() {
            return;
        }
        let dir = self.temp_dir.clone().unwrap_or_else(std::env::temp_dir);
        if !dir.exists()
            && let Err(err) = fs::create_dir_all(&dir)
        {
            self.fail_temp_file(format!("Failed to create temp output dir: {err}"));
            return;
        }
        let path = dir.join(format!("{}-{}.log", self.temp_file_prefix, Uuid::new_v4()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let chunks = std::mem::take(&mut self.raw_chunks);
                for chunk in chunks {
                    if let Err(err) = file.write_all(&chunk) {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        self.fail_temp_file(format!("Failed to write temp output file: {err}"));
                        return;
                    }
                }
                self.temp_file_path = Some(path);
                self.temp_file = Some(file);
            }
            Err(err) => {
                self.fail_temp_file(format!("Failed to open temp output file: {err}"));
            }
        }
    }

    fn write_to_temp_file(&mut self, data: &[u8]) {
        let Some(mut file) = self.temp_file.take() else {
            return;
        };
        if let Err(err) = file.write_all(data) {
            drop(file);
            if let Some(p) = self.temp_file_path.take() {
                let _ = fs::remove_file(p);
            }
            self.fail_temp_file(format!("Failed to write temp output file: {err}"));
        } else {
            self.temp_file = Some(file);
        }
    }

    fn close_temp_file(&mut self) {
        if let Some(mut file) = self.temp_file.take()
            && let Err(err) = file.flush()
        {
            drop(file);
            if let Some(p) = self.temp_file_path.take() {
                let _ = fs::remove_file(p);
            }
            self.fail_temp_file(format!("Failed to flush temp output file: {err}"));
        }
    }

    fn fail_temp_file(&mut self, err: String) {
        self.raw_chunks.clear();
        self.temp_file = None;
        if let Some(p) = self.temp_file_path.take() {
            let _ = fs::remove_file(p);
        }
        self.temp_file_error = Some(err);
    }

    pub fn temp_file_path(&self) -> Option<&Path> {
        self.temp_file_path.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiddleTruncationResult {
    pub content: String,
    pub truncated: bool,
    pub removed_chars: usize,
    pub total_bytes: usize,
    pub total_lines: usize,
}

pub fn truncate_middle(content: &str, max_bytes: usize) -> MiddleTruncationResult {
    let bytes = content.as_bytes();
    let total_lines = split_lines_for_counting(content).len();
    if bytes.len() <= max_bytes {
        return MiddleTruncationResult {
            content: content.to_owned(),
            truncated: false,
            removed_chars: 0,
            total_bytes: bytes.len(),
            total_lines,
        };
    }
    let mut head_end = max_bytes / 2;
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = bytes.len().saturating_sub(max_bytes - (max_bytes / 2));
    while tail_start < bytes.len() && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start < head_end {
        tail_start = head_end;
    }
    let head = &content[..head_end];
    let tail = &content[tail_start..];
    let removed_chars = content[head_end..tail_start].chars().count();
    MiddleTruncationResult {
        content: format!("{head}…{removed_chars} chars truncated…{tail}"),
        truncated: true,
        removed_chars,
        total_bytes: bytes.len(),
        total_lines,
    }
}

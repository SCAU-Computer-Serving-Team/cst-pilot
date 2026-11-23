use std::collections::BTreeMap;
use std::time::Instant;

use gpui_kit::{Hsla, IntoElement, ParentElement, SharedString, Styled, Window, div};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{DesktopPalette, TypographyScale, apply_scale, cjk_inline_label, scale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialToolKind {
    SysOverview,
    Runbook,
    WebSearch,
    BitLocker,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallPart {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingPart {
    pub thinking: String,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageContentPart {
    pub mime_type: String,
    pub data_base64: Option<String>,
    pub attachment_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConversationPart {
    Thinking(ThinkingPart),
    Text(String),
    Tools(Vec<ToolCallPart>),
    Special {
        kind: SpecialToolKind,
        call: ToolCallPart,
    },
    Image(ImageContentPart),
}

#[derive(Debug, Clone)]
pub struct TurnEntry {
    pub id: Option<String>,
    pub role: String,
    pub timestamp_ms: u64,
    pub duration_ms: Option<u64>,
    pub work_started_ms: Option<u64>,
    pub work_ended_ms: Option<u64>,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub raw: Value,
}

impl TurnEntry {
    pub fn from_json(val: &Value, fallback_idx: usize) -> Self {
        let msg = val.get("message").unwrap_or(val);
        let id = val
            .get("id")
            .or_else(|| msg.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| Some(format!("entry-{fallback_idx}")));
        let role = msg
            .get("role")
            .or_else(|| val.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let timestamp_ms = msg
            .get("timestamp")
            .or_else(|| val.get("timestamp"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let duration_ms = msg
            .get("durationMs")
            .or_else(|| val.get("durationMs"))
            .and_then(Value::as_u64);
        let work = msg.get("work").or_else(|| val.get("work"));
        let work_started_ms = work
            .and_then(|w| w.get("startedAt"))
            .and_then(Value::as_u64);
        let work_ended_ms = work.and_then(|w| w.get("endedAt")).and_then(Value::as_u64);
        let stop_reason = msg
            .get("stopReason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let error_message = msg
            .get("errorMessage")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let provider = msg
            .get("provider")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let model = msg.get("model").and_then(Value::as_str).map(str::to_owned);
        Self {
            id,
            role,
            timestamp_ms,
            duration_ms,
            work_started_ms,
            work_ended_ms,
            stop_reason,
            error_message,
            provider,
            model,
            raw: msg.clone(),
        }
    }

    pub fn stable_key(&self) -> String {
        self.id
            .clone()
            .unwrap_or_else(|| format!("stamp:{}", self.timestamp_ms))
    }
}

/// Classifies the 4 standalone special diagnostic tools (`sys(overview)`, `runbook*`, `web_search`, `bitlocker(unlock/decrypt)`).
pub fn special_tool_kind(call: &ToolCallPart) -> Option<SpecialToolKind> {
    let scope = call
        .arguments
        .get("scope")
        .or_else(|| call.arguments.get("action"))
        .and_then(Value::as_str);
    if call.name == "sys" && scope.unwrap_or("overview") == "overview" {
        return Some(SpecialToolKind::SysOverview);
    }
    if matches!(
        call.name.as_str(),
        "runbook" | "command_plan" | "runbook_deliver"
    ) {
        return Some(SpecialToolKind::Runbook);
    }
    if call.name == "web_search" {
        return Some(SpecialToolKind::WebSearch);
    }
    if call.name == "bitlocker" && matches!(scope, Some("unlock" | "decrypt")) {
        return Some(SpecialToolKind::BitLocker);
    }
    None
}

pub fn is_standalone_tool(call: &ToolCallPart, result: Option<&Value>) -> bool {
    if special_tool_kind(call).is_some() {
        return true;
    }
    if call.name == "read"
        && let Some(res) = result
    {
        let content = res
            .get("message")
            .unwrap_or(res)
            .get("content")
            .and_then(Value::as_array);
        if content.is_some_and(|arr| {
            arr.iter().any(|p| {
                p.get("type").and_then(Value::as_str) == Some("image")
                    && p.get("data")
                        .and_then(Value::as_str)
                        .is_some_and(|data| !data.trim().is_empty())
            })
        }) {
            return true;
        }
    }
    false
}

/// Extracts plain text of a message (`string` or concatenated `text` blocks).
pub fn extract_message_text(msg: &Value) -> String {
    let content = msg.get("content").unwrap_or(&Value::Null);
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    if let Some(arr) = content.as_array() {
        let mut lines = Vec::new();
        for item in arr {
            if item.get("type").and_then(Value::as_str) == Some("text")
                && let Some(t) = item.get("text").and_then(Value::as_str)
            {
                lines.push(t);
            }
        }
        return lines.join("\n");
    }
    if let Some(summary) = msg.get("summary").and_then(Value::as_str) {
        return summary.to_owned();
    }
    String::new()
}

pub fn extract_user_images(msg: &Value) -> Vec<ImageContentPart> {
    let mut out = Vec::new();
    if let Some(arr) = msg.get("content").and_then(Value::as_array) {
        for item in arr {
            if item.get("type").and_then(Value::as_str) == Some("image") {
                out.push(ImageContentPart {
                    mime_type: item
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("image/png")
                        .to_owned(),
                    data_base64: item.get("data").and_then(Value::as_str).map(str::to_owned),
                    attachment_id: item
                        .get("attachmentId")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
            }
        }
    }
    out
}

pub fn ordered_parts(msg: &Value) -> Vec<ConversationPart> {
    let mut parts: Vec<ConversationPart> = Vec::new();
    let content = msg.get("content").unwrap_or(&Value::Null);
    if let Some(s) = content.as_str() {
        if !s.is_empty() {
            parts.push(ConversationPart::Text(s.to_owned()));
        }
        return parts;
    }
    let Some(arr) = content.as_array() else {
        return parts;
    };

    for item in arr {
        let ptype = item.get("type").and_then(Value::as_str).unwrap_or("");
        match ptype {
            "image" => {
                parts.push(ConversationPart::Image(ImageContentPart {
                    mime_type: item
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("image/png")
                        .to_owned(),
                    data_base64: item.get("data").and_then(Value::as_str).map(str::to_owned),
                    attachment_id: item
                        .get("attachmentId")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }));
            }
            "thinking" => {
                let thinking = item
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let redacted = item
                    .get("redacted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                parts.push(ConversationPart::Thinking(ThinkingPart {
                    thinking,
                    redacted,
                }));
            }
            "toolCall" => {
                let call = ToolCallPart {
                    id: item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    arguments: item
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
                };
                if let Some(kind) = special_tool_kind(&call) {
                    parts.push(ConversationPart::Special { kind, call });
                } else if let Some(ConversationPart::Tools(calls)) = parts.last_mut() {
                    calls.push(call);
                } else {
                    parts.push(ConversationPart::Tools(vec![call]));
                }
            }
            "text" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                if let Some(ConversationPart::Text(prev)) = parts.last_mut() {
                    prev.push('\n');
                    prev.push_str(text);
                } else {
                    parts.push(ConversationPart::Text(text.to_owned()));
                }
            }
            _ => {}
        }
    }
    parts
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub user: Option<TurnEntry>,
    pub assistants: Vec<TurnEntry>,
    pub summary: Option<TurnEntry>,
    pub compaction: Option<TurnEntry>,
}

pub fn is_conversation_role(role: &str) -> bool {
    matches!(
        role,
        "user" | "assistant" | "branchSummary" | "compactionSummary"
    )
}

/// Groups visible session entries into turns (`user` + following `assistant`s; `branchSummary` / `compactionSummary` stand alone).
/// Also builds a lookup map from `toolCallId` -> `toolResult` JSON payload.
pub fn group_turns_and_results(raw_entries: &[Value]) -> (Vec<Turn>, BTreeMap<String, Value>) {
    let mut turns: Vec<Turn> = Vec::new();
    let mut tool_results: BTreeMap<String, Value> = BTreeMap::new();

    for (idx, raw) in raw_entries.iter().enumerate() {
        let entry = TurnEntry::from_json(raw, idx);
        if entry.role == "toolResult" {
            let tcid = entry
                .raw
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !tcid.is_empty() {
                tool_results.insert(tcid.to_owned(), entry.raw.clone());
            }
            continue;
        }
        if !is_conversation_role(&entry.role) {
            continue;
        }
        match entry.role.as_str() {
            "user" => turns.push(Turn {
                user: Some(entry),
                assistants: Vec::new(),
                summary: None,
                compaction: None,
            }),
            "branchSummary" => turns.push(Turn {
                user: None,
                assistants: Vec::new(),
                summary: Some(entry),
                compaction: None,
            }),
            "compactionSummary" => turns.push(Turn {
                user: None,
                assistants: Vec::new(),
                summary: None,
                compaction: Some(entry),
            }),
            "assistant" => {
                if let Some(last) = turns.last_mut()
                    && last.summary.is_none()
                    && last.compaction.is_none()
                {
                    last.assistants.push(entry);
                } else {
                    turns.push(Turn {
                        user: None,
                        assistants: vec![entry],
                        summary: None,
                        compaction: None,
                    });
                }
            }
            _ => {}
        }
    }

    (turns, tool_results)
}

pub fn active_turn_index(turns: &[Turn], running: bool) -> Option<usize> {
    if !running || turns.is_empty() {
        return None;
    }
    let mut idx = turns.len();
    while idx > 0 {
        idx -= 1;
        if turns[idx].summary.is_none() && turns[idx].compaction.is_none() {
            return Some(idx);
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct TurnPart {
    pub entry_key: String,
    pub timestamp_ms: u64,
    pub is_final_entry: bool,
    pub part: ConversationPart,
}

#[derive(Debug, Clone)]
pub struct SplitTurnOutput {
    pub process: Vec<TurnPart>,
    pub final_text: String,
    pub final_entry_id: Option<String>,
    pub total_duration_ms: u64,
    pub was_aborted: bool,
    pub assistant_error: Option<AssistantErrorPresentation>,
}

#[derive(Debug, Clone)]
pub struct AssistantErrorPresentation {
    pub headline: String,
    pub raw_detail: Option<String>,
}

pub fn format_assistant_error(provider: Option<&str>, raw_err: &str) -> AssistantErrorPresentation {
    let prov = provider.filter(|s| !s.is_empty()).unwrap_or("提供商");
    let lower = raw_err.to_ascii_lowercase();
    let headline = if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || raw_err.contains("凭据")
        || raw_err.contains("密钥")
    {
        format!("{prov} 凭据无效或已过期，请在「设置 · 提供商」更新后重试。")
    } else if lower.contains("429") || lower.contains("rate limit") || raw_err.contains("频繁") {
        format!("{prov} 请求过于频繁，请稍后重试或切换其他模型。")
    } else {
        format!("{prov} 服务响应异常，请检查网络后重新发送。")
    };
    let detail = (!raw_err.trim().is_empty()).then(|| raw_err.trim().to_owned());
    AssistantErrorPresentation {
        headline,
        raw_detail: detail,
    }
}

/// Splits an assistant chain into collapsible `process` items + standalone items vs trailing `final_text`.
pub fn split_turn(assistants: &[TurnEntry]) -> SplitTurnOutput {
    let mut process = Vec::new();
    let mut final_text = String::new();
    let mut final_entry_id = None;
    let mut total_duration_ms = 0u64;
    let mut earliest_work_start: Option<u64> = None;
    let mut latest_work_end: Option<u64> = None;
    let mut was_aborted = false;
    let mut assistant_error = None;

    for (index, entry) in assistants.iter().enumerate() {
        total_duration_ms = total_duration_ms.saturating_add(entry.duration_ms.unwrap_or(0));
        if let Some(ws) = entry.work_started_ms {
            earliest_work_start = Some(earliest_work_start.map_or(ws, |cur| cur.min(ws)));
        }
        if let Some(we) = entry.work_ended_ms {
            latest_work_end = Some(latest_work_end.map_or(we, |cur| cur.max(we)));
        }
        let is_final = index + 1 == assistants.len();
        if is_final {
            final_entry_id = entry.id.clone();
            if entry.stop_reason.as_deref() == Some("aborted") {
                was_aborted = true;
            }
            if let Some(ref err_msg) = entry.error_message
                && !err_msg.trim().is_empty()
            {
                assistant_error = Some(format_assistant_error(entry.provider.as_deref(), err_msg));
            }
        }

        let parts = ordered_parts(&entry.raw);
        let mut final_part_idx: Option<usize> = None;
        for (pi, p) in parts.iter().enumerate().rev() {
            match p {
                ConversationPart::Thinking(_) => continue,
                ConversationPart::Text(t) if t.trim().is_empty() => continue,
                _ => {
                    final_part_idx = Some(pi);
                    break;
                }
            }
        }

        for (pi, p) in parts.into_iter().enumerate() {
            if is_final
                && Some(pi) == final_part_idx
                && let ConversationPart::Text(t) = p
            {
                final_text = t;
                continue;
            }
            if let ConversationPart::Text(ref t) = p
                && t.trim().is_empty()
            {
                continue;
            }
            process.push(TurnPart {
                entry_key: entry.stable_key(),
                timestamp_ms: entry.timestamp_ms,
                is_final_entry: is_final,
                part: p,
            });
        }
    }

    if total_duration_ms == 0
        && let (Some(ws), Some(we)) = (earliest_work_start, latest_work_end)
    {
        total_duration_ms = we.saturating_sub(ws);
    }

    SplitTurnOutput {
        process,
        final_text,
        final_entry_id,
        total_duration_ms,
        was_aborted,
        assistant_error,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnSegmentKind {
    Process,
    Visible,
}

#[derive(Debug, Clone)]
pub struct TurnSegment {
    pub kind: TurnSegmentKind,
    pub key: String,
    pub parts: Vec<TurnPart>,
    expansion_anchor: Option<(usize, usize)>,
}

impl TurnSegment {
    /// Source part / call offsets before result-dependent splitting.
    pub fn expansion_anchor(&self) -> Option<(usize, usize)> {
        self.expansion_anchor
    }
}

fn push_turn_segment_item(
    segments: &mut Vec<TurnSegment>,
    kind: TurnSegmentKind,
    item: TurnPart,
    anchor: (usize, usize),
) {
    if let Some(prev) = segments.last_mut()
        && prev.kind == kind
    {
        prev.parts.push(item);
        return;
    }
    let key = format!(
        "{}-{}-{}",
        if kind == TurnSegmentKind::Visible {
            "visible"
        } else {
            "process"
        },
        item.timestamp_ms,
        segments.len()
    );
    segments.push(TurnSegment {
        kind,
        key,
        parts: vec![item],
        expansion_anchor: (kind == TurnSegmentKind::Process).then_some(anchor),
    });
}

/// Groups `process` parts into alternating `Process` and `Visible` (special tools / standalone images) segments,
/// always preserving `process-start` at index `0` for the turn working-duration header.
pub fn build_turn_segments(process: Vec<TurnPart>) -> Vec<TurnSegment> {
    build_turn_segments_with_results(process, &BTreeMap::new())
}

/// Splits `process` parts with `tool_results` awareness so standalone `read`+image calls render outside
/// the collapsible `Process` header just like web `splitToolCalls`.
pub fn build_turn_segments_with_results(
    process: Vec<TurnPart>,
    tool_results: &BTreeMap<String, Value>,
) -> Vec<TurnSegment> {
    let mut segments: Vec<TurnSegment> = Vec::new();
    for (part_idx, item) in process.into_iter().enumerate() {
        match item.part {
            ConversationPart::Special { .. } | ConversationPart::Image(_) => {
                push_turn_segment_item(
                    &mut segments,
                    TurnSegmentKind::Visible,
                    item,
                    (part_idx, 0),
                );
            }
            ConversationPart::Text(ref text) => {
                if !text.trim().is_empty() {
                    push_turn_segment_item(
                        &mut segments,
                        TurnSegmentKind::Process,
                        item,
                        (part_idx, 0),
                    );
                }
            }
            ConversationPart::Tools(calls) => {
                let mut calls = calls.into_iter().enumerate().peekable();
                let kind_for = |call: &ToolCallPart| {
                    if is_standalone_tool(call, tool_results.get(&call.id)) {
                        TurnSegmentKind::Visible
                    } else {
                        TurnSegmentKind::Process
                    }
                };
                while let Some((first_idx, first)) = calls.next() {
                    let kind = kind_for(&first);
                    let mut run = vec![first];
                    // Keep standalone calls as separate parts, preserving subsequent element IDs.
                    while let Some((_, call)) = calls.next_if(|(_, call)| {
                        kind == TurnSegmentKind::Process && kind_for(call) == kind
                    }) {
                        run.push(call);
                    }
                    let part = TurnPart {
                        entry_key: item.entry_key.clone(),
                        timestamp_ms: item.timestamp_ms,
                        is_final_entry: item.is_final_entry,
                        part: ConversationPart::Tools(run),
                    };
                    push_turn_segment_item(&mut segments, kind, part, (part_idx, first_idx));
                }
            }
            _ => {
                push_turn_segment_item(&mut segments, TurnSegmentKind::Process, item, (part_idx, 0))
            }
        }
    }
    if segments
        .first()
        .is_none_or(|s| s.kind != TurnSegmentKind::Process)
    {
        segments.insert(
            0,
            TurnSegment {
                kind: TurnSegmentKind::Process,
                key: "process-start".to_owned(),
                parts: Vec::new(),
                expansion_anchor: None,
            },
        );
    } else if let Some(first) = segments.first_mut() {
        first.key = "process-start".to_owned();
    }
    segments
}

/// Computes the exact Chinese header label for a collapsible `Process` segment (`turn-segments.ts` parity).
pub fn process_segment_label(
    parts: &[TurnPart],
    tool_results: &BTreeMap<String, Value>,
    live: bool,
) -> Option<String> {
    let mut ordinary_total = 0usize;
    let mut ordinary_completed = 0usize;
    for tp in parts {
        if let ConversationPart::Tools(ref calls) = tp.part {
            for call in calls {
                let res = tool_results.get(&call.id);
                if !is_standalone_tool(call, res) {
                    ordinary_total += 1;
                    if res.is_some() {
                        ordinary_completed += 1;
                    }
                }
            }
        }
    }
    if ordinary_total > 0 {
        if ordinary_completed == 0 {
            return Some(if live {
                format!("正在调用 {ordinary_total} 个工具")
            } else {
                "检查未完成".to_owned()
            });
        }
        return Some(if ordinary_completed == ordinary_total {
            format!("已完成 {ordinary_completed} 次工具调用")
        } else {
            format!("已完成 {ordinary_completed} / {ordinary_total} 次工具调用")
        });
    }

    let meaningful: Vec<&TurnPart> = parts
        .iter()
        .filter(|tp| match &tp.part {
            ConversationPart::Thinking(t) => t.redacted || !t.thinking.trim().is_empty(),
            ConversationPart::Text(s) => !s.trim().is_empty(),
            ConversationPart::Tools(c) => !c.is_empty(),
            _ => false,
        })
        .collect();
    if meaningful.is_empty() {
        return None;
    }
    if meaningful
        .iter()
        .all(|tp| matches!(tp.part, ConversationPart::Thinking(_)))
    {
        Some("思考过程".to_owned())
    } else {
        Some("处理过程".to_owned())
    }
}

/// Returns true when a `Process` segment (`seg_idx > 0`) is actively running and should carry
/// the `RunningLight` sweep highlight (e.g. `正在调用 1 个工具`, `已完成 1 / 2 次工具调用`, or active tail processing).
pub fn segment_is_running(
    parts: &[TurnPart],
    tool_results: &BTreeMap<String, Value>,
    live: bool,
    is_last_segment: bool,
    has_final_text: bool,
) -> bool {
    if !live {
        return false;
    }
    let mut has_pending_tool = false;
    let mut has_any_tool = false;
    for tp in parts {
        if let ConversationPart::Tools(ref calls) = tp.part {
            for call in calls {
                let res = tool_results.get(&call.id);
                if !is_standalone_tool(call, res) {
                    has_any_tool = true;
                    if res.is_none() {
                        has_pending_tool = true;
                    }
                }
            }
        }
    }
    if has_pending_tool {
        return true;
    }
    !has_any_tool && is_last_segment && !has_final_text && !parts.is_empty()
}

pub fn format_duration_zh(ms: u64) -> String {
    let tenths = ms / 100 + u64::from(ms % 100 >= 50);
    format!("{}.{:01}s", tenths / 10, tenths % 10)
}

/// Formats live elapsed turn duration on a whole-second cadence (`1s`, `2s`, ...) so each
/// tick pairs cleanly with the vertical cross-slide label transition without 10Hz jitter.
pub fn format_live_duration_zh(ms: u64) -> String {
    let secs = (ms / 1000).max(1);
    format!("{secs}s")
}

/// Segments `text` into extended Unicode grapheme clusters (`RunningLight` `lightGlyphs` parity).
pub fn light_glyphs(text: &str) -> Vec<&str> {
    text.graphemes(true).collect()
}

/// Computes the 5-glyph sweep highlight strength `[0.0, 0.48]` for grapheme `index` out of `count`
/// at normalized linear cycle `phase` (`0.0..1.0`, `2000ms` period).
/// Matches `glyphLightFrames` in `design/storybook.html` & `running-light-timing.ts`.
pub fn glyph_light_strength(index: usize, count: usize, phase: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let cycle = (count + 5) as f32;
    let weights: [f32; 7] = [0.0, 0.08, 0.24, 0.48, 0.24, 0.08, 0.0];
    let p = phase.rem_euclid(1.0);
    let pos = p * cycle - (index as f32);
    if pos <= 0.0 || pos >= 6.0 {
        return 0.0;
    }
    let step = (pos.floor() as usize).min(5);
    let frac = pos - (step as f32);
    weights[step] + (weights[step + 1] - weights[step]) * frac
}

/// Blends `base` toward `hi` using the 5-glyph `RunningLight` strength curve.
pub fn glyph_blend_color(base: Hsla, hi: Hsla, index: usize, count: usize, phase: f32) -> Hsla {
    let strength = glyph_light_strength(index, count, phase);
    // Strength peaks at 0.48 -> normalize to [0, 1] blend toward primary highlight
    let t = (strength / 0.48).clamp(0.0, 1.0);
    Hsla {
        h: base.h + (hi.h - base.h) * t,
        s: base.s + (hi.s - base.s) * t,
        l: base.l + (hi.l - base.l) * t,
        a: base.a + (hi.a - base.a) * t,
    }
}

/// Renders a `RunningLight` per-grapheme sweep label (`2000ms` linear cycle, 5-glyph window).
/// Immediately stops requesting animation frames (`0 FPS`) when `!active`, window unfocused, minimized, or `reduce_motion`.
pub fn render_running_light_label(
    text: &str,
    sweep_allowed: bool,
    started_at: Instant,
    palette: &DesktopPalette,
    window: &mut Window,
) -> impl IntoElement {
    render_running_light_label_with_scale(
        text,
        sweep_allowed,
        started_at,
        palette.text_secondary,
        palette.text_primary,
        scale::BODY,
        window,
    )
}

/// Renders a `RunningLight` per-grapheme sweep label with explicit typography scale and base/highlight colors.
pub fn render_running_light_label_with_scale(
    text: &str,
    sweep_allowed: bool,
    started_at: Instant,
    base: Hsla,
    hi: Hsla,
    typography: TypographyScale,
    window: &mut Window,
) -> impl IntoElement {
    let vp = window.viewport_size();
    let minimized = f32::from(vp.width) <= 1.0 || f32::from(vp.height) <= 1.0;
    let should_sweep =
        sweep_allowed && window.is_window_active() && window.is_visible() && !minimized;

    if !should_sweep {
        return apply_scale(div(), typography)
            .text_color(base)
            .child(cjk_inline_label(text.to_owned()));
    }

    let cycle_ms = crate::theme::motion::RUNNING_LIGHT_CYCLE.as_millis();
    let phase = (started_at.elapsed().as_millis() % cycle_ms) as f32 / cycle_ms as f32;
    let glyphs = light_glyphs(text);
    let count = glyphs.len();

    let mut row = apply_scale(div(), typography)
        .pb(crate::theme::space::MICRO)
        .relative()
        .flex()
        .items_center()
        .child(
            gpui_kit::canvas(
                |bounds, window, _| {
                    if bounds.intersects(&window.content_mask().bounds) {
                        #[cfg(feature = "test-support")]
                        crate::ux_probe::record_running_light_frame();
                        window.request_animation_frame();
                    }
                },
                |_, (), _, _| {},
            )
            .absolute()
            .size_full(),
        );
    for (idx, g) in glyphs.into_iter().enumerate() {
        let color = glyph_blend_color(base, hi, idx, count, phase);
        row = row.child(
            div()
                .text_color(color)
                .child(SharedString::from(g.to_owned())),
        );
    }
    row
}

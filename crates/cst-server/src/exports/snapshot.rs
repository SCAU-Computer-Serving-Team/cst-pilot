use std::collections::HashSet;

use chrono::{DateTime, Local, NaiveDate, SecondsFormat};
use pi_agent::AgentMessage;
use pi_ai::ContentBlock;
use pi_session::{SessionEntry, parse_iso_timestamp_ms};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

use crate::error::{ServiceError, ServiceErrorKind};
use crate::session::events::browser_json_value;
use crate::session::slot::SessionSlot;

pub const MAX_EXPORT_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub format: String,
    pub theme: String,
    pub include_thinking: bool,
    pub include_tools: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFileEntry {
    pub name: String,
    pub bytes: usize,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJobRecord {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub captured_at: String,
    pub options: Value,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    pub files: Vec<ExportFileEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

pub fn validate_artifact_date(value: &str) -> Result<String, ServiceError> {
    static DATE_RE: OnceLock<Regex> = OnceLock::new();
    let re = DATE_RE.get_or_init(|| Regex::new(r"^[1-9]\d{3}-\d{2}-\d{2}$").unwrap());
    if !re.is_match(value) {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_export",
            "请求的产物日期无效，请使用 YYYY-MM-DD",
        ));
    }
    let parsed = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
        ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_export",
            "请求的产物日期无效，请使用 YYYY-MM-DD",
        )
    })?;
    if parsed.format("%Y-%m-%d").to_string() != value {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_export",
            "请求的产物日期无效，请使用 YYYY-MM-DD",
        ));
    }
    Ok(value.to_owned())
}

pub fn artifact_date(value: Option<&str>, now: DateTime<Local>) -> Result<String, ServiceError> {
    match value {
        Some(v) => validate_artifact_date(v),
        None => Ok(now.format("%Y-%m-%d").to_string()),
    }
}

pub fn artifact_name(title: &str) -> String {
    let mut cleaned = String::with_capacity(title.len());
    let mut prev_space = false;
    for ch in title.chars() {
        if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            || ('\0'..='\u{1f}').contains(&ch)
        {
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space {
                cleaned.push(' ');
                prev_space = true;
            }
        } else {
            cleaned.push(ch);
            prev_space = false;
        }
    }
    let trimmed = cleaned.trim();
    let truncated: String = trimmed.chars().take(60).collect();
    let stripped = truncated.trim_end_matches(['.', ' ']);
    if stripped.is_empty() {
        "会话".to_owned()
    } else {
        stripped.to_owned()
    }
}

pub fn parse_export_options(input: &Value) -> Result<ExportOptions, ServiceError> {
    let Some(obj) = input.as_object() else {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_export",
            "请求的导出选项无效",
        ));
    };
    if obj.keys().any(|k| {
        !matches!(
            k.as_str(),
            "format" | "theme" | "includeThinking" | "includeTools"
        )
    }) {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_export",
            "请求包含未知的导出选项",
        ));
    }

    let format = match obj.get("format") {
        None => "png".to_owned(),
        Some(Value::String(s)) if s == "png" || s == "html" => s.clone(),
        _ => {
            return Err(ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_export",
                "请求的导出选项无效",
            ));
        }
    };
    let theme = match obj.get("theme") {
        None => "light".to_owned(),
        Some(Value::String(s)) if s == "light" || s == "dark" => s.clone(),
        _ => {
            return Err(ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_export",
                "请求的导出选项无效",
            ));
        }
    };
    let include_thinking = match obj.get("includeThinking") {
        None => false,
        Some(Value::Bool(b)) => *b,
        _ => {
            return Err(ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_export",
                "请求的导出选项无效",
            ));
        }
    };
    let include_tools = match obj.get("includeTools") {
        None => false,
        Some(Value::Bool(b)) => *b,
        _ => {
            return Err(ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_export",
                "请求的导出选项无效",
            ));
        }
    };

    Ok(ExportOptions {
        format,
        theme,
        include_thinking,
        include_tools,
    })
}

pub fn build_export_snapshot_from_entries(
    session_id: &str,
    title: &str,
    entries: &[Value],
    options: &ExportOptions,
    now: DateTime<Local>,
) -> Result<Value, ServiceError> {
    let mut chosen = Vec::new();
    for entry in entries {
        let Some(entry_id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(msg) = entry.get("message").and_then(Value::as_object) else {
            continue;
        };
        let Some(role) = msg.get("role").and_then(Value::as_str) else {
            continue;
        };
        if role == "toolResult" && !options.include_tools {
            continue;
        }
        if !matches!(role, "user" | "assistant" | "toolResult" | "branchSummary") {
            continue;
        }

        let raw_content = msg.get("content").cloned().unwrap_or(Value::Null);
        let filtered_content = match raw_content {
            Value::String(s) => Value::String(s),
            Value::Array(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    let ptype = part.get("type").and_then(Value::as_str).unwrap_or("");
                    if ptype == "thinking" {
                        let redacted = part
                            .get("redacted")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        if options.include_thinking && !redacted {
                            out.push(part);
                        }
                    } else if ptype == "toolCall" {
                        if options.include_tools {
                            out.push(json!({
                                "type": "toolCall",
                                "id": part.get("id"),
                                "name": part.get("name"),
                                "arguments": part.get("arguments"),
                            }));
                        }
                    } else {
                        out.push(part);
                    }
                }
                Value::Array(out)
            }
            other => other,
        };

        if role == "assistant"
            && filtered_content.as_array().is_some_and(Vec::is_empty)
            && msg.get("errorMessage").is_none()
            && msg.get("stopReason").and_then(Value::as_str) != Some("aborted")
        {
            continue;
        }

        let mut msg_obj = serde_json::Map::new();
        msg_obj.insert("role".to_owned(), json!(role));
        msg_obj.insert("content".to_owned(), filtered_content);
        msg_obj.insert(
            "timestamp".to_owned(),
            msg.get("timestamp").cloned().unwrap_or(Value::Null),
        );
        if role == "assistant" {
            if let Some(sr) = msg.get("stopReason") {
                msg_obj.insert("stopReason".to_owned(), sr.clone());
            }
            if let Some(em) = msg.get("errorMessage") {
                msg_obj.insert("errorMessage".to_owned(), em.clone());
            }
        } else if role == "toolResult" {
            if let Some(tn) = msg.get("toolName") {
                msg_obj.insert("toolName".to_owned(), tn.clone());
            }
            if let Some(tcid) = msg.get("toolCallId") {
                msg_obj.insert("toolCallId".to_owned(), tcid.clone());
            }
            if let Some(det) = msg.get("details") {
                msg_obj.insert("details".to_owned(), det.clone());
            }
            if let Some(ie) = msg.get("isError") {
                msg_obj.insert("isError".to_owned(), ie.clone());
            }
        } else if role == "branchSummary" {
            if let Some(sum) = msg.get("summary") {
                msg_obj.insert("summary".to_owned(), sum.clone());
            }
            if let Some(fid) = msg.get("fromId") {
                msg_obj.insert("fromId".to_owned(), fid.clone());
            }
        }

        chosen.push(json!({
            "id": entry_id,
            "message": Value::Object(msg_obj),
        }));
    }

    if chosen.is_empty() {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "empty_export",
            "当前没有可导出的消息",
        ));
    }

    let date_str = artifact_date(None, now)?;
    let captured_at = now.to_utc().to_rfc3339_opts(SecondsFormat::Millis, true);
    let raw_snapshot = json!({
        "sessionId": session_id,
        "title": title,
        "capturedAt": captured_at,
        "options": {
            "format": options.format,
            "theme": options.theme,
            "includeThinking": options.include_thinking,
            "includeTools": options.include_tools,
            "date": date_str,
        },
        "entries": chosen,
    });
    let sanitized = browser_json_value(&raw_snapshot);
    let encoded_len = serde_json::to_vec(&sanitized).map_or(0, |v| v.len());
    if encoded_len > MAX_EXPORT_SNAPSHOT_BYTES {
        return Err(ServiceError::new(
            ServiceErrorKind::TooLarge,
            "export_too_large",
            "会话内容过大，请缩短当前分支后导出",
        ));
    }
    Ok(sanitized)
}

pub fn build_export_preview_snapshot(
    slot: &SessionSlot,
    title: &str,
    options: &ExportOptions,
) -> Result<Value, ServiceError> {
    let branch = slot
        .session()
        .with_session_manager(|sm| sm.get_branch(None));

    let persisted_tool_results: HashSet<String> = branch
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::Message(m) => match &m.message {
                AgentMessage::ToolResult(tr) => Some(tr.tool_call_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();

    let mut raw_entries = Vec::new();
    for entry in branch {
        match entry {
            SessionEntry::BranchSummary(bs) => {
                let ts = parse_iso_timestamp_ms(&bs.timestamp).unwrap_or(0);
                raw_entries.push(json!({
                    "id": bs.id,
                    "parentId": bs.parent_id,
                    "message": {
                        "role": "branchSummary",
                        "content": "",
                        "summary": bs.summary,
                        "fromId": bs.from_id,
                        "timestamp": ts,
                    }
                }));
            }
            SessionEntry::Message(m) => {
                match &m.message {
                    AgentMessage::User(_)
                    | AgentMessage::Assistant(_)
                    | AgentMessage::ToolResult(_) => {}
                    _ => continue,
                }
                let decorated = slot.decorators.decorate_message_for_snapshot(&m.message);
                raw_entries.push(json!({
                    "id": m.id,
                    "parentId": m.parent_id,
                    "message": decorated,
                }));

                if options.include_tools
                    && let AgentMessage::Assistant(assistant) = &m.message
                {
                    for part in &assistant.content {
                        if let ContentBlock::ToolCall(tc) = part
                            && tc.name == "bitlocker"
                            && !persisted_tool_results.contains(&tc.id)
                            && let Some(verified) = slot
                                .decorators
                                .verified_bitlocker_result(assistant.timestamp, &tc.id)
                        {
                            raw_entries.push(json!({
                                "id": format!("{}:bitlocker-verification:{}", m.id, tc.id),
                                "message": {
                                    "role": "toolResult",
                                    "toolName": verified.tool_name,
                                    "toolCallId": verified.tool_call_id,
                                    "content": verified.content,
                                    "details": verified.details,
                                    "isError": verified.is_error,
                                    "timestamp": verified.timestamp,
                                }
                            }));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    build_export_snapshot_from_entries(&slot.id, title, &raw_entries, options, Local::now())
}

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Local;
use pi_agent::AgentMessage;
use pi_session::{SessionEntry, now_iso_timestamp, parse_iso_timestamp_ms};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

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

pub fn build_export_preview_snapshot(
    slot: &SessionSlot,
    title: &str,
    options: &ExportOptions,
) -> Result<Value, ServiceError> {
    let branch = slot
        .session()
        .with_session_manager(|sm| sm.get_branch(None));
    let mut chosen = Vec::new();

    for entry in branch {
        match entry {
            SessionEntry::BranchSummary(bs) => {
                let ts = parse_iso_timestamp_ms(&bs.timestamp).unwrap_or(0);
                chosen.push(json!({
                    "id": bs.id,
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
                    AgentMessage::ToolResult(_) if !options.include_tools => continue,
                    AgentMessage::User(_)
                    | AgentMessage::Assistant(_)
                    | AgentMessage::ToolResult(_) => {}
                    _ => continue,
                }
                let decorated = slot.decorators.decorate_message_for_snapshot(&m.message);
                let Some(role) = decorated.get("role").and_then(Value::as_str) else {
                    continue;
                };
                let timestamp = decorated.get("timestamp").cloned().unwrap_or(Value::Null);
                let raw_content = decorated.get("content").cloned().unwrap_or(Value::Null);

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
                    && decorated.get("errorMessage").is_none()
                    && decorated.get("stopReason").and_then(Value::as_str) != Some("aborted")
                {
                    continue;
                }

                let mut msg_obj = serde_json::Map::new();
                msg_obj.insert("role".to_owned(), json!(role));
                msg_obj.insert("content".to_owned(), filtered_content);
                msg_obj.insert("timestamp".to_owned(), timestamp);
                if role == "assistant" {
                    if let Some(sr) = decorated.get("stopReason") {
                        msg_obj.insert("stopReason".to_owned(), sr.clone());
                    }
                    if let Some(em) = decorated.get("errorMessage") {
                        msg_obj.insert("errorMessage".to_owned(), em.clone());
                    }
                    if let Some(work) = decorated.get("work") {
                        msg_obj.insert("work".to_owned(), work.clone());
                    }
                } else if role == "toolResult" {
                    if let Some(tn) = decorated.get("toolName") {
                        msg_obj.insert("toolName".to_owned(), tn.clone());
                    }
                    if let Some(tcid) = decorated.get("toolCallId") {
                        msg_obj.insert("toolCallId".to_owned(), tcid.clone());
                    }
                    if let Some(det) = decorated.get("details") {
                        msg_obj.insert("details".to_owned(), det.clone());
                    }
                    if let Some(ie) = decorated.get("isError") {
                        msg_obj.insert("isError".to_owned(), ie.clone());
                    }
                }

                chosen.push(json!({
                    "id": m.id,
                    "message": Value::Object(msg_obj),
                }));
            }
            _ => {}
        }
    }

    if chosen.is_empty() {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "empty_export",
            "当前没有可导出的消息",
        ));
    }

    let date_str = Local::now().format("%Y-%m-%d").to_string();
    let raw_snapshot = json!({
        "sessionId": slot.id,
        "title": title,
        "capturedAt": now_iso_timestamp(),
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

/// Stage 7 export route framework; Stage 8 wires `Chromiumoxide` rendering into this store.
#[derive(Clone, Default)]
pub struct ExportManager {
    jobs: Arc<Mutex<HashMap<String, ExportJobRecord>>>,
}

impl ExportManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list_for_session(&self, session_id: &str) -> Vec<ExportJobRecord> {
        let guard = self.jobs.lock().expect("exports lock");
        let mut list: Vec<ExportJobRecord> = guard
            .values()
            .filter(|j| j.session_id == session_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| b.captured_at.cmp(&a.captured_at));
        list.truncate(50);
        list
    }

    pub fn get_job(&self, export_id: &str) -> Option<ExportJobRecord> {
        self.jobs
            .lock()
            .expect("exports lock")
            .get(export_id)
            .cloned()
    }

    pub fn create_job(&self, preview_snapshot: &Value) -> ExportJobRecord {
        let id = Uuid::new_v4().to_string();
        let session_id = preview_snapshot
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let title = preview_snapshot
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("会话")
            .to_owned();
        let captured_at = preview_snapshot
            .get("capturedAt")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(now_iso_timestamp);
        let options = preview_snapshot
            .get("options")
            .cloned()
            .unwrap_or(Value::Null);

        let record = ExportJobRecord {
            id: id.clone(),
            session_id,
            title,
            captured_at,
            options,
            status: "failed".to_owned(),
            directory: None,
            files: Vec::new(),
            error: Some(json!({
                "code": "export_browser_missing",
                "message": "会话导出渲染引擎将在阶段 8 接入 Chromiumoxide。",
            })),
        };
        self.jobs
            .lock()
            .expect("exports lock")
            .insert(id, record.clone());
        record
    }
}

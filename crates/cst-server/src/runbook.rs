use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent::{AgentMessage, BoxFuture};
use pi_ai::{ContentBlock, ToolResultMessage, UserContentBlock};
use pi_session::SessionEntry;
use serde_json::{Value, json};

use crate::discovery::strip_windows_verbatim_prefix;
use crate::error::{ServiceError, ServiceErrorKind};
use crate::session::slot::SessionSlot;

pub type RunbookOpenerFn =
    Arc<dyn Fn(PathBuf) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub fn default_runbook_opener() -> RunbookOpenerFn {
    Arc::new(|file: PathBuf| {
        Box::pin(async move {
            #[cfg(windows)]
            {
                let system_root =
                    std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
                let notepad = PathBuf::from(system_root)
                    .join("System32")
                    .join("notepad.exe");
                tokio::process::Command::new(notepad)
                    .arg(&file)
                    .spawn()
                    .map(|_| ())
                    .map_err(|_| "无法启动记事本".to_owned())
            }
            #[cfg(not(windows))]
            {
                let _ = file;
                Err("当前系统不支持本地清单打开".to_owned())
            }
        })
    })
}

pub fn extract_delivered_runbook_file(message: &ToolResultMessage) -> Option<String> {
    if message.is_error {
        return None;
    }
    let details_val = message.details.clone().or_else(|| {
        let joined: String = message
            .content
            .iter()
            .filter_map(|part| match part {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str::<Value>(&joined).ok()
    })?;
    details_val
        .get("runbook")
        .and_then(|r| r.get("file"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub fn resolve_runbook_file_from_slot(
    slot: &SessionSlot,
    entry_id: &str,
    tool_call_id: &str,
) -> Result<String, ServiceError> {
    let branch = slot
        .session()
        .with_session_manager(|sm| sm.get_branch(None));
    let Some(index) = branch.iter().position(|e| e.id() == entry_id) else {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_request",
            "请求对应的清单调用不存在",
        ));
    };
    let SessionEntry::Message(msg_entry) = &branch[index] else {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_request",
            "请求对应的清单调用不存在",
        ));
    };
    let AgentMessage::Assistant(assistant) = &msg_entry.message else {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_request",
            "请求对应的清单调用不存在",
        ));
    };
    let Some(call_name) = assistant.content.iter().find_map(|part| match part {
        ContentBlock::ToolCall(tc)
            if tc.id == tool_call_id
                && matches!(tc.name.as_str(), "runbook" | "runbook_deliver") =>
        {
            Some(tc.name.clone())
        }
        _ => None,
    }) else {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_request",
            "请求对应的清单调用不存在",
        ));
    };

    let mut delivered: Option<ToolResultMessage> = None;
    for candidate in &branch[index + 1..] {
        let SessionEntry::Message(cand_msg) = candidate else {
            continue;
        };
        match &cand_msg.message {
            AgentMessage::Assistant(_) | AgentMessage::User(_) => break,
            AgentMessage::ToolResult(tr)
                if tr.tool_call_id == tool_call_id && tr.tool_name == call_name =>
            {
                delivered = Some(tr.clone());
                break;
            }
            _ => {}
        }
    }

    if delivered.is_none()
        && let Some(early) = slot
            .decorators
            .early_tool_result(assistant.timestamp, tool_call_id)
        && early.tool_name == call_name
    {
        delivered = Some(early);
    }

    delivered
        .as_ref()
        .and_then(extract_delivered_runbook_file)
        .ok_or_else(|| {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_request",
                "请求对应的清单尚未交付或已失效",
            )
        })
}

pub async fn validate_and_open_runbook(
    file: &str,
    kit_root: &Path,
    artifacts_root: &Path,
    opener: &RunbookOpenerFn,
) -> Result<Value, ServiceError> {
    let file_path = PathBuf::from(file);
    let canonical_target = match fs::canonicalize(&file_path) {
        Ok(p) => strip_windows_verbatim_prefix(&p),
        Err(err) => {
            return Err(match err.kind() {
                io::ErrorKind::NotFound => ServiceError::new(
                    ServiceErrorKind::NotFound,
                    "runbook_missing",
                    "清单文件已不存在，请使用卡片中的命令或重新生成清单。",
                ),
                io::ErrorKind::PermissionDenied => ServiceError::new(
                    ServiceErrorKind::AccessDenied,
                    "runbook_access_denied",
                    "没有权限访问清单，请检查文件权限。",
                ),
                _ => ServiceError::new(
                    ServiceErrorKind::Internal,
                    "runbook_unavailable",
                    "无法读取清单，请检查所在介质。",
                ),
            });
        }
    };

    let mut allowed_bases = Vec::new();
    if let Ok(canon_art) = fs::canonicalize(artifacts_root) {
        allowed_bases.push(strip_windows_verbatim_prefix(&canon_art));
    }
    if let Ok(canon_outbox) = fs::canonicalize(kit_root.join("outbox")) {
        allowed_bases.push(strip_windows_verbatim_prefix(&canon_outbox));
    }

    let is_contained = allowed_bases
        .iter()
        .any(|base| canonical_target.starts_with(base));
    let is_txt = canonical_target
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
    if !is_contained || !is_txt {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "runbook_invalid_file",
            "文件不属于已交付的文本清单",
        ));
    }

    match fs::metadata(&canonical_target) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "runbook_invalid_file",
                "交付路径未指向文本文件",
            ));
        }
        Err(_) => {
            return Err(ServiceError::new(
                ServiceErrorKind::NotFound,
                "runbook_missing",
                "清单文件已不存在，请使用卡片中的命令或重新生成清单。",
            ));
        }
    }

    opener(canonical_target).await.map_err(|_| {
        ServiceError::new(
            ServiceErrorKind::Unavailable,
            "runbook_launch_failed",
            "无法启动记事本，可使用详情中的路径手动打开。",
        )
    })?;

    Ok(json!({ "launched": true }))
}

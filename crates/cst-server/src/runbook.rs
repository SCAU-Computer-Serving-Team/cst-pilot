use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent::{AgentMessage, BoxFuture};
use pi_ai::{ContentBlock, ToolResultMessage};
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
                let notepad = cst_os::system_directory()?.join("notepad.exe");
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

pub use cst_kit::runbook::{extract_delivered_runbook_file, extract_runbook_plan_ref};

fn find_runbook_result_in_slot(
    slot: &SessionSlot,
    entry_id: &str,
    tool_call_id: &str,
    allow_plan_tools: bool,
) -> Result<ToolResultMessage, ServiceError> {
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
                && (if allow_plan_tools {
                    cst_kit::runbook::is_runbook_tool(&tc.name)
                } else {
                    cst_kit::runbook::is_runbook_delivery_tool(&tc.name)
                }) =>
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

    delivered.ok_or_else(|| {
        ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_request",
            "请求对应的清单尚未完成或已失效",
        )
    })
}

pub fn resolve_runbook_file_from_slot(
    slot: &SessionSlot,
    entry_id: &str,
    tool_call_id: &str,
) -> Result<String, ServiceError> {
    let result = find_runbook_result_in_slot(slot, entry_id, tool_call_id, false)?;
    extract_delivered_runbook_file(&result).ok_or_else(|| {
        ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_request",
            "请求对应的清单尚未交付或已失效",
        )
    })
}

pub fn resolve_runbook_plan_from_slot(
    slot: &SessionSlot,
    entry_id: &str,
    tool_call_id: &str,
) -> Result<(String, u64), ServiceError> {
    let result = find_runbook_result_in_slot(slot, entry_id, tool_call_id, true)?;
    extract_runbook_plan_ref(&result).ok_or_else(|| {
        ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "invalid_request",
            "请求对应的命令方案缺少有效 ID 或版本号",
        )
    })
}

pub async fn export_and_maybe_open_runbook(
    kit_root: &Path,
    artifacts_root: &Path,
    runbook_id: String,
    version: u64,
    open: bool,
    opener: &RunbookOpenerFn,
) -> Result<Value, ServiceError> {
    let store = cst_kit::runbook::RunbookStore::new(kit_root).map_err(|msg| {
        ServiceError::new(ServiceErrorKind::Unavailable, "runbook_unavailable", msg)
    })?;
    let mut exported = store
        .export_version(
            runbook_id,
            version,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .map_err(|msg| {
            ServiceError::new(ServiceErrorKind::InvalidInput, "runbook_export_failed", msg)
        })?;
    let mut launched = false;
    let mut launch_error: Option<String> = None;
    if open
        && let Some(file) = exported
            .pointer("/runbook/file")
            .and_then(Value::as_str)
            .map(str::to_owned)
    {
        match validate_and_open_runbook(&file, kit_root, artifacts_root, opener).await {
            Ok(_) => launched = true,
            Err(err) => launch_error = Some(err.message),
        }
    }
    if let Some(obj) = exported.as_object_mut() {
        obj.insert("launched".into(), json!(launched));
        if let Some(msg) = launch_error {
            obj.insert("launchError".into(), json!(msg));
        }
    }
    Ok(exported)
}

fn strip_trusted_base(target: &Path, base: &Path) -> Option<PathBuf> {
    if let Ok(rel) = target.strip_prefix(base) {
        return Some(rel.to_path_buf());
    }
    #[cfg(windows)]
    {
        let target_norm = strip_windows_verbatim_prefix(target);
        let base_norm = strip_windows_verbatim_prefix(base);
        let t_str = target_norm.to_string_lossy();
        let b_str = base_norm.to_string_lossy();
        let b_trimmed = b_str.trim_end_matches(['\\', '/']);
        if t_str.len() > b_trimmed.len()
            && t_str
                .get(..b_trimmed.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b_trimmed))
        {
            let rest = &t_str[b_trimmed.len()..];
            if let Some(rel) = rest.strip_prefix('\\').or_else(|| rest.strip_prefix('/')) {
                return Some(PathBuf::from(rel));
            }
        }
    }
    None
}

pub async fn validate_and_open_runbook(
    file: &str,
    kit_root: &Path,
    artifacts_root: &Path,
    opener: &RunbookOpenerFn,
) -> Result<Value, ServiceError> {
    let raw_target = strip_windows_verbatim_prefix(Path::new(file));
    if !raw_target.is_absolute() {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "runbook_invalid_file",
            "文件不属于已交付的文本清单",
        ));
    }

    let mut matched_storage: Option<(cst_os::fs_safe::Storage, PathBuf)> = None;
    for candidate_base in [artifacts_root.to_path_buf(), kit_root.join("outbox")] {
        let Ok(storage) = cst_os::fs_safe::Storage::new(&candidate_base) else {
            continue;
        };
        let clean_base = strip_windows_verbatim_prefix(&storage.root);
        if let Some(rel) = strip_trusted_base(&raw_target, &clean_base) {
            matched_storage = Some((storage, rel));
            break;
        }
    }

    let Some((storage, rel)) = matched_storage else {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "runbook_invalid_file",
            "文件不属于已交付的文本清单",
        ));
    };

    if rel.as_os_str().is_empty()
        || !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        || !rel
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
    {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "runbook_invalid_file",
            "文件不属于已交付的文本清单",
        ));
    }

    let rel_parent = rel.parent().unwrap_or_else(|| Path::new(""));
    let _dir_lease = storage.pin_directory(rel_parent, false).map_err(|msg| {
        if msg.contains("系统错误 2") || msg.contains("系统错误 3") {
            ServiceError::new(
                ServiceErrorKind::NotFound,
                "runbook_missing",
                "清单文件已不存在，请使用卡片中的命令或重新生成清单。",
            )
        } else if msg.contains("系统错误 5") {
            ServiceError::new(
                ServiceErrorKind::AccessDenied,
                "runbook_access_denied",
                "没有权限访问清单，请检查文件权限。",
            )
        } else {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "runbook_invalid_file",
                "文件不属于已交付的文本清单",
            )
        }
    })?;

    let verified_path = storage.root.join(&rel);
    match fs::symlink_metadata(&verified_path) {
        Ok(meta) => {
            if cst_os::fs_safe::is_reparse_or_symlink(&meta) || !meta.is_file() {
                return Err(ServiceError::new(
                    ServiceErrorKind::InvalidInput,
                    "runbook_invalid_file",
                    "交付路径包含链接、重解析点或未指向文本文件",
                ));
            }
        }
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
    }

    let opened = storage.open_read(&verified_path).map_err(|msg| {
        if msg.contains("系统错误 5") {
            ServiceError::new(
                ServiceErrorKind::AccessDenied,
                "runbook_access_denied",
                "没有权限访问清单，请检查文件权限。",
            )
        } else if msg.contains("硬链接") || msg.contains("链接") || msg.contains("重解析点")
        {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "runbook_invalid_file",
                "文件不属于已交付的文本清单",
            )
        } else {
            ServiceError::new(
                ServiceErrorKind::Internal,
                "runbook_unavailable",
                "无法读取清单，请检查所在介质。",
            )
        }
    })?;
    let Some(opened_file) = opened else {
        return Err(ServiceError::new(
            ServiceErrorKind::NotFound,
            "runbook_missing",
            "清单文件已不存在，请使用卡片中的命令或重新生成清单。",
        ));
    };
    drop(opened_file);

    let launch_path = strip_windows_verbatim_prefix(&verified_path);
    opener(launch_path).await.map_err(|_| {
        ServiceError::new(
            ServiceErrorKind::Unavailable,
            "runbook_launch_failed",
            "无法启动记事本，可使用详情中的路径手动打开。",
        )
    })?;

    Ok(json!({ "launched": true }))
}

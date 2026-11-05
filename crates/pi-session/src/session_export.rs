use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session_manager::{
    CURRENT_SESSION_VERSION, FileEntry, SessionHeader, SessionManager, now_iso_timestamp,
};
use crate::session_store::SessionStoreError;

pub type TrailingEntriesFn<'a> = &'a dyn Fn(Option<&str>, &str) -> Vec<Value>;

/// 将当前分支序列化为 JSONL 文本（支持在尾部附加导出专用条目，对照 `serializeSessionBranch`）。
pub fn serialize_session_branch(
    session_manager: &SessionManager,
    create_trailing_entries: Option<TrailingEntriesFn<'_>>,
) -> Result<String, SessionStoreError> {
    let timestamp = now_iso_timestamp();
    let header = SessionHeader {
        version: Some(CURRENT_SESSION_VERSION),
        id: session_manager.get_session_id().to_owned(),
        timestamp: timestamp.clone(),
        cwd: session_manager.get_cwd().to_owned(),
        parent_session: None,
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push(
        serde_json::to_string(&FileEntry::Session(header))
            .map_err(|e| SessionStoreError::Serialization(e.to_string()))?,
    );

    let mut parent_id: Option<String> = None;
    for mut entry in session_manager.get_branch(None) {
        entry.set_parent_id(parent_id);
        parent_id = Some(entry.id().to_owned());
        lines.push(
            serde_json::to_string(&entry)
                .map_err(|e| SessionStoreError::Serialization(e.to_string()))?,
        );
    }

    if let Some(create_trailing) = create_trailing_entries {
        for trailing in create_trailing(parent_id.as_deref(), &timestamp) {
            lines.push(
                serde_json::to_string(&trailing)
                    .map_err(|e| SessionStoreError::Serialization(e.to_string()))?,
            );
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// 将当前会话分支写入指定路径的 `.jsonl` 文件（对照 `exportSessionToJsonl`）。
pub fn export_session_to_jsonl(
    session_manager: &SessionManager,
    output_path: Option<&Path>,
    create_trailing_entries: Option<TrailingEntriesFn<'_>>,
) -> Result<PathBuf, SessionStoreError> {
    let file_path = match output_path {
        Some(p) => p.to_path_buf(),
        None => {
            let ts = now_iso_timestamp().replace([':', '.'], "-");
            PathBuf::from(format!("session-{ts}.jsonl"))
        }
    };
    if let Some(parent) = file_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        fs::create_dir_all(parent).map_err(|e| SessionStoreError::Io(e.to_string()))?;
    }
    let content = serialize_session_branch(session_manager, create_trailing_entries)?;
    fs::write(&file_path, content).map_err(|e| SessionStoreError::Io(e.to_string()))?;
    Ok(file_path)
}

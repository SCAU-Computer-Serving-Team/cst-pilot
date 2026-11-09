use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{file_lock::FileLock, owner::Owner};
use crate::runbook::storage::Storage;

#[derive(Clone)]
pub(crate) struct LocalFiles {
    pub base: Storage,
    pub owner: Arc<Owner>,
}
impl LocalFiles {
    pub fn new(root: &Path, owner: Arc<Owner>) -> Result<Self, String> {
        Ok(Self {
            base: Storage::new(root)?,
            owner,
        })
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.base.root.join("agent/home/telemetry").join(name)
    }
    pub fn transaction<T>(
        &self,
        path: &Path,
        cancel: &CancellationToken,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _lock = FileLock::acquire(&self.base.root, path, &self.owner.proof(), cancel)?;
        operation().map_err(|_| "遥测本地文件事务失败。".into())
    }
    pub fn read(&self, path: &Path, limit: usize) -> Result<Option<Vec<u8>>, String> {
        let relative = path
            .parent()
            .and_then(|p| p.strip_prefix(&self.base.root).ok())
            .ok_or("遥测文件路径无效。")?;
        match std::fs::symlink_metadata(path.parent().ok_or("遥测文件缺少目录。")?) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            _ => (),
        }
        let _lease = self.base.pin_directory(relative, false)?;
        self.base
            .read_limited(path, limit)
            .map_err(|_| "遥测文件无法安全读取。".into())
    }
    pub fn write(
        &self,
        path: &Path,
        bytes: &[u8],
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        self.base.atomic(path, bytes, true, cancel)
    }
    pub fn remove_exact(&self, path: &Path, expected: &[u8]) -> Result<(), String> {
        if self.read(path, expected.len())?.as_deref() == Some(expected) {
            std::fs::remove_file(path).map_err(|_| "遥测文件无法移除。")?;
        }
        Ok(())
    }
}
/// The same filename escaping as JavaScript encodeURIComponent, including UTF-8 bytes.
pub(crate) fn encode_name(id: &str) -> Result<String, String> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err("遥测记录标识无效。".into());
    }
    Ok(id
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect())
}

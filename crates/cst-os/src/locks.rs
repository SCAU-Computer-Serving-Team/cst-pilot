use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::fs_safe::io_error;
use crate::layout::normalize_canonical_path_for_hash;

/// Kernel-owned single-instance lock within one kit root.
/// Windows uses `\\.\pipe\cst-pilot-server-<hash>`; other targets use an OS file lock under `.state`.
pub struct ServerInstanceLock {
    address: String,
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _file: fs::File,
}

impl ServerInstanceLock {
    pub fn pipe_address(kit_root: &Path) -> Result<String, String> {
        let normalized = normalize_canonical_path_for_hash(kit_root)?;
        let hash = Sha256::digest(normalized.as_bytes());
        Ok(format!(r"\\.\pipe\cst-pilot-server-{hash:x}"))
    }

    pub fn acquire(kit_root: &Path) -> Result<Self, String> {
        let address = Self::pipe_address(kit_root)?;
        #[cfg(windows)]
        {
            let handle = acquire_windows_first_pipe(&address).map_err(|err| {
                if err.is_conflict() {
                    "server_already_running: 当前工具包目录已有 cst-server 实例正在运行。"
                        .to_owned()
                } else {
                    "无法创建服务单实例命名管道锁。".to_owned()
                }
            })?;
            Ok(Self {
                address,
                _handle: handle,
            })
        }
        #[cfg(not(windows))]
        {
            let file = acquire_file_lock(&kit_root.join(".state").join("locks"), &address)
                .map_err(|error| match error {
                    fs::TryLockError::WouldBlock => {
                        "server_already_running: 当前工具包目录已有 cst-server 实例正在运行。"
                            .to_owned()
                    }
                    fs::TryLockError::Error(_) => "无法获取服务单实例文件锁。".to_owned(),
                })?;
            Ok(Self {
                address,
                _file: file,
            })
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

/// Cross-process session single-writer lock.
/// Windows uses `\\.\pipe\cst-pilot-session-<hash>`, matching
/// `agent/home/extensions/runtime/owner.ts`; other targets use an OS file lock under `.state`.
pub struct SessionOwnerLock {
    address: String,
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _file: fs::File,
}

impl SessionOwnerLock {
    pub fn pipe_address(agent_dir: &Path, session_id: &str) -> Result<String, String> {
        let normalized = normalize_canonical_path_for_hash(agent_dir)?;
        let hash = Sha256::digest(format!("{normalized}\0{session_id}").as_bytes());
        Ok(format!(r"\\.\pipe\cst-pilot-session-{hash:x}"))
    }

    pub fn acquire(agent_dir: &Path, session_id: &str) -> Result<Self, String> {
        fs::create_dir_all(agent_dir).map_err(|_| "无法创建会话锁作用域目录。".to_owned())?;
        let address = Self::pipe_address(agent_dir, session_id)?;
        #[cfg(windows)]
        {
            let handle = acquire_windows_first_pipe(&address).map_err(|err| {
                if err.is_conflict() {
                    "session_owned: 该会话正由另一端使用，请先关闭持有该会话的 TUI 或 Web 服务，再重新打开。".to_owned()
                } else {
                    "无法获取会话单写权命名管道锁。".to_owned()
                }
            })?;
            Ok(Self {
                address,
                _handle: handle,
            })
        }
        #[cfg(not(windows))]
        {
            let lock_dir = resolve_non_windows_lock_dir(agent_dir);
            let file = acquire_file_lock(&lock_dir, &address).map_err(|error| match error {
                fs::TryLockError::WouldBlock => {
                    "session_owned: 该会话正由另一端使用，请先关闭持有该会话的 TUI 或 Web 服务，再重新打开。".to_owned()
                }
                fs::TryLockError::Error(_) => "无法获取会话单写权文件锁。".to_owned(),
            })?;
            Ok(Self {
                address,
                _file: file,
            })
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

/// Cross-process runbook/config directory lock.
/// Windows pipe name matches the legacy TypeScript writer (`\\.\pipe\cst-pilot-runbook-<hash>`).
/// Non-Windows uses a kernel-released OS file lock (never a directory lock).
pub struct RunbookDirectoryLock {
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _file: fs::File,
}

impl RunbookDirectoryLock {
    pub fn acquire(directory: &Path) -> Result<Self, String> {
        let canonical = directory
            .canonicalize()
            .map_err(|e| io_error("解析清单锁目录", e))?;
        let name = canonical.to_string_lossy().into_owned();
        #[cfg(windows)]
        let name = {
            let name = if let Some(unc) = name.strip_prefix(r"\\?\UNC\") {
                format!(r"\\{unc}")
            } else {
                name.strip_prefix(r"\\?\").unwrap_or(&name).to_owned()
            };
            name.to_lowercase()
        };
        let key = Sha256::digest(name.as_bytes());
        let address = format!(r"\\.\pipe\cst-pilot-runbook-{key:x}");
        #[cfg(windows)]
        {
            let handle = acquire_windows_first_pipe(&address).map_err(|err| {
                if matches!(err.raw_code, Some(5 | 231)) {
                    "清单目录正由另一进程写入，请稍后重试。".into()
                } else {
                    io_error("建立清单目录锁", err.io)
                }
            })?;
            Ok(Self { _handle: handle })
        }
        #[cfg(not(windows))]
        {
            let lock_dir = resolve_non_windows_lock_dir(&canonical);
            let file = acquire_file_lock(&lock_dir, &address)
                .map_err(|_| "清单目录正由另一进程写入，请稍后重试。".to_owned())?;
            Ok(Self { _file: file })
        }
    }
}

/// One updater per canonical kit, including during rollback and startup recovery.
/// Windows uses `\\.\pipe\cst-pilot-update-<hash>`; non-Windows uses a kernel-released OS file lock.
pub struct UpdateLock {
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _file: fs::File,
}

impl UpdateLock {
    pub fn address(root: &Path) -> Result<String, String> {
        let root = root.canonicalize().map_err(|_| "升级根目录无法读取。")?;
        let text = root.to_str().ok_or("升级目录必须为 Unicode 路径。")?;
        let text = text
            .strip_prefix(r"\\?\UNC\")
            .map(|v| format!(r"\\{v}"))
            .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(text).into());
        Ok(format!(
            r"\\.\pipe\cst-pilot-update-{:x}",
            Sha256::digest(text.trim_end_matches(['/', '\\']).to_lowercase().as_bytes())
        ))
    }

    pub fn acquire(root: &Path) -> Result<Self, String> {
        let name = Self::address(root)?;
        #[cfg(windows)]
        {
            let handle = acquire_windows_first_pipe(&name)
                .map_err(|_| "工具包已有升级或恢复任务进行中。".to_owned())?;
            Ok(Self { _handle: handle })
        }
        #[cfg(not(windows))]
        {
            let canonical_root = root.canonicalize().map_err(|_| "升级根目录无法读取。")?;
            let lock_dir = canonical_root.join(".state").join("locks");
            let file = acquire_file_lock(&lock_dir, &name)
                .map_err(|_| "工具包已有升级或恢复任务进行中。".to_owned())?;
            Ok(Self { _file: file })
        }
    }
}

#[cfg(not(windows))]
fn resolve_non_windows_lock_dir(canonical_target: &Path) -> std::path::PathBuf {
    for ancestor in canonical_target.ancestors() {
        if ancestor.join("app").is_dir() {
            return ancestor.join(".state").join("locks");
        }
        if ancestor.file_name().and_then(|n| n.to_str()) == Some("artifacts")
            && let Some(kit_root) = ancestor.parent()
        {
            return kit_root.join(".state").join("locks");
        }
    }
    canonical_target
        .parent()
        .unwrap_or(canonical_target)
        .join(".state")
        .join("locks")
}

#[cfg(any(test, not(windows)))]
pub fn acquire_file_lock(directory: &Path, address: &str) -> Result<fs::File, fs::TryLockError> {
    fs::create_dir_all(directory).map_err(fs::TryLockError::Error)?;
    let hash = Sha256::digest(address.as_bytes());
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!("{hash:x}.lock")))
        .map_err(fs::TryLockError::Error)?;
    file.try_lock()?;
    // Keep the file in place: unlinking it lets another process lock a different inode.
    Ok(file)
}

#[cfg(windows)]
struct PipeError {
    raw_code: Option<i32>,
    io: std::io::Error,
}

#[cfg(windows)]
impl PipeError {
    fn is_conflict(&self) -> bool {
        matches!(self.raw_code, Some(5 | 183 | 231))
    }
}

#[cfg(windows)]
fn acquire_windows_first_pipe(
    address: &str,
) -> Result<std::os::windows::io::OwnedHandle, PipeError> {
    use std::os::windows::io::FromRawHandle;
    use windows::{
        Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
            System::Pipes::{
                CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
                PIPE_WAIT,
            },
        },
        core::PCWSTR,
    };

    let wide: Vec<u16> = address.encode_utf16().chain([0]).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            0,
            0,
            None,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let io = std::io::Error::last_os_error();
        let raw_code = io.raw_os_error();
        return Err(PipeError { raw_code, io });
    }
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) })
}

#[cfg(test)]
mod tests {
    use super::acquire_file_lock;

    #[test]
    fn file_locks_preserve_full_identity_and_release_on_drop() {
        let root = std::env::temp_dir().join(format!("cst-os-lock-{}", uuid::Uuid::new_v4()));
        let dir = root.join(".state").join("kernel-locks");
        let address = "cst-test";
        let first = acquire_file_lock(&dir, address).expect("first holder");
        assert!(matches!(
            acquire_file_lock(&dir, address),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let other = acquire_file_lock(&dir, "cst-test-other").expect("distinct identity");
        drop(first);
        let reacquired = acquire_file_lock(&dir, address).expect("released kernel lock");
        drop((other, reacquired));
        std::fs::remove_dir_all(root).unwrap();
    }
}

use sha2::{Digest, Sha256};
use std::path::Path;

/// One updater per canonical kit, including during rollback and startup recovery.
pub struct UpdateLock {
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _listener: std::net::TcpListener,
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
            use std::os::windows::io::FromRawHandle;
            use windows::{
                Win32::{
                    Foundation::INVALID_HANDLE_VALUE,
                    Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
                    System::Pipes::{
                        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                        PIPE_TYPE_BYTE, PIPE_WAIT,
                    },
                },
                core::PCWSTR,
            };
            let name: Vec<_> = name.encode_utf16().chain([0]).collect();
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
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
                return Err("工具包已有升级或恢复任务进行中。".into());
            }
            Ok(Self {
                _handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) },
            })
        }
        #[cfg(not(windows))]
        {
            let hash = Sha256::digest(name.as_bytes());
            let prefix = u32::from_be_bytes(hash[..4].try_into().map_err(|_| "升级锁摘要无效。")?);
            let listener = std::net::TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                32000 + (prefix % 20000) as u16,
            ))
            .map_err(|_| "工具包已有升级或恢复任务进行中。")?;
            Ok(Self {
                _listener: listener,
            })
        }
    }
}

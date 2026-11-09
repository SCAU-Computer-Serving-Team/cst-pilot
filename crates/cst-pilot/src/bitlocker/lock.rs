use super::model::normalize_id;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// The name and normalization match the TypeScript tool, across kits and sessions.
pub struct VolumeLock {
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _listener: std::net::TcpListener,
}
impl VolumeLock {
    pub fn address(id: &str) -> Result<String, String> {
        let hash = Sha256::digest(normalize_id(id)?.as_bytes());
        Ok(format!(r"\\.\pipe\cst-pilot-bitlocker-{hash:x}"))
    }
    pub fn acquire(id: &str) -> Result<Self, String> {
        #[cfg(windows)]
        let lock = {
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
            let name: Vec<u16> = Self::address(id)?.encode_utf16().chain([0]).collect();
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
                return Err(
                    if matches!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(5 | 231)
                    ) {
                        "目标卷已有操作进行中".into()
                    } else {
                        "无法取得目标卷操作锁，未提交操作。".into()
                    },
                );
            }
            Self {
                _handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) },
            }
        };
        #[cfg(not(windows))]
        let lock = {
            let hash = Sha256::digest(normalize_id(id)?.as_bytes());
            let prefix = u32::from_be_bytes(hash[..4].try_into().map_err(|_| "卷锁摘要无效。")?);
            let listener = std::net::TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                32000 + (prefix % 20000) as u16,
            ))
            .map_err(|_| "目标卷已有操作进行中")?;
            Self {
                _listener: listener,
            }
        };
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        Ok(lock)
    }
    /// Includes UI waits and workers still holding an unknown COM operation.
    pub fn active_count() -> usize {
        ACTIVE.load(Ordering::SeqCst)
    }
}
impl Drop for VolumeLock {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

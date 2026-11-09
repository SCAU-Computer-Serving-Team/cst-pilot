use std::fs;
use std::path::{Path, PathBuf};

use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::locks::{StorageError, atomic_write_json_sync};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerDiscoveryRecord {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub started_at: String,
}

pub fn strip_windows_verbatim_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

pub fn normalize_canonical_path_for_hash(path: &Path) -> Result<String, String> {
    let canonical = path
        .canonicalize()
        .map_err(|_| "目录无法解析真实路径。".to_owned())?;
    let cleaned = strip_windows_verbatim_prefix(&canonical);
    let raw = cleaned.to_string_lossy();
    if cfg!(windows) {
        Ok(raw.to_lowercase())
    } else {
        Ok(raw.into_owned())
    }
}

pub fn generate_random_hex_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

pub fn discovery_file_path(kit_root: &Path) -> PathBuf {
    kit_root.join(".state").join("server.json")
}

pub fn write_discovery_file(
    kit_root: &Path,
    port: u16,
    token: &str,
) -> Result<ServerDiscoveryRecord, StorageError> {
    let record = ServerDiscoveryRecord {
        pid: std::process::id(),
        port,
        token: token.to_owned(),
        started_at: pi_session::now_iso_timestamp(),
    };
    let path = discovery_file_path(kit_root);
    atomic_write_json_sync(&path, &record, "写入服务发现文件")?;
    Ok(record)
}

pub fn read_discovery_file(kit_root: &Path) -> Option<ServerDiscoveryRecord> {
    let bytes = fs::read(discovery_file_path(kit_root)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn cleanup_discovery_file(kit_root: &Path, expected_pid: u32, expected_port: u16) {
    let path = discovery_file_path(kit_root);
    if let Some(current) = read_discovery_file(kit_root)
        && current.pid == expected_pid
        && current.port == expected_port
    {
        let _ = fs::remove_file(path);
    }
}

/// Kernel-owned single-instance lock for `cst-server` within one kit root (`\\.\pipe\cst-pilot-server-<hash>`).
pub struct ServerInstanceLock {
    address: String,
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _listener: std::net::TcpListener,
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
            let handle = acquire_windows_first_pipe(&address).map_err(|conflict| {
                if conflict {
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
            let normalized = normalize_canonical_path_for_hash(kit_root)?;
            let hash = Sha256::digest(normalized.as_bytes());
            let prefix = u32::from_be_bytes(hash[..4].try_into().unwrap());
            let listener = std::net::TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                42000 + (prefix % 10000) as u16,
            ))
            .map_err(|_| {
                "server_already_running: 当前工具包目录已有 cst-server 实例正在运行。".to_owned()
            })?;
            Ok(Self {
                address,
                _listener: listener,
            })
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

/// Cross-process session single-writer lock (`\\.\pipe\cst-pilot-session-<hash>`),
/// matching `agent/home/extensions/runtime/owner.ts`.
pub struct SessionOwnerLock {
    address: String,
    #[cfg(windows)]
    _handle: std::os::windows::io::OwnedHandle,
    #[cfg(not(windows))]
    _listener: std::net::TcpListener,
}

impl SessionOwnerLock {
    pub fn pipe_address(agent_dir: &Path, session_id: &str) -> Result<String, String> {
        let normalized = normalize_canonical_path_for_hash(agent_dir)?;
        let hash = Sha256::digest(format!("{normalized}\0{session_id}").as_bytes());
        Ok(format!(r"\\.\pipe\cst-pilot-session-{hash:x}"))
    }

    pub fn acquire(agent_dir: &Path, session_id: &str) -> Result<Self, String> {
        let address = Self::pipe_address(agent_dir, session_id)?;
        #[cfg(windows)]
        {
            let handle = acquire_windows_first_pipe(&address).map_err(|conflict| {
                if conflict {
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
            let normalized = normalize_canonical_path_for_hash(agent_dir)?;
            let hash = Sha256::digest(format!("{normalized}\0{session_id}").as_bytes());
            let prefix = u32::from_be_bytes(hash[..4].try_into().unwrap());
            let listener = std::net::TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                12000 + (prefix % 20000) as u16,
            ))
            .map_err(|_| {
                "session_owned: 该会话正由另一端使用，请先关闭持有该会话的 TUI 或 Web 服务，再重新打开。"
                    .to_owned()
            })?;
            Ok(Self {
                address,
                _listener: listener,
            })
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

#[cfg(windows)]
fn acquire_windows_first_pipe(address: &str) -> Result<std::os::windows::io::OwnedHandle, bool> {
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
        let code = std::io::Error::last_os_error().raw_os_error();
        return Err(matches!(code, Some(5 | 183 | 231)));
    }
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) })
}

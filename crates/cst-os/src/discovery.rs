use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs_safe::{StorageError, atomic_write_json_sync};

/// `.state/server.json` discovery contract shared by `cst-server`, `cst-updater`, and `cst-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerDiscoveryRecord {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub started_at: String,
}

pub fn discovery_file_path(kit_root: &Path) -> PathBuf {
    kit_root.join(".state").join("server.json")
}

pub fn write_discovery_file_with_timestamp(
    kit_root: &Path,
    port: u16,
    token: &str,
    started_at: String,
) -> Result<ServerDiscoveryRecord, StorageError> {
    let record = ServerDiscoveryRecord {
        pid: std::process::id(),
        port,
        token: token.to_owned(),
        started_at,
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

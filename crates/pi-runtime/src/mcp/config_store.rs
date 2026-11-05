use std::path::Path;

use serde_json::{Value, json};

use super::{McpServerConfig, mcp_namespace, validate_mcp_server_config};
use crate::auth_storage::{
    AtomicWriteOutcome, DedicatedFileLockGuard, atomic_write_file_sync, strip_utf8_bom,
};

pub fn add_mcp_server_config(
    path: &Path,
    name: &str,
    config: &McpServerConfig,
) -> Result<(), String> {
    let value = serde_json::to_value(config).map_err(|e| e.to_string())?;
    validate_mcp_server_config(name, &value)?;
    mutate(path, name, |servers| {
        if servers.contains_key(name) {
            return Err(format!("MCP server already exists: {name}"));
        }
        servers.insert(name.to_owned(), value);
        Ok(())
    })
}

/// Merges top-level fields. Unrecognized fields and unrelated configuration remain intact.
pub fn update_mcp_server_config(path: &Path, name: &str, patch: &Value) -> Result<(), String> {
    let patch = patch
        .as_object()
        .ok_or("MCP configuration patch must be an object")?;
    mutate(path, name, |servers| {
        let value = servers
            .get_mut(name)
            .ok_or_else(|| format!("Unknown MCP server: {name}"))?;
        let object = value
            .as_object_mut()
            .ok_or("Invalid MCP server configuration")?;
        object.extend(patch.clone());
        validate_mcp_server_config(name, value)?;
        Ok(())
    })
}

/// Writes exposure/enable overrides for a registered or inherited server without copying its transport.
pub fn set_mcp_server_overrides(
    path: &Path,
    name: &str,
    patch: &Value,
    base: &McpServerConfig,
) -> Result<(), String> {
    let patch = patch
        .as_object()
        .ok_or("MCP configuration patch must be an object")?;
    if patch.is_empty()
        || patch
            .keys()
            .any(|key| !["enabled", "exposure", "toolExposure"].contains(&key.as_str()))
    {
        return Err("MCP overrides can only set enabled, exposure or toolExposure".to_owned());
    }
    mutate(path, name, |servers| {
        let mut stored = servers.get(name).cloned().unwrap_or_else(|| json!({}));
        stored
            .as_object_mut()
            .ok_or("Invalid MCP server configuration")?
            .extend(patch.clone());
        let mut effective = serde_json::to_value(base).map_err(|error| error.to_string())?;
        effective
            .as_object_mut()
            .expect("typed MCP config object")
            .extend(stored.as_object().expect("checked object").clone());
        validate_mcp_server_config(name, &effective)?;
        servers.insert(name.to_owned(), stored);
        Ok(())
    })
}

pub fn remove_mcp_server_config(path: &Path, name: &str) -> Result<(), String> {
    mutate(path, name, |servers| {
        servers
            .remove(name)
            .ok_or_else(|| format!("Unknown MCP server: {name}"))?;
        Ok(())
    })
}

fn mutate(
    path: &Path,
    name: &str,
    update: impl FnOnce(&mut serde_json::Map<String, Value>) -> Result<(), String>,
) -> Result<(), String> {
    let _lock = DedicatedFileLockGuard::acquire(path)?;
    let mut root: Value = match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(strip_utf8_bom(&raw))
            .map_err(|_| "Invalid mcp.json; configuration was preserved")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error.to_string()),
    };
    let object = root
        .as_object_mut()
        .ok_or("mcp.json must contain an object")?;
    let servers = object
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("mcpServers must contain an object")?;
    update(servers)?;
    if servers.contains_key(name)
        && servers
            .keys()
            .any(|other| other != name && mcp_namespace(other) == mcp_namespace(name))
    {
        return Err(format!("MCP namespace collision: {name}"));
    }
    let mut bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    match atomic_write_file_sync(path, &bytes, 0o600)? {
        AtomicWriteOutcome::CommittedAndSynced => Ok(()),
        AtomicWriteOutcome::CommittedWithParentSyncWarning(warning) => Err(warning),
    }
}

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::auth_storage::strip_utf8_bom;
use crate::settings_manager::CONFIG_DIR_NAME;

pub const LIST_MCP_RESOURCES_TOOL: &str = "list_mcp_resources";
pub const LIST_MCP_RESOURCE_TEMPLATES_TOOL: &str = "list_mcp_resource_templates";
pub const READ_MCP_RESOURCE_TOOL: &str = "read_mcp_resource";
pub const MCP_RESOURCE_TOOLS: &[&str] = &[
    LIST_MCP_RESOURCES_TOOL,
    LIST_MCP_RESOURCE_TEMPLATES_TOOL,
    READ_MCP_RESOURCE_TOOL,
];

pub use pi_session::{
    McpExposure, McpHttpServerConfig, McpOAuthConfig, McpProviderAuthConfig, McpServerConfig,
    McpStdioServerConfig, get_mcp_tool_exposure, mcp_namespace, validate_mcp_server_config,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerEntry {
    pub name: String,
    pub config: McpServerConfig,
    pub source: String,
    pub scope: Option<String>,
    pub override_source: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedMcpConfig {
    pub servers: Vec<McpServerEntry>,
    pub auto_enable_codemode: Option<bool>,
    pub errors: Vec<String>,
    pub project_config: Option<PathBuf>,
}

const OVERRIDE_KEYS: &[&str] = &["enabled", "exposure", "toolExposure"];

fn is_override_entry(obj: &Map<String, Value>) -> bool {
    !obj.contains_key("command") && !obj.contains_key("url") && !obj.contains_key("type")
}

fn read_mcp_config_file(
    path: &Path,
    scope: &str,
    servers: &mut BTreeMap<String, McpServerEntry>,
    server_order: &mut Vec<String>,
    auto_enable_codemode: &mut Option<bool>,
    errors: &mut Vec<String>,
) {
    if !path.exists() {
        return;
    }
    let path_str = path.to_string_lossy().into_owned();
    let raw = match fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) => {
            errors.push(format!("{path_str}: {e}"));
            return;
        }
    };
    let parsed: Value = match serde_json::from_str(strip_utf8_bom(&raw)) {
        Ok(v) => v,
        Err(e) => {
            errors.push(format!("{path_str}: {e}"));
            return;
        }
    };
    let Some(root) = parsed.as_object() else {
        errors.push(format!(
            "{path_str}: expected an object with an \"mcpServers\" object"
        ));
        return;
    };
    if let Some(ms) = root.get("mcpServers")
        && !ms.is_object()
    {
        errors.push(format!(
            "{path_str}: expected an object with an \"mcpServers\" object"
        ));
        return;
    }
    if let Some(aec) = root.get("autoEnableCodemode") {
        if let Some(b) = aec.as_bool() {
            *auto_enable_codemode = Some(b);
        } else {
            errors.push(format!("{path_str}: autoEnableCodemode must be a boolean"));
        }
    }

    let Some(mcp_servers) = root.get("mcpServers").and_then(|v| v.as_object()) else {
        return;
    };

    for (name, val) in mcp_servers {
        if let Some(obj) = val.as_object()
            && is_override_entry(obj)
        {
            let extra: Vec<&str> = obj
                .keys()
                .map(String::as_str)
                .filter(|k| !OVERRIDE_KEYS.contains(k))
                .collect();
            let Some(base) = servers.get(name).cloned() else {
                errors.push(format!(
                    "{path_str}: server \"{name}\" needs \"command\" or \"url\", or a registered/global server to override"
                ));
                continue;
            };
            if !extra.is_empty() {
                errors.push(format!(
                    "{path_str}: server \"{name}\": an override can only set {}",
                    OVERRIDE_KEYS.join(", ")
                ));
                continue;
            }
            let mut merged_val = serde_json::to_value(&base.config).unwrap_or_default();
            if let Some(m_obj) = merged_val.as_object_mut() {
                for (k, v) in obj {
                    m_obj.insert(k.clone(), v.clone());
                }
            }
            match validate_mcp_server_config(name, &merged_val) {
                Ok(cfg) => {
                    servers.insert(
                        name.clone(),
                        McpServerEntry {
                            config: cfg,
                            override_source: Some(path_str.clone()),
                            ..base
                        },
                    );
                }
                Err(err) => errors.push(format!("{path_str}: {err}")),
            }
            continue;
        }

        let cfg = match validate_mcp_server_config(name, val) {
            Ok(c) => c,
            Err(err) => {
                errors.push(format!("{path_str}: {err}"));
                continue;
            }
        };

        let clash = server_order
            .iter()
            .find(|other| *other != name && mcp_namespace(other) == mcp_namespace(name))
            .cloned();
        if let Some(clash_name) = clash {
            errors.push(format!(
                "{path_str}: server \"{name}\" conflicts with \"{clash_name}\""
            ));
            continue;
        }

        if scope == "project"
            && let McpServerConfig::Http(http_cfg) = &cfg
            && http_cfg.auth.is_some()
        {
            errors.push(format!(
                "{path_str}: server \"{name}\": auth is only allowed in the global mcp.json"
            ));
            continue;
        }

        if !servers.contains_key(name) {
            server_order.push(name.clone());
        }
        servers.insert(
            name.clone(),
            McpServerEntry {
                name: name.clone(),
                config: cfg,
                source: path_str.clone(),
                scope: Some(scope.to_owned()),
                override_source: None,
            },
        );
    }
}

pub fn load_mcp_config(agent_dir: &Path, cwd: &Path, project_trusted: bool) -> LoadedMcpConfig {
    load_mcp_config_with_registered(agent_dir, cwd, project_trusted, &[])
}

pub fn load_mcp_config_with_registered(
    agent_dir: &Path,
    cwd: &Path,
    project_trusted: bool,
    registered: &[pi_session::RegisteredMcpServer],
) -> LoadedMcpConfig {
    let mut servers = BTreeMap::new();
    let mut server_order: Vec<String> = Vec::new();
    let mut auto_enable_codemode = None;
    let mut errors = Vec::new();
    for server in registered {
        if server.source_info.scope == "project" && !project_trusted {
            continue;
        }
        if server_order.iter().any(|other| {
            other != &server.name && mcp_namespace(other) == mcp_namespace(&server.name)
        }) {
            errors.push(format!("MCP namespace collision: {}", server.name));
            continue;
        }
        if !servers.contains_key(&server.name) {
            server_order.push(server.name.clone());
        }
        servers.insert(
            server.name.clone(),
            McpServerEntry {
                name: server.name.clone(),
                config: server.config.clone(),
                source: server.source_info.path.clone(),
                scope: Some(server.source_info.scope.clone()),
                override_source: None,
            },
        );
    }

    read_mcp_config_file(
        &agent_dir.join("mcp.json"),
        "global",
        &mut servers,
        &mut server_order,
        &mut auto_enable_codemode,
        &mut errors,
    );

    let project_config = if project_trusted {
        let p = cwd.join(CONFIG_DIR_NAME).join("mcp.json");
        read_mcp_config_file(
            &p,
            "project",
            &mut servers,
            &mut server_order,
            &mut auto_enable_codemode,
            &mut errors,
        );
        Some(p)
    } else {
        None
    };

    let ordered_servers = server_order
        .into_iter()
        .filter_map(|n| servers.remove(&n))
        .collect();

    LoadedMcpConfig {
        servers: ordered_servers,
        auto_enable_codemode,
        errors,
        project_config,
    }
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SourceInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredMcpServer {
    pub name: String,
    pub config: McpServerConfig,
    pub source_info: SourceInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpExposure {
    #[default]
    Codemode,
    Deferred,
    Direct,
    Hidden,
}

impl McpExposure {
    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "codemode" | "codemode-deferred" => Some(Self::Codemode),
            "deferred" => Some(Self::Deferred),
            "direct" => Some(Self::Direct),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codemode => "codemode",
            Self::Deferred => "deferred",
            Self::Direct => "direct",
            Self::Hidden => "hidden",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpProviderAuthConfig {
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStdioServerConfig {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<McpExposure>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_exposure: BTreeMap<String, McpExposure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_wait_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpHttpServerConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpProviderAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<McpExposure>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_exposure: BTreeMap<String, McpExposure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_wait_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio(McpStdioServerConfig),
    Http(McpHttpServerConfig),
}

impl McpServerConfig {
    pub fn enabled(&self) -> bool {
        match self {
            Self::Stdio(c) => c.enabled.unwrap_or(true),
            Self::Http(c) => c.enabled.unwrap_or(true),
        }
    }

    pub fn exposure(&self) -> McpExposure {
        match self {
            Self::Stdio(c) => c.exposure.unwrap_or(McpExposure::Codemode),
            Self::Http(c) => c.exposure.unwrap_or(McpExposure::Codemode),
        }
    }

    pub fn tool_exposure(&self) -> &BTreeMap<String, McpExposure> {
        match self {
            Self::Stdio(c) => &c.tool_exposure,
            Self::Http(c) => &c.tool_exposure,
        }
    }

    pub fn timeout_ms(&self) -> u64 {
        match self {
            Self::Stdio(c) => c.timeout_ms.unwrap_or(60_000),
            Self::Http(c) => c.timeout_ms.unwrap_or(60_000),
        }
    }

    pub fn startup_wait_ms(&self) -> u64 {
        match self {
            Self::Stdio(c) => c.startup_wait_ms.unwrap_or(10_000),
            Self::Http(c) => c.startup_wait_ms.unwrap_or(10_000),
        }
    }
}

pub fn mcp_namespace(server: &str) -> String {
    let sanitized: String = server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("mcp__{sanitized}")
}

fn glob_matches(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut pos = 0;
    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if idx == 0 {
            if !text.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if idx == parts.len() - 1 {
            if !text[pos..].ends_with(part) {
                return false;
            }
            pos = text.len();
        } else {
            let Some(found) = text[pos..].find(part) else {
                return false;
            };
            pos += found + part.len();
        }
    }
    true
}

pub fn get_mcp_tool_exposure(
    server_exposure: McpExposure,
    tool_exposure: &BTreeMap<String, McpExposure>,
    tool_name: &str,
) -> McpExposure {
    if let Some(&exact) = tool_exposure.get(tool_name) {
        return exact;
    }
    for (pattern, &exp) in tool_exposure {
        if pattern.contains('*') && glob_matches(pattern, tool_name) {
            return exp;
        }
    }
    server_exposure
}

pub fn validate_mcp_server_config(name: &str, value: &Value) -> Result<McpServerConfig, String> {
    if name.trim().is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid MCP server name \"{name}\": use letters, digits, \"_\", and \"-\""
        ));
    }
    let Some(obj) = value.as_object() else {
        return Err(format!("server \"{name}\" must be an object"));
    };

    let enabled = match obj.get("enabled") {
        None => None,
        Some(Value::Bool(b)) => Some(*b),
        _ => return Err(format!("server \"{name}\".enabled must be a boolean")),
    };
    let exposure = match obj.get("exposure") {
        None => None,
        Some(Value::String(s)) => Some(McpExposure::parse_str(s).ok_or_else(|| {
            format!(
                "server \"{name}\".exposure must be \"codemode\", \"deferred\", \"direct\", or \"hidden\""
            )
        })?),
        _ => {
            return Err(format!(
                "server \"{name}\".exposure must be \"codemode\", \"deferred\", \"direct\", or \"hidden\""
            ))
        }
    };
    let mut tool_exposure = BTreeMap::new();
    if let Some(te_val) = obj.get("toolExposure") {
        let Some(te_obj) = te_val.as_object() else {
            return Err(format!(
                "server \"{name}\".toolExposure must be an object mapping tool names to exposures"
            ));
        };
        for (tool, exp_val) in te_obj {
            if tool.is_empty() {
                return Err(format!(
                    "server \"{name}\".toolExposure cannot have an empty tool name"
                ));
            }
            let Some(exp_str) = exp_val.as_str() else {
                return Err(format!(
                    "server \"{name}\".toolExposure.{tool} must be a valid exposure string"
                ));
            };
            let exp = McpExposure::parse_str(exp_str).ok_or_else(|| {
                format!(
                    "server \"{name}\".toolExposure.{tool} must be \"codemode\", \"deferred\", \"direct\", or \"hidden\""
                )
            })?;
            tool_exposure.insert(tool.clone(), exp);
        }
    }

    let timeout_ms = match obj.get("timeoutMs") {
        None => None,
        Some(v) => {
            let Some(n) = v.as_f64() else {
                return Err(format!(
                    "server \"{name}\".timeoutMs must be a positive number"
                ));
            };
            if !n.is_finite() || n <= 0.0 {
                return Err(format!(
                    "server \"{name}\".timeoutMs must be a positive number"
                ));
            }
            Some(n as u64)
        }
    };

    let startup_wait_ms = match obj.get("startupWaitMs") {
        None => None,
        Some(v) => {
            let Some(n) = v.as_f64() else {
                return Err(format!(
                    "server \"{name}\".startupWaitMs must be a non-negative number"
                ));
            };
            if !n.is_finite() || n < 0.0 {
                return Err(format!(
                    "server \"{name}\".startupWaitMs must be a non-negative number"
                ));
            }
            Some(n as u64)
        }
    };

    let has_command = obj.contains_key("command");
    let has_url = obj.contains_key("url");
    if has_command && has_url {
        return Err(format!(
            "server \"{name}\" cannot have both \"command\" and \"url\""
        ));
    }

    if has_command {
        let Some(cmd) = obj.get("command").and_then(|v| v.as_str()) else {
            return Err(format!(
                "server \"{name}\".command must be a non-empty string"
            ));
        };
        if cmd.trim().is_empty() {
            return Err(format!(
                "server \"{name}\".command must be a non-empty string"
            ));
        }
        let mut args = Vec::new();
        if let Some(args_val) = obj.get("args") {
            let Some(arr) = args_val.as_array() else {
                return Err(format!(
                    "server \"{name}\".args must be an array of strings"
                ));
            };
            for item in arr {
                let Some(s) = item.as_str() else {
                    return Err(format!(
                        "server \"{name}\".args must be an array of strings"
                    ));
                };
                args.push(s.to_owned());
            }
        }
        let mut env = BTreeMap::new();
        if let Some(env_val) = obj.get("env") {
            let Some(env_obj) = env_val.as_object() else {
                return Err(format!(
                    "server \"{name}\".env must be an object of string values"
                ));
            };
            for (k, v) in env_obj {
                let Some(s) = v.as_str() else {
                    return Err(format!(
                        "server \"{name}\".env must be an object of string values"
                    ));
                };
                env.insert(k.clone(), s.to_owned());
            }
        }
        let cwd = match obj.get("cwd") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .ok_or_else(|| format!("server \"{name}\".cwd must be a string"))?
                    .to_owned(),
            ),
        };
        return Ok(McpServerConfig::Stdio(McpStdioServerConfig {
            command: cmd.to_owned(),
            args,
            env,
            cwd,
            enabled,
            exposure,
            tool_exposure,
            timeout_ms,
            startup_wait_ms,
        }));
    }

    if has_url {
        let Some(url) = obj.get("url").and_then(|v| v.as_str()) else {
            return Err(format!("server \"{name}\".url must be a valid URL"));
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!("server \"{name}\".url must be a valid URL"));
        }
        let mut headers = BTreeMap::new();
        if let Some(h_val) = obj.get("headers") {
            let Some(h_obj) = h_val.as_object() else {
                return Err(format!(
                    "server \"{name}\".headers must be an object of string values"
                ));
            };
            for (k, v) in h_obj {
                let Some(s) = v.as_str() else {
                    return Err(format!(
                        "server \"{name}\".headers must be an object of string values"
                    ));
                };
                headers.insert(k.clone(), s.to_owned());
            }
        }
        if obj.contains_key("auth") && obj.contains_key("oauth") {
            return Err(format!(
                "server \"{name}\" cannot have both \"auth\" and \"oauth\""
            ));
        }
        let auth = match obj.get("auth") {
            None => None,
            Some(a_val) => {
                let Some(provider) = a_val
                    .as_object()
                    .and_then(|o| o.get("provider"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                else {
                    return Err(format!(
                        "server \"{name}\".auth.provider must be a non-empty string"
                    ));
                };
                Some(McpProviderAuthConfig {
                    provider: provider.to_owned(),
                })
            }
        };
        let oauth = match obj.get("oauth") {
            None => None,
            Some(o_val) => Some(
                serde_json::from_value::<McpOAuthConfig>(o_val.clone())
                    .map_err(|e| format!("server \"{name}\".oauth is invalid: {e}"))?,
            ),
        };
        return Ok(McpServerConfig::Http(McpHttpServerConfig {
            url: url.to_owned(),
            headers,
            oauth,
            auth,
            enabled,
            exposure,
            tool_exposure,
            timeout_ms,
            startup_wait_ms,
        }));
    }

    Err(format!("server \"{name}\" needs \"command\" or \"url\""))
}

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use pi_agent::AgentToolResult;
use pi_ai::{JsonValue, UserContentBlock};
use pi_session::{
    Extension, ExtensionFactory, ExtensionRegistrar, InlineExtension, SourceInfo, ToolAnnotations,
    ToolDefinition, ToolExposure, ToolNamespace,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::mcp::{
    CallToolResult, LIST_MCP_RESOURCE_TEMPLATES_TOOL, LIST_MCP_RESOURCES_TOOL,
    ListResourceTemplatesResult, ListResourcesResult, McpClient, McpContentBlock, McpExposure,
    McpRequestOptions, McpResource, McpResourceTemplate, McpServerConfig, McpServerEntry, McpTool,
    McpTransport, ProgressNotification, READ_MCP_RESOURCE_TOOL, ReadResourceResult, StdioTransport,
    StdioTransportOptions, StreamableHttpTransport, get_mcp_tool_exposure, mcp_namespace,
    to_llm_content,
};
use crate::tools::truncate::{format_size, truncate_middle};

pub const MAX_TOOL_NAME_LENGTH: usize = 64;
pub const MCP_OUTPUT_MAX_BYTES: usize = 20 * 1024;

pub fn to_tool_exposure(exposure: McpExposure) -> ToolExposure {
    match exposure {
        McpExposure::Codemode => ToolExposure::Codemode,
        McpExposure::Deferred => ToolExposure::Deferred,
        McpExposure::Direct => ToolExposure::Direct,
        McpExposure::Hidden => ToolExposure::Hidden,
    }
}

pub fn create_mcp_tool_name(server: &str, tool: &str, is_taken: impl Fn(&str) -> bool) -> String {
    let raw = format!("mcp__{server}__{tool}");
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.len() <= MAX_TOOL_NAME_LENGTH && !is_taken(&sanitized) {
        return sanitized;
    }
    let mut hasher = Sha256::new();
    hasher.update(format!("{server}\0{tool}").as_bytes());
    let digest = hasher.finalize();
    let hash = format!("{digest:x}");
    let short_hash = &hash[..8];
    let prefix_len = MAX_TOOL_NAME_LENGTH.saturating_sub(short_hash.len() + 1);
    let prefix = &sanitized[..sanitized.len().min(prefix_len)];
    format!("{prefix}_{short_hash}")
}

const DATA_SCHEMA_KEYWORDS: &[&str] = &["const", "enum", "default", "examples"];
const MAP_SCHEMA_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "$defs",
    "definitions",
];

fn relocate_schema(schema: &JsonValue, pointer: &str) -> JsonValue {
    match schema {
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|v| relocate_schema(v, pointer)).collect())
        }
        Value::Object(obj) => {
            let mut out = Map::new();
            for (k, v) in obj {
                if k == "$ref"
                    && let Some(s) = v.as_str()
                    && (s == "#" || s.starts_with("#/"))
                {
                    out.insert(k.clone(), Value::String(format!("{pointer}{}", &s[1..])));
                } else if DATA_SCHEMA_KEYWORDS.contains(&k.as_str()) {
                    out.insert(k.clone(), v.clone());
                } else if MAP_SCHEMA_KEYWORDS.contains(&k.as_str())
                    && let Some(sub_obj) = v.as_object()
                {
                    let mut mapped = Map::new();
                    for (sub_k, sub_v) in sub_obj {
                        mapped.insert(sub_k.clone(), relocate_schema(sub_v, pointer));
                    }
                    out.insert(k.clone(), Value::Object(mapped));
                } else {
                    out.insert(k.clone(), relocate_schema(v, pointer));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

pub fn create_mcp_result_schema(structured_content_schema: Option<&JsonValue>) -> JsonValue {
    let content_block_schema = json!({
        "type": "object",
        "properties": {
            "type": { "enum": ["text", "image", "audio", "resource_link", "resource"] },
            "text": { "type": "string" },
            "data": { "type": "string" },
            "mimeType": { "type": "string" }
        },
        "required": ["type"],
        "additionalProperties": true
    });
    let structured_schema = match structured_content_schema {
        Some(s) => relocate_schema(s, "#/properties/structuredContent"),
        None => json!({}),
    };
    json!({
        "type": "object",
        "properties": {
            "content": { "type": "array", "items": content_block_schema },
            "structuredContent": structured_schema,
            "isError": { "type": "boolean" }
        },
        "required": ["content"],
        "additionalProperties": true
    })
}

pub fn save_mcp_temp_file(data: &[u8], extension: &str) -> Result<PathBuf, String> {
    let ext = if extension.starts_with('.') {
        extension.to_owned()
    } else {
        format!(".{extension}")
    };
    let path = std::env::temp_dir().join(format!("pi-mcp-{}{ext}", Uuid::new_v4()));
    std::fs::write(&path, data).map_err(|e| e.to_string())?;
    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDetails {
    pub server: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

pub fn limit_mcp_content(
    content: Vec<UserContentBlock>,
) -> (Vec<UserContentBlock>, Option<String>) {
    let combined = content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let trunc = truncate_middle(&combined, MCP_OUTPUT_MAX_BYTES);
    if !trunc.truncated {
        return (content, None);
    }

    let (full_output_path, where_note) = match save_mcp_temp_file(combined.as_bytes(), ".txt") {
        Ok(p) => {
            let s = p.to_string_lossy().into_owned();
            let note = format!("[Full output: {s} (read it with offset/limit)]");
            (Some(s), note)
        }
        Err(err) => (None, format!("[Could not save the full output: {err}]")),
    };
    let tokens = trunc.total_bytes.div_ceil(4);
    let text = format!(
        "Warning: truncated output (original token count: {tokens})\nTotal output lines: {}\n\n{}\n\n{where_note}",
        trunc.total_lines, trunc.content
    );
    let mut out = vec![UserContentBlock::text(text)];
    out.extend(
        content
            .into_iter()
            .filter(|b| matches!(b, UserContentBlock::Image(_))),
    );
    (out, full_output_path)
}

fn is_text_mime_type(mime_type: Option<&str>) -> bool {
    let Some(mime) = mime_type else {
        return false;
    };
    let base = mime.split(';').next().unwrap_or("").trim().to_lowercase();
    base.starts_with("text/")
        || base == "application/json"
        || base.ends_with("+json")
        || base.ends_with("+xml")
}

fn extension_of_uri(uri: &str) -> String {
    let path_part = uri.split('?').next().unwrap_or(uri);
    if let Some(dot) = path_part.rfind('.') {
        let ext = &path_part[dot..];
        if ext.len() >= 2 && ext.len() <= 9 && ext[1..].chars().all(|c| c.is_ascii_alphanumeric()) {
            return ext.to_owned();
        }
    }
    ".bin".to_owned()
}

pub fn to_model_content(
    server: &str,
    blocks: &[McpContentBlock],
    readable_resources: bool,
) -> Vec<UserContentBlock> {
    let mut out = Vec::new();
    for block in blocks {
        match block {
            McpContentBlock::ResourceLink {
                uri,
                name,
                title,
                description,
                mime_type,
                size,
            } => {
                let mut details = Vec::new();
                if let Some(m) = mime_type {
                    details.push(m.clone());
                }
                if let Some(sz) = size {
                    details.push(format_size(*sz));
                }
                let read_hint = if readable_resources {
                    format!(". Read it with {READ_MCP_RESOURCE_TOOL} (server \"{server}\")")
                } else {
                    String::new()
                };
                let desc = description
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                let details_str = if details.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", details.join(", "))
                };
                let label = title.as_deref().unwrap_or(name);
                out.push(UserContentBlock::text(format!(
                    "[Resource {uri} \"{label}\"{details_str}{desc}{read_hint}]"
                )));
            }
            McpContentBlock::Resource { resource } => {
                if let Some(blob) = &resource.blob
                    && !resource
                        .mime_type
                        .as_deref()
                        .is_some_and(|m| m.starts_with("image/"))
                {
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(blob)
                        .unwrap_or_default();
                    if is_text_mime_type(resource.mime_type.as_deref()) {
                        out.push(UserContentBlock::text(
                            String::from_utf8_lossy(&data).into_owned(),
                        ));
                    } else {
                        let kind = format!(
                            "{}, {}",
                            resource.mime_type.as_deref().unwrap_or("unknown type"),
                            format_size(data.len())
                        );
                        match save_mcp_temp_file(&data, &extension_of_uri(&resource.uri)) {
                            Ok(path) => out.push(UserContentBlock::text(format!(
                                "[Binary resource {} ({kind}) saved to {}]",
                                resource.uri,
                                path.display()
                            ))),
                            Err(err) => out.push(UserContentBlock::text(format!(
                                "[Binary resource {} ({kind}) could not be saved: {err}]",
                                resource.uri
                            ))),
                        }
                    }
                    continue;
                }
                out.extend(to_llm_content(&CallToolResult {
                    content: vec![block.clone()],
                    structured_content: None,
                    is_error: None,
                    meta: None,
                }));
            }
            other => {
                out.extend(to_llm_content(&CallToolResult {
                    content: vec![other.clone()],
                    structured_content: None,
                    is_error: None,
                    meta: None,
                }));
            }
        }
    }
    out
}

pub fn convert_mcp_result(
    server: &str,
    tool: &str,
    result: &CallToolResult,
    readable_resources: bool,
) -> AgentToolResult {
    let mut converted = if !result.content.is_empty() {
        to_model_content(server, &result.content, readable_resources)
    } else {
        to_llm_content(result)
    };
    let is_error = result.is_error.unwrap_or(false);
    let has_text = converted.iter().any(|b| match b {
        UserContentBlock::Text(t) => !t.text.is_empty(),
        UserContentBlock::Image(_) => false,
    });
    if is_error && !has_text {
        converted.push(UserContentBlock::text(format!(
            "MCP tool {server}/{tool} returned an error"
        )));
    }
    let (content, full_output_path) = limit_mcp_content(converted);

    let mut script_result =
        serde_json::to_value(result).unwrap_or_else(|_| json!({ "content": [] }));
    if let Some(obj) = script_result.as_object_mut() {
        obj.remove("_meta");
    }

    AgentToolResult {
        content,
        details: serde_json::to_value(McpToolDetails {
            server: server.to_owned(),
            tool: tool.to_owned(),
            full_output_path,
        })
        .unwrap_or_default(),
        structured_content: Some(script_result),
        usage: None,
        is_error,
        terminate: None,
    }
}

fn normalize_input_schema(schema: &JsonValue) -> JsonValue {
    let mut obj = schema.as_object().cloned().unwrap_or_default();
    if !obj.contains_key("type") {
        obj.insert("type".to_owned(), Value::String("object".to_owned()));
    }
    if !obj.contains_key("properties") {
        obj.insert("properties".to_owned(), Value::Object(Map::new()));
    }
    Value::Object(obj)
}

fn to_tool_annotations(tool: &McpTool) -> Option<ToolAnnotations> {
    let ann = tool.annotations.as_ref()?;
    if ann.read_only_hint.is_none()
        && ann.destructive_hint.is_none()
        && ann.idempotent_hint.is_none()
        && ann.open_world_hint.is_none()
    {
        return None;
    }
    Some(ToolAnnotations {
        read_only_hint: ann.read_only_hint,
        destructive_hint: ann.destructive_hint,
        idempotent_hint: ann.idempotent_hint,
        open_world_hint: ann.open_world_hint,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn create_mcp_tool_definition(
    server: String,
    tool: McpTool,
    name: String,
    exposure: McpExposure,
    namespace: ToolNamespace,
    timeout_ms: u64,
    client: Arc<McpClient>,
    readable_resources: bool,
) -> ToolDefinition {
    let title = tool
        .title
        .clone()
        .or_else(|| tool.annotations.as_ref().and_then(|a| a.title.clone()));
    let description = tool
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or(title)
        .unwrap_or_else(|| format!("MCP tool {} from server {server}", tool.name));
    let label = format!("{server}/{}", tool.name);
    let parameters = normalize_input_schema(&tool.input_schema);
    let output_schema = create_mcp_result_schema(tool.output_schema.as_ref());
    let annotations = to_tool_annotations(&tool);

    let tool_name = tool.name.clone();
    let mut def = ToolDefinition::new(
        name,
        label,
        description,
        parameters,
        Arc::new(
            move |_tool_call_id, params, cancel_token, on_update, _ctx| {
                let client = client.clone();
                let server = server.clone();
                let tool_name = tool_name.clone();
                Box::pin(async move {
                    let progress_cb = on_update.map(|update_fn| {
                        let s = server.clone();
                        let t = tool_name.clone();
                        Arc::new(move |prog: ProgressNotification| {
                            let total_str =
                                prog.total.map(|tot| format!("/{tot}")).unwrap_or_default();
                            let text = prog.message.unwrap_or_else(|| {
                                format!("Progress {}{total_str}", prog.progress)
                            });
                            update_fn(AgentToolResult {
                                content: vec![UserContentBlock::text(text)],
                                details: json!({ "server": s, "tool": t }),
                                structured_content: None,
                                usage: None,
                                is_error: false,
                                terminate: None,
                            });
                        }) as crate::mcp::ProgressCallback
                    });

                    let result = client
                        .call_tool(
                            &tool_name,
                            params,
                            McpRequestOptions {
                                cancel_token,
                                timeout_ms: Some(timeout_ms),
                                on_progress: progress_cb,
                            },
                        )
                        .await?;
                    Ok(convert_mcp_result(
                        &server,
                        &tool_name,
                        &result,
                        readable_resources,
                    ))
                })
            },
        ),
    )
    .with_output_schema(output_schema)
    .with_exposure(to_tool_exposure(exposure))
    .with_namespace(namespace);

    if let Some(ann) = annotations {
        def = def.with_annotations(ann);
    }
    def
}

#[derive(Clone)]
pub struct ConnectedMcpResourceServer {
    pub name: String,
    pub timeout_ms: u64,
    pub client: Arc<McpClient>,
}

fn is_mcp_app_resource(uri: &str, mime_type: Option<&str>) -> bool {
    if uri.starts_with("ui://") {
        return true;
    }
    if let Some(mime) = mime_type {
        let lower = mime.to_lowercase();
        if lower.contains("profile=mcp-app") || lower.contains("profile=\"mcp-app\"") {
            return true;
        }
    }
    false
}

fn listed_resource(server: &str, res: &McpResource) -> JsonValue {
    let mut val = serde_json::to_value(res).unwrap_or_else(|_| json!({}));
    if let Some(obj) = val.as_object_mut() {
        obj.remove("_meta");
        obj.remove("icons");
        obj.insert("server".to_owned(), Value::String(server.to_owned()));
    }
    val
}

fn listed_template(server: &str, tmpl: &McpResourceTemplate) -> JsonValue {
    let mut val = serde_json::to_value(tmpl).unwrap_or_else(|_| json!({}));
    if let Some(obj) = val.as_object_mut() {
        obj.remove("_meta");
        obj.remove("icons");
        obj.insert("server".to_owned(), Value::String(server.to_owned()));
    }
    val
}

pub fn create_mcp_resource_tool_definitions(
    exposure: McpExposure,
    servers: Arc<dyn Fn() -> Vec<ConnectedMcpResourceServer> + Send + Sync>,
) -> Vec<ToolDefinition> {
    let read_only = ToolAnnotations {
        read_only_hint: Some(true),
        ..Default::default()
    };
    let list_params = json!({
        "type": "object",
        "properties": {
            "server": {
                "type": "string",
                "description": "MCP server name. Omit to list every server with resources."
            },
            "cursor": {
                "type": "string",
                "description": "Opaque cursor from a previous call with the same server; omit for the first page."
            }
        },
        "additionalProperties": false
    });
    let read_params = json!({
        "type": "object",
        "properties": {
            "server": {
                "type": "string",
                "description": "MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources."
            },
            "uri": {
                "type": "string",
                "description": "Resource URI to read. Must be one of the URIs returned by list_mcp_resources."
            }
        },
        "required": ["server", "uri"],
        "additionalProperties": false
    });

    let servers_for_list = servers.clone();
    let list_resources = ToolDefinition::new(
        LIST_MCP_RESOURCES_TOOL,
        LIST_MCP_RESOURCES_TOOL,
        "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.",
        list_params.clone(),
        Arc::new(move |_id, params, cancel_token, _on_update, _ctx| {
            let servers_fn = servers_for_list.clone();
            Box::pin(async move {
                let server_arg = params
                    .get("server")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                let cursor_arg = params
                    .get("cursor")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);

                let all_servers = servers_fn();
                let payload = if let Some(ref srv_name) = server_arg {
                    let Some(srv) = all_servers.iter().find(|s| &s.name == srv_name) else {
                        let avail = all_servers
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let suffix = if avail.is_empty() {
                            String::new()
                        } else {
                            format!(". Servers with resources: {avail}")
                        };
                        return Err(format!("MCP server \"{srv_name}\" has no resources{suffix}"));
                    };
                    let page: ListResourcesResult = srv
                        .client
                        .list_resources_page(
                            cursor_arg.as_deref(),
                            McpRequestOptions {
                                cancel_token,
                                timeout_ms: Some(srv.timeout_ms),
                                on_progress: None,
                            },
                        )
                        .await?;
                    let items: Vec<JsonValue> = page
                        .resources
                        .iter()
                        .filter(|r| !is_mcp_app_resource(&r.uri, r.mime_type.as_deref()))
                        .map(|r| listed_resource(&srv.name, r))
                        .collect();
                    let mut obj = Map::new();
                    obj.insert("server".to_owned(), Value::String(srv.name.clone()));
                    obj.insert("resources".to_owned(), Value::Array(items));
                    if let Some(nc) = page.next_cursor {
                        obj.insert("nextCursor".to_owned(), Value::String(nc));
                    }
                    Value::Object(obj)
                } else {
                    if cursor_arg.is_some() {
                        return Err("cursor can only be used when a server is specified".to_owned());
                    }
                    let mut sorted = all_servers;
                    sorted.sort_by(|a, b| a.name.cmp(&b.name));
                    let mut items = Vec::new();
                    let mut errors = Vec::new();
                    for srv in sorted {
                        match srv
                            .client
                            .list_resources(McpRequestOptions {
                                cancel_token: cancel_token.clone(),
                                timeout_ms: Some(srv.timeout_ms),
                                on_progress: None,
                            })
                            .await
                        {
                            Ok(list) => {
                                items.extend(
                                    list.iter()
                                        .filter(|r| !is_mcp_app_resource(&r.uri, r.mime_type.as_deref()))
                                        .map(|r| listed_resource(&srv.name, r)),
                                );
                            }
                            Err(err) => {
                                errors.push(json!({ "server": srv.name, "error": err }));
                            }
                        }
                    }
                    let mut obj = Map::new();
                    obj.insert("resources".to_owned(), Value::Array(items));
                    if !errors.is_empty() {
                        obj.insert("errors".to_owned(), Value::Array(errors));
                    }
                    Value::Object(obj)
                };

                let text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
                let (content, full_output_path) = limit_mcp_content(vec![UserContentBlock::text(text)]);
                Ok(AgentToolResult {
                    content,
                    details: serde_json::to_value(McpToolDetails {
                        server: server_arg.unwrap_or_default(),
                        tool: LIST_MCP_RESOURCES_TOOL.to_owned(),
                        full_output_path,
                    })
                    .unwrap_or_default(),
                    structured_content: Some(payload),
                    usage: None,
                    is_error: false,
                    terminate: None,
                })
            })
        }),
    )
    .with_output_schema(json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "resources": { "type": "array" },
            "nextCursor": { "type": "string" },
            "errors": { "type": "array" }
        },
        "required": ["resources"]
    }))
    .with_exposure(to_tool_exposure(exposure))
    .with_annotations(read_only.clone());

    let servers_for_templates = servers.clone();
    let list_templates = ToolDefinition::new(
        LIST_MCP_RESOURCE_TEMPLATES_TOOL,
        LIST_MCP_RESOURCE_TEMPLATES_TOOL,
        "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.",
        list_params,
        Arc::new(move |_id, params, cancel_token, _on_update, _ctx| {
            let servers_fn = servers_for_templates.clone();
            Box::pin(async move {
                let server_arg = params
                    .get("server")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                let cursor_arg = params
                    .get("cursor")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);

                let all_servers = servers_fn();
                let payload = if let Some(ref srv_name) = server_arg {
                    let Some(srv) = all_servers.iter().find(|s| &s.name == srv_name) else {
                        let avail = all_servers
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let suffix = if avail.is_empty() {
                            String::new()
                        } else {
                            format!(". Servers with resources: {avail}")
                        };
                        return Err(format!("MCP server \"{srv_name}\" has no resources{suffix}"));
                    };
                    let page: ListResourceTemplatesResult = srv
                        .client
                        .list_resource_templates_page(
                            cursor_arg.as_deref(),
                            McpRequestOptions {
                                cancel_token,
                                timeout_ms: Some(srv.timeout_ms),
                                on_progress: None,
                            },
                        )
                        .await?;
                    let items: Vec<JsonValue> = page
                        .resource_templates
                        .iter()
                        .filter(|t| !is_mcp_app_resource(&t.uri_template, t.mime_type.as_deref()))
                        .map(|t| listed_template(&srv.name, t))
                        .collect();
                    let mut obj = Map::new();
                    obj.insert("server".to_owned(), Value::String(srv.name.clone()));
                    obj.insert("resourceTemplates".to_owned(), Value::Array(items));
                    if let Some(nc) = page.next_cursor {
                        obj.insert("nextCursor".to_owned(), Value::String(nc));
                    }
                    Value::Object(obj)
                } else {
                    if cursor_arg.is_some() {
                        return Err("cursor can only be used when a server is specified".to_owned());
                    }
                    let mut sorted = all_servers;
                    sorted.sort_by(|a, b| a.name.cmp(&b.name));
                    let mut items = Vec::new();
                    let mut errors = Vec::new();
                    for srv in sorted {
                        match srv
                            .client
                            .list_resource_templates(McpRequestOptions {
                                cancel_token: cancel_token.clone(),
                                timeout_ms: Some(srv.timeout_ms),
                                on_progress: None,
                            })
                            .await
                        {
                            Ok(list) => {
                                items.extend(
                                    list.iter()
                                        .filter(|t| !is_mcp_app_resource(&t.uri_template, t.mime_type.as_deref()))
                                        .map(|t| listed_template(&srv.name, t)),
                                );
                            }
                            Err(err) => {
                                errors.push(json!({ "server": srv.name, "error": err }));
                            }
                        }
                    }
                    let mut obj = Map::new();
                    obj.insert("resourceTemplates".to_owned(), Value::Array(items));
                    if !errors.is_empty() {
                        obj.insert("errors".to_owned(), Value::Array(errors));
                    }
                    Value::Object(obj)
                };

                let text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
                let (content, full_output_path) = limit_mcp_content(vec![UserContentBlock::text(text)]);
                Ok(AgentToolResult {
                    content,
                    details: serde_json::to_value(McpToolDetails {
                        server: server_arg.unwrap_or_default(),
                        tool: LIST_MCP_RESOURCE_TEMPLATES_TOOL.to_owned(),
                        full_output_path,
                    })
                    .unwrap_or_default(),
                    structured_content: Some(payload),
                    usage: None,
                    is_error: false,
                    terminate: None,
                })
            })
        }),
    )
    .with_output_schema(json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "resourceTemplates": { "type": "array" },
            "nextCursor": { "type": "string" },
            "errors": { "type": "array" }
        },
        "required": ["resourceTemplates"]
    }))
    .with_exposure(to_tool_exposure(exposure))
    .with_annotations(read_only.clone());

    let servers_for_read = servers;
    let read_resource = ToolDefinition::new(
        READ_MCP_RESOURCE_TOOL,
        READ_MCP_RESOURCE_TOOL,
        "Read a specific resource from an MCP server given the server name and resource URI.",
        read_params,
        Arc::new(move |_id, params, cancel_token, _on_update, _ctx| {
            let servers_fn = servers_for_read.clone();
            Box::pin(async move {
                let server_name = params
                    .get("server")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "server must be provided".to_owned())?
                    .to_owned();
                let uri = params
                    .get("uri")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "uri must be provided".to_owned())?
                    .to_owned();

                let all_servers = servers_fn();
                let Some(srv) = all_servers.iter().find(|s| s.name == server_name) else {
                    let avail = all_servers
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let suffix = if avail.is_empty() {
                        String::new()
                    } else {
                        format!(". Servers with resources: {avail}")
                    };
                    return Err(format!(
                        "MCP server \"{server_name}\" has no resources{suffix}"
                    ));
                };

                let res: ReadResourceResult = srv
                    .client
                    .read_resource(
                        &uri,
                        McpRequestOptions {
                            cancel_token,
                            timeout_ms: Some(srv.timeout_ms),
                            on_progress: None,
                        },
                    )
                    .await?;

                let mut blocks = Vec::new();
                let multi = res.contents.len() > 1;
                for c in &res.contents {
                    if multi {
                        blocks.push(McpContentBlock::Text {
                            text: format!("{}:", c.uri),
                        });
                    }
                    blocks.push(McpContentBlock::Resource {
                        resource: c.clone(),
                    });
                }
                let converted = to_model_content(&srv.name, &blocks, true);
                let fallback = if converted.is_empty() {
                    vec![UserContentBlock::text(format!("Resource {uri} is empty."))]
                } else {
                    converted
                };
                let (content, full_output_path) = limit_mcp_content(fallback);
                let contents_json: Vec<JsonValue> = res
                    .contents
                    .iter()
                    .map(|c| {
                        let mut v = serde_json::to_value(c).unwrap_or_default();
                        if let Some(o) = v.as_object_mut() {
                            o.remove("_meta");
                        }
                        v
                    })
                    .collect();

                Ok(AgentToolResult {
                    content,
                    details: serde_json::to_value(McpToolDetails {
                        server: srv.name.clone(),
                        tool: READ_MCP_RESOURCE_TOOL.to_owned(),
                        full_output_path,
                    })
                    .unwrap_or_default(),
                    structured_content: Some(json!({
                        "server": srv.name,
                        "uri": uri,
                        "contents": contents_json,
                    })),
                    usage: None,
                    is_error: false,
                    terminate: None,
                })
            })
        }),
    )
    .with_output_schema(json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "uri": { "type": "string" },
            "contents": { "type": "array" }
        },
        "required": ["server", "uri", "contents"]
    }))
    .with_exposure(to_tool_exposure(exposure))
    .with_annotations(read_only);

    vec![list_resources, list_templates, read_resource]
}

#[derive(Clone)]
pub struct PreconnectedMcpServer {
    pub name: String,
    pub exposure: McpExposure,
    pub tool_exposure: BTreeMap<String, McpExposure>,
    pub timeout_ms: u64,
    pub client: Arc<McpClient>,
    pub tools: Vec<McpTool>,
    pub has_resources: bool,
}

#[derive(Clone, Default)]
pub struct McpExtensionOptions {
    pub agent_dir: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub project_trusted: bool,
    pub preconnected_servers: Vec<PreconnectedMcpServer>,
    pub auto_enable_codemode: Option<bool>,
    pub on_oauth_redirect: Option<crate::mcp::McpOAuthRedirectFn>,
}

pub fn create_default_transport(
    entry: &McpServerEntry,
    cwd: &Path,
    agent_dir: &Path,
) -> Arc<dyn McpTransport> {
    match &entry.config {
        McpServerConfig::Stdio(stdio) => {
            let server_cwd = stdio
                .cwd
                .as_deref()
                .map(|p| {
                    let pb = PathBuf::from(p);
                    if pb.is_absolute() { pb } else { cwd.join(pb) }
                })
                .unwrap_or_else(|| cwd.to_path_buf());
            Arc::new(StdioTransport::new(StdioTransportOptions {
                command: stdio.command.clone(),
                args: stdio.args.clone(),
                env: stdio.env.clone(),
                cwd: Some(server_cwd),
                stderr_log_path: Some(agent_dir.join("mcp.log")),
            }))
        }
        McpServerConfig::Http(http) => Arc::new(StreamableHttpTransport::new(
            http.url.clone(),
            http.headers.clone(),
        )),
    }
}

pub(super) fn mcp_tool_definitions(
    servers: &[PreconnectedMcpServer],
) -> (Vec<ToolDefinition>, bool, bool) {
    let mut tools = Vec::new();
    let mut taken_names = BTreeSet::new();
    let mut resources = Vec::new();
    let mut resource_exposure = McpExposure::Hidden;
    let mut needs_codemode = false;
    let mut needs_tool_search = false;
    for server in servers {
        let namespace = ToolNamespace {
            name: mcp_namespace(&server.name),
            aliases: vec![server.name.clone()],
            description: server.client.server_info().and_then(|info| info.title),
            instructions: server.client.instructions(),
        };
        for tool in &server.tools {
            let exposure =
                get_mcp_tool_exposure(server.exposure, &server.tool_exposure, &tool.name);
            needs_codemode |= exposure == McpExposure::Codemode;
            needs_tool_search |= exposure == McpExposure::Deferred;
            let name =
                create_mcp_tool_name(&server.name, &tool.name, |name| taken_names.contains(name));
            taken_names.insert(name.clone());
            tools.push(create_mcp_tool_definition(
                server.name.clone(),
                tool.clone(),
                name,
                exposure,
                namespace.clone(),
                server.timeout_ms,
                server.client.clone(),
                server.has_resources,
            ));
        }
        if server.has_resources && server.exposure != McpExposure::Hidden {
            resource_exposure = match (resource_exposure, server.exposure) {
                (McpExposure::Direct, _) | (_, McpExposure::Direct) => McpExposure::Direct,
                (McpExposure::Deferred, _) | (_, McpExposure::Deferred) => McpExposure::Deferred,
                _ => McpExposure::Codemode,
            };
            needs_codemode |= server.exposure == McpExposure::Codemode;
            needs_tool_search |= server.exposure == McpExposure::Deferred;
            resources.push(ConnectedMcpResourceServer {
                name: server.name.clone(),
                timeout_ms: server.timeout_ms,
                client: server.client.clone(),
            });
        }
    }
    if !resources.is_empty() {
        tools.extend(create_mcp_resource_tool_definitions(
            resource_exposure,
            Arc::new(move || resources.clone()),
        ));
    }
    (tools, needs_codemode, needs_tool_search)
}

pub fn create_mcp_extension(options: McpExtensionOptions) -> ExtensionFactory {
    Arc::new(move || {
        let options = options.clone();
        Ok(Box::new(
            InlineExtension::new("builtin:mcp", move |registrar: &mut ExtensionRegistrar| {
                super::mcp_runtime::McpRuntime::install(registrar, options.clone());
                Ok(())
            })
            .with_source_info(SourceInfo::builtin("builtin:mcp"))
            .with_replaceable(true),
        ) as Box<dyn Extension>)
    })
}

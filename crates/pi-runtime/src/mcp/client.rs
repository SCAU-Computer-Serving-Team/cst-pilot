use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::JsonValue;
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use crate::mcp::protocol::{
    CallToolResult, ClientCapabilities, Implementation, InitializeResult, JsonRpcId,
    LATEST_PROTOCOL_VERSION, ListResourceTemplatesResult, ListResourcesResult, McpError,
    McpResource, McpResourceTemplate, McpTool, ProgressNotification, ReadResourceResult,
    SUPPORTED_PROTOCOL_VERSIONS, ServerCapabilities,
};
use crate::mcp::transport::McpTransport;

pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_LIST_PAGES: usize = 1_000;

#[derive(Debug, Clone)]
pub struct McpClientOptions {
    pub name: String,
    pub version: String,
    pub title: Option<String>,
    pub capabilities: ClientCapabilities,
    pub protocol_version: Option<String>,
    pub request_timeout_ms: u64,
}

impl Default for McpClientOptions {
    fn default() -> Self {
        Self {
            name: "pi".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            title: None,
            capabilities: ClientCapabilities::default(),
            protocol_version: None,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        }
    }
}

pub type ProgressCallback = Arc<dyn Fn(ProgressNotification) + Send + Sync>;

#[derive(Clone, Default)]
pub struct McpRequestOptions {
    pub cancel_token: Option<CancellationToken>,
    pub timeout_ms: Option<u64>,
    pub on_progress: Option<ProgressCallback>,
}

struct PendingEntry {
    tx: oneshot::Sender<Result<JsonValue, String>>,
    progress_reset: Arc<Notify>,
    on_progress: Option<ProgressCallback>,
}

struct PendingRequestGuard {
    id: i64,
    pending: Arc<Mutex<BTreeMap<i64, PendingEntry>>>,
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        self.pending.lock().expect("pending lock").remove(&self.id);
    }
}

struct ClientLifetime(CancellationToken);

impl Drop for ClientLifetime {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone)]
pub struct McpClient {
    options: McpClientOptions,
    transport: Arc<Mutex<Option<Arc<dyn McpTransport>>>>,
    next_id: Arc<AtomicI64>,
    pending: Arc<Mutex<BTreeMap<i64, PendingEntry>>>,
    server_info: Arc<Mutex<Option<Implementation>>>,
    server_capabilities: Arc<Mutex<Option<ServerCapabilities>>>,
    instructions: Arc<Mutex<Option<String>>>,
    protocol_version: Arc<Mutex<Option<String>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    receive_cancel: CancellationToken,
    _lifetime: Arc<ClientLifetime>,
    notifications: tokio::sync::broadcast::Sender<(String, JsonValue)>,
}

impl McpClient {
    pub fn new(options: McpClientOptions) -> Self {
        let receive_cancel = CancellationToken::new();
        Self {
            options,
            transport: Arc::new(Mutex::new(None)),
            next_id: Arc::new(AtomicI64::new(1)),
            pending: Arc::new(Mutex::new(BTreeMap::new())),
            server_info: Arc::new(Mutex::new(None)),
            server_capabilities: Arc::new(Mutex::new(None)),
            instructions: Arc::new(Mutex::new(None)),
            protocol_version: Arc::new(Mutex::new(None)),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            _lifetime: Arc::new(ClientLifetime(receive_cancel.clone())),
            receive_cancel,
            notifications: tokio::sync::broadcast::channel(64).0,
        }
    }

    pub fn subscribe_notifications(&self) -> tokio::sync::broadcast::Receiver<(String, JsonValue)> {
        self.notifications.subscribe()
    }

    pub fn server_info(&self) -> Option<Implementation> {
        self.server_info.lock().expect("info lock").clone()
    }

    pub fn server_capabilities(&self) -> Option<ServerCapabilities> {
        self.server_capabilities.lock().expect("caps lock").clone()
    }

    pub fn instructions(&self) -> Option<String> {
        self.instructions.lock().expect("inst lock").clone()
    }

    pub fn protocol_version(&self) -> Option<String> {
        self.protocol_version.lock().expect("ver lock").clone()
    }

    pub async fn connect(
        &self,
        transport: Arc<dyn McpTransport>,
    ) -> Result<InitializeResult, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("Cannot connect MCP client in closed state".to_owned());
        }
        let mut rx = transport.start().await?;
        *self.transport.lock().expect("transport lock") = Some(transport.clone());

        let pending_map = self.pending.clone();
        let notifications = self.notifications.clone();
        let (info, capabilities, instructions, version) = (
            self.server_info.clone(),
            self.server_capabilities.clone(),
            self.instructions.clone(),
            self.protocol_version.clone(),
        );
        transport.on_session_reinitialized(Arc::new(move |initialized| {
            *info.lock().expect("info lock") = Some(initialized.server_info);
            *capabilities.lock().expect("caps lock") = Some(initialized.capabilities);
            *instructions.lock().expect("inst lock") = initialized.instructions;
            *version.lock().expect("ver lock") = Some(initialized.protocol_version);
        }));
        let transport_for_loop = transport.clone();
        let closed_for_loop = self.closed.clone();
        let receive_cancel = self.receive_cancel.clone();

        tokio::spawn(async move {
            loop {
                let msg = tokio::select! {
                    biased;
                    () = receive_cancel.cancelled() => break,
                    msg = rx.recv() => match msg {
                        Some(msg) => msg,
                        None => break,
                    },
                };
                let Some(obj) = msg.as_object() else {
                    continue;
                };
                if let Some(method) = obj.get("method").and_then(|v| v.as_str()) {
                    if obj.get("id").is_none() {
                        let _ = notifications.send((
                            method.to_owned(),
                            obj.get("params").cloned().unwrap_or(Value::Null),
                        ));
                    }
                    if let Some(req_id) = obj.get("id") {
                        // Server-to-client request
                        let reply = match method {
                            "ping" => json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {},
                            }),
                            "roots/list" => json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": { "roots": [] },
                            }),
                            _ => json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "error": {
                                    "code": -32601,
                                    "message": format!("Method not found: {method}"),
                                },
                            }),
                        };
                        let t = transport_for_loop.clone();
                        tokio::spawn(async move {
                            let _ = t.send(reply).await;
                        });
                    } else if method == "notifications/progress"
                        && let Some(params) = obj.get("params")
                        && let Ok(prog) =
                            serde_json::from_value::<ProgressNotification>(params.clone())
                        && let JsonRpcId::Number(token_id) = prog.progress_token
                    {
                        let (reset_opt, cb_opt) = {
                            let guard = pending_map.lock().expect("pending lock");
                            guard
                                .get(&token_id)
                                .map(|e| (e.progress_reset.clone(), e.on_progress.clone()))
                                .unzip()
                        };
                        if let Some(reset) = reset_opt {
                            reset.notify_one();
                        }
                        if let Some(Some(cb)) = cb_opt {
                            // Caller callbacks must not terminate the connection receiver.
                            let _ = catch_unwind(AssertUnwindSafe(|| cb(prog)));
                        }
                    }
                    continue;
                }

                let Some(id) = obj.get("id").and_then(|v| v.as_i64()) else {
                    continue;
                };
                let Some(entry) = pending_map.lock().expect("pending lock").remove(&id) else {
                    continue;
                };
                if let Some(err_val) = obj.get("error") {
                    let msg = err_val
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("MCP RPC error")
                        .to_owned();
                    let _ = entry.tx.send(Err(msg));
                } else {
                    let res = obj.get("result").cloned().unwrap_or(Value::Null);
                    let _ = entry.tx.send(Ok(res));
                }
            }

            closed_for_loop.store(true, Ordering::SeqCst);
            let drained: Vec<PendingEntry> =
                std::mem::take(&mut *pending_map.lock().expect("pending lock"))
                    .into_values()
                    .collect();
            for entry in drained {
                let _ = entry.tx.send(Err("MCP connection closed".to_owned()));
            }
        });

        let init_params = json!({
            "protocolVersion": self.options.protocol_version.as_deref().unwrap_or(LATEST_PROTOCOL_VERSION),
            "capabilities": self.options.capabilities,
            "clientInfo": {
                "name": self.options.name,
                "version": self.options.version,
            }
        });

        let raw_init = self
            .request(
                "initialize",
                Some(init_params),
                McpRequestOptions::default(),
            )
            .await?;
        let init_res: InitializeResult = serde_json::from_value(raw_init)
            .map_err(|_| McpError::invalid_request("Invalid MCP initialize result").to_string())?;

        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&init_res.protocol_version.as_str()) {
            let _ = self.close().await;
            return Err(format!(
                "MCP server selected unsupported protocol version {}",
                init_res.protocol_version
            ));
        }

        transport.set_protocol_version(&init_res.protocol_version);
        *self.protocol_version.lock().expect("ver lock") = Some(init_res.protocol_version.clone());
        *self.server_info.lock().expect("info lock") = Some(init_res.server_info.clone());
        *self.server_capabilities.lock().expect("caps lock") = Some(init_res.capabilities.clone());
        *self.instructions.lock().expect("inst lock") = init_res.instructions.clone();

        self.notify("notifications/initialized", None).await?;
        Ok(init_res)
    }

    pub async fn notify(&self, method: &str, params: Option<JsonValue>) -> Result<(), String> {
        let transport = self
            .transport
            .lock()
            .expect("transport lock")
            .clone()
            .ok_or_else(|| "MCP client is not connected".to_owned())?;
        let mut msg = Map::new();
        msg.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        msg.insert("method".to_owned(), Value::String(method.to_owned()));
        if let Some(p) = params {
            msg.insert("params".to_owned(), p);
        }
        transport.send(Value::Object(msg)).await
    }

    pub async fn request(
        &self,
        method: &str,
        params: Option<JsonValue>,
        options: McpRequestOptions,
    ) -> Result<JsonValue, String> {
        if let Some(cancel) = &options.cancel_token
            && cancel.is_cancelled()
        {
            return Err(format!("MCP request {method} was aborted"));
        }
        let transport = self
            .transport
            .lock()
            .expect("transport lock")
            .clone()
            .ok_or_else(|| "MCP client is not connected".to_owned())?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let progress_reset = Arc::new(Notify::new());
        let (tx, mut rx) = oneshot::channel();
        self.pending.lock().expect("pending lock").insert(
            id,
            PendingEntry {
                tx,
                progress_reset: progress_reset.clone(),
                on_progress: options.on_progress.clone(),
            },
        );

        let mut final_params = params.unwrap_or_else(|| Value::Object(Map::new()));
        if options.on_progress.is_some()
            && let Some(p_obj) = final_params.as_object_mut()
        {
            let meta = p_obj
                .entry("_meta".to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(m_obj) = meta.as_object_mut() {
                m_obj
                    .entry("progressToken".to_owned())
                    .or_insert_with(|| Value::Number(id.into()));
            }
        }

        let req_msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": final_params,
        });

        let _pending_guard = PendingRequestGuard {
            id,
            pending: self.pending.clone(),
        };
        let send = transport.send(req_msg);
        tokio::pin!(send);
        let mut sent = false;
        let timeout_ms = options
            .timeout_ms
            .unwrap_or(self.options.request_timeout_ms);
        let cancel_token = options.cancel_token.unwrap_or_default();
        let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                biased;
                res = &mut rx => {
                    return res.unwrap_or_else(|_| Err("MCP connection closed".to_owned()));
                }
                result = &mut send, if !sent => {
                    result?;
                    sent = true;
                }
                () = cancel_token.cancelled() => {
                    self.send_cancellation(id, format!("MCP request {method} was aborted"));
                    return Err(format!("MCP request {method} was aborted"));
                }
                () = progress_reset.notified() => {
                    deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(timeout_ms));
                }
                () = &mut deadline => {
                    self.send_cancellation(id, format!("MCP request {method} timed out after {timeout_ms}ms"));
                    return Err(format!("MCP request {method} timed out after {timeout_ms}ms"));
                }
            }
        }
    }

    fn send_cancellation(&self, id: i64, reason: String) {
        let client = self.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                client.notify(
                    "notifications/cancelled",
                    Some(json!({ "requestId": id, "reason": reason })),
                ),
            )
            .await;
        });
    }

    pub async fn list_tools(&self, options: McpRequestOptions) -> Result<Vec<McpTool>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = cursor.as_ref().map(|c| json!({ "cursor": c }));
            let raw = self.request("tools/list", params, options.clone()).await?;
            let Some(obj) = raw.as_object() else {
                return Err("Invalid MCP tools/list result".to_owned());
            };
            let Some(arr) = obj.get("tools").and_then(|v| v.as_array()) else {
                return Err("Invalid MCP tools/list result".to_owned());
            };
            for item in arr {
                let tool: McpTool = serde_json::from_value(item.clone())
                    .map_err(|_| "Invalid entry in MCP tools/list result".to_owned())?;
                tools.push(tool);
            }
            cursor = obj
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err("MCP tools/list exceeded maximum page limit".to_owned())
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: JsonValue,
        options: McpRequestOptions,
    ) -> Result<CallToolResult, String> {
        let raw = self
            .request(
                "tools/call",
                Some(json!({
                    "name": name,
                    "arguments": arguments,
                })),
                options,
            )
            .await?;
        if !raw.is_object() {
            return Err("Invalid MCP tools/call result".to_owned());
        }
        serde_json::from_value(raw).map_err(|_| "Invalid MCP tools/call result".to_owned())
    }

    pub async fn list_resources_page(
        &self,
        cursor: Option<&str>,
        options: McpRequestOptions,
    ) -> Result<ListResourcesResult, String> {
        let params = cursor.map(|c| json!({ "cursor": c }));
        let raw = self.request("resources/list", params, options).await?;
        let Some(obj) = raw.as_object() else {
            return Err("Invalid MCP resources/list result".to_owned());
        };
        let Some(arr) = obj.get("resources").and_then(|v| v.as_array()) else {
            return Err("Invalid MCP resources/list result".to_owned());
        };
        let mut resources = Vec::with_capacity(arr.len());
        for item in arr {
            let mut item_obj = item
                .as_object()
                .ok_or_else(|| "Invalid entry in MCP resources/list result".to_owned())?
                .clone();
            let uri = item_obj
                .get("uri")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Invalid entry in MCP resources/list result".to_owned())?
                .to_owned();
            if !item_obj.contains_key("name") {
                item_obj.insert("name".to_owned(), Value::String(uri));
            }
            let res: McpResource = serde_json::from_value(Value::Object(item_obj))
                .map_err(|_| "Invalid entry in MCP resources/list result".to_owned())?;
            resources.push(res);
        }
        let next_cursor = obj
            .get("nextCursor")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        Ok(ListResourcesResult {
            resources,
            next_cursor,
        })
    }

    pub async fn list_resources(
        &self,
        options: McpRequestOptions,
    ) -> Result<Vec<McpResource>, String> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = self
                .list_resources_page(cursor.as_deref(), options.clone())
                .await?;
            all.extend(page.resources);
            if page.next_cursor.is_none() {
                return Ok(all);
            }
            cursor = page.next_cursor;
        }
        Err("MCP resources/list exceeded maximum page limit".to_owned())
    }

    pub async fn list_resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: McpRequestOptions,
    ) -> Result<ListResourceTemplatesResult, String> {
        let params = cursor.map(|c| json!({ "cursor": c }));
        let raw = self
            .request("resources/templates/list", params, options)
            .await?;
        let Some(obj) = raw.as_object() else {
            return Err("Invalid MCP resources/templates/list result".to_owned());
        };
        let Some(arr) = obj.get("resourceTemplates").and_then(|v| v.as_array()) else {
            return Err("Invalid MCP resources/templates/list result".to_owned());
        };
        let mut resource_templates = Vec::with_capacity(arr.len());
        for item in arr {
            let mut item_obj = item
                .as_object()
                .ok_or_else(|| "Invalid entry in MCP resources/templates/list result".to_owned())?
                .clone();
            let uri_template = item_obj
                .get("uriTemplate")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Invalid entry in MCP resources/templates/list result".to_owned())?
                .to_owned();
            if !item_obj.contains_key("name") {
                item_obj.insert("name".to_owned(), Value::String(uri_template));
            }
            let tmpl: McpResourceTemplate = serde_json::from_value(Value::Object(item_obj))
                .map_err(|_| "Invalid entry in MCP resources/templates/list result".to_owned())?;
            resource_templates.push(tmpl);
        }
        let next_cursor = obj
            .get("nextCursor")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        Ok(ListResourceTemplatesResult {
            resource_templates,
            next_cursor,
        })
    }

    pub async fn list_resource_templates(
        &self,
        options: McpRequestOptions,
    ) -> Result<Vec<McpResourceTemplate>, String> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = self
                .list_resource_templates_page(cursor.as_deref(), options.clone())
                .await?;
            all.extend(page.resource_templates);
            if page.next_cursor.is_none() {
                return Ok(all);
            }
            cursor = page.next_cursor;
        }
        Err("MCP resources/templates/list exceeded maximum page limit".to_owned())
    }

    pub async fn read_resource(
        &self,
        uri: &str,
        options: McpRequestOptions,
    ) -> Result<ReadResourceResult, String> {
        let raw = self
            .request("resources/read", Some(json!({ "uri": uri })), options)
            .await?;
        serde_json::from_value(raw).map_err(|_| "Invalid MCP resources/read result".to_owned())
    }

    pub async fn close(&self) -> Result<(), String> {
        self.closed.store(true, Ordering::SeqCst);
        self.receive_cancel.cancel();
        let pending = std::mem::take(&mut *self.pending.lock().expect("pending lock"));
        for entry in pending.into_values() {
            let _ = entry.tx.send(Err("MCP connection closed".to_owned()));
        }
        let t_opt = self.transport.lock().expect("transport lock").take();
        if let Some(t) = t_opt {
            tokio::time::timeout(Duration::from_secs(2), t.close())
                .await
                .map_err(|_| "Timed out closing MCP transport".to_owned())??;
        }
        Ok(())
    }
}

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::BoxFuture;
use pi_ai::JsonValue;
use reqwest::{Method, Response, StatusCode};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::oauth::McpOAuthSession;
use super::protocol::{InitializeResult, SUPPORTED_PROTOCOL_VERSIONS};
use super::sse::{MAX_MCP_MESSAGE_BYTES, SseDecoder};
use super::transport::{McpTransport, SessionReinitializedHandler};

#[derive(Clone)]
pub struct StreamableHttpTransportOptions {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub oauth: Option<Arc<McpOAuthSession>>,
    pub open_get_stream: bool,
    pub reconnect_initial_delay_ms: u64,
    pub reconnect_max_retries: usize,
}
impl Default for StreamableHttpTransportOptions {
    fn default() -> Self {
        Self {
            url: String::new(),
            headers: BTreeMap::new(),
            oauth: None,
            open_get_stream: true,
            reconnect_initial_delay_ms: 1000,
            reconnect_max_retries: 5,
        }
    }
}

#[derive(Default)]
struct HttpSession {
    id: Option<String>,
    version: Option<String>,
    initialize: Option<JsonValue>,
    expired: bool,
}
struct HttpInner {
    options: StreamableHttpTransportOptions,
    client: reqwest::Client,
    tx: Mutex<Option<mpsc::UnboundedSender<JsonValue>>>,
    session: Mutex<HttpSession>,
    started: AtomicBool,
    closed: AtomicBool,
    cancel: CancellationToken,
    reconnect: tokio::sync::Mutex<()>,
    get_cancel: Mutex<Option<CancellationToken>>,
    last_token: Mutex<Option<String>>,
    reinitialized: Mutex<Option<SessionReinitializedHandler>>,
}
impl Drop for HttpInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct HttpLifetime(CancellationToken);
impl Drop for HttpLifetime {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone)]
pub struct StreamableHttpTransport {
    inner: Arc<HttpInner>,
    _lifetime: Arc<HttpLifetime>,
}

impl StreamableHttpTransport {
    pub fn new(url: impl Into<String>, headers: BTreeMap<String, String>) -> Self {
        Self::with_options(StreamableHttpTransportOptions {
            url: url.into(),
            headers,
            ..Default::default()
        })
    }
    pub fn with_options(options: StreamableHttpTransportOptions) -> Self {
        let cancel = CancellationToken::new();
        let lifetime = Arc::new(HttpLifetime(cancel.clone()));
        Self {
            _lifetime: lifetime,
            inner: Arc::new(HttpInner {
                options,
                client: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("MCP HTTP client configuration"),
                tx: Mutex::new(None),
                session: Mutex::new(HttpSession::default()),
                started: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel,
                reconnect: tokio::sync::Mutex::new(()),
                get_cancel: Mutex::new(None),
                last_token: Mutex::new(None),
                reinitialized: Mutex::new(None),
            }),
        }
    }
    pub fn session_id(&self) -> Option<String> {
        self.inner
            .session
            .lock()
            .expect("HTTP session lock")
            .id
            .clone()
    }
}

impl McpTransport for StreamableHttpTransport {
    fn start(&self) -> BoxFuture<'_, Result<mpsc::UnboundedReceiver<JsonValue>, String>> {
        Box::pin(async move {
            if self.inner.closed.load(Ordering::SeqCst)
                || self.inner.started.swap(true, Ordering::SeqCst)
            {
                return Err("MCP HTTP transport already started or closed".to_owned());
            }
            let (tx, rx) = mpsc::unbounded_channel();
            *self.inner.tx.lock().expect("HTTP sender lock") = Some(tx);
            Ok(rx)
        })
    }
    fn set_protocol_version(&self, version: &str) {
        self.inner
            .session
            .lock()
            .expect("HTTP session lock")
            .version = Some(version.to_owned());
    }
    fn on_session_reinitialized(&self, handler: SessionReinitializedHandler) {
        *self
            .inner
            .reinitialized
            .lock()
            .expect("reinitialize handler lock") = Some(handler);
    }
    fn send(&self, message: JsonValue) -> BoxFuture<'_, Result<(), String>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            if !inner.started.load(Ordering::SeqCst) || inner.closed.load(Ordering::SeqCst) {
                return Err("MCP HTTP transport is not open".to_owned());
            }
            let cancel = inner.cancel.clone();
            tokio::select! { biased; () = cancel.cancelled() => Err("MCP HTTP transport closed".to_owned()), result = inner.send_message(message) => result }
        })
    }
    fn close(&self) -> BoxFuture<'_, Result<(), String>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            if inner.closed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            inner.cancel.cancel();
            inner.stop_get();
            inner.tx.lock().expect("HTTP sender lock").take();
            let (id, version) = {
                let mut session = inner.session.lock().expect("HTTP session lock");
                (session.id.take(), session.version.clone())
            };
            if let Some(id) = id {
                let mut request = inner
                    .client
                    .delete(&inner.options.url)
                    .header("Mcp-Session-Id", id);
                for (name, value) in &inner.options.headers {
                    request = request.header(name, value);
                }
                if let Some(version) = version {
                    request = request.header("MCP-Protocol-Version", version);
                }
                if let Some(token) = inner.last_token.lock().expect("token lock").clone() {
                    request = request.bearer_auth(token);
                }
                let _ = request.timeout(Duration::from_secs(1)).send().await;
            }
            Ok(())
        })
    }
}

impl HttpInner {
    async fn request(
        &self,
        method: Method,
        message: Option<&JsonValue>,
        last_id: Option<&str>,
    ) -> Result<(Response, Option<String>), String> {
        let mut token = match &self.options.oauth {
            Some(oauth) => oauth.access_token().await?,
            None => None,
        };
        for attempt in 0..2 {
            let (id, version) = {
                let session = self.session.lock().expect("HTTP session lock");
                (session.id.clone(), session.version.clone())
            };
            let mut request = self
                .client
                .request(method.clone(), &self.options.url)
                .header("Accept", "application/json, text/event-stream");
            for (name, value) in &self.options.headers {
                request = request.header(name, value);
            }
            if let Some(id) = &id {
                request = request.header("Mcp-Session-Id", id);
            }
            if let Some(version) = version {
                request = request.header("MCP-Protocol-Version", version);
            }
            if let Some(last_id) = last_id {
                request = request.header("Last-Event-ID", last_id);
            }
            if let Some(token) = &token {
                request = request.bearer_auth(token);
            }
            if let Some(message) = message {
                request = request.json(message);
            }
            *self.last_token.lock().expect("token lock") = token.clone();
            let response = request.send().await.map_err(|_| {
                "MCP HTTP request failed; request outcome may be unknown".to_owned()
            })?;
            let challenge = response
                .headers()
                .get("www-authenticate")
                .and_then(|header| header.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let unauthorized = response.status() == StatusCode::UNAUTHORIZED
                || (response.status() == StatusCode::FORBIDDEN
                    && challenge.contains("insufficient_scope"));
            if unauthorized && let Some(oauth) = &self.options.oauth {
                let refreshed = oauth.on_unauthorized(&challenge, token.as_deref()).await?;
                if attempt == 0 && refreshed.is_some() && refreshed != token {
                    token = refreshed;
                    continue;
                }
                return Err("MCP OAuth authorization required; use /mcp auth <server>".to_owned());
            }
            return Ok((response, id));
        }
        Err("MCP authentication retry exhausted".to_owned())
    }

    async fn send_message(self: &Arc<Self>, message: JsonValue) -> Result<(), String> {
        let initialize = message.get("method").and_then(JsonValue::as_str) == Some("initialize");
        if initialize {
            self.session.lock().expect("HTTP session lock").initialize = Some(message.clone());
        } else if self.session.lock().expect("HTTP session lock").expired {
            self.reinitialize(None).await?;
        }
        let (mut response, mut attempted) =
            self.request(Method::POST, Some(&message), None).await?;
        if response.status() == StatusCode::NOT_FOUND && attempted.is_some() && !initialize {
            drop(response);
            self.reinitialize(attempted.as_deref()).await?;
            (response, attempted) = self.request(Method::POST, Some(&message), None).await?;
        }
        let _ = attempted;
        check_status(&response)?;
        if initialize
            && let Some(id) = response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
        {
            self.session.lock().expect("HTTP session lock").id = Some(id.to_owned());
        }
        let is_request = message.get("method").is_some() && message.get("id").is_some();
        if !is_request {
            if message["method"] == "notifications/initialized" {
                self.start_get();
            }
            return Ok(());
        }
        if matches!(
            response.status(),
            StatusCode::ACCEPTED | StatusCode::NO_CONTENT
        ) {
            return Err("MCP server accepted a request without returning a response".to_owned());
        }
        let sender = self
            .tx
            .lock()
            .expect("HTTP sender lock")
            .clone()
            .ok_or("MCP HTTP transport closed")?;
        self.consume_response(response, message.get("id"), &sender)
            .await
    }

    async fn consume_response(
        &self,
        response: Response,
        target: Option<&JsonValue>,
        sender: &mpsc::UnboundedSender<JsonValue>,
    ) -> Result<(), String> {
        if !is_sse(&response) {
            let value = read_json(response).await?;
            let values = value.as_array().cloned().unwrap_or_else(|| vec![value]);
            for value in values {
                sender.send(value).map_err(|_| "MCP connection closed")?;
            }
            return Ok(());
        }
        let mut response = response;
        let mut last_id = None;
        let mut delay = self.options.reconnect_initial_delay_ms;
        for attempt in 0..=self.options.reconnect_max_retries {
            let mut decoder = SseDecoder::with_cursor(last_id.clone());
            let mut interrupted = false;
            loop {
                match response.chunk().await {
                    Ok(Some(bytes)) => {
                        let values = decoder.push(&bytes, false)?;
                        last_id = decoder.last_event_id.clone();
                        if let Some(retry) = decoder.retry_ms {
                            delay = retry.min(30_000);
                        }
                        for value in values {
                            let complete = target.is_some_and(|id| {
                                value.get("id") == Some(id) && value.get("method").is_none()
                            });
                            sender.send(value).map_err(|_| "MCP connection closed")?;
                            if complete {
                                return Ok(());
                            }
                        }
                    }
                    Ok(None) => {
                        for value in decoder.push(&[], true)? {
                            let complete = target.is_some_and(|id| {
                                value.get("id") == Some(id) && value.get("method").is_none()
                            });
                            sender.send(value).map_err(|_| "MCP connection closed")?;
                            if complete {
                                return Ok(());
                            }
                        }
                        last_id = decoder.last_event_id.clone();
                        break;
                    }
                    Err(_) => {
                        interrupted = true;
                        break;
                    }
                }
            }
            if target.is_none() && !interrupted {
                return Ok(());
            }
            let Some(cursor) = &last_id else {
                return Err(
                    "MCP response stream ended before the result; request outcome is unknown"
                        .to_owned(),
                );
            };
            if attempt == self.options.reconnect_max_retries {
                break;
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
            delay = delay.saturating_mul(2).min(30_000);
            let (resumed, _) = self.request(Method::GET, None, Some(cursor)).await?;
            check_status(&resumed)?;
            if !is_sse(&resumed) {
                return Err("MCP response resumption did not return an SSE stream".to_owned());
            }
            response = resumed;
        }
        Err("MCP SSE resumption retries exhausted; request outcome is unknown".to_owned())
    }

    async fn reinitialize(self: &Arc<Self>, previous: Option<&str>) -> Result<(), String> {
        let _gate = self.reconnect.lock().await;
        {
            let session = self.session.lock().expect("HTTP session lock");
            if !session.expired && session.id.as_deref() != previous {
                return Ok(());
            }
        }
        self.stop_get();
        let mut initialize = {
            let mut session = self.session.lock().expect("HTTP session lock");
            session.id = None;
            session.expired = true;
            session
                .initialize
                .clone()
                .ok_or("Missing MCP initialization request")?
        };
        initialize["id"] = json!(format!("pi-reconnect-{}", uuid::Uuid::new_v4()));
        let (response, _) = self.request(Method::POST, Some(&initialize), None).await?;
        check_status(&response)?;
        let id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let (sender, mut replies) = mpsc::unbounded_channel();
        self.consume_response(response, initialize.get("id"), &sender)
            .await?;
        let mut result = None;
        while let Ok(reply) = replies.try_recv() {
            if reply.get("id") == initialize.get("id") {
                result = reply.get("result").cloned();
            }
        }
        let initialized: InitializeResult =
            serde_json::from_value(result.ok_or("Missing MCP reinitialization result")?)
                .map_err(|_| "Invalid MCP reinitialization result")?;
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&initialized.protocol_version.as_str()) {
            return Err("Unsupported MCP protocol after reinitialization".to_owned());
        }
        {
            let mut session = self.session.lock().expect("HTTP session lock");
            session.id = id;
            session.version = Some(initialized.protocol_version.clone());
        }
        let (acknowledged, _) = self
            .request(
                Method::POST,
                Some(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"})),
                None,
            )
            .await?;
        check_status(&acknowledged)?;
        self.session.lock().expect("HTTP session lock").expired = false;
        if let Some(handler) = self
            .reinitialized
            .lock()
            .expect("reinitialize handler lock")
            .clone()
        {
            handler(initialized);
        }
        if let Some(sender) = self.tx.lock().expect("HTTP sender lock").as_ref() {
            let _ =
                sender.send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}));
        }
        self.start_get();
        Ok(())
    }

    fn stop_get(&self) {
        if let Some(cancel) = self.get_cancel.lock().expect("GET stream lock").take() {
            cancel.cancel();
        }
    }

    fn start_get(self: &Arc<Self>) {
        if !self.options.open_get_stream || self.closed.load(Ordering::SeqCst) {
            return;
        }
        let cancel = self.cancel.child_token();
        {
            let mut existing = self.get_cancel.lock().expect("GET stream lock");
            if existing.as_ref().is_some_and(|token| !token.is_cancelled()) {
                return;
            }
            *existing = Some(cancel.clone());
        }
        let weak = Arc::downgrade(self);
        // A weak reference between retries avoids keeping the transport alive after its owner drops it.
        tokio::spawn(async move {
            let run = async {
                let mut cursor = None;
                let Some(initial) = weak.upgrade() else {
                    return;
                };
                let max_retries = initial.options.reconnect_max_retries;
                let base_delay = initial.options.reconnect_initial_delay_ms.min(30_000);
                drop(initial);
                let mut delay = base_delay;
                let mut failures = 0;
                loop {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let response = tokio::time::timeout(
                        Duration::from_secs(30),
                        inner.request(Method::GET, None, cursor.as_deref()),
                    )
                    .await;
                    let mut progressed = false;
                    match response {
                        Ok(Ok((mut response, attempted))) => {
                            if matches!(
                                response.status(),
                                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
                            ) {
                                return;
                            }
                            if response.status() == StatusCode::NOT_FOUND {
                                let mut session = inner.session.lock().expect("HTTP session lock");
                                if attempted.is_some() && session.id == attempted {
                                    session.id = None;
                                    session.expired = true;
                                }
                                return;
                            }
                            if response.status().is_success() && is_sse(&response) {
                                let Some(sender) =
                                    inner.tx.lock().expect("HTTP sender lock").clone()
                                else {
                                    return;
                                };
                                let mut decoder = SseDecoder::with_cursor(cursor.clone());
                                loop {
                                    let (bytes, eof) = match response.chunk().await {
                                        Ok(Some(bytes)) => (bytes, false),
                                        Ok(None) => (bytes::Bytes::new(), true),
                                        Err(_) => break,
                                    };
                                    let Ok(values) = decoder.push(&bytes, eof) else {
                                        return;
                                    };
                                    cursor = decoder.last_event_id.clone();
                                    if let Some(retry) = decoder.retry_ms {
                                        delay = retry.min(30_000);
                                    }
                                    if !values.is_empty() {
                                        progressed = true;
                                    }
                                    for value in values {
                                        if sender.send(value).is_err() {
                                            return;
                                        }
                                    }
                                    if eof {
                                        break;
                                    }
                                }
                            } else if !response.status().is_server_error()
                                && !matches!(
                                    response.status(),
                                    StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
                                )
                            {
                                return;
                            }
                        }
                        Ok(Err(error)) if !error.starts_with("MCP HTTP request failed") => return,
                        _ => {}
                    }
                    drop(inner);
                    if progressed {
                        failures = 0;
                    } else {
                        failures += 1;
                    }
                    if failures > max_retries {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    delay = delay.saturating_mul(2).min(30_000);
                }
            };
            tokio::select! { biased; () = cancel.cancelled() => {}, () = run => {} }
            cancel.cancel();
        });
    }
}

fn is_sse(response: &Response) -> bool {
    response
        .headers()
        .get("content-type")
        .and_then(|header| header.to_str().ok())
        .is_some_and(|header| {
            header
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("text/event-stream")
        })
}
fn check_status(response: &Response) -> Result<(), String> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("MCP HTTP status {}", response.status().as_u16()))
    }
}
async fn read_json(mut response: Response) -> Result<JsonValue, String> {
    let mut body = Vec::new();
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|_| "Failed to read MCP HTTP response; request outcome may be unknown")?
    {
        if body.len().saturating_add(bytes.len()) > MAX_MCP_MESSAGE_BYTES {
            return Err("MCP HTTP response exceeds size limit".to_owned());
        }
        body.extend_from_slice(&bytes);
    }
    serde_json::from_slice(&body).map_err(|_| "Invalid JSON in MCP HTTP response".to_owned())
}

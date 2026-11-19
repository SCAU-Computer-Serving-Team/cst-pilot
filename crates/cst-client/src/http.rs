use std::time::Duration;

use reqwest::{Method, Response};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::ClientError;
use crate::state::{
    AccountStatusDto, AppHealthDto, DesktopSettingsDto, DesktopSettingsPatchDto,
    LifecycleReportDto, MissingKeyClaimDto, ModelsSnapshot, ProviderStatusDto, SessionSummary,
    UiHintsDto, UpdateStateDto, UpdateStatusDto, WebOpenDto,
};

/// Loopback HTTP client pinned to `127.0.0.1:<port>` with environment proxies disabled,
/// redirects disabled, `X-CST-Client: app`, and `Zeroizing` Bearer token storage.
#[derive(Clone)]
pub struct AppHttpClient {
    port: u16,
    base_url: String,
    host_header: String,
    app_token: Zeroizing<String>,
    client: reqwest::Client,
}

impl std::fmt::Debug for AppHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppHttpClient")
            .field("port", &self.port)
            .field("base_url", &self.base_url)
            .field("app_token", &"[REDACTED]")
            .finish()
    }
}

impl AppHttpClient {
    pub fn new(port: u16, app_token: impl Into<String>) -> Result<Self, ClientError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| {
                ClientError::BootstrapFailed(format!("无法初始化本机 HTTP 客户端：{e}"))
            })?;
        Ok(Self {
            port,
            base_url: format!("http://127.0.0.1:{port}"),
            host_header: format!("127.0.0.1:{port}"),
            app_token: Zeroizing::new(app_token.into()),
            client,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_header_value(&self) -> String {
        format!("Bearer {}", self.app_token.as_str())
    }

    fn loopback_url(&self, path_and_query: &str) -> Result<String, ClientError> {
        if !path_and_query.starts_with('/') {
            return Err(ClientError::BadRequest {
                code: "invalid_path".into(),
                message: "本机接口路径必须以 / 开头。".into(),
            });
        }
        Ok(format!("{}{}", self.base_url, path_and_query))
    }

    pub async fn request_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<T, ClientError> {
        let val = self
            .request_value(method, path_and_query, body, idempotency_key, timeout)
            .await?;
        serde_json::from_value(val).map_err(|e| ClientError::Server {
            status: 200,
            code: "invalid_response".into(),
            message: format!("解析服务端响应失败：{e}"),
        })
    }

    pub async fn request_value(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<Value, ClientError> {
        let url = self.loopback_url(path_and_query)?;
        let is_read = method == Method::GET || method == Method::HEAD;
        let mut req = self
            .client
            .request(method, url)
            .header("Host", &self.host_header)
            .header("X-CST-Client", "app")
            .header("Authorization", self.auth_header_value());

        if let Some(dur) = timeout {
            req = req.timeout(dur);
        } else {
            req = req.timeout(Duration::from_secs(30));
        }

        if !is_read {
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("app-{}", Uuid::now_v7().simple()));
            req = req
                .header("Idempotency-Key", key)
                .json(body.unwrap_or(&json!({})));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Disconnected(format!("无法连接本机后台服务：{e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ClientError::Disconnected(format!("读取本机后台服务响应中断：{e}")))?;
        let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();

        if !(200..300).contains(&status) {
            return Err(ClientError::from_http_response(status, parsed.as_ref()));
        }
        Ok(parsed.unwrap_or(Value::Null))
    }

    pub async fn request_bytes(
        &self,
        path_and_query: &str,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, ClientError> {
        let url = self.loopback_url(path_and_query)?;
        let req = self
            .client
            .get(url)
            .header("Host", &self.host_header)
            .header("X-CST-Client", "app")
            .header("Authorization", self.auth_header_value())
            .timeout(timeout.unwrap_or(Duration::from_secs(15)));
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Disconnected(format!("无法连接本机后台服务：{e}")))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ClientError::Disconnected(format!("读取本机后台服务响应中断：{e}")))?;
        if !(200..300).contains(&status) {
            let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();
            return Err(ClientError::from_http_response(status, parsed.as_ref()));
        }
        Ok(bytes.to_vec())
    }

    pub async fn open_sse_stream(
        &self,
        path_and_query: &str,
        last_event_id: Option<&str>,
    ) -> Result<Response, ClientError> {
        let url = self.loopback_url(path_and_query)?;
        let mut req = self
            .client
            .get(url)
            .header("Host", &self.host_header)
            .header("X-CST-Client", "app")
            .header("Accept", "text/event-stream")
            .header("Authorization", self.auth_header_value());
        if let Some(id) = last_event_id.filter(|s| !s.is_empty()) {
            req = req.header("Last-Event-ID", id);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Disconnected(format!("建立事件流连接失败：{e}")))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let bytes = resp.bytes().await.unwrap_or_default();
            let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();
            return Err(ClientError::from_http_response(status, parsed.as_ref()));
        }
        Ok(resp)
    }

    pub async fn health(&self, timeout: Duration) -> Result<AppHealthDto, ClientError> {
        self.request_json(Method::GET, "/api/health", None, None, Some(timeout))
            .await
    }

    pub async fn state(&self) -> Result<Value, ClientError> {
        self.request_value(Method::GET, "/api/state", None, None, None)
            .await
    }

    pub async fn lifecycle(&self) -> Result<LifecycleReportDto, ClientError> {
        self.request_json(
            Method::GET,
            "/api/lifecycle",
            None,
            None,
            Some(Duration::from_secs(3)),
        )
        .await
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, ClientError> {
        let val = self
            .request_value(Method::GET, "/api/sessions", None, None, None)
            .await?;
        if let Some(arr) = val.get("sessions").or(Some(&val)).and_then(Value::as_array) {
            serde_json::from_value(Value::Array(arr.clone())).map_err(|e| ClientError::Server {
                status: 200,
                code: "invalid_response".into(),
                message: e.to_string(),
            })
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn get_session_snapshot(&self, session_id: &str) -> Result<Value, ClientError> {
        let encoded = encode_segment(session_id);
        self.request_value(
            Method::GET,
            &format!("/api/sessions/{encoded}"),
            None,
            None,
            None,
        )
        .await
    }

    pub async fn create_session(
        &self,
        request_key: &str,
        provider: Option<&str>,
        model_id: Option<&str>,
        thinking_level: Option<&str>,
    ) -> Result<String, ClientError> {
        let mut body = serde_json::Map::new();
        if let (Some(p), Some(m)) = (provider, model_id) {
            body.insert("provider".into(), Value::String(p.to_owned()));
            body.insert("modelId".into(), Value::String(m.to_owned()));
        }
        if let Some(t) = thinking_level {
            body.insert("thinkingLevel".into(), Value::String(t.to_owned()));
        }
        let val = self
            .request_value(
                Method::POST,
                "/api/sessions",
                Some(&Value::Object(body)),
                Some(request_key),
                None,
            )
            .await?;
        val.get("session")
            .and_then(|s| s.get("id"))
            .or_else(|| val.get("sessionId"))
            .or_else(|| val.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ClientError::Server {
                status: 200,
                code: "invalid_response".into(),
                message: "新建会话未返回有效会话 ID。".into(),
            })
    }

    pub async fn rename_session(&self, session_id: &str, name: &str) -> Result<Value, ClientError> {
        let encoded = encode_segment(session_id);
        let key = uuid::Uuid::new_v4().to_string();
        self.request_value(
            Method::PATCH,
            &format!("/api/sessions/{encoded}"),
            Some(&json!({ "name": name })),
            Some(&key),
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<Value, ClientError> {
        let encoded = encode_segment(session_id);
        let key = uuid::Uuid::new_v4().to_string();
        self.request_value(
            Method::DELETE,
            &format!("/api/sessions/{encoded}"),
            None,
            Some(&key),
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn check_session_creation(
        &self,
        request_key: &str,
    ) -> Result<(String, Option<String>), ClientError> {
        let encoded = encode_segment(request_key);
        let val = self
            .request_value(
                Method::GET,
                &format!("/api/sessions/creation/{encoded}"),
                None,
                None,
                Some(Duration::from_secs(5)),
            )
            .await?;
        let status = val
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("missing")
            .to_owned();
        let session_id = val
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok((status, session_id))
    }

    pub async fn post_session_message(
        &self,
        session_id: &str,
        client_message_id: &str,
        payload: &Value,
    ) -> Result<Value, ClientError> {
        let encoded = encode_segment(session_id);
        self.request_value(
            Method::POST,
            &format!("/api/sessions/{encoded}/messages"),
            Some(payload),
            Some(client_message_id),
            None,
        )
        .await
    }

    pub async fn check_message_receipt(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<Value>, ClientError> {
        let sid = encode_segment(session_id);
        let mid = encode_segment(message_id);
        let val = self
            .request_value(
                Method::GET,
                &format!("/api/sessions/{sid}/messages/{mid}"),
                None,
                None,
                Some(Duration::from_secs(5)),
            )
            .await?;
        Ok(val.get("item").filter(|v| !v.is_null()).cloned())
    }

    pub async fn put_multiplex_subscriptions(
        &self,
        client_id: &str,
        sessions: &[String],
        last_event_ids: Option<&serde_json::Map<String, Value>>,
    ) -> Result<Value, ClientError> {
        let cid = encode_segment(client_id);
        let mut body = serde_json::Map::new();
        body.insert(
            "sessions".into(),
            Value::Array(sessions.iter().cloned().map(Value::String).collect()),
        );
        if let Some(ids) = last_event_ids
            && !ids.is_empty()
        {
            body.insert("lastEventIds".into(), Value::Object(ids.clone()));
        }
        self.request_value(
            Method::PUT,
            &format!("/api/events/stream/{cid}/subscriptions"),
            Some(&Value::Object(body)),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn get_models(
        &self,
        session_id: Option<&str>,
    ) -> Result<ModelsSnapshot, ClientError> {
        let path = match session_id {
            Some(sid) => format!("/api/models?sessionId={}", encode_segment(sid)),
            None => "/api/models".into(),
        };
        self.request_json(Method::GET, &path, None, None, None)
            .await
    }

    pub async fn get_auth_providers(&self) -> Result<Vec<ProviderStatusDto>, ClientError> {
        let val = self
            .request_value(Method::GET, "/api/auth", None, None, None)
            .await?;
        let arr = val
            .get("providers")
            .or(Some(&val))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        serde_json::from_value(Value::Array(arr)).map_err(|e| ClientError::Server {
            status: 200,
            code: "invalid_response".into(),
            message: e.to_string(),
        })
    }

    pub async fn get_account(&self, refresh: bool) -> Result<AccountStatusDto, ClientError> {
        let path = if refresh {
            "/api/account?refresh=1"
        } else {
            "/api/account"
        };
        self.request_json(Method::GET, path, None, None, None).await
    }

    pub async fn get_desktop_settings(&self) -> Result<DesktopSettingsDto, ClientError> {
        self.request_json(Method::GET, "/api/app/settings", None, None, None)
            .await
    }

    pub async fn patch_desktop_settings(
        &self,
        patch: &DesktopSettingsPatchDto,
    ) -> Result<DesktopSettingsDto, ClientError> {
        let body = serde_json::to_value(patch).unwrap_or_else(|_| json!({}));
        self.request_json(
            Method::PATCH,
            "/api/app/settings",
            Some(&body),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn open_web(&self) -> Result<WebOpenDto, ClientError> {
        self.request_json(
            Method::POST,
            "/api/app/web/open",
            Some(&json!({})),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn close_web(&self) -> Result<Value, ClientError> {
        self.request_value(
            Method::POST,
            "/api/app/web/close",
            Some(&json!({})),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn app_exit(&self, timeout: Duration) -> Result<Value, ClientError> {
        self.request_value(
            Method::POST,
            "/api/app/exit",
            Some(&json!({})),
            None,
            Some(timeout),
        )
        .await
    }

    pub async fn get_ui_hints(&self) -> Result<UiHintsDto, ClientError> {
        self.request_json(Method::GET, "/api/ui-hints", None, None, None)
            .await
    }

    pub async fn claim_missing_key_prompt(
        &self,
        client_last_prompted_at_ms: Option<u64>,
    ) -> Result<MissingKeyClaimDto, ClientError> {
        let body = match client_last_prompted_at_ms {
            Some(ts) => json!({ "clientLastPromptedAtMs": ts }),
            None => json!({}),
        };
        self.request_json(
            Method::POST,
            "/api/ui-hints/missing-key-prompt/claim",
            Some(&body),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn get_update_status(&self) -> Result<UpdateStateDto, ClientError> {
        self.request_json(Method::GET, "/api/app/update/status", None, None, None)
            .await
    }

    pub async fn check_update(&self) -> Result<UpdateStatusDto, ClientError> {
        self.request_json(
            Method::POST,
            "/api/app/update/check",
            Some(&json!({})),
            None,
            Some(Duration::from_secs(30)),
        )
        .await
    }

    pub async fn prepare_update(&self) -> Result<UpdateStatusDto, ClientError> {
        self.request_json(
            Method::POST,
            "/api/app/update/prepare",
            Some(&json!({})),
            None,
            Some(Duration::from_secs(300)),
        )
        .await
    }

    pub async fn cancel_update(
        &self,
        operation_id: Option<&str>,
    ) -> Result<UpdateStatusDto, ClientError> {
        let body = match operation_id {
            Some(id) => json!({ "operationId": id }),
            None => json!({}),
        };
        self.request_json(
            Method::POST,
            "/api/app/update/cancel",
            Some(&body),
            None,
            Some(Duration::from_secs(10)),
        )
        .await
    }

    pub async fn launch_update(
        &self,
        app_pid: u32,
        app_created: u64,
        ui: &str,
    ) -> Result<Value, ClientError> {
        let body = json!({
            "appPid": app_pid,
            "appCreated": app_created,
            "ui": ui,
        });
        self.request_value(
            Method::POST,
            "/api/app/update/launch",
            Some(&body),
            None,
            Some(Duration::from_secs(15)),
        )
        .await
    }

    pub async fn abort_session(&self, session_id: &str) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        self.request_value(
            Method::POST,
            &format!("/api/sessions/{sid}/abort"),
            Some(&json!({})),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn compact_session(
        &self,
        session_id: &str,
        instructions: Option<&str>,
    ) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        let body = match instructions.filter(|s| !s.trim().is_empty()) {
            Some(inst) => json!({ "instructions": inst }),
            None => json!({}),
        };
        self.request_value(
            Method::POST,
            &format!("/api/sessions/{sid}/compact"),
            Some(&body),
            None,
            Some(Duration::from_secs(120)),
        )
        .await
    }

    pub async fn fork_session(
        &self,
        session_id: &str,
        entry_id: &str,
    ) -> Result<String, ClientError> {
        let sid = encode_segment(session_id);
        let val = self
            .request_value(
                Method::POST,
                &format!("/api/sessions/{sid}/fork"),
                Some(&json!({ "entryId": entry_id })),
                None,
                Some(Duration::from_secs(10)),
            )
            .await?;
        val.get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ClientError::Server {
                status: 200,
                code: "invalid_response".into(),
                message: "派生会话未返回新会话 ID。".into(),
            })
    }

    pub async fn navigate_session_tree(
        &self,
        session_id: &str,
        entry_id: &str,
        summarize: bool,
        custom_instructions: Option<&str>,
    ) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        let mut body = serde_json::Map::new();
        body.insert("entryId".into(), Value::String(entry_id.to_owned()));
        if summarize {
            body.insert("summarize".into(), Value::Bool(true));
        }
        if let Some(inst) = custom_instructions.filter(|s| !s.trim().is_empty()) {
            body.insert("customInstructions".into(), Value::String(inst.to_owned()));
        }
        self.request_value(
            Method::POST,
            &format!("/api/sessions/{sid}/tree/navigate"),
            Some(&Value::Object(body)),
            None,
            Some(Duration::from_secs(120)),
        )
        .await
    }

    pub async fn edit_queue_item(
        &self,
        session_id: &str,
        item_id: &str,
        version: u64,
        text: &str,
    ) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        let iid = encode_segment(item_id);
        self.request_value(
            Method::PATCH,
            &format!("/api/sessions/{sid}/queue/{iid}"),
            Some(&json!({ "version": version, "text": text })),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn delete_queue_item(
        &self,
        session_id: &str,
        item_id: &str,
        version: u64,
    ) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        let iid = encode_segment(item_id);
        self.request_value(
            Method::DELETE,
            &format!("/api/sessions/{sid}/queue/{iid}"),
            Some(&json!({ "version": version })),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn reorder_queue(
        &self,
        session_id: &str,
        version: u64,
        ids: &[String],
    ) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        self.request_value(
            Method::PUT,
            &format!("/api/sessions/{sid}/queue/order"),
            Some(&json!({ "version": version, "ids": ids })),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn steer_queue_item(
        &self,
        session_id: &str,
        item_id: &str,
        version: u64,
    ) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        let iid = encode_segment(item_id);
        self.request_value(
            Method::POST,
            &format!("/api/sessions/{sid}/queue/{iid}/steer"),
            Some(&json!({ "version": version })),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn retry_queue_item(
        &self,
        session_id: &str,
        item_id: &str,
        version: u64,
    ) -> Result<Value, ClientError> {
        let sid = encode_segment(session_id);
        let iid = encode_segment(item_id);
        self.request_value(
            Method::POST,
            &format!("/api/sessions/{sid}/queue/{iid}/retry"),
            Some(&json!({ "version": version })),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn select_model(
        &self,
        session_id: Option<&str>,
        provider: &str,
        model_id: &str,
    ) -> Result<Value, ClientError> {
        let mut body = serde_json::Map::new();
        body.insert("provider".into(), Value::String(provider.to_owned()));
        body.insert("modelId".into(), Value::String(model_id.to_owned()));
        if let Some(sid) = session_id {
            body.insert("sessionId".into(), Value::String(sid.to_owned()));
        }
        self.request_value(
            Method::POST,
            "/api/models/select",
            Some(&Value::Object(body)),
            None,
            Some(Duration::from_secs(10)),
        )
        .await
    }

    pub async fn set_session_thinking_level(
        &self,
        session_id: &str,
        level: &str,
    ) -> Result<Value, ClientError> {
        self.request_value(
            Method::POST,
            "/api/models/thinking",
            Some(&json!({ "sessionId": session_id, "level": level })),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    }

    pub async fn get_provider_quota(
        &self,
        provider: &str,
        refresh: bool,
    ) -> Result<Value, ClientError> {
        let p = encode_segment(provider);
        let path = if refresh {
            format!("/api/quota?provider={p}&refresh=1")
        } else {
            format!("/api/quota?provider={p}")
        };
        self.request_value(
            Method::GET,
            &path,
            None,
            None,
            Some(Duration::from_secs(10)),
        )
        .await
    }

    pub async fn search_files(
        &self,
        session_id: Option<&str>,
        query: &str,
    ) -> Result<Vec<String>, ClientError> {
        let q = encode_segment(query);
        let path = match session_id {
            Some(sid) => format!("/api/files?sessionId={}&query={q}", encode_segment(sid)),
            None => format!("/api/files?query={q}"),
        };
        let val = self
            .request_value(Method::GET, &path, None, None, Some(Duration::from_secs(5)))
            .await?;
        let files = val
            .get("files")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("path").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(files)
    }

    pub async fn get_session_attachment_bytes(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, ClientError> {
        let sid = encode_segment(session_id);
        let aid = encode_segment(attachment_id);
        self.request_bytes(&format!("/api/sessions/{sid}/attachments/{aid}"), None)
            .await
    }
}

fn encode_segment(raw: &str) -> String {
    url::form_urlencoded::byte_serialize(raw.as_bytes()).collect()
}

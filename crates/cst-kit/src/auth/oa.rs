use std::sync::Arc;
use std::time::Duration;

use pi_ai::{AuthEvent, AuthInfoLink, AuthInteraction, BoxFuture, OAuthCredential};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{Channel, device, storage::JsonFiles};
use crate::tools::ssrf::{HttpRequest, HttpResponse, HttpTransport};

pub const LOGIN_REQUIRED: &str = "cstoa_login_required";
fn login_required() -> String {
    format!("{LOGIN_REQUIRED}: 登录状态已失效，请重新登录。")
}
pub fn requires_login(error: &str) -> bool {
    error.contains(LOGIN_REQUIRED)
}

pub trait Clock: Send + Sync {
    fn now(&self) -> u64;
    fn sleep(
        &self,
        duration: Duration,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<(), String>>;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> u64 {
        pi_ai::now_ms()
    }
    fn sleep(
        &self,
        duration: Duration,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            tokio::select! { _ = cancel.cancelled() => Err("Login cancelled".into()), _ = tokio::time::sleep(duration) => Ok(()) }
        })
    }
}

/// Explicit host configuration belongs to the trusted launcher, never to environment variables or model output.
#[derive(Clone)]
pub struct OaClient {
    host: Url,
    transport: Arc<dyn HttpTransport>,
    pub(crate) clock: Arc<dyn Clock>,
}
impl OaClient {
    pub fn official() -> Result<Self, String> {
        Self::new("https://www.cstoa.top")
    }
    pub fn new(host: &str) -> Result<Self, String> {
        Self::with_transport(host, Arc::new(JsonHttp::new()?), Arc::new(SystemClock))
    }
    pub fn with_transport(
        host: &str,
        transport: Arc<dyn HttpTransport>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, String> {
        let mut host = validate_base(host)?;
        if host.path() != "/" && !host.path().is_empty() {
            return Err("OA 地址须为站点根地址。".into());
        }
        host.set_path("/");
        Ok(Self {
            host,
            transport,
            clock,
        })
    }
    pub fn host(&self) -> &str {
        self.host.as_str().trim_end_matches('/')
    }
    pub fn model_base(&self, credential: &pi_ai::Credential) -> String {
        format!(
            "{}{}",
            self.host(),
            if matches!(credential, pi_ai::Credential::OAuth(_)) {
                "/api/agent/llm/v1"
            } else {
                "/v1"
            }
        )
    }
    pub(crate) async fn request(
        &self,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
        cancel: CancellationToken,
        seconds: u64,
    ) -> Result<(u16, Value), String> {
        let url = format!("{}{path}", self.host());
        self.request_url(url, body, token, cancel, seconds).await
    }
    pub(crate) async fn request_url(
        &self,
        url: String,
        body: Option<Value>,
        token: Option<&str>,
        cancel: CancellationToken,
        seconds: u64,
    ) -> Result<(u16, Value), String> {
        let mut headers = vec![("Accept".into(), "application/json".into())];
        if let Some(token) = token {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }
        let request = self
            .transport
            .request(HttpRequest { url, body, headers }, cancel.clone());
        let response = tokio::select! { biased;
            _ = cancel.cancelled() => return Err("Login cancelled".into()),
            result = tokio::time::timeout(Duration::from_secs(seconds), request) => result.ok().and_then(Result::ok).ok_or("无法连接账号服务，请检查网络后重试。")?,
        };
        if (300..400).contains(&response.status) {
            return Err("账号服务拒绝 HTTP 重定向，请检查服务地址。".into());
        }
        if response.body.len() > 2 * 1024 * 1024 {
            return Err("账号服务响应超过大小限制。".into());
        }
        let value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        Ok((response.status, value))
    }
    pub(crate) async fn login(
        &self,
        files: &JsonFiles,
        channel: Channel,
        interaction: &AuthInteraction,
    ) -> Result<OAuthCredential, String> {
        let cancel = interaction.cancel_token.clone().unwrap_or_default();
        if cancel.is_cancelled() {
            return Err("Login cancelled".into());
        }
        let id = device::get(files).await?;
        let (status, body) = self.request("/api/oauth/device_authorization", Some(json!({
            "device_id": id, "name": "cst-pilot", "platform": "Windows", "agent_version": env!("CARGO_PKG_VERSION"), "scope": ["llm:chat"]
        })), None, cancel.clone(), 30).await?;
        if !(200..300).contains(&status) {
            return Err(format!("设备授权申请失败（HTTP {status}），请稍后重试。"));
        }
        let device_code = text(&body, "device_code", 8192).ok_or("设备授权响应缺少必要字段。")?;
        let url = text(&body, "verification_uri_complete", 8192)
            .and_then(|s| Url::parse(s).ok())
            .ok_or("设备授权链接无效。")?;
        if url.origin() != self.host.origin()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("设备授权链接与 OA 站点不一致。".into());
        }
        let expires = number(&body, "expires_in", 300).min(900);
        let mut interval = number(&body, "interval", 5).clamp(1, 60);
        let deadline = self.clock.now().saturating_add(expires * 1000);
        match channel {
            Channel::Tui => interaction.notify_user(AuthEvent::Info {
                message: "请在手机上打开授权链接，完成 OA 登录与动态验证码验证。等待授权中，按 Esc 取消。".into(),
                links: Some(vec![AuthInfoLink { url: url.into(), label: Some("授权链接".into()) }]),
            }),
            Channel::Gui => interaction.notify_user(AuthEvent::DeviceCode {
                user_code: String::new(), verification_uri: url.into(), open_browser_on_confirm: Some(false), copy_code_on_confirm: Some(false),
                interval_seconds: Some(interval), expires_in_seconds: Some(expires),
            }),
        }
        loop {
            let remaining = deadline.saturating_sub(self.clock.now());
            if remaining == 0 {
                return Err("设备码已过期，请重新发起登录。".into());
            }
            self.clock
                .sleep(
                    Duration::from_millis((interval * 1000).min(remaining)),
                    cancel.clone(),
                )
                .await?;
            if self.clock.now() >= deadline {
                return Err("设备码已过期，请重新发起登录。".into());
            }
            let remaining = Duration::from_millis(deadline.saturating_sub(self.clock.now()));
            let result = tokio::time::timeout(remaining, self.request("/api/oauth/token", Some(json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code", "device_code": device_code,
            })), None, cancel.clone(), 30)).await.map_err(|_| "设备码已过期，请重新发起登录。")??;
            let (status, value) = result;
            if (200..300).contains(&status) {
                return self.parse_credential(&value);
            }
            match value.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => (),
                Some("slow_down") => interval = interval.saturating_add(5),
                Some("access_denied") => return Err("授权已被拒绝，请重新发起登录。".into()),
                Some("expired_token") => return Err("设备码已过期，请重新发起登录。".into()),
                Some("invalid_grant" | "invalid_request") => {
                    return Err("设备码无效，请重新发起登录。".into());
                }
                _ => return Err(format!("令牌请求失败（HTTP {status}），请稍后重试。")),
            }
        }
    }
    fn parse_credential(&self, value: &Value) -> Result<OAuthCredential, String> {
        let access = text(value, "access_token", 16384)
            .ok_or("令牌响应缺少有效 access_token。")?
            .to_owned();
        let refresh = value
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("");
        if refresh.len() > 16384 {
            return Err("刷新令牌响应格式无效。".into());
        }
        Ok(OAuthCredential {
            access,
            refresh: refresh.into(),
            expires: self
                .clock
                .now()
                .saturating_add(number(value, "expires_in", 86400).min(7 * 86400) * 1000),
            extra: Default::default(),
        })
    }
    pub async fn refresh(
        &self,
        credential: OAuthCredential,
        cancel: CancellationToken,
    ) -> Result<OAuthCredential, String> {
        if cancel.is_cancelled() {
            return Err("Login cancelled".into());
        }
        if credential.refresh.is_empty() {
            return if self.clock.now() < credential.expires {
                Ok(credential)
            } else {
                Err(login_required())
            };
        }
        let result = self
            .request(
                "/api/oauth/token",
                Some(json!({"grant_type": "refresh_token", "refresh_token": credential.refresh})),
                None,
                cancel.clone(),
                30,
            )
            .await;
        if cancel.is_cancelled() {
            return Err("Login cancelled".into());
        }
        match result {
            Ok((status, body))
                if status == 401
                    || status == 403
                    || body.get("error").and_then(Value::as_str) == Some("invalid_grant") =>
            {
                Err(login_required())
            }
            Ok((status, body)) if (200..300).contains(&status) => {
                match self.parse_credential(&body) {
                    Ok(next) => Ok(next),
                    Err(_) if self.clock.now() < credential.expires => Ok(credential),
                    Err(_) => Err(login_required()),
                }
            }
            _ if self.clock.now() < credential.expires => Ok(credential),
            _ => Err(login_required()),
        }
    }
    pub(crate) async fn validate_key(
        &self,
        key: &str,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let (status, body) = self
            .request("/api/agent/key-profile", None, Some(key), cancel, 8)
            .await?;
        if status == 401 || status == 403 || body.get("code").and_then(Value::as_i64) == Some(19008)
        {
            return Err(login_required());
        }
        if !(200..300).contains(&status)
            || body.get("result") != Some(&Value::Bool(true))
            || text(&body["data"], "memberId", 64).is_none()
        {
            return Err("CSTOA 账号服务暂不可用，请稍后重试。".into());
        }
        Ok(())
    }
}
fn text<'a>(value: &'a Value, name: &str, limit: usize) -> Option<&'a str> {
    value
        .get(name)?
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= limit && !s.chars().any(char::is_control))
}
fn number(value: &Value, name: &str, default: u64) -> u64 {
    value
        .get(name)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(default)
}
pub(crate) fn validate_base(value: &str) -> Result<Url, String> {
    let url = Url::parse(value.trim()).map_err(|_| "服务地址无效。")?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("服务地址须为 HTTPS 或本机 HTTP，且不包含凭据、查询或片段。".into());
    }
    Ok(url)
}

struct JsonHttp(reqwest::Client);
impl JsonHttp {
    fn new() -> Result<Self, String> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map(Self)
            .map_err(|_| "账号网络客户端初始化失败。".into())
    }
}
impl HttpTransport for JsonHttp {
    fn request(
        &self,
        request: HttpRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>> {
        Box::pin(async move {
            let task = async {
                let mut query = if let Some(body) = request.body {
                    self.0.post(&request.url).json(&body)
                } else {
                    self.0.get(&request.url)
                };
                for (key, value) in request.headers {
                    query = query.header(key, value);
                }
                let mut response = query.send().await.map_err(|_| "账号服务网络请求失败。")?;
                let status = response.status().as_u16();
                let mut bytes = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| "账号服务响应读取失败。")?
                {
                    if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                        return Err("账号响应超过 2 MiB。".into());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(HttpResponse {
                    status,
                    body: String::from_utf8(bytes).map_err(|_| "账号服务响应编码无效。")?,
                })
            };
            tokio::select! { biased; _ = cancel.cancelled() => Err("Login cancelled".into()), result = task => result }
        })
    }
}

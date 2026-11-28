use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use pi_agent::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Default)]
pub struct NetworkPolicy {
    pub allow_tun: bool,
}

impl NetworkPolicy {
    pub fn validate_url(&self, value: &str) -> Result<Url, String> {
        let url = Url::parse(value).map_err(|_| "URL 无效。")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("仅允许不含登录凭据的 HTTP 或 HTTPS 地址。".into());
        }
        if let Some(host) = url.host_str() {
            let host = host.trim_end_matches('.').to_lowercase();
            if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
                return Err("网络策略禁止访问本机或内网地址。".into());
            }
        }
        match url.host() {
            Some(Host::Ipv4(ip)) => self.validate_ip(ip.into())?,
            Some(Host::Ipv6(ip)) => self.validate_ip(ip.into())?,
            _ => {}
        }
        Ok(url)
    }

    pub fn validate_ip(&self, ip: IpAddr) -> Result<(), String> {
        let allowed = match ip {
            IpAddr::V4(ip) => public_v4(ip, self.allow_tun),
            IpAddr::V6(ip) => {
                if let Some(v4) = ip.to_ipv4_mapped() {
                    public_v4(v4, self.allow_tun)
                } else {
                    public_v6(ip, self.allow_tun)
                }
            }
        };
        if allowed {
            Ok(())
        } else {
            Err("网络策略禁止访问本机或内网地址。".into())
        }
    }
}

fn tun_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 198 && (b == 18 || b == 19)
}

fn tun_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    // Standard Mihomo / Clash / Sing-box Fake-IPv6 ranges (`fc00::/18` and `fdfe:dcba:9876::/48`).
    (segments[0] == 0xfc00 && (segments[1] & 0xc000) == 0)
        || (segments[0] == 0xfdfe && segments[1] == 0xdcba && segments[2] == 0x9876)
}

fn ula_v6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn public_v4(ip: Ipv4Addr, allow_tun: bool) -> bool {
    if tun_v4(ip) {
        return allow_tun;
    }
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn public_v6(ip: Ipv6Addr, allow_tun: bool) -> bool {
    if tun_v6(ip) {
        return allow_tun;
    }
    let segments = ip.segments();
    // Only global unicast. This also excludes loopback, link-local, ULA,
    // multicast, IPv4-compatible addresses and NAT64 local translation ranges.
    (segments[0] & 0xe000) == 0x2000
        && !(segments[0] == 0x2001 && segments[1] < 0x0200)
        && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        && segments[0] != 0x2002
        && !(segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
}

pub trait DnsResolver: Send + Sync {
    fn resolve(&self, host: String, port: u16) -> BoxFuture<'_, Result<Vec<SocketAddr>, String>>;
}

struct SystemDns;
impl DnsResolver for SystemDns {
    fn resolve(&self, host: String, port: u16) -> BoxFuture<'_, Result<Vec<SocketAddr>, String>> {
        Box::pin(async move {
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .map(|ips| ips.collect())
                .map_err(|_| "域名解析失败。".into())
        })
    }
}

#[derive(Clone, Default)]
pub struct HttpRequest {
    pub url: String,
    pub body: Option<serde_json::Value>,
    /// Secret values are never included in Debug or transport errors.
    pub headers: Vec<(String, String)>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait HttpTransport: Send + Sync {
    fn request(
        &self,
        request: HttpRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>>;
}

#[derive(Clone)]
pub struct SafeHttpClient {
    policy: NetworkPolicy,
    resolver: Arc<dyn DnsResolver>,
    connector: Arc<dyn Connector>,
}

impl SafeHttpClient {
    pub fn new(policy: NetworkPolicy) -> Self {
        Self {
            policy,
            resolver: Arc::new(SystemDns),
            connector: Arc::new(ReqwestConnector),
        }
    }

    pub fn with_resolver(policy: NetworkPolicy, resolver: Arc<dyn DnsResolver>) -> Self {
        Self {
            policy,
            resolver,
            connector: Arc::new(ReqwestConnector),
        }
    }

    pub async fn resolve_target(&self, value: &str) -> Result<(Url, Vec<SocketAddr>), String> {
        let url = self.policy.validate_url(value)?;
        let port = url.port_or_known_default().ok_or("URL 缺少端口。")?;
        let addresses = match url.host().ok_or("URL 缺少主机。")? {
            Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
            Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
            Host::Domain(host) => self.resolver.resolve(host.into(), port).await?,
        };
        if addresses.is_empty() {
            return Err("域名没有可用地址。".into());
        }
        // When dual-stack TUN returns `198.18.0.0/15` alongside a custom ULA Fake-IPv6 (`fd00::/8`),
        // strip the non-routed ULA companion while still rejecting mixed public/private answers.
        let addresses = if self.policy.allow_tun
            && addresses.iter().any(|a| match a.ip() {
                IpAddr::V4(v4) => tun_v4(v4),
                IpAddr::V6(v6) => v6.to_ipv4_mapped().is_some_and(tun_v4),
            }) && addresses.iter().all(|a| {
            self.policy.validate_ip(a.ip()).is_ok()
                || matches!(a.ip(), IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() && ula_v6(v6))
        }) {
            addresses
                .into_iter()
                .filter(|a| self.policy.validate_ip(a.ip()).is_ok())
                .collect::<Vec<_>>()
        } else {
            addresses
        };
        if addresses.is_empty() {
            return Err("域名没有可用地址。".into());
        }
        // Mixed public/private answers are rejected as a whole.
        for address in &addresses {
            self.policy.validate_ip(address.ip())?;
        }
        Ok((url, addresses))
    }

    pub async fn preflight_url(
        &self,
        value: &str,
        cancel: CancellationToken,
        timeout_ms: u64,
    ) -> Result<Url, String> {
        let clamped = timeout_ms.clamp(1_000, 180_000);
        let timeout = Duration::from_millis(clamped);
        let (url, _) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("请求已取消。".into()),
            res = tokio::time::timeout(timeout, self.resolve_target(value)) => {
                res.map_err(|_| format!("域名解析超过 {} 秒。", clamped.div_ceil(1000)))??
            }
        };
        Ok(url)
    }

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let mut target = request.url.clone();
        for hop in 0..=MAX_REDIRECTS {
            let (url, addresses) = self.resolve_target(&target).await?;
            let response = self
                .connector
                .send(url.clone(), addresses, request.clone())
                .await?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECTS || request.body.is_some() || !request.headers.is_empty() {
                    return Err("网络请求重定向被拒绝。".into());
                }
                let location = response.location.as_deref().ok_or("重定向地址无效。")?;
                let next = url.join(location).map_err(|_| "重定向地址无效。")?;
                if url.scheme() == "https" && next.scheme() != "https" {
                    return Err("禁止从 HTTPS 重定向到 HTTP。".into());
                }
                target = next.to_string();
                continue;
            }
            return Ok(HttpResponse {
                status: response.status,
                body: response.body,
            });
        }
        Err("网络请求重定向次数超限。".into())
    }
}

impl HttpTransport for SafeHttpClient {
    fn request(
        &self,
        request: HttpRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>> {
        let timeout_ms = request
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1_000, 180_000);
        let timeout = Duration::from_millis(timeout_ms);
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err("请求已取消。".into()),
                result = tokio::time::timeout(timeout, self.send(request)) => {
                    result.map_err(|_| format!("网络请求超过 {} 秒。", timeout_ms.div_ceil(1000)))?
                }
            }
        })
    }
}

struct HttpHop {
    status: u16,
    location: Option<String>,
    body: String,
}

trait Connector: Send + Sync {
    fn send(
        &self,
        url: Url,
        addresses: Vec<SocketAddr>,
        request: HttpRequest,
    ) -> BoxFuture<'_, Result<HttpHop, String>>;
}

fn is_binary_content_type(content_type: Option<&str>) -> bool {
    let Some(raw) = content_type else {
        return false;
    };
    let mime = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime.is_empty() {
        return false;
    }
    mime.starts_with("image/")
        || mime.starts_with("audio/")
        || mime.starts_with("video/")
        || mime.starts_with("font/")
        || matches!(
            mime.as_str(),
            "application/pdf"
                | "application/octet-stream"
                | "application/zip"
                | "application/gzip"
                | "application/x-gzip"
                | "application/x-tar"
                | "application/x-rar-compressed"
                | "application/x-7z-compressed"
                | "application/wasm"
                | "application/msword"
        )
        || mime.starts_with("application/vnd.")
}

pub(crate) fn decode_http_body(bytes: &[u8], content_type: Option<&str>) -> Result<String, String> {
    if is_binary_content_type(content_type) {
        return Err("网络响应为不支持的二进制或媒体类型。".into());
    }
    if memchr::memchr(0, bytes).is_some() {
        return Err("网络正文不是支持的文本编码。".into());
    }
    let prefer_gb = content_type.is_some_and(|ct| {
        let lower = ct.to_ascii_lowercase();
        lower.contains("charset=gb")
            || lower.contains("charset=\"gb")
            || lower.contains("gbk")
            || lower.contains("gb2312")
            || lower.contains("gb18030")
    });
    if !prefer_gb && let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_owned());
    }
    if let Some(decoded) =
        encoding_rs::GB18030.decode_without_bom_handling_and_without_replacement(bytes)
    {
        return Ok(decoded.into_owned());
    }
    if prefer_gb && let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_owned());
    }
    Err("网络正文不是 UTF-8 或 GB18030 文本。".into())
}

struct ReqwestConnector;
impl Connector for ReqwestConnector {
    fn send(
        &self,
        url: Url,
        addresses: Vec<SocketAddr>,
        request: HttpRequest,
    ) -> BoxFuture<'_, Result<HttpHop, String>> {
        let timeout = Duration::from_millis(
            request
                .timeout_ms
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(1_000, 180_000),
        );
        Box::pin(async move {
            let host = url.host_str().ok_or("URL 缺少主机。")?;
            // Pin validated DNS results; proxies and automatic redirects stay disabled.
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(timeout)
                .resolve_to_addrs(host, &addresses)
                .build()
                .map_err(|_| "网络客户端初始化失败。")?;
            let mut builder = if let Some(body) = &request.body {
                client.post(url).json(body)
            } else {
                client.get(url)
            };
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            let mut response = builder
                .send()
                .await
                .map_err(|_| "网络请求失败，请检查网络后重试。")?;
            let status = response.status().as_u16();
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                return Ok(HttpHop {
                    status,
                    location,
                    body: String::new(),
                });
            }
            if (200..300).contains(&status) && is_binary_content_type(content_type.as_deref()) {
                return Err("网络响应为不支持的二进制或媒体类型。".into());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "网络正文读取失败。")?
            {
                if bytes.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err("网络正文超过 2 MiB，请缩小请求范围。".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = decode_http_body(&bytes, content_type.as_deref())?;
            Ok(HttpHop {
                status,
                location,
                body,
            })
        })
    }
}

#[cfg(test)]
#[path = "ssrf_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsrfConfig {
    #[serde(default = "default_allow_ranges")]
    pub allow_ranges: Vec<String>,
    #[serde(default)]
    pub trust_env_proxy: bool,
}

fn default_allow_ranges() -> Vec<String> {
    vec!["198.18.0.0/15".to_owned()]
}

impl Default for SsrfConfig {
    fn default() -> Self {
        Self {
            allow_ranges: default_allow_ranges(),
            trust_env_proxy: false,
        }
    }
}

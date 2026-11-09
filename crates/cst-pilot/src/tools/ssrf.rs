use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use pi_agent::BoxFuture;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

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
                    public_v6(ip)
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

fn public_v4(ip: Ipv4Addr, allow_tun: bool) -> bool {
    let [a, b, c, _] = ip.octets();
    if a == 198 && (b == 18 || b == 19) {
        return allow_tun;
    }
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

fn public_v6(ip: Ipv6Addr) -> bool {
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

#[derive(Clone)]
pub struct HttpRequest {
    pub url: String,
    pub body: Option<serde_json::Value>,
    /// Secret values are never included in Debug or transport errors.
    pub headers: Vec<(String, String)>,
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
        // Mixed public/private answers are rejected as a whole.
        for address in &addresses {
            self.policy.validate_ip(address.ip())?;
        }
        Ok((url, addresses))
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
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err("请求已取消。".into()),
                result = tokio::time::timeout(Duration::from_secs(10), self.send(request)) => {
                    result.map_err(|_| "网络请求超过 10 秒。".to_owned())?
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

struct ReqwestConnector;
impl Connector for ReqwestConnector {
    fn send(
        &self,
        url: Url,
        addresses: Vec<SocketAddr>,
        request: HttpRequest,
    ) -> BoxFuture<'_, Result<HttpHop, String>> {
        Box::pin(async move {
            let host = url.host_str().ok_or("URL 缺少主机。")?;
            // Pin validated DNS results; proxies and automatic redirects stay disabled.
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
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
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                return Ok(HttpHop {
                    status,
                    location,
                    body: String::new(),
                });
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "网络正文读取失败。")?
            {
                if bytes.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err("网络正文超过 2 MiB，请缩小请求范围。".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = String::from_utf8(bytes).map_err(|_| "网络正文不是 UTF-8 文本。")?;
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

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SsrfConfig {
    #[serde(default)]
    pub allow_ranges: Vec<String>,
}

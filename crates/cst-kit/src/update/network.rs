use crate::tools::ssrf::{NetworkPolicy, SafeHttpClient};
use futures_util::{Stream, StreamExt};
use pi_agent::BoxFuture;
use std::error::Error as StdError;
use std::fmt;
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use url::Url;

pub const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;
const MAX_RETRY_AFTER_SECS: u64 = 86_400;

pub type Body = Pin<Box<dyn Stream<Item = Result<Vec<u8>, String>> + Send>>;
pub struct Download {
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateError {
    OfflineOrDns,
    Timeout {
        phase: &'static str,
        received_bytes: u64,
    },
    PortalOrCrossOriginRedirect {
        status: Option<u16>,
    },
    TlsVerification,
    ChannelEmpty,
    ReleaseNotFound,
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    BandwidthBudgetExceeded {
        retry_after_secs: Option<u64>,
    },
    ServiceUnavailable {
        status: u16,
        code: Option<String>,
        retry_after_secs: Option<u64>,
    },
    BelowMinUpgradable,
    InsufficientDiskSpace,
    VerificationFailed {
        stage: &'static str,
    },
    DisallowedAddress,
    Cancelled,
    Other,
}

impl UpdateError {
    pub fn is_manual_upgrade_required(&self) -> bool {
        matches!(self, Self::BelowMinUpgradable)
    }

    pub fn ui_message(&self, is_download: bool) -> String {
        match self {
            Self::OfflineOrDns => "无法连接更新服务器，请检查网络连接后重试".into(),
            Self::Timeout { phase, .. } => match *phase {
                "manifest" | "connect" => "连接更新服务器超时（超过 10 秒），请稍后重试".into(),
                "download_chunk" => "连接更新服务器超时（30 秒无数据），请稍后重试".into(),
                "download_total" => "连接更新服务器超时（超过 10 分钟），请稍后重试".into(),
                _ => "连接更新服务器超时，请稍后重试".into(),
            },
            Self::PortalOrCrossOriginRedirect { .. } => {
                "网络可能存在认证拦截，请先完成校园网登录后重试".into()
            }
            Self::TlsVerification => "安全连接建立失败，请检查本机系统时间或网络环境".into(),
            Self::ChannelEmpty => "暂未发布在线更新".into(),
            Self::ReleaseNotFound => "更新版本已变更，请重新检查更新".into(),
            Self::RateLimited {
                retry_after_secs: Some(secs),
            } => format!("请求过于频繁，请于 {secs} 秒后重试"),
            Self::RateLimited {
                retry_after_secs: None,
            } => "请求过于频繁，请稍后重试".into(),
            Self::BandwidthBudgetExceeded { .. } => {
                "当前下载人数较多，更新通道暂歇，请稍后重试".into()
            }
            Self::ServiceUnavailable { .. } => "更新服务暂时不可用，请稍后重试".into(),
            Self::BelowMinUpgradable => "当前版本过旧，无法原位升级，请重新下载完整便携包".into(),
            Self::InsufficientDiskSpace => "工具包所在磁盘空间不足，请清理后重试".into(),
            Self::VerificationFailed { .. } => {
                "更新包安全校验未通过，已停止升级，当前程序不受影响".into()
            }
            Self::DisallowedAddress => "网络策略禁止访问本机或内网地址。".into(),
            Self::Cancelled => {
                if is_download {
                    "升级准备已取消。".into()
                } else {
                    "升级检查已取消。".into()
                }
            }
            Self::Other => {
                if is_download {
                    "升级包准备失败，当前程序与用户数据保持不变。".into()
                } else {
                    "升级检查未完成，请稍后重试。".into()
                }
            }
        }
    }

    /// Sanitized diagnostic summary without credentials, URLs, or untrusted upstream strings.
    pub fn log_summary(&self) -> String {
        match self {
            Self::OfflineOrDns => "offline_or_dns".into(),
            Self::Timeout {
                phase,
                received_bytes,
            } => format!("timeout(phase={phase}, received_bytes={received_bytes})"),
            Self::PortalOrCrossOriginRedirect { status } => {
                format!("portal_or_redirect(status={status:?})")
            }
            Self::TlsVerification => "tls_verification_failed".into(),
            Self::ChannelEmpty => "http_404(code=channel_empty)".into(),
            Self::ReleaseNotFound => "http_404(code=release_not_found)".into(),
            Self::RateLimited { retry_after_secs } => {
                format!("http_429(code=rate_limited, retry_after={retry_after_secs:?})")
            }
            Self::BandwidthBudgetExceeded { retry_after_secs } => format!(
                "http_503(code=bandwidth_budget_exceeded, retry_after={retry_after_secs:?})"
            ),
            Self::ServiceUnavailable {
                status,
                code,
                retry_after_secs,
            } => format!("http_{status}(code={code:?}, retry_after={retry_after_secs:?})"),
            Self::BelowMinUpgradable => "below_min_upgradable_version".into(),
            Self::InsufficientDiskSpace => "insufficient_disk_space".into(),
            Self::VerificationFailed { stage } => format!("verification_failed(stage={stage})"),
            Self::DisallowedAddress => "disallowed_address".into(),
            Self::Cancelled => "cancelled".into(),
            Self::Other => "other".into(),
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.ui_message(false))
    }
}

pub trait Fetcher: Send + Sync {
    fn get(
        &self,
        url: String,
        limit: u64,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, String>>;

    fn fetch(
        &self,
        url: String,
        limit: u64,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, UpdateError>> {
        Box::pin(async move {
            self.get(url, limit, cancel)
                .await
                .map_err(|err| classify_legacy_fetcher_error(&err))
        })
    }
}

fn classify_legacy_fetcher_error(err: &str) -> UpdateError {
    if err.contains("取消") {
        UpdateError::Cancelled
    } else if err.contains("内网") || err.contains("本机") {
        UpdateError::DisallowedAddress
    } else if err.contains("跨源") || err.contains("重定向") {
        UpdateError::PortalOrCrossOriginRedirect { status: None }
    } else if err.contains("超时") || (err.contains("超过") && err.contains("秒")) {
        UpdateError::Timeout {
            phase: "request",
            received_bytes: 0,
        }
    } else {
        UpdateError::Other
    }
}

#[derive(Clone)]
pub struct NativeFetcher {
    safe: SafeHttpClient,
}

impl Default for NativeFetcher {
    fn default() -> Self {
        Self {
            safe: SafeHttpClient::new(NetworkPolicy::default()),
        }
    }
}

impl NativeFetcher {
    pub fn new(policy: NetworkPolicy) -> Self {
        Self {
            safe: SafeHttpClient::new(policy),
        }
    }

    pub fn with_resolver(
        policy: NetworkPolicy,
        resolver: Arc<dyn crate::tools::ssrf::DnsResolver>,
    ) -> Self {
        Self {
            safe: SafeHttpClient::with_resolver(policy, resolver),
        }
    }
}

pub fn release_url(text: &str) -> Result<Url, String> {
    cst_updater::manifest::validate_release_url(text)
}

pub(crate) fn redirect(original: &Url, current: &Url, location: &str) -> Result<Url, String> {
    let next = release_url(
        current
            .join(location)
            .map_err(|_| "升级重定向地址无效。")?
            .as_str(),
    )?;
    if next.origin() != original.origin() {
        return Err("升级请求禁止跨源重定向。".into());
    }
    Ok(next)
}

pub fn parse_retry_after_seconds(raw: Option<&str>) -> Option<u64> {
    let text = raw?.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let parsed = text.parse::<u64>().ok()?;
    Some(parsed.clamp(1, MAX_RETRY_AFTER_SECS))
}

pub(crate) fn looks_like_html_body(bytes: &[u8]) -> bool {
    let trimmed = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|start| &bytes[start..])
        .unwrap_or(&[]);
    let head_len = trimmed.len().min(32);
    let head = String::from_utf8_lossy(&trimmed[..head_len]).to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

fn looks_like_html(content_type: Option<&str>, body: &[u8]) -> bool {
    content_type
        .map(|ct| ct.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false)
        || looks_like_html_body(body)
}

fn extract_error_code(body: &[u8]) -> Option<String> {
    if body.is_empty() || body.len() > MAX_ERROR_BODY_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let code = value.get("error")?.get("code")?.as_str()?.trim();
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    Some(code.to_owned())
}

pub fn parse_http_error(
    status: u16,
    retry_after_header: Option<&str>,
    content_type: Option<&str>,
    body: &[u8],
) -> UpdateError {
    let retry_after_secs = parse_retry_after_seconds(retry_after_header);
    let code = extract_error_code(body);
    match (status, code.as_deref()) {
        (404, Some("channel_empty")) => UpdateError::ChannelEmpty,
        (404, Some("release_not_found")) => UpdateError::ReleaseNotFound,
        (429, _) | (_, Some("rate_limited")) => UpdateError::RateLimited { retry_after_secs },
        (503, Some("bandwidth_budget_exceeded")) => {
            UpdateError::BandwidthBudgetExceeded { retry_after_secs }
        }
        (503, Some("service_starting" | "server_busy")) => UpdateError::ServiceUnavailable {
            status: 503,
            code,
            retry_after_secs,
        },
        (500..=599, _) => UpdateError::ServiceUnavailable {
            status,
            code,
            retry_after_secs,
        },
        _ if looks_like_html(content_type, body) => UpdateError::PortalOrCrossOriginRedirect {
            status: Some(status),
        },
        _ => UpdateError::ServiceUnavailable {
            status,
            code,
            retry_after_secs,
        },
    }
}

fn classify_reqwest_error(err: &reqwest::Error, phase: &'static str) -> UpdateError {
    if err.is_timeout() {
        return UpdateError::Timeout {
            phase,
            received_bytes: 0,
        };
    }
    let mut current: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(source) = current {
        let lower = source.to_string().to_ascii_lowercase();
        if lower.contains("certificate")
            || lower.contains("tls")
            || lower.contains("ssl")
            || lower.contains("handshake")
            || lower.contains("webpki")
            || lower.contains("unknownissuer")
            || lower.contains("unknown issuer")
            || lower.contains("notvalidyet")
            || lower.contains("expired")
            || lower.contains("rustls")
        {
            return UpdateError::TlsVerification;
        }
        if lower.contains("timed out") || lower.contains("timeout") {
            return UpdateError::Timeout {
                phase,
                received_bytes: 0,
            };
        }
        current = source.source();
    }
    if err.is_connect() {
        return UpdateError::OfflineOrDns;
    }
    UpdateError::OfflineOrDns
}

fn classify_resolve_error(err: &str) -> UpdateError {
    if err.contains("域名解析失败") {
        UpdateError::OfflineOrDns
    } else if err.contains("内网")
        || err.contains("本机")
        || err.contains("仅允许")
        || err.contains("URL")
    {
        UpdateError::DisallowedAddress
    } else {
        UpdateError::OfflineOrDns
    }
}

async fn read_bounded_error_body(mut response: reqwest::Response) -> Vec<u8> {
    let mut buf = Vec::new();
    while buf.len() < MAX_ERROR_BODY_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = MAX_ERROR_BODY_BYTES - buf.len();
                let take = chunk.len().min(remaining);
                buf.extend_from_slice(&chunk[..take]);
                if chunk.len() >= remaining {
                    break;
                }
            }
            _ => break,
        }
    }
    buf
}

impl Fetcher for NativeFetcher {
    fn get(
        &self,
        url: String,
        limit: u64,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, String>> {
        Box::pin(async move {
            self.fetch(url, limit, cancel)
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn fetch(
        &self,
        url: String,
        limit: u64,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, UpdateError>> {
        Box::pin(async move {
            let operation = async {
                let original = release_url(&url).map_err(|_| UpdateError::DisallowedAddress)?;
                let mut current = original.clone();
                for hop in 0..=5 {
                    let (url, addresses) = self
                        .safe
                        .resolve_target(current.as_str())
                        .await
                        .map_err(|e| classify_resolve_error(&e))?;
                    let host = url.host_str().ok_or(UpdateError::DisallowedAddress)?;
                    let client = reqwest::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .connect_timeout(Duration::from_secs(10))
                        .read_timeout(Duration::from_secs(30))
                        .resolve_to_addrs(host, &addresses)
                        .build()
                        .map_err(|_| UpdateError::Other)?;
                    let response = client
                        .get(url)
                        .send()
                        .await
                        .map_err(|e| classify_reqwest_error(&e, "connect"))?;
                    let status = response.status();
                    if status.is_redirection() {
                        if hop == 5 {
                            return Err(UpdateError::PortalOrCrossOriginRedirect {
                                status: Some(status.as_u16()),
                            });
                        }
                        let location = response
                            .headers()
                            .get(reqwest::header::LOCATION)
                            .and_then(|v| v.to_str().ok())
                            .ok_or(UpdateError::PortalOrCrossOriginRedirect {
                                status: Some(status.as_u16()),
                            })?;
                        current = redirect(&original, &current, location).map_err(|_| {
                            UpdateError::PortalOrCrossOriginRedirect {
                                status: Some(status.as_u16()),
                            }
                        })?;
                        continue;
                    }
                    if status != reqwest::StatusCode::OK {
                        let retry_after = response
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned);
                        let content_type = response
                            .headers()
                            .get(reqwest::header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned);
                        let body = read_bounded_error_body(response).await;
                        return Err(parse_http_error(
                            status.as_u16(),
                            retry_after.as_deref(),
                            content_type.as_deref(),
                            &body,
                        ));
                    }
                    if response.content_length().is_some_and(|n| n > limit) {
                        return Err(UpdateError::VerificationFailed {
                            stage: "content_length",
                        });
                    }
                    let body = futures_util::stream::try_unfold(
                        (response, 0u64),
                        move |(mut response, received)| async move {
                            match response.chunk().await {
                                Ok(Some(chunk)) => {
                                    let count = received
                                        .checked_add(chunk.len() as u64)
                                        .filter(|n| *n <= limit)
                                        .ok_or_else(|| "升级响应超过大小限制。".to_owned())?;
                                    Ok(Some((chunk.to_vec(), (response, count))))
                                }
                                Ok(None) => Ok(None),
                                Err(err) if err.is_timeout() => {
                                    Err("升级正文读取超时。".to_owned())
                                }
                                Err(_) => Err("升级正文读取失败。".to_owned()),
                            }
                        },
                    );
                    return Ok(Download {
                        body: Box::pin(body),
                    });
                }
                Err(UpdateError::PortalOrCrossOriginRedirect { status: None })
            };
            tokio::select! { biased;
                _ = cancel.cancelled() => Err(UpdateError::Cancelled),
                result = tokio::time::timeout(Duration::from_secs(10), operation) => result.map_err(|_| UpdateError::Timeout {
                    phase: "connect",
                    received_bytes: 0,
                })?,
            }
        })
    }
}

pub(crate) async fn read(
    fetcher: &dyn Fetcher,
    url: &str,
    limit: u64,
    cancel: CancellationToken,
) -> Result<Vec<u8>, UpdateError> {
    let mut received_bytes = 0u64;
    let operation = async {
        let mut response = fetcher.fetch(url.into(), limit, cancel.clone()).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.body.next().await {
            let chunk = chunk.map_err(|err| {
                if err.contains("超时") {
                    UpdateError::Timeout {
                        phase: "manifest_body",
                        received_bytes,
                    }
                } else if err.contains("超过大小限制") {
                    UpdateError::VerificationFailed {
                        stage: "manifest_size",
                    }
                } else {
                    UpdateError::OfflineOrDns
                }
            })?;
            if bytes.len() as u64 + chunk.len() as u64 > limit {
                return Err(UpdateError::VerificationFailed {
                    stage: "manifest_size",
                });
            }
            bytes.extend_from_slice(&chunk);
            received_bytes = bytes.len() as u64;
        }
        Ok(bytes)
    };
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(UpdateError::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(10), operation) => result.map_err(|_| UpdateError::Timeout {
            phase: "manifest",
            received_bytes,
        })?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_must_stay_https_same_origin() {
        let origin = release_url("https://www.cstoa.top/releases/latest").unwrap();
        assert_eq!(redirect(&origin, &origin, "../v1").unwrap().path(), "/v1");
        for bad in [
            "https://other.example/a",
            "http://www.cstoa.top/a",
            "https://127.0.0.1/a",
            "https://www.cstoa.top:444/a",
            "https://user@www.cstoa.top/a",
        ] {
            assert!(redirect(&origin, &origin, bad).is_err());
        }
    }

    #[test]
    fn parses_structured_server_errors_and_retry_after() {
        assert_eq!(
            parse_http_error(
                404,
                None,
                Some("application/json"),
                br#"{"error":{"code":"channel_empty","message":"no active release"}}"#,
            ),
            UpdateError::ChannelEmpty
        );
        assert_eq!(
            parse_http_error(
                404,
                None,
                Some("application/json"),
                br#"{"error":{"code":"release_not_found","message":"missing"}}"#,
            ),
            UpdateError::ReleaseNotFound
        );
        assert_eq!(
            parse_http_error(
                429,
                Some("45"),
                Some("application/json"),
                br#"{"error":{"code":"rate_limited","message":"too many requests"}}"#,
            ),
            UpdateError::RateLimited {
                retry_after_secs: Some(45)
            }
        );
        assert_eq!(
            UpdateError::RateLimited {
                retry_after_secs: Some(45)
            }
            .ui_message(true),
            "请求过于频繁，请于 45 秒后重试"
        );
        assert_eq!(
            parse_http_error(
                503,
                Some("120"),
                Some("application/json"),
                br#"{"error":{"code":"bandwidth_budget_exceeded","message":"exhausted"}}"#,
            ),
            UpdateError::BandwidthBudgetExceeded {
                retry_after_secs: Some(120)
            }
        );
        assert_eq!(
            parse_http_error(
                503,
                Some("5"),
                Some("application/json"),
                br#"{"error":{"code":"service_starting","message":"starting"}}"#,
            ),
            UpdateError::ServiceUnavailable {
                status: 503,
                code: Some("service_starting".into()),
                retry_after_secs: Some(5),
            }
        );
        assert_eq!(
            parse_http_error(
                403,
                None,
                Some("text/html; charset=utf-8"),
                b"<!DOCTYPE html><html><body>Campus Portal Login</body></html>",
            ),
            UpdateError::PortalOrCrossOriginRedirect { status: Some(403) }
        );
    }
}

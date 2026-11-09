use crate::tools::ssrf::{NetworkPolicy, SafeHttpClient};
use futures_util::{Stream, StreamExt};
use pi_agent::BoxFuture;
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use url::Url;

pub type Body = Pin<Box<dyn Stream<Item = Result<Vec<u8>, String>> + Send>>;
pub struct Download {
    pub body: Body,
}
pub trait Fetcher: Send + Sync {
    fn get(
        &self,
        url: String,
        limit: u64,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, String>>;
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
    let url = NetworkPolicy::default().validate_url(text)?;
    if url.scheme() != "https" || url.fragment().is_some() {
        return Err("升级源必须使用不含片段的 HTTPS 地址。".into());
    }
    Ok(url)
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
impl Fetcher for NativeFetcher {
    fn get(
        &self,
        url: String,
        limit: u64,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, String>> {
        Box::pin(async move {
            let operation = async {
                let original = release_url(&url)?;
                let mut current = original.clone();
                for hop in 0..=5 {
                    let (url, addresses) = self.safe.resolve_target(current.as_str()).await?;
                    let client = reqwest::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .timeout(Duration::from_secs(10))
                        .resolve_to_addrs(url.host_str().ok_or("升级地址无效。")?, &addresses)
                        .build()
                        .map_err(|_| "升级网络客户端无法初始化。")?;
                    let response = client.get(url).send().await.map_err(|_| "升级请求失败。")?;
                    if response.status().is_redirection() {
                        if hop == 5 {
                            return Err("升级重定向次数超限。".into());
                        }
                        let location = response
                            .headers()
                            .get(reqwest::header::LOCATION)
                            .and_then(|v| v.to_str().ok())
                            .ok_or("升级重定向地址无效。")?;
                        current = redirect(&original, &current, location)?;
                        continue;
                    }
                    if response.status() != reqwest::StatusCode::OK {
                        return Err("升级源未返回有效文件。".into());
                    }
                    if response.content_length().is_some_and(|n| n > limit) {
                        return Err("升级响应超过大小限制。".into());
                    }
                    let body = futures_util::stream::try_unfold(
                        (response, 0u64),
                        move |(mut response, received)| async move {
                            match response
                                .chunk()
                                .await
                                .map_err(|_| "升级正文读取失败。".to_owned())?
                            {
                                Some(chunk) => {
                                    let count = received
                                        .checked_add(chunk.len() as u64)
                                        .filter(|n| *n <= limit)
                                        .ok_or("升级响应超过大小限制。")?;
                                    Ok(Some((chunk.to_vec(), (response, count))))
                                }
                                None => Ok(None),
                            }
                        },
                    );
                    return Ok(Download {
                        body: Box::pin(body),
                    });
                }
                Err("升级重定向次数超限。".into())
            };
            tokio::select! { biased;
                _ = cancel.cancelled() => Err("升级请求已取消。".into()),
                result = tokio::time::timeout(Duration::from_secs(10), operation) => result.map_err(|_| "升级请求超过 10 秒。".to_owned())?,
            }
        })
    }
}
pub(crate) async fn read(
    fetcher: &dyn Fetcher,
    url: &str,
    limit: u64,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    let operation = async {
        let mut response = fetcher
            .get(url.into(), limit, cancel.clone())
            .await
            .map_err(|_| "升级清单请求失败。")?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.body.next().await {
            let chunk = chunk.map_err(|_| "升级清单正文读取失败。")?;
            if bytes.len() as u64 + chunk.len() as u64 > limit {
                return Err("升级清单超过大小限制。".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    };
    tokio::select! { biased;
        _ = cancel.cancelled() => Err("升级请求已取消。".into()),
        result = tokio::time::timeout(Duration::from_secs(10), operation) => result.map_err(|_| "升级请求超过 10 秒。".to_owned())?,
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
}

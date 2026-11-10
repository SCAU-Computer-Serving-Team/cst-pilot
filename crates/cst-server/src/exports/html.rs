use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use pi_agent::BoxFuture;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::{ServiceError, ServiceErrorKind};

pub const EXPORT_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'";
const MAX_ASSET: usize = 16 * 1024 * 1024;
const MAX_CACHE: usize = 24 * 1024 * 1024;
const MAX_HTML: usize = 200 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStyleSheet {
    pub css: String,
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDocumentSource {
    pub body: String,
    pub styles: Vec<ExportStyleSheet>,
}

pub type AssetByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

pub struct AssetResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: AssetByteStream,
}

pub type AssetFetcherFn = Arc<
    dyn Fn(String, CancellationToken) -> BoxFuture<'static, Result<AssetResponse, String>>
        + Send
        + Sync,
>;

pub fn default_asset_fetcher() -> AssetFetcherFn {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest asset client");
    Arc::new(move |url: String, cancel: CancellationToken| {
        let client = client.clone();
        Box::pin(async move {
            let resp = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err("导出已取消".to_owned()),
                res = client.get(&url).send() => res.map_err(|e| e.to_string())?,
            };
            let status = resp.status().as_u16();
            let content_type = resp
                .headers()
                .get(http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let stream = resp
                .bytes_stream()
                .map(|item| item.map_err(|e| e.to_string()));
            Ok(AssetResponse {
                status,
                content_type,
                body: Box::pin(stream),
            })
        })
    })
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[derive(Default)]
struct LruAssetCache {
    entries: VecDeque<(String, String)>,
    total_bytes: usize,
}

impl LruAssetCache {
    fn get(&mut self, url: &str) -> Option<String> {
        let pos = self.entries.iter().position(|(k, _)| k == url)?;
        let (k, v) = self.entries.remove(pos)?;
        let out = v.clone();
        self.entries.push_back((k, v));
        Some(out)
    }

    fn insert(&mut self, url: String, value: String) {
        let size = value.len();
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &url)
            && let Some((_, old)) = self.entries.remove(pos)
        {
            self.total_bytes = self.total_bytes.saturating_sub(old.len());
        }
        while self.total_bytes.saturating_add(size) > MAX_CACHE && !self.entries.is_empty() {
            if let Some((_, evicted)) = self.entries.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(evicted.len());
            }
        }
        if size <= MAX_CACHE {
            self.total_bytes += size;
            self.entries.push_back((url, value));
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }
}

struct CssUrlMatch {
    start: usize,
    end: usize,
    resolved_url: String,
}

#[derive(Clone)]
pub struct OfflineHtml {
    origin: Arc<Mutex<String>>,
    cache: Arc<Mutex<LruAssetCache>>,
    fetcher: AssetFetcherFn,
}

impl OfflineHtml {
    pub fn new(origin: &str) -> Result<Self, ServiceError> {
        Self::with_fetcher(origin, default_asset_fetcher())
    }

    pub fn with_fetcher(origin: &str, fetcher: AssetFetcherFn) -> Result<Self, ServiceError> {
        let parsed = Url::parse(origin).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_export",
                "导出源地址无效",
            )
        })?;
        Ok(Self {
            origin: Arc::new(Mutex::new(parsed.origin().ascii_serialization())),
            cache: Arc::new(Mutex::new(LruAssetCache::default())),
            fetcher,
        })
    }

    pub fn set_origin(&self, origin: &str) -> Result<(), ServiceError> {
        let parsed = Url::parse(origin).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "invalid_export",
                "导出源地址无效",
            )
        })?;
        *self.origin.lock().expect("origin lock") = parsed.origin().ascii_serialization();
        self.clear();
        Ok(())
    }

    fn resolve_url(&self, raw: &str, base: &str) -> Result<String, ServiceError> {
        let base_url = Url::parse(base).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "export_asset_forbidden",
                "导出样式引用了非本机随包资源",
            )
        })?;
        let url = base_url.join(raw).map_err(|_| {
            ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "export_asset_forbidden",
                "导出样式引用了非本机随包资源",
            )
        })?;
        if url.scheme() == "data" {
            return Ok(raw.to_owned());
        }
        let origin_str = url.origin().ascii_serialization();
        let expected_origin = self.origin.lock().expect("origin lock").clone();
        let path = url.path();
        if origin_str != expected_origin
            || !(path.starts_with("/fonts/") || path.starts_with("/assets/"))
        {
            return Err(ServiceError::new(
                ServiceErrorKind::InvalidInput,
                "export_asset_forbidden",
                "导出样式引用了非本机随包资源",
            ));
        }
        Ok(url.into())
    }

    async fn fetch_asset(
        &self,
        url: &str,
        cancel: CancellationToken,
    ) -> Result<String, ServiceError> {
        if cancel.is_cancelled() {
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_interrupted",
                "导出已取消",
            ));
        }
        if let Some(cached) = self.cache.lock().expect("html cache lock").get(url) {
            return Ok(cached);
        }

        let mut resp = (self.fetcher)(url.to_owned(), cancel.clone())
            .await
            .map_err(|_| {
                if cancel.is_cancelled() {
                    ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_interrupted",
                        "导出已取消",
                    )
                } else {
                    ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_asset_unavailable",
                        "导出字体或样式资源无法读取，请检查静态文件",
                    )
                }
            })?;

        if !(200..300).contains(&resp.status) {
            drop(resp.body);
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_asset_unavailable",
                "导出字体或样式资源无法读取，请检查静态文件",
            ));
        }

        let mut buf = Vec::new();
        loop {
            let next_chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    drop(resp.body);
                    return Err(ServiceError::new(
                        ServiceErrorKind::Unavailable,
                        "export_interrupted",
                        "导出已取消",
                    ));
                }
                item = resp.body.next() => item,
            };
            let Some(chunk_res) = next_chunk else {
                break;
            };
            let chunk = chunk_res.map_err(|_| {
                ServiceError::new(
                    ServiceErrorKind::Unavailable,
                    "export_asset_unavailable",
                    "导出字体或样式资源无法读取，请检查静态文件",
                )
            })?;
            if buf.len().saturating_add(chunk.len()) > MAX_ASSET {
                drop(resp.body);
                return Err(ServiceError::new(
                    ServiceErrorKind::TooLarge,
                    "export_too_large",
                    "导出资源超过大小限制",
                ));
            }
            buf.extend_from_slice(&chunk);
        }

        if cancel.is_cancelled() {
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_interrupted",
                "导出已取消",
            ));
        }

        let mime = resp
            .content_type
            .as_deref()
            .and_then(|ct| ct.split(';').next())
            .filter(|s| !s.is_empty())
            .unwrap_or("application/octet-stream");
        let encoded = base64::engine::general_purpose::STANDARD.encode(&buf);
        let data_url = format!("data:{mime};base64,{encoded}");
        self.cache
            .lock()
            .expect("html cache lock")
            .insert(url.to_owned(), data_url.clone());
        Ok(data_url)
    }

    pub async fn render(
        &self,
        snapshot: &Value,
        source: &ExportDocumentSource,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, ServiceError> {
        if cancel.is_cancelled() {
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_interrupted",
                "导出已取消",
            ));
        }

        static URL_RE: OnceLock<Regex> = OnceLock::new();
        let url_re = URL_RE
            .get_or_init(|| Regex::new(r#"url\(\s*(?:"([^"]*)"|'([^']*)'|([^)]*))\s*\)"#).unwrap());

        let mut url_usages: HashMap<String, (usize, usize)> = HashMap::new();
        let mut pending_order: Vec<String> = Vec::new();
        let estimated_bytes =
            source.body.len() + source.styles.iter().map(|s| s.css.len()).sum::<usize>();

        let mut parsed_sheets: Vec<(&str, Vec<CssUrlMatch>)> =
            Vec::with_capacity(source.styles.len());
        for sheet in &source.styles {
            let mut matches = Vec::new();
            for caps in url_re.captures_iter(&sheet.css) {
                let whole = caps.get(0).expect("full match");
                let raw_inner = caps
                    .get(1)
                    .or_else(|| caps.get(2))
                    .or_else(|| caps.get(3))
                    .map(|m| m.as_str().trim())
                    .unwrap_or("");
                let resolved = self.resolve_url(raw_inner, &sheet.base)?;
                if !resolved.starts_with("data:") {
                    let entry = url_usages.entry(resolved.clone()).or_insert_with(|| {
                        pending_order.push(resolved.clone());
                        (0, 0)
                    });
                    entry.0 += 1;
                    entry.1 += whole.as_str().len();
                }
                matches.push(CssUrlMatch {
                    start: whole.start(),
                    end: whole.end(),
                    resolved_url: resolved,
                });
            }
            parsed_sheets.push((&sheet.css, matches));
        }

        if url_usages.len() > 128 {
            return Err(ServiceError::new(
                ServiceErrorKind::TooLarge,
                "export_too_large",
                "导出资源数量超过限制",
            ));
        }
        if estimated_bytes > MAX_HTML {
            return Err(ServiceError::new(
                ServiceErrorKind::TooLarge,
                "export_too_large",
                "导出正文或样式超过大小限制",
            ));
        }

        let worker_count = pending_order.len().min(4);
        let next_idx = Arc::new(AtomicUsize::new(0));
        let values: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let est_shared = Arc::new(Mutex::new(estimated_bytes));
        let batch_cancel = cancel.child_token();

        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let this = self.clone();
            let pending = pending_order.clone();
            let usages = url_usages.clone();
            let idx_ref = next_idx.clone();
            let vals_ref = values.clone();
            let est_ref = est_shared.clone();
            let worker_cancel = batch_cancel.clone();
            handles.push(tokio::spawn(async move {
                loop {
                    if worker_cancel.is_cancelled() {
                        return Err(ServiceError::new(
                            ServiceErrorKind::Unavailable,
                            "export_interrupted",
                            "导出已取消",
                        ));
                    }
                    let i = idx_ref.fetch_add(1, Ordering::SeqCst);
                    if i >= pending.len() {
                        return Ok(());
                    }
                    let url = &pending[i];
                    let data_url = match this.fetch_asset(url, worker_cancel.clone()).await {
                        Ok(v) => v,
                        Err(err) => {
                            worker_cancel.cancel();
                            return Err(err);
                        }
                    };
                    let (count, replaced_bytes) = usages.get(url).copied().unwrap_or((1, 0));
                    {
                        let mut est = est_ref.lock().expect("est lock");
                        let added = (data_url.len() + r#"url("")"#.len()) * count;
                        *est = est.saturating_add(added).saturating_sub(replaced_bytes);
                        if *est > MAX_HTML {
                            worker_cancel.cancel();
                            return Err(ServiceError::new(
                                ServiceErrorKind::TooLarge,
                                "export_too_large",
                                "产物超过大小限制，请缩短当前分支后重试",
                            ));
                        }
                    }
                    vals_ref
                        .lock()
                        .expect("vals lock")
                        .insert(url.clone(), data_url);
                }
            }));
        }

        let mut first_err: Option<ServiceError> = None;
        for h in handles {
            match h.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(_) => {
                    if first_err.is_none() {
                        first_err = Some(ServiceError::new(
                            ServiceErrorKind::Internal,
                            "export_failed",
                            "导出资源读取异常中断",
                        ));
                    }
                }
            }
        }
        if let Some(err) = first_err {
            return Err(err);
        }
        if cancel.is_cancelled() {
            return Err(ServiceError::new(
                ServiceErrorKind::Unavailable,
                "export_interrupted",
                "导出已取消",
            ));
        }

        let resolved_map = values.lock().expect("vals lock");
        let mut combined_css = String::new();
        for (idx, (css, matches)) in parsed_sheets.into_iter().enumerate() {
            if idx > 0 {
                combined_css.push('\n');
            }
            let mut cursor = 0usize;
            for m in matches {
                combined_css.push_str(&css[cursor..m.start]);
                let replacement = resolved_map
                    .get(&m.resolved_url)
                    .map(String::as_str)
                    .unwrap_or(&m.resolved_url);
                combined_css.push_str("url(\"");
                combined_css.push_str(replacement);
                combined_css.push_str("\")");
                cursor = m.end;
            }
            combined_css.push_str(&css[cursor..]);
        }

        static STYLE_CLOSE_RE: OnceLock<Regex> = OnceLock::new();
        let style_close_re = STYLE_CLOSE_RE.get_or_init(|| Regex::new("(?i)</style").unwrap());
        let safe_css = style_close_re.replace_all(&combined_css, r"<\/style");

        let title = snapshot
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("会话");
        let theme = snapshot
            .get("options")
            .and_then(|o| o.get("theme"))
            .and_then(Value::as_str)
            .unwrap_or("light");
        let html = format!(
            "<!DOCTYPE html><html lang=\"zh-CN\" data-theme=\"{theme}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"{EXPORT_CSP}\"><title>{}</title><style>{safe_css}</style></head><body class=\"export-page\">{}</body></html>",
            escape_html(title),
            source.body
        );
        if html.len() > MAX_HTML {
            return Err(ServiceError::new(
                ServiceErrorKind::TooLarge,
                "export_too_large",
                "产物超过大小限制，请缩短当前分支后重试",
            ));
        }
        Ok(html.into_bytes())
    }

    pub fn clear(&self) {
        self.cache.lock().expect("html cache lock").clear();
    }
}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use url::Url;

use crate::runbook::storage::Storage;

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: String,
    pub ca: Option<String>,
    pub unavailable: bool,
}
#[derive(Clone, Debug)]
pub struct Config {
    pub endpoints: Vec<Endpoint>,
    pub auth_provider: String,
    pub currency: Option<BTreeMap<String, String>>,
}
impl Config {
    /// Disabled/missing/invalid configuration performs no environment or identity work.
    pub fn load(root: &Path) -> Result<Option<Self>, String> {
        let storage = Storage::new(root)?;
        let Some(bytes) = read(&storage, Path::new("agent/home/telemetry.json"), 64 * 1024)? else {
            return Ok(None);
        };
        let raw: Value = match serde_json::from_slice(&bytes) {
            Ok(Value::Object(object)) => Value::Object(object),
            _ => return Ok(None),
        };
        if raw.get("enabled") == Some(&Value::Bool(false)) {
            return Ok(None);
        }
        let candidates = raw
            .get("endpoints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                raw.get("endpoint")
                    .and_then(Value::as_str)
                    .map(|url| vec![Value::String(url.into())])
                    .unwrap_or_default()
            });
        let mut endpoints = Vec::<Endpoint>::new();
        for input in candidates {
            let Some(text) = input
                .as_str()
                .or_else(|| input.get("url").and_then(Value::as_str))
            else {
                continue;
            };
            let Some(url) = endpoint_url(text) else {
                continue;
            };
            if endpoints.iter().any(|endpoint| endpoint.url == url) {
                continue;
            }
            let mut endpoint = Endpoint {
                url,
                ca: None,
                unavailable: false,
            };
            if let Some(file) = input.get("caFile") {
                let valid = file
                    .as_str()
                    .filter(|name| {
                        name.strip_suffix(".crt").is_some_and(|stem| {
                            !stem.is_empty()
                                && stem
                                    .bytes()
                                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                        })
                    })
                    .filter(|_| endpoint.url.starts_with("https:"));
                endpoint.ca = valid.and_then(|file| {
                    let path = PathBuf::from("agent/home/extensions/telemetry").join(file);
                    let bytes = read(&storage, &path, 128 * 1024).ok()??;
                    reqwest::Certificate::from_pem(&bytes).ok()?;
                    String::from_utf8(bytes).ok()
                });
                endpoint.unavailable = endpoint.ca.is_none();
            }
            endpoints.push(endpoint);
        }
        if endpoints.is_empty() {
            return Ok(None);
        }
        let currency = read(
            &storage,
            Path::new("agent/home/extensions/telemetry/currency.json"),
            64 * 1024,
        )
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, String>>(&bytes).ok())
        .map(|mut map| {
            map.retain(|_, currency| matches!(currency.as_str(), "CNY" | "USD"));
            map
        });
        Ok(Some(Self {
            endpoints,
            auth_provider: raw
                .get("authProvider")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or("cstoa")
                .into(),
            currency,
        }))
    }
    pub fn currency_of(&self, provider: &str) -> &str {
        match &self.currency {
            Some(map) => map.get(provider).map(String::as_str).unwrap_or("USD"),
            None => "",
        }
    }
}
pub fn endpoint_url(text: &str) -> Option<String> {
    let url = Url::parse(text).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return None;
    }
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return None;
    }
    Some(url.into())
}
fn read(storage: &Storage, relative: &Path, limit: usize) -> Result<Option<Vec<u8>>, String> {
    let parent = relative.parent().ok_or("遥测配置路径无效。")?;
    if !storage.root.join(parent).exists() {
        return Ok(None);
    }
    let _paths = storage
        .pin_directory(parent, false)
        .map_err(|_| "遥测配置目录包含链接或不可读。")?;
    storage
        .read_limited(&storage.root.join(relative), limit)
        .map_err(|_| "遥测配置无法安全读取，原文件已保留。".into())
}

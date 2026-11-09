use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pi_session::ToolDefinition;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::json_result;
use super::ssrf::{HttpRequest, HttpTransport, NetworkPolicy, SafeHttpClient, SsrfConfig};

const CACHE_ENTRIES: usize = 32;
const CACHE_BYTES: usize = 8 * 1024 * 1024;
const ENTRY_BYTES: usize = 2 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchConfig {
    search_provider: Option<String>,
    workflow: Option<String>,
    exa_api_key: Option<String>,
    #[serde(default)]
    ssrf: SsrfConfig,
}

#[derive(Clone)]
struct CachedContent {
    id: String,
    text: String,
    created: Instant,
}

#[derive(Clone)]
pub struct WebAccess {
    transport: Arc<dyn HttpTransport>,
    api_key: Option<String>,
    cache: Arc<Mutex<VecDeque<CachedContent>>>,
}

impl WebAccess {
    pub fn from_config(path: &Path) -> Result<Self, String> {
        let config: SearchConfig = match std::fs::read(path) {
            Ok(bytes) if bytes.len() <= 64 * 1024 => serde_json::from_slice(&bytes)
                .map_err(|_| "联网检索配置无法读取，请检查 web-search.json。")?,
            Ok(_) => return Err("联网检索配置超过 64 KiB。".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SearchConfig::default(),
            Err(_) => return Err("联网检索配置无法读取，请检查 web-search.json。".into()),
        };
        if config
            .search_provider
            .as_deref()
            .is_some_and(|name| name != "exa")
            || config
                .workflow
                .as_deref()
                .is_some_and(|mode| mode != "none")
            || config
                .ssrf
                .allow_ranges
                .iter()
                .any(|range| range != "198.18.0.0/15")
        {
            return Err("联网检索仅支持 Exa、none 工作流与 198.18.0.0/15 放行配置。".into());
        }
        let policy = NetworkPolicy {
            allow_tun: !config.ssrf.allow_ranges.is_empty(),
        };
        Ok(Self {
            transport: Arc::new(SafeHttpClient::new(policy)),
            api_key: config.exa_api_key.filter(|key| !key.trim().is_empty()),
            cache: Arc::new(Mutex::new(VecDeque::new())),
        })
    }

    /// Transport injection keeps contract tests offline and lets embedders own HTTP lifecycles.
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport,
            api_key: None,
            cache: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn store(&self, text: String) -> Result<String, String> {
        let text = clip_bytes(&text, ENTRY_BYTES).to_owned();
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = self.cache.lock().map_err(|_| "检索缓存不可用。")?;
        cache.retain(|item| item.created.elapsed() < CACHE_TTL);
        while cache.len() >= CACHE_ENTRIES
            || cache.iter().map(|item| item.text.len()).sum::<usize>() + text.len() > CACHE_BYTES
        {
            cache.pop_front();
        }
        cache.push_back(CachedContent {
            id: id.clone(),
            text,
            created: Instant::now(),
        });
        Ok(id)
    }

    pub async fn search(
        &self,
        input: SearchInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        let mut queries = input.queries;
        if let Some(query) = input.query.or(input.claim) {
            queries.insert(0, query);
        }
        if queries.is_empty()
            || queries.len() > 3
            || queries
                .iter()
                .any(|query| query.trim().is_empty() || query.len() > 4096)
        {
            return Err("每次提供 1–3 个非空检索词，每项不超过 4096 字节。".into());
        }
        let count = input.num_results.unwrap_or(5).clamp(1, 10);
        let mut documents = Vec::new();
        let mut full = String::new();
        for query in queries {
            let request = if let Some(key) = &self.api_key {
                HttpRequest {
                    url: "https://api.exa.ai/search".into(),
                    headers: vec![("x-api-key".into(), key.clone())],
                    body: Some(
                        json!({"query": query, "numResults": count, "type": "auto", "contents": {"text": {"maxCharacters": 50000}}}),
                    ),
                }
            } else {
                HttpRequest {
                    url: "https://mcp.exa.ai/mcp".into(),
                    headers: vec![(
                        "Accept".into(),
                        "application/json, text/event-stream".into(),
                    )],
                    body: Some(
                        json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"web_search_exa", "arguments":{"query":query,"numResults":count,"type":"auto"}}}),
                    ),
                }
            };
            let response = self.transport.request(request, cancel.clone()).await?;
            if !(200..300).contains(&response.status) {
                return Err(format!("Exa 检索失败（HTTP {}）。", response.status));
            }
            let text = search_text(&response.body)?;
            let text = clip_bytes(&text, ENTRY_BYTES.saturating_sub(full.len()));
            full.push_str(&format!("## {query}\n{text}\n\n"));
            documents.push(json!({"query":query,"preview":clip_chars(text,1200)}));
        }
        let id = self.store(full)?;
        Ok(json!({"responseId":id,"queries":documents,"provider":"exa"}))
    }

    pub async fn fetch(
        &self,
        input: FetchInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        let mut urls = input.urls;
        if let Some(url) = input.url {
            urls.insert(0, url);
        }
        if urls.is_empty() || urls.len() > 5 {
            return Err("每次提供 1–5 个网页地址。".into());
        }
        let mut documents = Vec::new();
        let mut full = String::new();
        for url in urls {
            // URL syntax is checked even for injected transports.
            let parsed = NetworkPolicy { allow_tun: true }.validate_url(&url)?;
            let response = self
                .transport
                .request(
                    HttpRequest {
                        url: parsed.into(),
                        body: None,
                        headers: Vec::new(),
                    },
                    cancel.clone(),
                )
                .await?;
            if !(200..300).contains(&response.status) {
                return Err(format!("网页获取失败（HTTP {}）。", response.status));
            }
            let text = readable_text(&response.body);
            let text = clip_bytes(&text, ENTRY_BYTES.saturating_sub(full.len()));
            full.push_str(&format!("## {url}\n{text}\n\n"));
            documents.push(json!({"url":url,"preview":clip_chars(text,1200)}));
        }
        let id = self.store(full)?;
        Ok(json!({"responseId":id,"urls":documents}))
    }

    pub fn get(&self, input: ContentInput) -> Result<Value, String> {
        let mut cache = self.cache.lock().map_err(|_| "检索缓存不可用。")?;
        cache.retain(|item| item.created.elapsed() < CACHE_TTL);
        let entry = cache
            .iter()
            .find(|item| item.id == input.response_id)
            .ok_or("检索内容不存在或已过期，请重新检索。")?;
        let text = if let Some(needle) = input.find_text {
            if needle.is_empty() || needle.len() > 4096 {
                return Err("查找文本为空或过长。".into());
            }
            let mut excerpts = Vec::new();
            for (index, _) in entry.text.match_indices(&needle).take(20) {
                let begin = floor_boundary(&entry.text, index.saturating_sub(120));
                let end = floor_boundary(
                    &entry.text,
                    (index + needle.len() + 240).min(entry.text.len()),
                );
                excerpts.push(&entry.text[begin..end]);
            }
            clip_chars(&excerpts.join("\n\n"), 6000).to_owned()
        } else {
            entry
                .text
                .chars()
                .skip(input.offset.unwrap_or(0))
                .take(input.limit.unwrap_or(6000).clamp(1, 6000))
                .collect()
        };
        Ok(
            json!({"responseId":entry.id,"content":text,"totalCharacters":entry.text.chars().count(),"offset":input.offset.unwrap_or(0)}),
        )
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = Vec::new();
        for name in ["web_search", "source_check"] {
            let web = self.clone();
            definitions.push(ToolDefinition::from_typed::<SearchInput, _, _>(
                name,
                name,
                "通过 Exa 检索公开网页，正文保存在当前会话的内存缓存。",
                move |_, input, cancel, _, _| {
                    let web = web.clone();
                    async move { json_result(web.search(input, cancel.unwrap_or_default()).await?) }
                },
            ));
        }
        let web = self.clone();
        definitions.push(ToolDefinition::from_typed::<FetchInput, _, _>(
            "fetch_content",
            "获取网页",
            "获取公开网页正文，每次最多 5 个地址，禁止内网与私网重定向。",
            move |_, input, cancel, _, _| {
                let web = web.clone();
                async move { json_result(web.fetch(input, cancel.unwrap_or_default()).await?) }
            },
        ));
        let web = self.clone();
        definitions.push(ToolDefinition::from_typed::<ContentInput, _, _>(
            "get_search_content",
            "读取检索正文",
            "使用 responseId 分页读取或查找当前会话的检索正文。",
            move |_, input, _, _, _| {
                let web = web.clone();
                async move { json_result(web.get(input)?) }
            },
        ));
        for definition in &mut definitions {
            definition.prompt_snippet = Some(definition.description.clone());
        }
        definitions
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchInput {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub claim: Option<String>,
    #[serde(default)]
    pub queries: Vec<String>,
    #[serde(default)]
    pub num_results: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FetchInput {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentInput {
    pub response_id: String,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub find_text: Option<String>,
}

fn search_text(body: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(body)
        .or_else(|_| {
            body.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
                .next_back()
                .ok_or(())
        })
        .map_err(|_| "Exa 返回的数据格式无效。")?;
    if value.get("error").is_some() || value.pointer("/result/isError") == Some(&Value::Bool(true))
    {
        return Err("Exa 未能完成检索，请稍后重试。".into());
    }
    if let Some(results) = value.get("results").and_then(Value::as_array) {
        let mut text = String::new();
        for item in results.iter().take(10) {
            let title = item.get("title").and_then(Value::as_str).unwrap_or("");
            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
            let content = item.get("text").and_then(Value::as_str).unwrap_or("");
            text.push_str(&format!("# {title}\n{url}\n{content}\n\n"));
        }
        return Ok(text);
    }
    let contents = value
        .pointer("/result/content")
        .and_then(Value::as_array)
        .ok_or("Exa 返回的数据缺少正文。")?;
    Ok(contents
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n"))
}

fn readable_text(body: &str) -> String {
    if !body.trim_start().starts_with('<') {
        return body.to_owned();
    }
    let mut text = body.to_owned();
    for pattern in [
        r"(?is)<script\b[^>]*>.*?</script\s*>",
        r"(?is)<style\b[^>]*>.*?</style\s*>",
        r"(?s)<[^>]*>",
    ] {
        if let Ok(regex) = regex::Regex::new(pattern) {
            text = regex.replace_all(&text, "\n").into_owned();
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn clip_bytes(text: &str, limit: usize) -> &str {
    &text[..floor_boundary(text, text.len().min(limit))]
}

fn clip_chars(text: &str, limit: usize) -> &str {
    &text[..text
        .char_indices()
        .nth(limit)
        .map_or(text.len(), |(i, _)| i)]
}

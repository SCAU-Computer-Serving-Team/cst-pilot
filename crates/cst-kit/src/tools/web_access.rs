use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use pi_session::ToolDefinition;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::json_result;
use super::ssrf::{HttpRequest, HttpTransport, NetworkPolicy, SafeHttpClient, SsrfConfig};

const CACHE_ENTRIES: usize = 32;
const CACHE_BYTES: usize = 8 * 1024 * 1024;
const ENTRY_BYTES: usize = 2 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const DEEPSEEK_MAX_QUERY_CHARS: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    #[default]
    Exa,
    Deepseek,
}

impl WebSearchProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exa => "exa",
            Self::Deepseek => "deepseek",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Fast,
    #[default]
    Auto,
    Deep,
}

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Auto => "auto",
            Self::Deep => "deep",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ContentMode {
    #[default]
    Highlights,
    Summary,
    Text,
}

impl ContentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Highlights => "highlights",
            Self::Summary => "summary",
            Self::Text => "text",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FetchStrategy {
    #[default]
    Auto,
    Provider,
    Direct,
}

impl FetchStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Provider => "provider",
            Self::Direct => "direct",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDefaults {
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default = "default_num_results")]
    pub num_results: usize,
    #[serde(default)]
    pub content_mode: ContentMode,
    #[serde(default = "default_max_excerpt_chars")]
    pub max_excerpt_chars: usize,
    #[serde(default = "default_max_fetch_chars")]
    pub max_fetch_chars: usize,
    #[serde(default)]
    pub include_domains: Vec<String>,
    #[serde(default)]
    pub exclude_domains: Vec<String>,
}

impl Default for SearchDefaults {
    fn default() -> Self {
        Self {
            mode: SearchMode::Auto,
            num_results: default_num_results(),
            content_mode: ContentMode::Highlights,
            max_excerpt_chars: default_max_excerpt_chars(),
            max_fetch_chars: default_max_fetch_chars(),
            include_domains: Vec::new(),
            exclude_domains: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExaConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_exa_base_url")]
    pub base_url: String,
    #[serde(default = "default_true")]
    pub allow_free_mcp_fallback: bool,
    #[serde(default = "default_exa_type")]
    pub default_type: String,
    #[serde(default = "default_exa_deep_type")]
    pub deep_type: String,
    #[serde(default = "default_exa_content_view")]
    pub content_view: String,
    #[serde(default = "default_exa_max_age_hours")]
    pub max_age_hours: Option<i64>,
    #[serde(default = "default_exa_verbosity")]
    pub text_verbosity: String,
    #[serde(default)]
    pub moderation: bool,
}

impl Default for ExaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: String::new(),
            base_url: default_exa_base_url(),
            allow_free_mcp_fallback: true,
            default_type: default_exa_type(),
            deep_type: default_exa_deep_type(),
            content_view: default_exa_content_view(),
            max_age_hours: default_exa_max_age_hours(),
            text_verbosity: default_exa_verbosity(),
            moderation: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_true")]
    pub reuse_model_api_key: bool,
    #[serde(default = "default_deepseek_base_url")]
    pub base_url: String,
    #[serde(default = "default_deepseek_count")]
    pub default_count: usize,
    #[serde(default)]
    pub need_content: bool,
    #[serde(default = "default_true")]
    pub send_original_question: bool,
    #[serde(default)]
    pub localization: String,
    #[serde(default = "default_languages")]
    pub languages: Vec<String>,
    #[serde(default)]
    pub min_rerank_score: f64,
    #[serde(default = "default_max_qps")]
    pub max_qps: u32,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: String::new(),
            reuse_model_api_key: true,
            base_url: default_deepseek_base_url(),
            default_count: default_deepseek_count(),
            need_content: false,
            send_original_question: true,
            localization: String::new(),
            languages: default_languages(),
            min_rerank_score: 0.0,
            max_qps: default_max_qps(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvidersConfig {
    #[serde(default)]
    pub exa: ExaConfig,
    #[serde(default)]
    pub deepseek: DeepSeekConfig,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub default_provider: WebSearchProvider,
    #[serde(default)]
    pub fallback_provider: Option<WebSearchProvider>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_deep_timeout_ms")]
    pub deep_timeout_ms: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default)]
    pub fetch_strategy: FetchStrategy,
    #[serde(default)]
    pub ssrf: SsrfConfig,
    #[serde(default)]
    pub defaults: SearchDefaults,
    #[serde(default)]
    pub providers: ProvidersConfig,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            enabled: true,
            default_provider: WebSearchProvider::Exa,
            fallback_provider: None,
            timeout_ms: default_timeout_ms(),
            deep_timeout_ms: default_deep_timeout_ms(),
            max_concurrent: default_max_concurrent(),
            fetch_strategy: FetchStrategy::Auto,
            ssrf: SsrfConfig::default(),
            defaults: SearchDefaults::default(),
            providers: ProvidersConfig::default(),
        }
    }
}

fn default_version() -> u32 {
    2
}
fn default_true() -> bool {
    true
}
fn default_timeout_ms() -> u64 {
    15_000
}
fn default_deep_timeout_ms() -> u64 {
    45_000
}
fn default_max_concurrent() -> usize {
    3
}
fn default_num_results() -> usize {
    5
}
fn default_max_excerpt_chars() -> usize {
    2_000
}
fn default_max_fetch_chars() -> usize {
    8_000
}
fn default_exa_base_url() -> String {
    "https://api.exa.ai".to_owned()
}
fn default_exa_type() -> String {
    "auto".to_owned()
}
fn default_exa_deep_type() -> String {
    "deep".to_owned()
}
fn default_exa_content_view() -> String {
    "highlights".to_owned()
}
fn default_exa_max_age_hours() -> Option<i64> {
    Some(24)
}
fn default_exa_verbosity() -> String {
    "compact".to_owned()
}
fn default_deepseek_base_url() -> String {
    "https://api.deepseek.com".to_owned()
}
fn default_deepseek_count() -> usize {
    8
}
fn default_languages() -> Vec<String> {
    vec!["zh".to_owned(), "en".to_owned()]
}
fn default_max_qps() -> u32 {
    5
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyV1Config {
    search_provider: Option<String>,
    workflow: Option<String>,
    exa_api_key: Option<String>,
    #[serde(default)]
    ssrf: Option<LegacySsrfConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySsrfConfig {
    #[serde(default)]
    allow_ranges: Vec<String>,
}

impl WebSearchConfig {
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) if bytes.len() <= 64 * 1024 => bytes,
            Ok(_) => return Err("联网检索配置超过 64 KiB。".into()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("联网检索配置无法读取，请检查 web-search.json。".into()),
        };
        let raw: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "联网检索配置无法读取，请检查 web-search.json。")?;
        Self::from_json_value(raw)
    }

    pub fn from_json_value(raw: Value) -> Result<Self, String> {
        let Some(obj) = raw.as_object() else {
            return Err("联网检索配置必须是 JSON 对象。".into());
        };
        let is_v2 = obj.get("version").and_then(Value::as_u64) == Some(2)
            || obj.contains_key("providers")
            || obj.contains_key("defaultProvider");
        let mut config = if is_v2 {
            serde_json::from_value::<Self>(raw)
                .map_err(|_| "联网检索配置格式无效，请检查 web-search.json。")?
        } else {
            let legacy: LegacyV1Config = serde_json::from_value(raw)
                .map_err(|_| "联网检索配置格式无效，请检查 web-search.json。")?;
            if legacy
                .workflow
                .as_deref()
                .is_some_and(|mode| mode != "none")
            {
                return Err("联网检索配置仅支持 none 工作流。".into());
            }
            let mut cfg = Self::default();
            if let Some(provider) = legacy.search_provider.as_deref() {
                cfg.default_provider = match provider {
                    "exa" => WebSearchProvider::Exa,
                    "deepseek" => WebSearchProvider::Deepseek,
                    _ => return Err("联网检索仅支持 exa 或 deepseek 提供商。".into()),
                };
            }
            if let Some(key) = legacy.exa_api_key {
                cfg.providers.exa.api_key = key.trim().to_owned();
            }
            if let Some(ssrf) = legacy.ssrf {
                cfg.ssrf.allow_ranges = ssrf.allow_ranges;
            }
            cfg
        };
        config.version = 2;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .ssrf
            .allow_ranges
            .iter()
            .any(|range| range != "198.18.0.0/15")
        {
            return Err("联网检索 SSRF 放行列表仅允许 198.18.0.0/15。".into());
        }
        if !(1_000..=120_000).contains(&self.timeout_ms) {
            return Err("常规超时 timeoutMs 须在 1000–120000 毫秒之间。".into());
        }
        if !(5_000..=180_000).contains(&self.deep_timeout_ms) {
            return Err("深度检索超时 deepTimeoutMs 须在 5000–180000 毫秒之间。".into());
        }
        if !(1..=5).contains(&self.max_concurrent) {
            return Err("最大并发数 maxConcurrent 须在 1–5 之间。".into());
        }
        if !(1..=20).contains(&self.defaults.num_results) {
            return Err("默认检索条数 numResults 须在 1–20 之间。".into());
        }
        if !matches!(
            self.providers.exa.default_type.as_str(),
            "lite" | "auto" | "deep-lite" | "deep"
        ) {
            return Err("Exa defaultType 仅支持 lite、auto、deep-lite 或 deep。".into());
        }
        if !matches!(self.providers.exa.deep_type.as_str(), "deep" | "deep-lite") {
            return Err("Exa deepType 仅支持 deep 或 deep-lite。".into());
        }
        if !matches!(
            self.providers.exa.content_view.as_str(),
            "highlights" | "text" | "summary"
        ) {
            return Err("Exa contentView 仅支持 highlights、text 或 summary。".into());
        }
        if !matches!(
            self.providers.exa.text_verbosity.as_str(),
            "compact" | "standard" | "full"
        ) {
            return Err("Exa textVerbosity 仅支持 compact、standard 或 full。".into());
        }
        if !(1..=50).contains(&self.providers.deepseek.default_count) {
            return Err("DeepSeek defaultCount 须在 1–50 之间。".into());
        }
        if !self.providers.deepseek.min_rerank_score.is_finite()
            || !(0.0..=3.0).contains(&self.providers.deepseek.min_rerank_score)
        {
            return Err("DeepSeek minRerankScore 须在 0.0–3.0 之间。".into());
        }
        let policy = NetworkPolicy {
            allow_tun: !self.ssrf.allow_ranges.is_empty(),
        };
        policy
            .validate_url(self.providers.exa.base_url.trim())
            .map_err(|_| "Exa baseUrl 无效或不满足网络安全策略。")?;
        policy
            .validate_url(self.providers.deepseek.base_url.trim())
            .map_err(|_| "DeepSeek baseUrl 无效或不满足网络安全策略。")?;
        Ok(())
    }

    pub fn apply_patch(&mut self, patch: Value) -> Result<(), String> {
        let Some(obj) = patch.as_object() else {
            return Err("联网检索配置补丁必须是 JSON 对象。".into());
        };
        let mut current = serde_json::to_value(&*self).map_err(|_| "联网检索配置编码失败。")?;
        merge_json_patch(&mut current, obj);
        let mut next: Self =
            serde_json::from_value(current).map_err(|_| "联网检索配置字段类型或枚举值无效。")?;
        next.version = 2;
        next.providers.exa.api_key = next.providers.exa.api_key.trim().to_owned();
        next.providers.exa.base_url = next
            .providers
            .exa
            .base_url
            .trim()
            .trim_end_matches('/')
            .to_owned();
        next.providers.deepseek.api_key = next.providers.deepseek.api_key.trim().to_owned();
        next.providers.deepseek.base_url = next
            .providers
            .deepseek
            .base_url
            .trim()
            .trim_end_matches('/')
            .to_owned();
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn masked_view(&self, config_dir: Option<&Path>) -> Value {
        let mut val = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        if let Some(exa) = val
            .pointer_mut("/providers/exa")
            .and_then(Value::as_object_mut)
        {
            let key = self.providers.exa.api_key.trim();
            exa.remove("apiKey");
            exa.insert("hasApiKey".into(), json!(!key.is_empty()));
            exa.insert("apiKeyTail".into(), json!(mask_tail(key)));
        }
        if let Some(ds) = val
            .pointer_mut("/providers/deepseek")
            .and_then(Value::as_object_mut)
        {
            let key = self.providers.deepseek.api_key.trim();
            let model_key_available = resolve_deepseek_model_api_key(config_dir).is_some();
            ds.remove("apiKey");
            ds.insert("hasApiKey".into(), json!(!key.is_empty()));
            ds.insert("apiKeyTail".into(), json!(mask_tail(key)));
            ds.insert(
                "resolvedModelApiKey".into(),
                json!(self.providers.deepseek.reuse_model_api_key && model_key_available),
            );
        }
        val
    }
}

fn mask_tail(key: &str) -> Option<String> {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() >= 8 {
        Some(chars[chars.len() - 4..].iter().collect())
    } else {
        None
    }
}

fn merge_json_patch(target: &mut Value, patch: &Map<String, Value>) {
    let Some(dst) = target.as_object_mut() else {
        return;
    };
    for (k, v) in patch {
        if let (Some(sub_patch), Some(existing)) = (v.as_object(), dst.get_mut(k))
            && existing.is_object()
        {
            merge_json_patch(existing, sub_patch);
        } else {
            dst.insert(k.clone(), v.clone());
        }
    }
}

fn resolve_deepseek_model_api_key(config_dir: Option<&Path>) -> Option<String> {
    if let Ok(env_key) = std::env::var("DEEPSEEK_API_KEY") {
        let trimmed = env_key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    let dir = config_dir?;
    let auth_bytes = std::fs::read(dir.join("auth.json")).ok()?;
    let auth_val: Value = serde_json::from_slice(&auth_bytes).ok()?;
    let ds = auth_val.get("deepseek")?;
    let key = ds
        .get("key")
        .or_else(|| ds.get("apiKey"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(key.to_owned())
}

#[derive(Clone)]
struct CachedContent {
    id: String,
    text: String,
    created: Instant,
}

#[derive(Clone)]
pub struct WebAccess {
    config_path: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    static_config: WebSearchConfig,
    transport_override: Option<Arc<dyn HttpTransport>>,
    cache: Arc<Mutex<VecDeque<CachedContent>>>,
}

impl WebAccess {
    pub fn from_config(path: &Path) -> Result<Self, String> {
        let config = WebSearchConfig::load_from_file(path)?;
        Ok(Self {
            config_path: Some(path.to_path_buf()),
            config_dir: path.parent().map(Path::to_path_buf),
            static_config: config,
            transport_override: None,
            cache: Arc::new(Mutex::new(VecDeque::new())),
        })
    }

    /// Transport injection keeps contract tests offline and lets embedders own HTTP lifecycles.
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self::with_transport_and_config(transport, WebSearchConfig::default())
    }

    pub fn with_transport_and_config(
        transport: Arc<dyn HttpTransport>,
        config: WebSearchConfig,
    ) -> Self {
        Self {
            config_path: None,
            config_dir: None,
            static_config: config,
            transport_override: Some(transport),
            cache: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn effective_config(&self) -> Result<WebSearchConfig, String> {
        if let Some(path) = &self.config_path {
            WebSearchConfig::load_from_file(path)
        } else {
            Ok(self.static_config.clone())
        }
    }

    fn transport_for(&self, config: &WebSearchConfig) -> Arc<dyn HttpTransport> {
        if let Some(t) = &self.transport_override {
            return t.clone();
        }
        let policy = NetworkPolicy {
            allow_tun: !config.ssrf.allow_ranges.is_empty(),
        };
        Arc::new(SafeHttpClient::new(policy))
    }

    fn resolve_deepseek_key(&self, config: &WebSearchConfig) -> Option<String> {
        let direct = config.providers.deepseek.api_key.trim();
        if !direct.is_empty() {
            return Some(direct.to_owned());
        }
        if config.providers.deepseek.reuse_model_api_key {
            return resolve_deepseek_model_api_key(self.config_dir.as_deref());
        }
        None
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

    pub async fn probe_provider(
        &self,
        provider: WebSearchProvider,
        candidate_api_key: Option<String>,
        candidate_base_url: Option<String>,
        cancel: CancellationToken,
    ) -> Value {
        let started = Instant::now();
        let mut config = match self.effective_config() {
            Ok(c) => c,
            Err(err) => {
                return json!({
                    "ok": false,
                    "provider": provider.as_str(),
                    "latencyMs": started.elapsed().as_millis() as u64,
                    "message": err,
                });
            }
        };
        match provider {
            WebSearchProvider::Exa => {
                if let Some(k) = candidate_api_key {
                    config.providers.exa.api_key = k.trim().to_owned();
                }
                if let Some(u) = candidate_base_url {
                    config.providers.exa.base_url = u.trim().trim_end_matches('/').to_owned();
                }
            }
            WebSearchProvider::Deepseek => {
                if let Some(k) = candidate_api_key {
                    config.providers.deepseek.api_key = k.trim().to_owned();
                }
                if let Some(u) = candidate_base_url {
                    config.providers.deepseek.base_url = u.trim().trim_end_matches('/').to_owned();
                }
            }
        }
        if let Err(err) = config.validate() {
            return json!({
                "ok": false,
                "provider": provider.as_str(),
                "latencyMs": started.elapsed().as_millis() as u64,
                "message": err,
            });
        }
        let transport = self.transport_for(&config);
        let opts = SearchRunOpts {
            mode: SearchMode::Fast,
            num_results: 1,
            content_mode: ContentMode::Highlights,
            include_domains: Vec::new(),
            exclude_domains: Vec::new(),
            after_date: None,
            before_date: None,
            languages: Vec::new(),
            additional_queries: Vec::new(),
            ori_question: None,
        };
        let outcome = self
            .run_single_search(provider, "Windows", &opts, &config, &transport, cancel)
            .await;
        let latency_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(_) => json!({
                "ok": true,
                "provider": provider.as_str(),
                "latencyMs": latency_ms,
                "message": "连通测试成功。",
            }),
            Err(msg) => json!({
                "ok": false,
                "provider": provider.as_str(),
                "latencyMs": latency_ms,
                "message": msg,
            }),
        }
    }

    pub async fn search(
        &self,
        input: SearchInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        let config = self.effective_config()?;
        if !config.enabled {
            return Err("联网检索已在配置中关闭。".into());
        }
        let mut queries = input.queries;
        if let Some(query) = input.query {
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

        let primary = input.provider.unwrap_or(config.default_provider);
        let mode = input.mode.unwrap_or(config.defaults.mode);
        let num_results = input
            .num_results
            .unwrap_or(config.defaults.num_results)
            .clamp(1, 20);
        let content_mode = input.content_mode.unwrap_or(config.defaults.content_mode);
        let include_domains = if input.include_domains.is_empty() {
            config.defaults.include_domains.clone()
        } else {
            input.include_domains
        };
        let exclude_domains = if input.exclude_domains.is_empty() {
            config.defaults.exclude_domains.clone()
        } else {
            input.exclude_domains
        };
        let opts = SearchRunOpts {
            mode,
            num_results,
            content_mode,
            include_domains,
            exclude_domains,
            after_date: input.after_date,
            before_date: input.before_date,
            languages: input.languages,
            additional_queries: input.additional_queries,
            ori_question: input.ori_question,
        };
        let transport = self.transport_for(&config);

        let (used_provider, degraded_from, outcomes) = {
            let first = self
                .run_batch_search(
                    primary,
                    &queries,
                    &opts,
                    &config,
                    &transport,
                    cancel.clone(),
                )
                .await;
            let any_ok = first.iter().any(Result::is_ok);
            if !any_ok
                && input.provider.is_none()
                && let Some(fallback) = config.fallback_provider
                && fallback != primary
            {
                let first_reason = first
                    .iter()
                    .find_map(|r| r.as_ref().err().cloned())
                    .unwrap_or_else(|| "主提供商检索失败".into());
                let second = self
                    .run_batch_search(fallback, &queries, &opts, &config, &transport, cancel)
                    .await;
                if second.iter().any(Result::is_ok) {
                    (
                        fallback,
                        Some(format!("{}: {first_reason}", primary.as_str())),
                        second,
                    )
                } else {
                    (primary, None, first)
                }
            } else {
                (primary, None, first)
            }
        };

        let mut query_rows = Vec::with_capacity(queries.len());
        let mut collection_errors = Vec::new();
        let mut notices = Vec::new();
        let mut full = String::new();
        let mut successful_queries = 0usize;

        for (query, outcome) in queries.iter().zip(outcomes) {
            match outcome {
                Ok(parsed) => {
                    successful_queries += 1;
                    let cached_section =
                        clip_bytes(&parsed.full_text, ENTRY_BYTES.saturating_sub(full.len()));
                    full.push_str(&format!("## {query}\n{cached_section}\n\n"));
                    for n in parsed.notices {
                        if !notices.contains(&n) {
                            notices.push(n);
                        }
                    }
                    let mut entry = json!({
                        "query": query,
                        "ok": true,
                        "resultCount": parsed.results.len(),
                        "results": parsed.results,
                        "preview": clip_chars(cached_section, config.defaults.max_excerpt_chars),
                    });
                    if let Some(synth) = parsed.synthesis {
                        entry["synthesis"] = json!(synth);
                    }
                    if let Some(grounding) = parsed.grounding {
                        entry["grounding"] = grounding;
                    }
                    query_rows.push(entry);
                }
                Err(err) => {
                    collection_errors.push(json!({
                        "source": query,
                        "message": err,
                    }));
                    query_rows.push(json!({
                        "query": query,
                        "ok": false,
                        "resultCount": 0,
                        "results": [],
                        "error": err,
                    }));
                }
            }
        }

        if successful_queries == 0 {
            let reason = collection_errors
                .first()
                .and_then(|e| e["message"].as_str())
                .unwrap_or("联网检索全部失败，请稍后重试。");
            return Err(reason.to_owned());
        }

        let response_id = self.store(full)?;
        let mut out = json!({
            "provider": used_provider.as_str(),
            "mode": mode.as_str(),
            "queryCount": queries.len(),
            "successfulQueries": successful_queries,
            "responseId": response_id,
            "queries": query_rows,
        });
        let degraded = successful_queries < queries.len() || degraded_from.is_some();
        if degraded {
            out["degraded"] = json!(true);
        }
        if let Some(from) = degraded_from {
            out["degradedFrom"] = json!(from);
        }
        if !collection_errors.is_empty() {
            out["collectionErrors"] = json!(collection_errors);
        }
        if !notices.is_empty() {
            out["notice"] = json!(notices.join("；"));
        }
        Ok(out)
    }

    async fn run_batch_search(
        &self,
        provider: WebSearchProvider,
        queries: &[String],
        opts: &SearchRunOpts,
        config: &WebSearchConfig,
        transport: &Arc<dyn HttpTransport>,
        cancel: CancellationToken,
    ) -> Vec<Result<ParsedSearch, String>> {
        let sem = Arc::new(Semaphore::new(config.max_concurrent.clamp(1, 5)));
        let futs = queries.iter().map(|q| {
            let sem = sem.clone();
            let cancel = cancel.clone();
            let q = q.clone();
            async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|_| "检索并发槽不可用。".to_owned())?;
                self.run_single_search(provider, &q, opts, config, transport, cancel)
                    .await
            }
        });
        join_all(futs).await
    }

    async fn run_single_search(
        &self,
        provider: WebSearchProvider,
        query: &str,
        opts: &SearchRunOpts,
        config: &WebSearchConfig,
        transport: &Arc<dyn HttpTransport>,
        cancel: CancellationToken,
    ) -> Result<ParsedSearch, String> {
        match provider {
            WebSearchProvider::Exa => {
                if !config.providers.exa.enabled {
                    return Err("Exa 提供商已在配置中停用。".into());
                }
                self.search_exa(query, opts, config, transport, cancel)
                    .await
            }
            WebSearchProvider::Deepseek => {
                if !config.providers.deepseek.enabled {
                    return Err("DeepSeek 提供商已在配置中停用。".into());
                }
                self.search_deepseek(query, opts, config, transport, cancel)
                    .await
            }
        }
    }

    async fn search_exa(
        &self,
        query: &str,
        opts: &SearchRunOpts,
        config: &WebSearchConfig,
        transport: &Arc<dyn HttpTransport>,
        cancel: CancellationToken,
    ) -> Result<ParsedSearch, String> {
        let exa = &config.providers.exa;
        let key = exa.api_key.trim();
        let exa_type = match opts.mode {
            SearchMode::Fast => "lite",
            SearchMode::Auto => exa.default_type.as_str(),
            SearchMode::Deep => exa.deep_type.as_str(),
        };
        let timeout_ms = if matches!(exa_type, "deep" | "deep-lite") {
            config.deep_timeout_ms
        } else {
            config.timeout_ms
        };

        if key.is_empty() {
            if !exa.allow_free_mcp_fallback {
                return Err("未配置 Exa API Key，且已关闭免费 MCP 回退。".into());
            }
            let mcp_type = if exa_type == "lite" { "fast" } else { "auto" };
            let request = HttpRequest {
                url: "https://mcp.exa.ai/mcp".into(),
                headers: vec![(
                    "Accept".into(),
                    "application/json, text/event-stream".into(),
                )],
                body: Some(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "web_search_exa",
                        "arguments": {
                            "query": query,
                            "numResults": opts.num_results,
                            "type": mcp_type
                        }
                    }
                })),
                timeout_ms: Some(timeout_ms),
            };
            let response = transport.request(request, cancel).await?;
            if !(200..300).contains(&response.status) {
                return Err(format!("Exa 检索失败（HTTP {}）。", response.status));
            }
            return parse_exa_response(&response.body, config.defaults.max_excerpt_chars);
        }

        let body = build_exa_search_payload(
            query,
            opts,
            exa,
            exa_type,
            config.defaults.max_excerpt_chars,
        );
        let url = format!("{}/search", exa.base_url.trim_end_matches('/'));
        let request = HttpRequest {
            url,
            headers: vec![("x-api-key".into(), key.to_owned())],
            body: Some(body),
            timeout_ms: Some(timeout_ms),
        };
        let response = transport.request(request, cancel).await?;
        if !(200..300).contains(&response.status) {
            return Err(format!("Exa 检索失败（HTTP {}）。", response.status));
        }
        parse_exa_response(&response.body, config.defaults.max_excerpt_chars)
    }

    async fn search_deepseek(
        &self,
        query: &str,
        opts: &SearchRunOpts,
        config: &WebSearchConfig,
        transport: &Arc<dyn HttpTransport>,
        cancel: CancellationToken,
    ) -> Result<ParsedSearch, String> {
        let ds = &config.providers.deepseek;
        let key = self
            .resolve_deepseek_key(config)
            .ok_or("未配置 DeepSeek Search API Key，请配置密钥或开启复用模型 Key。")?;
        let (composed_query, omitted_filters) = build_deepseek_query(
            query,
            &opts.include_domains,
            &opts.exclude_domains,
            opts.after_date.as_deref(),
            opts.before_date.as_deref(),
            if opts.languages.is_empty() {
                &ds.languages
            } else {
                &opts.languages
            },
        )?;
        let count = if opts.mode == SearchMode::Deep {
            opts.num_results.max(ds.default_count).clamp(10, 50)
        } else {
            opts.num_results.clamp(1, 50)
        };
        let need_content = opts.mode == SearchMode::Deep
            || opts.content_mode == ContentMode::Text
            || ds.need_content;
        let mut payload = Map::new();
        payload.insert("query".into(), json!(composed_query));
        payload.insert("count".into(), json!(count));
        payload.insert("need_content".into(), json!(need_content));
        if ds.send_original_question
            && let Some(ori) = opts
                .ori_question
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            payload.insert(
                "ori_question".into(),
                json!(clip_chars(ori, DEEPSEEK_MAX_QUERY_CHARS)),
            );
        }
        if !ds.localization.trim().is_empty() {
            payload.insert("localization".into(), json!(ds.localization.trim()));
        }

        let timeout_ms = if opts.mode == SearchMode::Deep {
            config.deep_timeout_ms
        } else {
            config.timeout_ms
        };
        let url = format!("{}/v1/tools/search", ds.base_url.trim_end_matches('/'));
        let request = HttpRequest {
            url,
            headers: vec![("Authorization".into(), format!("Bearer {key}"))],
            body: Some(Value::Object(payload)),
            timeout_ms: Some(timeout_ms),
        };
        let response = transport.request(request, cancel).await?;
        if !(200..300).contains(&response.status) {
            return Err(format!("DeepSeek 检索失败（HTTP {}）。", response.status));
        }
        let mut parsed = parse_deepseek_response(
            &response.body,
            ds.min_rerank_score,
            config.defaults.max_excerpt_chars,
        )?;
        if omitted_filters {
            parsed.notices.push(
                "DeepSeek 查询长度受 128 字符限制，已优先保留主查询词并省略部分过滤算子。"
                    .to_owned(),
            );
        }
        Ok(parsed)
    }

    pub async fn fetch(
        &self,
        input: FetchInput,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        let config = self.effective_config()?;
        if !config.enabled {
            return Err("联网检索已在配置中关闭。".into());
        }
        let mut urls = input.urls;
        if let Some(url) = input.url {
            urls.insert(0, url);
        }
        if urls.is_empty() || urls.len() > 5 {
            return Err("每次提供 1–5 个网页地址。".into());
        }
        let strategy = input.mode.unwrap_or(config.fetch_strategy);
        let provider = input.provider.unwrap_or(config.default_provider);
        let max_chars = input
            .max_characters
            .unwrap_or(config.defaults.max_fetch_chars)
            .clamp(500, 100_000);
        let policy = NetworkPolicy {
            allow_tun: !config.ssrf.allow_ranges.is_empty(),
        };
        let transport = self.transport_for(&config);

        // Decide effective fetch route: Exa paid `/contents`, DeepSeek `/v1/tools/fetch`, or direct HTTP.
        let use_provider = match strategy {
            FetchStrategy::Direct => false,
            FetchStrategy::Provider => true,
            FetchStrategy::Auto => match provider {
                WebSearchProvider::Exa => !config.providers.exa.api_key.trim().is_empty(),
                WebSearchProvider::Deepseek => self.resolve_deepseek_key(&config).is_some(),
            },
        };
        let fetch_mode = if use_provider { "provider" } else { "direct" };

        let sem = Arc::new(Semaphore::new(config.max_concurrent.clamp(1, 5)));
        let futs = urls.iter().map(|raw_url| {
            let sem = sem.clone();
            let cancel = cancel.clone();
            let raw_url = raw_url.clone();
            let policy = policy.clone();
            let config = config.clone();
            let transport = transport.clone();
            async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|_| "抓取并发槽不可用。".to_owned())?;
                // Local SSRF URL policy ALWAYS runs before direct OR cloud provider fetch.
                let validated = policy.validate_url(&raw_url)?;
                if use_provider {
                    self.fetch_via_provider(
                        provider,
                        validated.as_str(),
                        max_chars,
                        &config,
                        &transport,
                        cancel,
                    )
                    .await
                } else {
                    self.fetch_direct(validated.as_str(), &config, &transport, cancel)
                        .await
                }
            }
        });
        let outcomes = join_all(futs).await;

        let mut url_rows = Vec::with_capacity(urls.len());
        let mut collection_errors = Vec::new();
        let mut full = String::new();
        let mut successful = 0usize;

        for (raw_url, outcome) in urls.iter().zip(outcomes) {
            match outcome {
                Ok(page) => {
                    successful += 1;
                    let total_chars = page.content.chars().count();
                    let clipped_cache =
                        clip_bytes(&page.content, ENTRY_BYTES.saturating_sub(full.len()));
                    full.push_str(&format!("## {raw_url}\n{clipped_cache}\n\n"));
                    let truncated = total_chars > max_chars;
                    let body = clip_chars(&page.content, max_chars).to_owned();
                    let mut row = json!({
                        "url": raw_url,
                        "ok": true,
                        "totalCharacters": total_chars,
                        "truncated": truncated,
                        "content": body,
                    });
                    if let Some(title) = page.title {
                        row["title"] = json!(title);
                    }
                    url_rows.push(row);
                }
                Err(err) => {
                    collection_errors.push(json!({
                        "source": raw_url,
                        "message": err,
                    }));
                    url_rows.push(json!({
                        "url": raw_url,
                        "ok": false,
                        "content": "",
                        "error": err,
                    }));
                }
            }
        }

        if successful == 0 {
            let reason = collection_errors
                .first()
                .and_then(|e| e["message"].as_str())
                .unwrap_or("网页抓取全部失败。");
            return Err(reason.to_owned());
        }

        let response_id = self.store(full)?;
        let mut out = json!({
            "provider": if use_provider { provider.as_str() } else { "direct" },
            "fetchMode": fetch_mode,
            "urlCount": urls.len(),
            "successful": successful,
            "responseId": response_id,
            "urls": url_rows,
        });
        if successful < urls.len() {
            out["degraded"] = json!(true);
        }
        if !collection_errors.is_empty() {
            out["collectionErrors"] = json!(collection_errors);
        }
        Ok(out)
    }

    async fn fetch_via_provider(
        &self,
        provider: WebSearchProvider,
        url: &str,
        max_chars: usize,
        config: &WebSearchConfig,
        transport: &Arc<dyn HttpTransport>,
        cancel: CancellationToken,
    ) -> Result<FetchedPage, String> {
        match provider {
            WebSearchProvider::Exa => {
                let key = config.providers.exa.api_key.trim();
                if key.is_empty() {
                    return Err("未配置 Exa API Key，无法使用 Exa 正文抓取。".into());
                }
                let endpoint = format!(
                    "{}/contents",
                    config.providers.exa.base_url.trim_end_matches('/')
                );
                let req = HttpRequest {
                    url: endpoint,
                    headers: vec![("x-api-key".into(), key.to_owned())],
                    body: Some(json!({
                        "urls": [url],
                        "text": { "maxCharacters": max_chars }
                    })),
                    timeout_ms: Some(config.timeout_ms),
                };
                let resp = transport.request(req, cancel).await?;
                if !(200..300).contains(&resp.status) {
                    return Err(format!("Exa 网页抓取失败（HTTP {}）。", resp.status));
                }
                let val: Value =
                    serde_json::from_str(&resp.body).map_err(|_| "Exa 返回数据格式无效。")?;
                let first = val
                    .get("results")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .ok_or("Exa 未返回网页正文。")?;
                let title = first
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned);
                let content = first
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if content.trim().is_empty() {
                    return Err("网页正文为空。".into());
                }
                Ok(FetchedPage { title, content })
            }
            WebSearchProvider::Deepseek => {
                let key = self
                    .resolve_deepseek_key(config)
                    .ok_or("未配置 DeepSeek API Key，无法使用 DeepSeek 正文抓取。")?;
                let endpoint = format!(
                    "{}/v1/tools/fetch",
                    config.providers.deepseek.base_url.trim_end_matches('/')
                );
                let req = HttpRequest {
                    url: endpoint,
                    headers: vec![("Authorization".into(), format!("Bearer {key}"))],
                    body: Some(json!({ "url": url })),
                    timeout_ms: Some(config.timeout_ms),
                };
                let resp = transport.request(req, cancel).await?;
                if !(200..300).contains(&resp.status) {
                    return Err(format!("DeepSeek 网页抓取失败（HTTP {}）。", resp.status));
                }
                let val: Value =
                    serde_json::from_str(&resp.body).map_err(|_| "DeepSeek 返回数据格式无效。")?;
                let title = val
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned);
                let content = val
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if content.trim().is_empty() {
                    return Err("网页正文为空。".into());
                }
                Ok(FetchedPage { title, content })
            }
        }
    }

    async fn fetch_direct(
        &self,
        url: &str,
        config: &WebSearchConfig,
        transport: &Arc<dyn HttpTransport>,
        cancel: CancellationToken,
    ) -> Result<FetchedPage, String> {
        let resp = transport
            .request(
                HttpRequest {
                    url: url.to_owned(),
                    body: None,
                    headers: Vec::new(),
                    timeout_ms: Some(config.timeout_ms),
                },
                cancel,
            )
            .await?;
        if !(200..300).contains(&resp.status) {
            return Err(format!("网页获取失败（HTTP {}）。", resp.status));
        }
        let title = extract_html_title(&resp.body);
        let content = readable_text(&resp.body);
        Ok(FetchedPage { title, content })
    }

    pub fn get(&self, input: ContentInput) -> Result<Value, String> {
        let mut cache = self.cache.lock().map_err(|_| "检索缓存不可用。")?;
        cache.retain(|item| item.created.elapsed() < CACHE_TTL);
        let entry = cache
            .iter()
            .find(|item| item.id == input.response_id)
            .ok_or("检索内容不存在或已过期，请重新检索。")?;
        let total_chars = entry.text.chars().count();
        if let Some(needle) = input.find_text {
            if needle.is_empty() || needle.len() > 4096 {
                return Err("查找文本为空或过长。".into());
            }
            let total_matches = entry.text.match_indices(&needle).count();
            let mut excerpts = Vec::new();
            for (index, _) in entry.text.match_indices(&needle).take(20) {
                let begin = floor_boundary(&entry.text, index.saturating_sub(120));
                let end = floor_boundary(
                    &entry.text,
                    (index + needle.len() + 240).min(entry.text.len()),
                );
                excerpts.push(&entry.text[begin..end]);
            }
            let joined = excerpts.join("\n\n");
            let clipped = clip_chars(&joined, 6000).to_owned();
            let returned_chars = clipped.chars().count();
            let truncated = total_matches > 20 || joined.chars().count() > 6000;
            return Ok(json!({
                "responseId": entry.id,
                "mode": "find",
                "content": clipped,
                "totalCharacters": total_chars,
                "offset": 0,
                "returnedCharacters": returned_chars,
                "truncated": truncated,
                "matchCount": total_matches,
            }));
        }

        let offset = input.offset.unwrap_or(0);
        let limit = input.limit.unwrap_or(6000).clamp(1, 6000);
        let text: String = entry.text.chars().skip(offset).take(limit).collect();
        let returned_chars = text.chars().count();
        let next = offset.saturating_add(returned_chars);
        let truncated = next < total_chars;
        let mut out = json!({
            "responseId": entry.id,
            "mode": "range",
            "content": text,
            "totalCharacters": total_chars,
            "offset": offset,
            "returnedCharacters": returned_chars,
            "truncated": truncated,
        });
        if truncated {
            out["nextOffset"] = json!(next);
        }
        Ok(out)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = Vec::new();
        {
            let web = self.clone();
            definitions.push(ToolDefinition::from_typed::<SearchInput, _, _>(
                "web_search",
                "联网检索",
                "通过 Exa 或 DeepSeek 检索公开网页（支持 1–3 词并发、fast/auto/deep 模式与局部容错），正文保存在当前会话内存缓存。",
                move |_, input, cancel, _, _| {
                    let web = web.clone();
                    async move { json_result(web.search(input, cancel.unwrap_or_default()).await?) }
                },
            ));
        }
        {
            let web = self.clone();
            definitions.push(ToolDefinition::from_typed::<FetchInput, _, _>(
                "web_fetch",
                "抓取网页",
                "并发抓取 1–5 个公开网页正文（支持服务商清洗或直连 UTF-8/GB18030 解码），禁止内网与私网重定向。",
                move |_, input, cancel, _, _| {
                    let web = web.clone();
                    async move { json_result(web.fetch(input, cancel.unwrap_or_default()).await?) }
                },
            ));
        }
        {
            let web = self.clone();
            definitions.push(ToolDefinition::from_typed::<ContentInput, _, _>(
                "get_search_content",
                "读取检索正文",
                "使用 responseId 分页读取或按关键词查找当前会话的检索与抓取正文缓存。",
                move |_, input, _, _, _| {
                    let web = web.clone();
                    async move { json_result(web.get(input)?) }
                },
            ));
        }
        for definition in &mut definitions {
            definition.prompt_snippet = Some(definition.description.clone());
        }
        definitions
    }
}

struct SearchRunOpts {
    mode: SearchMode,
    num_results: usize,
    content_mode: ContentMode,
    include_domains: Vec<String>,
    exclude_domains: Vec<String>,
    after_date: Option<String>,
    before_date: Option<String>,
    languages: Vec<String>,
    additional_queries: Vec<String>,
    ori_question: Option<String>,
}

struct ParsedSearch {
    results: Vec<Value>,
    synthesis: Option<String>,
    grounding: Option<Value>,
    full_text: String,
    notices: Vec<String>,
}

struct FetchedPage {
    title: Option<String>,
    content: String,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchInput {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub queries: Vec<String>,
    #[serde(default)]
    pub mode: Option<SearchMode>,
    #[serde(default)]
    pub num_results: Option<usize>,
    #[serde(default)]
    pub include_domains: Vec<String>,
    #[serde(default)]
    pub exclude_domains: Vec<String>,
    #[serde(default)]
    pub after_date: Option<String>,
    #[serde(default)]
    pub before_date: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub content_mode: Option<ContentMode>,
    #[serde(default)]
    pub provider: Option<WebSearchProvider>,
    #[serde(default)]
    pub additional_queries: Vec<String>,
    #[serde(default)]
    pub ori_question: Option<String>,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FetchInput {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub max_characters: Option<usize>,
    #[serde(default)]
    pub mode: Option<FetchStrategy>,
    #[serde(default)]
    pub provider: Option<WebSearchProvider>,
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

fn build_exa_search_payload(
    query: &str,
    opts: &SearchRunOpts,
    exa: &ExaConfig,
    exa_type: &str,
    max_excerpt_chars: usize,
) -> Value {
    let is_lite = exa_type == "lite";
    let is_deep = matches!(exa_type, "deep" | "deep-lite");
    let mut payload = Map::new();
    payload.insert("query".into(), json!(query));
    payload.insert("type".into(), json!(exa_type));
    payload.insert("numResults".into(), json!(opts.num_results));

    let effective_view = match opts.content_mode {
        ContentMode::Summary if is_lite => ContentMode::Highlights,
        other => other,
    };
    let mut contents = Map::new();
    match effective_view {
        ContentMode::Highlights => {
            contents.insert(
                "highlights".into(),
                json!({ "maxCharacters": max_excerpt_chars }),
            );
        }
        ContentMode::Text => {
            contents.insert(
                "text".into(),
                json!({
                    "maxCharacters": max_excerpt_chars.max(4000),
                    "verbosity": exa.text_verbosity,
                }),
            );
        }
        ContentMode::Summary => {
            contents.insert("summary".into(), json!({ "query": query }));
        }
    }
    if !is_lite && let Some(hours) = exa.max_age_hours {
        contents.insert("maxAgeHours".into(), json!(hours));
    }
    payload.insert("contents".into(), Value::Object(contents));

    if !opts.include_domains.is_empty() {
        payload.insert("includeDomains".into(), json!(opts.include_domains));
    }
    if !opts.exclude_domains.is_empty() {
        payload.insert("excludeDomains".into(), json!(opts.exclude_domains));
    }
    if let Some(after) = opts
        .after_date
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        payload.insert("startPublishedDate".into(), json!(after));
    }
    if let Some(before) = opts
        .before_date
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        payload.insert("endPublishedDate".into(), json!(before));
    }
    if exa.moderation {
        payload.insert("moderation".into(), json!(true));
    }
    if is_deep && !opts.additional_queries.is_empty() {
        let extra: Vec<&String> = opts.additional_queries.iter().take(10).collect();
        payload.insert("additionalQueries".into(), json!(extra));
    }
    Value::Object(payload)
}

pub(crate) fn build_deepseek_query(
    raw_query: &str,
    include_domains: &[String],
    exclude_domains: &[String],
    after_date: Option<&str>,
    before_date: Option<&str>,
    languages: &[String],
) -> Result<(String, bool), String> {
    let base = raw_query.trim();
    if base.is_empty() {
        return Err("检索词不能为空。".into());
    }
    if base.chars().count() > DEEPSEEK_MAX_QUERY_CHARS {
        return Err(format!(
            "DeepSeek 检索词超过 {DEEPSEEK_MAX_QUERY_CHARS} 个字符上限，请缩短检索词。"
        ));
    }
    let mut ops = Vec::new();
    for d in include_domains {
        let d = d.trim();
        if !d.is_empty() {
            ops.push(format!("site:{d}"));
        }
    }
    for d in exclude_domains {
        let d = d.trim();
        if !d.is_empty() {
            ops.push(format!("-site:{d}"));
        }
    }
    if let Some(after) = after_date.map(str::trim).filter(|s| !s.is_empty()) {
        ops.push(format!("after:{after}"));
    }
    if let Some(before) = before_date.map(str::trim).filter(|s| !s.is_empty()) {
        ops.push(format!("before:{before}"));
    }
    let langs: Vec<&str> = languages
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !langs.is_empty() {
        ops.push(format!("lang:{}", langs.join(",")));
    }

    let mut out = base.to_owned();
    let mut current_chars = out.chars().count();
    let mut omitted = false;
    for op in ops {
        let add = 1 + op.chars().count();
        if current_chars + add <= DEEPSEEK_MAX_QUERY_CHARS {
            out.push(' ');
            out.push_str(&op);
            current_chars += add;
        } else {
            omitted = true;
        }
    }
    Ok((out, omitted))
}

fn parse_exa_response(body: &str, max_excerpt_chars: usize) -> Result<ParsedSearch, String> {
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
    let mut notices = Vec::new();
    if let Some(arr) = value.get("notices").and_then(Value::as_array) {
        for item in arr {
            if let Some(s) = item.as_str() {
                notices.push(s.to_owned());
            } else if let Some(msg) = item.get("message").and_then(Value::as_str) {
                notices.push(msg.to_owned());
            }
        }
    }
    let synthesis = value
        .pointer("/output/content")
        .and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| serde_json::to_string(v).ok())
        })
        .filter(|s| !s.trim().is_empty());
    let grounding = value.pointer("/output/grounding").cloned();

    if let Some(results) = value.get("results").and_then(Value::as_array) {
        let mut items = Vec::with_capacity(results.len());
        let mut full_text = String::new();
        if let Some(ref synth) = synthesis {
            full_text.push_str("## Synthesis\n");
            full_text.push_str(synth);
            full_text.push_str("\n\n");
        }
        for item in results.iter().take(20) {
            let title = item.get("title").and_then(Value::as_str).unwrap_or("");
            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
            let highlights = item
                .get("highlights")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|s| !s.is_empty());
            let summary = item
                .get("summary")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let excerpt_raw = highlights.or(summary).or(text.clone()).unwrap_or_default();
            let snippet = clip_chars(&excerpt_raw, 240).to_owned();
            let excerpt = clip_chars(&excerpt_raw, max_excerpt_chars).to_owned();
            full_text.push_str(&format!(
                "# {title}\n{url}\n{}\n\n",
                text.as_deref().unwrap_or(&excerpt_raw)
            ));
            let mut row = json!({
                "title": title,
                "url": url,
                "snippet": snippet,
                "excerpt": excerpt,
            });
            if let Some(pub_date) = item.get("publishedDate").and_then(Value::as_str) {
                row["publishedAt"] = json!(pub_date);
            }
            if let Some(score) = item.get("score").and_then(Value::as_f64) {
                row["score"] = json!(score);
            }
            items.push(row);
        }
        return Ok(ParsedSearch {
            results: items,
            synthesis,
            grounding,
            full_text,
            notices,
        });
    }

    // Hosted Exa MCP JSON-RPC response (`result.content[].text`).
    let contents = value
        .pointer("/result/content")
        .and_then(Value::as_array)
        .ok_or("Exa 返回的数据缺少正文。")?;
    let full_text = contents
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    let results = parse_mcp_text_results(&full_text, max_excerpt_chars);
    Ok(ParsedSearch {
        results,
        synthesis: None,
        grounding: None,
        full_text,
        notices,
    })
}

fn parse_mcp_text_results(raw: &str, max_excerpt_chars: usize) -> Vec<Value> {
    let mut results = Vec::new();
    let mut title = String::new();
    let mut url = String::new();
    let mut body = String::new();
    let flush = |title: &mut String, url: &mut String, body: &mut String, out: &mut Vec<Value>| {
        if !title.is_empty() || !url.is_empty() {
            let trimmed = body.trim();
            out.push(json!({
                "title": title.trim(),
                "url": url.trim(),
                "snippet": clip_chars(trimmed, 240),
                "excerpt": clip_chars(trimmed, max_excerpt_chars),
            }));
        }
        title.clear();
        url.clear();
        body.clear();
    };
    for line in raw.lines() {
        if let Some(t) = line.strip_prefix("Title:") {
            flush(&mut title, &mut url, &mut body, &mut results);
            title = t.trim().to_owned();
        } else if let Some(u) = line.strip_prefix("URL:") {
            url = u.trim().to_owned();
        } else if let Some(t) = line.strip_prefix("Text:") {
            body.push_str(t.trim());
            body.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&mut title, &mut url, &mut body, &mut results);
    if results.is_empty() && !raw.trim().is_empty() {
        results.push(json!({
            "title": "Exa 检索摘要",
            "url": "",
            "snippet": clip_chars(raw.trim(), 240),
            "excerpt": clip_chars(raw.trim(), max_excerpt_chars),
        }));
    }
    results
}

fn parse_deepseek_response(
    body: &str,
    min_rerank_score: f64,
    max_excerpt_chars: usize,
) -> Result<ParsedSearch, String> {
    let value: Value = serde_json::from_str(body).map_err(|_| "DeepSeek 返回的数据格式无效。")?;
    if value.get("error").is_some() {
        return Err("DeepSeek 未能完成检索，请稍后重试。".into());
    }
    let arr = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or("DeepSeek 返回的数据缺少 results。")?;
    let mut results = Vec::new();
    let mut full_text = String::new();
    for item in arr {
        let score = item.get("rerank_score").and_then(Value::as_f64);
        if min_rerank_score > 0.0 && score.is_some_and(|s| s < min_rerank_score) {
            continue;
        }
        let title = item.get("title").and_then(Value::as_str).unwrap_or("");
        let url = item.get("url").and_then(Value::as_str).unwrap_or("");
        let snippet = item.get("snippet").and_then(Value::as_str).unwrap_or("");
        let summary = item.get("summary").and_then(Value::as_str).unwrap_or("");
        let content = item.get("content").and_then(Value::as_str).unwrap_or("");
        let rich = if !content.is_empty() {
            content
        } else if !summary.is_empty() {
            summary
        } else {
            snippet
        };
        full_text.push_str(&format!("# {title}\n{url}\n{rich}\n\n"));
        let mut row = json!({
            "title": title,
            "url": url,
            "snippet": if snippet.is_empty() { clip_chars(rich, 240) } else { snippet },
            "excerpt": clip_chars(if !summary.is_empty() { summary } else { rich }, max_excerpt_chars),
        });
        if let Some(date) = item.get("date").and_then(Value::as_str) {
            row["publishedAt"] = json!(date);
        }
        if let Some(s) = score {
            row["score"] = json!(s);
        }
        if let Some(auth) = item.get("authority").and_then(Value::as_i64) {
            row["authority"] = json!(auth);
        }
        if let Some(site) = item.get("site_name").and_then(Value::as_str) {
            row["siteName"] = json!(site);
        }
        results.push(row);
    }
    Ok(ParsedSearch {
        results,
        synthesis: None,
        grounding: None,
        full_text,
        notices: Vec::new(),
    })
}

fn extract_html_title(body: &str) -> Option<String> {
    static TITLE_RE: OnceLock<Regex> = OnceLock::new();
    let re = TITLE_RE.get_or_init(|| Regex::new(r"(?is)<title\b[^>]*>(.*?)</title\s*>").unwrap());
    re.captures(body)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().replace(['\r', '\n'], " "))
        .filter(|s| !s.is_empty())
}

fn readable_text(body: &str) -> String {
    if !body.trim_start().starts_with('<') {
        return body.to_owned();
    }
    static SCRIPT_RE: OnceLock<Regex> = OnceLock::new();
    static STYLE_RE: OnceLock<Regex> = OnceLock::new();
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let script_re =
        SCRIPT_RE.get_or_init(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap());
    let style_re =
        STYLE_RE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").unwrap());
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"(?s)<[^>]*>").unwrap());

    let text = script_re.replace_all(body, "\n");
    let text = style_re.replace_all(&text, "\n");
    let text = tag_re.replace_all(&text, "\n");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ssrf::HttpResponse;
    use pi_agent::BoxFuture;

    struct ScriptedTransport {
        handler: Arc<dyn Fn(HttpRequest) -> Result<HttpResponse, String> + Send + Sync>,
        seen: Mutex<Vec<HttpRequest>>,
    }
    impl ScriptedTransport {
        fn new(
            f: impl Fn(HttpRequest) -> Result<HttpResponse, String> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                handler: Arc::new(f),
                seen: Mutex::new(Vec::new()),
            })
        }
    }
    impl HttpTransport for ScriptedTransport {
        fn request(
            &self,
            request: HttpRequest,
            _cancel: CancellationToken,
        ) -> BoxFuture<'_, Result<HttpResponse, String>> {
            self.seen.lock().unwrap().push(request.clone());
            let res = (self.handler)(request);
            Box::pin(async move { res })
        }
    }

    #[test]
    fn v1_config_migrates_cleanly_to_v2_with_exa_default() {
        let cfg = WebSearchConfig::from_json_value(json!({
            "searchProvider": "exa",
            "workflow": "none",
            "exaApiKey": "exa-secret-1234",
            "ssrf": { "allowRanges": ["198.18.0.0/15"] }
        }))
        .unwrap();
        assert_eq!(cfg.version, 2);
        assert_eq!(cfg.default_provider, WebSearchProvider::Exa);
        assert_eq!(cfg.providers.exa.api_key, "exa-secret-1234");
        assert!(cfg.providers.exa.allow_free_mcp_fallback);
        let masked = cfg.masked_view(None);
        assert_eq!(masked["providers"]["exa"]["hasApiKey"], true);
        assert_eq!(masked["providers"]["exa"]["apiKeyTail"], "1234");
        assert!(masked["providers"]["exa"].get("apiKey").is_none());
    }

    #[test]
    fn exa_lite_guard_strips_unsupported_fields() {
        let exa = ExaConfig {
            max_age_hours: Some(24),
            ..Default::default()
        };
        let opts = SearchRunOpts {
            mode: SearchMode::Fast,
            num_results: 5,
            content_mode: ContentMode::Summary,
            include_domains: vec!["learn.microsoft.com".into()],
            exclude_domains: vec![],
            after_date: None,
            before_date: None,
            languages: vec![],
            additional_queries: vec!["extra".into()],
            ori_question: None,
        };
        let payload = build_exa_search_payload("bsod", &opts, &exa, "lite", 2000);
        assert_eq!(payload["type"], "lite");
        assert!(payload["contents"].get("maxAgeHours").is_none());
        assert!(payload["contents"].get("summary").is_none());
        assert!(payload["contents"].get("highlights").is_some());
        assert!(payload.get("additionalQueries").is_none());
    }

    #[test]
    fn deepseek_query_guard_preserves_main_query_within_128_chars() {
        let base = "蓝屏错误代码 0x0000007B 启动修复";
        let domains: Vec<String> = (0..10)
            .map(|i| format!("very-long-domain-name-{i}.example.com"))
            .collect();
        let (composed, omitted) = build_deepseek_query(
            base,
            &domains,
            &[],
            Some("2025-01-01"),
            None,
            &["zh".into()],
        )
        .unwrap();
        assert!(composed.starts_with(base));
        assert!(composed.chars().count() <= 128);
        assert!(omitted);
        assert!(build_deepseek_query(&"a".repeat(129), &[], &[], None, None, &[]).is_err());
    }

    #[tokio::test]
    async fn multi_query_partial_failure_returns_degraded_with_successful_results() {
        let transport = ScriptedTransport::new(|req| {
            let q = req.body.as_ref().unwrap()["params"]["arguments"]["query"]
                .as_str()
                .unwrap_or("");
            if q == "bad" {
                Ok(HttpResponse {
                    status: 429,
                    body: "rate limited".into(),
                })
            } else {
                Ok(HttpResponse {
                    status: 200,
                    body: json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": { "content": [{ "type": "text", "text": "Title: OK\nURL: https://example.com\n正文" }] }
                    })
                    .to_string(),
                })
            }
        });
        let web = WebAccess::with_transport(transport);
        let res = web
            .search(
                SearchInput {
                    queries: vec!["good".into(), "bad".into()],
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(res["queryCount"], 2);
        assert_eq!(res["successfulQueries"], 1);
        assert_eq!(res["degraded"], true);
        assert_eq!(res["queries"][0]["ok"], true);
        assert_eq!(res["queries"][1]["ok"], false);
        assert_eq!(res["collectionErrors"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn provider_fetch_blocks_private_ip_before_sending_external_request() {
        let transport = ScriptedTransport::new(|_| {
            panic!("private IP must be blocked locally before calling external fetch provider")
        });
        let mut cfg = WebSearchConfig {
            default_provider: WebSearchProvider::Deepseek,
            ..Default::default()
        };
        cfg.providers.deepseek.api_key = "ds-key-test".into();
        let web = WebAccess::with_transport_and_config(transport.clone(), cfg);
        let err = web
            .fetch(
                FetchInput {
                    url: Some("http://127.0.0.1/admin".into()),
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.contains("内网") || err.contains("本机"));
        assert!(transport.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn deepseek_search_and_fallback_work_offline() {
        let transport = ScriptedTransport::new(|req| {
            if req.url.contains("api.deepseek.com/v1/tools/search") {
                Ok(HttpResponse {
                    status: 200,
                    body: json!({
                        "results": [{
                            "rerank_score": 2.4,
                            "title": "MS Docs",
                            "url": "https://learn.microsoft.com/bsod",
                            "snippet": "蓝屏排查",
                            "summary": "详细蓝屏排查摘要",
                            "content": "",
                            "site_name": "Microsoft Learn",
                            "date": "2026-10-20T00:00:00Z",
                            "authority": 4
                        }]
                    })
                    .to_string(),
                })
            } else {
                Ok(HttpResponse {
                    status: 500,
                    body: "exa down".into(),
                })
            }
        });
        let mut cfg = WebSearchConfig {
            default_provider: WebSearchProvider::Exa,
            fallback_provider: Some(WebSearchProvider::Deepseek),
            ..Default::default()
        };
        cfg.providers.deepseek.api_key = "ds-key-12345678".into();
        let web = WebAccess::with_transport_and_config(transport, cfg);
        let res = web
            .search(
                SearchInput {
                    query: Some("BSOD".into()),
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(res["provider"], "deepseek");
        assert_eq!(res["degraded"], true);
        assert!(res["degradedFrom"].as_str().unwrap().starts_with("exa:"));
        assert_eq!(res["queries"][0]["results"][0]["authority"], 4);
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use pi_ai::ModelsRefreshOptions;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{AuthRuntime, oa::validate_base};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderEndpointMode {
    Single,
    MultiEndpoint,
    CloudflareTemplate,
    ManagedOauth,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEndpointMetadata {
    pub endpoint_mode: ProviderEndpointMode,
    pub default_base_url: Option<String>,
    pub configured_base_url: Option<String>,
    pub endpoint_summary: Option<String>,
    pub cloudflare_account_id: Option<String>,
    pub cloudflare_gateway_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CustomApi {
    OpenaiCompletions,
    OpenaiResponses,
    OpenaiCodexResponses,
    AnthropicMessages,
    GoogleGenerativeAi,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomModel {
    pub id: String,
    pub name: String,
    pub context_window: u64,
    pub max_tokens: u64,
    pub reasoning: bool,
    pub image: bool,
}
// Never derive Debug: this input can contain a new secret.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomProviderInput {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api: CustomApi,
    pub api_key: Option<String>,
    pub models: Vec<CustomModel>,
    pub revision: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomProviderErrorKind {
    InvalidInput,
    Conflict,
    ConfigurationUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomProviderError {
    pub kind: CustomProviderErrorKind,
    pub message: String,
}

impl CustomProviderError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: CustomProviderErrorKind::InvalidInput,
            message: message.into(),
        }
    }

    pub fn conflict() -> Self {
        Self {
            kind: CustomProviderErrorKind::Conflict,
            message: "服务配置已修改，请刷新后重试。".into(),
        }
    }

    pub fn config_unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: CustomProviderErrorKind::ConfigurationUnavailable,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CustomProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CustomProviderError {}

fn legacy_error_string(err: CustomProviderError) -> String {
    match err.kind {
        CustomProviderErrorKind::Conflict => format!("config_conflict: {}", err.message),
        _ => err.message,
    }
}

fn revision(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn valid_revision(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn valid_id(id: &str) -> bool {
    (2..=64).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
        && !matches!(id, "__proto__" | "constructor" | "prototype")
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}
fn is_custom(value: &Value) -> bool {
    value.is_object()
        && value["baseUrl"].is_string()
        && value["api"].is_string()
        && value["models"].is_array()
}
fn config(value: Option<Value>) -> Result<Value, CustomProviderError> {
    let mut value = value.unwrap_or_else(|| json!({"providers":{}}));
    if value.get("providers").is_none() {
        value["providers"] = json!({});
    }
    if !value["providers"].is_object() {
        return Err(CustomProviderError::config_unavailable(
            "models.json 的 providers 格式无效，原文件已保留。",
        ));
    }
    Ok(value)
}
fn reserved(id: &str) -> bool {
    id == "cstoa" || pi_ai::builtin_providers().iter().any(|p| p.id() == id)
}

struct BuiltinProviderCatalogInfo {
    mode: ProviderEndpointMode,
    default_base_url: Option<String>,
    first_base_url: Option<String>,
    model_ids: BTreeSet<String>,
}

fn builtin_catalog_info() -> &'static BTreeMap<String, BuiltinProviderCatalogInfo> {
    static INFO: OnceLock<BTreeMap<String, BuiltinProviderCatalogInfo>> = OnceLock::new();
    INFO.get_or_init(|| {
        let mut map = BTreeMap::new();
        map.insert(
            "cstoa".to_owned(),
            BuiltinProviderCatalogInfo {
                mode: ProviderEndpointMode::ManagedOauth,
                default_base_url: None,
                first_base_url: None,
                model_ids: BTreeSet::new(),
            },
        );
        for p in pi_ai::builtin_providers() {
            let pid = p.id().to_owned();
            let models = p.get_models();
            let first_base_url = models
                .iter()
                .map(|m| m.base_url.trim())
                .find(|u| !u.is_empty())
                .map(str::to_owned);
            let distinct_urls: BTreeSet<String> = models
                .iter()
                .map(|m| normalize_endpoint_compare(&m.base_url))
                .filter(|u| !u.is_empty())
                .map(str::to_owned)
                .collect();
            let model_ids: BTreeSet<String> = models.into_iter().map(|m| m.id).collect();
            let auth = p.auth();
            let is_managed_oauth = matches!(
                pid.as_str(),
                "openai-codex" | "github-copilot" | "google-gemini-cli" | "google-antigravity"
            ) || (auth.oauth.is_some()
                && auth.api_key.as_ref().is_none_or(|a| a.env_only));
            let mode = if matches!(
                pid.as_str(),
                "cloudflare-workers-ai" | "cloudflare-ai-gateway"
            ) {
                ProviderEndpointMode::CloudflareTemplate
            } else if is_managed_oauth {
                ProviderEndpointMode::ManagedOauth
            } else if matches!(
                pid.as_str(),
                "opencode-go" | "opencode" | "openrouter" | "fireworks"
            ) || distinct_urls.len() > 1
            {
                ProviderEndpointMode::MultiEndpoint
            } else {
                ProviderEndpointMode::Single
            };
            let default_base_url = (mode == ProviderEndpointMode::Single)
                .then(|| first_base_url.clone())
                .flatten();
            map.insert(
                pid,
                BuiltinProviderCatalogInfo {
                    mode,
                    default_base_url,
                    first_base_url,
                    model_ids,
                },
            );
        }
        map
    })
}

pub fn classify_builtin_endpoint_mode(provider_id: &str) -> ProviderEndpointMode {
    builtin_catalog_info()
        .get(provider_id)
        .map(|i| i.mode)
        .unwrap_or(ProviderEndpointMode::Single)
}

pub fn default_builtin_base_url(provider_id: &str) -> Option<&'static str> {
    builtin_catalog_info()
        .get(provider_id)
        .and_then(|i| i.default_base_url.as_deref())
}

fn legacy_subprovider_ids(provider_id: &str) -> &'static [&'static str] {
    match provider_id {
        "opencode" => &["opencode-openai", "opencode-responses"],
        "opencode-go" => &["opencode-go-google"],
        _ => &[],
    }
}

fn entry_has_base_url_override(entry: Option<&Value>) -> bool {
    let Some(obj) = entry.and_then(Value::as_object) else {
        return false;
    };
    if obj
        .get("baseUrl")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    obj.get("models")
        .and_then(Value::as_array)
        .is_some_and(|arr| {
            arr.iter().any(|item| {
                item.as_object()
                    .and_then(|m| m.get("baseUrl"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
            })
        })
}

fn strip_entry_base_url_overrides(entry: Option<&Value>, provider_id: &str) -> Option<Value> {
    let mut next_obj = entry
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    next_obj.remove("baseUrl");
    let builtin_ids = builtin_catalog_info()
        .get(provider_id)
        .map(|i| &i.model_ids);
    let default_url = default_builtin_base_url(provider_id);
    if let Some(models) = next_obj.get_mut("models").and_then(Value::as_array_mut) {
        for item in models {
            let Some(m_obj) = item.as_object_mut() else {
                continue;
            };
            if !m_obj.contains_key("baseUrl") {
                continue;
            }
            let is_builtin_model = m_obj
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|mid| builtin_ids.is_some_and(|set| set.contains(mid)));
            if is_builtin_model || default_url.is_none() {
                m_obj.remove("baseUrl");
            } else if let Some(def) = default_url {
                m_obj.insert("baseUrl".to_owned(), json!(def));
            }
        }
    }
    (!next_obj.is_empty()).then_some(Value::Object(next_obj))
}

type EntrySnapshotMap = BTreeMap<String, Option<Value>>;

fn clear_provider_and_legacy_overrides(
    value: &mut Value,
    provider_id: &str,
) -> (EntrySnapshotMap, EntrySnapshotMap, bool) {
    let mut previous_map = BTreeMap::new();
    let mut committed_map = BTreeMap::new();
    let mut touched_subkeys = false;

    let prev_main = value["providers"].get(provider_id).cloned();
    let next_main = strip_entry_base_url_overrides(prev_main.as_ref(), provider_id);
    if let Some(ref next_val) = next_main {
        value["providers"][provider_id] = next_val.clone();
    } else {
        value["providers"]
            .as_object_mut()
            .expect("validated")
            .remove(provider_id);
    }
    previous_map.insert(provider_id.to_owned(), prev_main);
    committed_map.insert(provider_id.to_owned(), next_main);

    for &sub_id in legacy_subprovider_ids(provider_id) {
        let Some(prev_sub) = value["providers"].get(sub_id).cloned() else {
            continue;
        };
        touched_subkeys = true;
        let next_sub = strip_entry_base_url_overrides(Some(&prev_sub), sub_id);
        if let Some(ref next_val) = next_sub {
            value["providers"][sub_id] = next_val.clone();
        } else {
            value["providers"]
                .as_object_mut()
                .expect("validated")
                .remove(sub_id);
        }
        previous_map.insert(sub_id.to_owned(), Some(prev_sub));
        committed_map.insert(sub_id.to_owned(), next_sub);
    }

    (previous_map, committed_map, touched_subkeys)
}

fn normalize_endpoint_compare(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

fn strip_url_scheme_and_slash(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed)
        .to_owned()
}
impl AuthRuntime {
    /// Defaults for a new editor row; no local table of model capabilities is maintained.
    pub fn custom_model_defaults(&self) -> Value {
        let mut counts = BTreeMap::<u64, usize>::new();
        for model in self.runtime.get_all_models() {
            if model.max_tokens > 0 {
                *counts.entry(model.max_tokens).or_default() += 1;
            }
        }
        let max = counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(n, _)| n)
            .unwrap_or(16384);
        json!({"contextWindow":262144,"maxTokens":max})
    }
    pub fn custom_providers(&self) -> Result<Vec<String>, String> {
        let value = config(
            self.files
                .read("models.json")
                .map_err(CustomProviderError::config_unavailable)
                .map_err(legacy_error_string)?,
        )
        .map_err(legacy_error_string)?;
        Ok(value["providers"]
            .as_object()
            .expect("validated")
            .iter()
            .filter(|(id, v)| !reserved(id) && is_custom(v))
            .map(|(id, _)| id.clone())
            .collect())
    }
    pub fn custom_provider(&self, id: &str) -> Result<Option<Value>, String> {
        self.custom_provider_checked(id)
            .map_err(legacy_error_string)
    }
    pub fn custom_provider_checked(&self, id: &str) -> Result<Option<Value>, CustomProviderError> {
        let value = config(
            self.files
                .read("models.json")
                .map_err(CustomProviderError::config_unavailable)?,
        )?;
        let Some(p) = value["providers"]
            .get(id)
            .filter(|v| !reserved(id) && is_custom(v))
        else {
            return Ok(None);
        };
        let models: Vec<Value> = p["models"].as_array().expect("validated").iter().map(|m| json!({
            "id":m["id"],"name":m.get("name").unwrap_or(&m["id"]),
            "contextWindow":m.get("contextWindow").and_then(Value::as_u64).unwrap_or(128000),
            "maxTokens":m.get("maxTokens").and_then(Value::as_u64).unwrap_or(16384),
            "reasoning":m["reasoning"] == true,
            "image":m["input"].as_array().is_some_and(|v| v.iter().any(|v| v == "image"))
        })).collect();
        // Explicit allowlist: do not expose auth/header/unknown fields from disk.
        Ok(Some(
            json!({"id":id,"name":p.get("name").unwrap_or(&json!(id)),"baseUrl":p["baseUrl"],"api":p["api"],
            "apiKeySet":p["apiKey"].as_str().is_some_and(|s| !s.is_empty()),"models":models,"revision":revision(p)}),
        ))
    }
    pub async fn save_custom_provider(&self, input: CustomProviderInput) -> Result<Value, String> {
        self.save_custom_provider_checked(input)
            .await
            .map_err(legacy_error_string)
    }
    pub async fn save_custom_provider_checked(
        &self,
        input: CustomProviderInput,
    ) -> Result<Value, CustomProviderError> {
        if !valid_id(&input.id) || reserved(&input.id) {
            return Err(CustomProviderError::invalid(
                "服务 ID 无效或已由内置服务占用。",
            ));
        }
        if !text(&input.name, 100) || input.base_url.len() > 2048 {
            return Err(CustomProviderError::invalid("服务名称或地址无效。"));
        }
        let url = validate_base(&input.base_url).map_err(CustomProviderError::invalid)?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if input
            .api_key
            .as_ref()
            .is_some_and(|s| !text(s, 8192) || s.len() > 8192)
        {
            return Err(CustomProviderError::invalid("API key 无效。"));
        }
        if input.revision.as_ref().is_some_and(|r| !valid_revision(r)) {
            return Err(CustomProviderError::invalid("配置版本无效。"));
        }
        if input.models.is_empty() || input.models.len() > 32 {
            return Err(CustomProviderError::invalid("请配置 1 至 32 个模型。"));
        }
        let mut seen = BTreeSet::new();
        for model in &input.models {
            if model.id.is_empty()
                || model.id.len() > 128
                || !model.id.as_bytes()[0].is_ascii_alphanumeric()
                || !model
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-/:+".contains(&c))
                || !seen.insert(&model.id)
                || !text(&model.name, 128)
                || !(1024..=100_000_000).contains(&model.context_window)
                || model.max_tokens == 0
                || model.max_tokens > model.context_window
            {
                return Err(CustomProviderError::invalid(
                    "模型 ID、名称或上下文参数无效，模型 ID 不得重复。",
                ));
            }
        }
        let _serial = self.config_writes.lock().await;
        let tx = self
            .files
            .transaction()
            .await
            .map_err(CustomProviderError::config_unavailable)?;
        let mut value = config(Some(
            tx.read("models.json")
                .map_err(CustomProviderError::config_unavailable)?,
        ))?;
        let previous = value["providers"].get(&input.id).cloned();
        if previous.as_ref().is_some_and(|p| !is_custom(p))
            || (previous.is_none() && self.runtime.models().get_provider(&input.id).is_some())
        {
            return Err(CustomProviderError::conflict());
        }
        if (previous.is_some() && input.revision.is_none())
            || input
                .revision
                .as_ref()
                .is_some_and(|r| *r != revision(previous.as_ref().unwrap_or(&Value::Null)))
        {
            return Err(CustomProviderError::conflict());
        }
        let mut updated = previous.clone().unwrap_or_else(|| json!({}));
        let has_key = updated["apiKey"].as_str().is_some_and(|s| !s.is_empty());
        if input.api_key.is_none()
            && !has_key
            && !self.runtime.has_configured_auth(&input.id)
            && !local
        {
            return Err(CustomProviderError::invalid("请输入 API key。"));
        }
        updated["name"] = json!(input.name.trim());
        updated["baseUrl"] = json!(url.as_str().trim_end_matches('/'));
        updated["api"] = serde_json::to_value(input.api)
            .map_err(|_| CustomProviderError::invalid("接口协议无效。"))?;
        if let Some(key) = input.api_key {
            updated["apiKey"] = json!(key.trim());
        } else if !has_key && local && url.scheme() == "http" {
            updated["apiKey"] = json!("local");
        }
        updated["models"] = Value::Array(
            input
                .models
                .iter()
                .map(|m| {
                    let mut earlier = previous
                        .as_ref()
                        .and_then(|p| p["models"].as_array())
                        .and_then(|list| list.iter().find(|v| v["id"] == m.id))
                        .filter(|v| v.is_object())
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    earlier["id"] = json!(m.id);
                    earlier["name"] = json!(m.name.trim());
                    earlier["contextWindow"] = json!(m.context_window);
                    earlier["maxTokens"] = json!(m.max_tokens);
                    earlier["reasoning"] = json!(m.reasoning);
                    earlier["input"] = if m.image {
                        json!(["text", "image"])
                    } else {
                        json!(["text"])
                    };
                    earlier
                })
                .collect(),
        );
        value["providers"][&input.id] = updated.clone();
        tx.write("models.json", &value)
            .map_err(CustomProviderError::config_unavailable)?;
        drop(tx);
        if !self.reload_custom(&input.id, true).await {
            self.rollback_custom(&input.id, Some(&updated), previous.as_ref())
                .await
                .map_err(CustomProviderError::config_unavailable)?;
            return Err(CustomProviderError::invalid(
                "模型服务无法加载，已保留原配置，请检查参数后重试。",
            ));
        }
        self.invalidate_account();
        Ok(json!({"id":input.id,"revision":revision(&updated)}))
    }
    pub async fn delete_custom_provider(&self, id: &str, expected: &str) -> Result<Value, String> {
        self.delete_custom_provider_checked(id, expected)
            .await
            .map_err(legacy_error_string)
    }
    pub async fn delete_custom_provider_checked(
        &self,
        id: &str,
        expected: &str,
    ) -> Result<Value, CustomProviderError> {
        if !valid_id(id) || reserved(id) || !valid_revision(expected) {
            return Err(CustomProviderError::invalid("服务 ID 或配置版本无效。"));
        }
        let _serial = self.config_writes.lock().await;
        let tx = self
            .files
            .transaction()
            .await
            .map_err(CustomProviderError::config_unavailable)?;
        let mut value = config(Some(
            tx.read("models.json")
                .map_err(CustomProviderError::config_unavailable)?,
        ))?;
        let previous = value["providers"]
            .get(id)
            .filter(|p| is_custom(p))
            .cloned()
            .ok_or_else(|| CustomProviderError::invalid("自定义服务不存在。"))?;
        if revision(&previous) != expected {
            return Err(CustomProviderError::conflict());
        }
        value["providers"]
            .as_object_mut()
            .expect("validated")
            .remove(id);
        tx.write("models.json", &value)
            .map_err(CustomProviderError::config_unavailable)?;
        drop(tx);
        if !self.reload_custom(id, false).await {
            self.rollback_custom(id, None, Some(&previous))
                .await
                .map_err(CustomProviderError::config_unavailable)?;
            return Err(CustomProviderError::invalid(
                "模型服务无法刷新，已保留原配置。",
            ));
        }
        self.invalidate_account();
        Ok(json!({"id":id}))
    }
    pub fn read_configured_base_urls(
        &self,
    ) -> Result<BTreeMap<String, String>, CustomProviderError> {
        let value = config(
            self.files
                .read("models.json")
                .map_err(CustomProviderError::config_unavailable)?,
        )?;
        let providers_obj = value["providers"].as_object().expect("validated");
        let mut out = BTreeMap::new();
        for (id, entry) in providers_obj {
            if (reserved(id) || !is_custom(entry))
                && let Some(url) = entry
                    .get("baseUrl")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            {
                out.insert(id.clone(), url.to_owned());
            }
        }
        for parent in ["opencode", "opencode-go"] {
            if !out.contains_key(parent) {
                for &sub_id in legacy_subprovider_ids(parent) {
                    if let Some(url) = providers_obj
                        .get(sub_id)
                        .and_then(|e| e.get("baseUrl"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        out.insert(parent.to_owned(), url.to_owned());
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn provider_endpoint_metadata(
        &self,
        provider_id: &str,
        custom: bool,
        configured_base_url: Option<String>,
        stored_env: Option<&pi_ai::ProviderEnv>,
    ) -> ProviderEndpointMetadata {
        if custom {
            return ProviderEndpointMetadata {
                endpoint_mode: ProviderEndpointMode::Custom,
                default_base_url: None,
                configured_base_url: None,
                endpoint_summary: None,
                cloudflare_account_id: None,
                cloudflare_gateway_id: None,
            };
        }
        let mode = classify_builtin_endpoint_mode(provider_id);
        let default_base_url = (mode == ProviderEndpointMode::Single)
            .then(|| default_builtin_base_url(provider_id).map(str::to_owned))
            .flatten();
        let configured_base_url = configured_base_url.filter(|cfg| {
            mode != ProviderEndpointMode::Single
                || default_base_url.as_deref().is_none_or(|def| {
                    normalize_endpoint_compare(def) != normalize_endpoint_compare(cfg)
                })
        });
        let (cf_acc, cf_gw) = if mode == ProviderEndpointMode::CloudflareTemplate {
            (
                pi_ai::get_provider_env("CLOUDFLARE_ACCOUNT_ID", stored_env)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty()),
                pi_ai::get_provider_env("CLOUDFLARE_GATEWAY_ID", stored_env)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty()),
            )
        } else {
            (None, None)
        };
        let endpoint_summary = match mode {
            ProviderEndpointMode::MultiEndpoint => match provider_id {
                "opencode-go" => Some("opencode.ai/inference/go/*".to_owned()),
                "opencode" => Some("opencode.ai/inference/*".to_owned()),
                "openrouter" => Some("openrouter.ai/api/*".to_owned()),
                "fireworks" => Some("api.fireworks.ai/inference/*".to_owned()),
                _ => builtin_catalog_info()
                    .get(provider_id)
                    .and_then(|i| i.first_base_url.as_deref())
                    .map(strip_url_scheme_and_slash),
            },
            ProviderEndpointMode::CloudflareTemplate => match provider_id {
                "cloudflare-workers-ai" => {
                    let acc = cf_acc.as_deref().unwrap_or("{accountId}");
                    Some(format!("api.cloudflare.com/client/v4/accounts/{acc}/ai/v1"))
                }
                "cloudflare-ai-gateway" => {
                    let acc = cf_acc.as_deref().unwrap_or("{accountId}");
                    let gw = cf_gw.as_deref().unwrap_or("{gatewayId}");
                    Some(format!("gateway.ai.cloudflare.com/v1/{acc}/{gw}/*"))
                }
                _ => None,
            },
            ProviderEndpointMode::ManagedOauth => match provider_id {
                "cstoa" => Some(
                    self.runtime
                        .models()
                        .get_provider("cstoa")
                        .and_then(|p| p.base_url().map(strip_url_scheme_and_slash))
                        .unwrap_or_else(|| "www.cstoa.top".to_owned()),
                ),
                "openai-codex" => Some("chatgpt.com/backend-api".to_owned()),
                "github-copilot" => Some("api.individual.githubcopilot.com".to_owned()),
                "google-gemini-cli" => Some("cloudcode-pa.googleapis.com".to_owned()),
                "google-antigravity" => {
                    Some("daily-cloudcode-pa.sandbox.googleapis.com".to_owned())
                }
                _ => builtin_catalog_info()
                    .get(provider_id)
                    .and_then(|i| i.first_base_url.as_deref())
                    .map(strip_url_scheme_and_slash),
            },
            ProviderEndpointMode::Single | ProviderEndpointMode::Custom => None,
        };
        ProviderEndpointMetadata {
            endpoint_mode: mode,
            default_base_url,
            configured_base_url,
            endpoint_summary,
            cloudflare_account_id: cf_acc,
            cloudflare_gateway_id: cf_gw,
        }
    }

    pub async fn with_provider_endpoint<T, Fut>(
        &self,
        provider_id: &str,
        base_url: &str,
        login: impl FnOnce() -> Fut,
    ) -> Result<T, String>
    where
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        self.with_provider_endpoint_checked(provider_id, base_url, login)
            .await
            .map_err(legacy_error_string)
    }
    pub fn validate_provider_endpoint(
        &self,
        provider_id: &str,
        base_url: &str,
    ) -> Result<String, CustomProviderError> {
        if reserved(provider_id) {
            match classify_builtin_endpoint_mode(provider_id) {
                ProviderEndpointMode::ManagedOauth => {
                    return Err(CustomProviderError::invalid(if provider_id == "cstoa" {
                        "CSTOA 使用固定服务地址，无需自定义 Provider。"
                    } else {
                        "该服务使用托管授权地址，不支持修改服务地址。"
                    }));
                }
                ProviderEndpointMode::MultiEndpoint => {
                    return Err(CustomProviderError::invalid(
                        "该服务按模型分流多协议地址，不支持单地址覆写。",
                    ));
                }
                ProviderEndpointMode::CloudflareTemplate => {
                    return Err(CustomProviderError::invalid(
                        "Cloudflare 服务地址由 Account ID 与 Gateway ID 自动生成。",
                    ));
                }
                ProviderEndpointMode::Single | ProviderEndpointMode::Custom => {}
            }
        }
        let trimmed = base_url.trim();
        if trimmed.is_empty() || base_url.len() > 2048 {
            return Err(CustomProviderError::invalid("请求的 BaseURL 无效"));
        }
        let url = validate_base(trimmed).map_err(CustomProviderError::invalid)?;
        let endpoint = if trimmed.ends_with('/') {
            url.as_str().to_owned()
        } else {
            url.as_str().trim_end_matches('/').to_owned()
        };
        if !self
            .runtime
            .get_all_models()
            .iter()
            .any(|m| m.provider == provider_id)
        {
            return Err(CustomProviderError::invalid(
                "请求的 Provider 没有可用模型，请先在 Pi 中配置模型",
            ));
        }
        Ok(endpoint)
    }

    pub async fn with_provider_endpoint_checked<T, Fut>(
        &self,
        provider_id: &str,
        base_url: &str,
        login: impl FnOnce() -> Fut,
    ) -> Result<T, CustomProviderError>
    where
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        let endpoint = self.validate_provider_endpoint(provider_id, base_url)?;
        let matches_default = reserved(provider_id)
            && default_builtin_base_url(provider_id).is_some_and(|def| {
                normalize_endpoint_compare(def) == normalize_endpoint_compare(&endpoint)
            });
        // Hold one outer ActivityLease across initial write, local refresh, login, and rollback
        // so an update handoff cannot close the gate mid-sequence and strand a half-committed BaseURL.
        let _activity = self
            .files
            .enter_activity()
            .map_err(CustomProviderError::config_unavailable)?;
        let _serial = self.config_writes.lock().await;
        let tx = self
            .files
            .transaction()
            .await
            .map_err(CustomProviderError::config_unavailable)?;
        let mut value = config(Some(
            tx.read("models.json")
                .map_err(CustomProviderError::config_unavailable)?,
        ))?;
        let previous = value["providers"].get(provider_id).cloned();
        if previous.as_ref().is_some_and(|v| !v.is_object()) {
            return Err(CustomProviderError::invalid("请求的 Provider 配置无效"));
        }
        let has_legacy_subkeys = legacy_subprovider_ids(provider_id)
            .iter()
            .any(|sub| value["providers"].get(*sub).is_some());
        if matches_default && !entry_has_base_url_override(previous.as_ref()) && !has_legacy_subkeys
        {
            drop(tx);
            let output = login().await.map_err(CustomProviderError::invalid)?;
            self.invalidate_account();
            return Ok(output);
        }
        let (previous_map, committed_map, touched_subkeys) = if matches_default {
            clear_provider_and_legacy_overrides(&mut value, provider_id)
        } else {
            let mut updated = previous.clone().unwrap_or_else(|| json!({}));
            updated["baseUrl"] = json!(endpoint);
            if let Some(models) = updated.get_mut("models").and_then(Value::as_array_mut) {
                for item in models {
                    if item
                        .as_object()
                        .is_some_and(|obj| obj.contains_key("baseUrl"))
                    {
                        item["baseUrl"] = json!(endpoint);
                    }
                }
            }
            value["providers"][provider_id] = updated.clone();
            let mut prev_m = BTreeMap::new();
            prev_m.insert(provider_id.to_owned(), previous);
            let mut comm_m = BTreeMap::new();
            comm_m.insert(provider_id.to_owned(), Some(updated));
            (prev_m, comm_m, false)
        };
        tx.write("models.json", &value)
            .map_err(CustomProviderError::config_unavailable)?;
        drop(tx);
        let refreshed = self
            .runtime
            .refresh(Some(ModelsRefreshOptions {
                providers: (!touched_subkeys).then(|| vec![provider_id.into()]),
                allow_network: Some(false),
                force: Some(false),
            }))
            .await;
        let provider_models: Vec<_> = self
            .runtime
            .get_all_models()
            .into_iter()
            .filter(|m| m.provider == provider_id)
            .collect();
        let default_restored = if matches_default
            && classify_builtin_endpoint_mode(provider_id) == ProviderEndpointMode::Single
            && let Some(def) = default_builtin_base_url(provider_id)
        {
            provider_models
                .iter()
                .all(|m| normalize_endpoint_compare(&m.base_url) == normalize_endpoint_compare(def))
        } else {
            true
        };
        let endpoint_ok = self.runtime.get_provider_error(provider_id).is_none()
            && !refreshed.errors.contains_key(provider_id)
            && !provider_models.is_empty()
            && if matches_default {
                default_restored
            } else {
                provider_models.iter().all(|m| m.base_url == endpoint)
            };
        if !endpoint_ok {
            self.rollback_entries(provider_id, &committed_map, &previous_map, touched_subkeys)
                .await
                .map_err(CustomProviderError::config_unavailable)?;
            return Err(CustomProviderError::invalid(
                "请求的 Provider 无法使用此 BaseURL，请检查模型配置",
            ));
        }
        match login().await {
            Ok(output) => {
                self.invalidate_account();
                Ok(output)
            }
            Err(err) => {
                self.rollback_entries(provider_id, &committed_map, &previous_map, touched_subkeys)
                    .await
                    .map_err(CustomProviderError::config_unavailable)?;
                Err(CustomProviderError::invalid(err))
            }
        }
    }

    pub async fn update_provider_endpoint_checked(
        &self,
        provider_id: &str,
        base_url: Option<&str>,
    ) -> Result<Option<String>, CustomProviderError> {
        if self.runtime.models().get_provider(provider_id).is_none() {
            return Err(CustomProviderError::invalid("请求的模型服务不存在"));
        }
        let cleaned = base_url.map(str::trim).filter(|s| !s.is_empty());
        let normalized_set = match cleaned {
            None => None,
            Some(raw) => {
                let endpoint = self.validate_provider_endpoint(provider_id, raw)?;
                let matches_default = reserved(provider_id)
                    && default_builtin_base_url(provider_id).is_some_and(|def| {
                        normalize_endpoint_compare(def) == normalize_endpoint_compare(&endpoint)
                    });
                (!matches_default).then_some(endpoint)
            }
        };

        let _activity = self
            .files
            .enter_activity()
            .map_err(CustomProviderError::config_unavailable)?;
        let _serial = self.config_writes.lock().await;
        let tx = self
            .files
            .transaction()
            .await
            .map_err(CustomProviderError::config_unavailable)?;
        let mut value = config(Some(
            tx.read("models.json")
                .map_err(CustomProviderError::config_unavailable)?,
        ))?;
        let previous = value["providers"].get(provider_id).cloned();
        if previous.as_ref().is_some_and(|v| !v.is_object()) {
            return Err(CustomProviderError::invalid("请求的 Provider 配置无效"));
        }
        if !reserved(provider_id) && previous.as_ref().is_some_and(is_custom) {
            return Err(CustomProviderError::invalid(
                "自定义服务请通过自定义配置接口修改地址。",
            ));
        }
        let (previous_map, committed_map, touched_subkeys) = match normalized_set {
            None => clear_provider_and_legacy_overrides(&mut value, provider_id),
            Some(ref endpoint) => {
                let mut updated = previous.clone().unwrap_or_else(|| json!({}));
                updated["baseUrl"] = json!(endpoint);
                if let Some(models) = updated.get_mut("models").and_then(Value::as_array_mut) {
                    for item in models {
                        if item
                            .as_object()
                            .is_some_and(|obj| obj.contains_key("baseUrl"))
                        {
                            item["baseUrl"] = json!(endpoint);
                        }
                    }
                }
                value["providers"][provider_id] = updated.clone();
                let mut prev_m = BTreeMap::new();
                prev_m.insert(provider_id.to_owned(), previous);
                let mut comm_m = BTreeMap::new();
                comm_m.insert(provider_id.to_owned(), Some(updated));
                (prev_m, comm_m, false)
            }
        };
        tx.write("models.json", &value)
            .map_err(CustomProviderError::config_unavailable)?;
        drop(tx);
        let refreshed = self
            .runtime
            .refresh(Some(ModelsRefreshOptions {
                providers: (!touched_subkeys).then(|| vec![provider_id.into()]),
                allow_network: Some(false),
                force: Some(false),
            }))
            .await;
        let provider_models: Vec<_> = self
            .runtime
            .get_all_models()
            .into_iter()
            .filter(|m| m.provider == provider_id)
            .collect();
        let url_verified = match normalized_set {
            Some(ref ep) => provider_models.iter().all(|m| &m.base_url == ep),
            None => {
                if classify_builtin_endpoint_mode(provider_id) == ProviderEndpointMode::Single
                    && let Some(def) = default_builtin_base_url(provider_id)
                {
                    provider_models.iter().all(|m| {
                        normalize_endpoint_compare(&m.base_url) == normalize_endpoint_compare(def)
                    })
                } else {
                    true
                }
            }
        };
        let endpoint_ok = self.runtime.get_provider_error(provider_id).is_none()
            && !refreshed.errors.contains_key(provider_id)
            && (provider_id == "cstoa" || !provider_models.is_empty())
            && url_verified;
        if !endpoint_ok {
            self.rollback_entries(provider_id, &committed_map, &previous_map, touched_subkeys)
                .await
                .map_err(CustomProviderError::config_unavailable)?;
            return Err(CustomProviderError::invalid(
                "请求的 Provider 无法使用此 BaseURL，请检查模型配置",
            ));
        }
        self.invalidate_account();
        Ok(normalized_set)
    }
    async fn reload_custom(&self, id: &str, require_models: bool) -> bool {
        let result = self
            .runtime
            .refresh(Some(ModelsRefreshOptions {
                providers: Some(vec![id.into()]),
                allow_network: Some(false),
                force: Some(false),
            }))
            .await;
        self.runtime.get_provider_error(id).is_none()
            && !result.errors.contains_key(id)
            && (!require_models
                || self
                    .runtime
                    .get_all_models()
                    .iter()
                    .any(|m| m.provider == id))
    }
    async fn rollback_custom(
        &self,
        id: &str,
        committed: Option<&Value>,
        previous: Option<&Value>,
    ) -> Result<(), String> {
        let mut comm_m = BTreeMap::new();
        comm_m.insert(id.to_owned(), committed.cloned());
        let mut prev_m = BTreeMap::new();
        prev_m.insert(id.to_owned(), previous.cloned());
        self.rollback_entries(id, &comm_m, &prev_m, false).await
    }
    async fn rollback_entries(
        &self,
        id: &str,
        committed: &EntrySnapshotMap,
        previous: &EntrySnapshotMap,
        full_refresh: bool,
    ) -> Result<(), String> {
        let tx = self.files.transaction().await?;
        let mut value = config(Some(tx.read("models.json")?)).map_err(legacy_error_string)?;
        let mut wrote = false;
        for (key, comm_val) in committed {
            if value["providers"].get(key) == comm_val.as_ref() {
                if let Some(Some(prev_val)) = previous.get(key) {
                    value["providers"][key] = prev_val.clone();
                } else {
                    value["providers"]
                        .as_object_mut()
                        .expect("validated")
                        .remove(key);
                }
                wrote = true;
            }
        }
        if wrote {
            tx.write("models.json", &value)?;
        }
        drop(tx);
        if full_refresh {
            let _ = self
                .runtime
                .refresh(Some(ModelsRefreshOptions {
                    providers: None,
                    allow_network: Some(false),
                    force: Some(false),
                }))
                .await;
        } else {
            let req_models = previous.get(id).and_then(Option::as_ref).is_some();
            let _ = self.reload_custom(id, req_models).await;
        }
        Ok(())
    }
}

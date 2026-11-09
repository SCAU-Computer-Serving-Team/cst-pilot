use std::collections::{BTreeMap, BTreeSet};

use pi_ai::ModelsRefreshOptions;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{AuthRuntime, oa::validate_base};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CustomApi {
    OpenaiCompletions,
    OpenaiResponses,
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
fn conflict() -> String {
    "config_conflict: 服务配置已修改，请刷新后重试。".into()
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
fn config(value: Option<Value>) -> Result<Value, String> {
    let mut value = value.unwrap_or_else(|| json!({"providers":{}}));
    if value.get("providers").is_none() {
        value["providers"] = json!({});
    }
    if !value["providers"].is_object() {
        return Err("models.json 的 providers 格式无效，原文件已保留。".into());
    }
    Ok(value)
}
fn reserved(id: &str) -> bool {
    id == "cstoa" || pi_ai::builtin_providers().iter().any(|p| p.id() == id)
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
        let value = config(self.files.read("models.json")?)?;
        Ok(value["providers"]
            .as_object()
            .expect("validated")
            .iter()
            .filter(|(id, v)| !reserved(id) && is_custom(v))
            .map(|(id, _)| id.clone())
            .collect())
    }
    pub fn custom_provider(&self, id: &str) -> Result<Option<Value>, String> {
        let value = config(self.files.read("models.json")?)?;
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
        if !valid_id(&input.id) || reserved(&input.id) {
            return Err("服务 ID 无效或已由内置服务占用。".into());
        }
        if !text(&input.name, 100) || input.base_url.len() > 2048 {
            return Err("服务名称或地址无效。".into());
        }
        let url = validate_base(&input.base_url)?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if input
            .api_key
            .as_ref()
            .is_some_and(|s| !text(s, 8192) || s.len() > 8192)
        {
            return Err("API key 无效。".into());
        }
        if input.revision.as_ref().is_some_and(|r| !valid_revision(r)) {
            return Err("配置版本无效。".into());
        }
        if input.models.is_empty() || input.models.len() > 32 {
            return Err("请配置 1 至 32 个模型。".into());
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
                return Err("模型 ID、名称或上下文参数无效，模型 ID 不得重复。".into());
            }
        }
        let _serial = self.config_writes.lock().await;
        let tx = self.files.transaction().await?;
        let mut value = config(Some(tx.read("models.json")?))?;
        let previous = value["providers"].get(&input.id).cloned();
        if previous.as_ref().is_some_and(|p| !is_custom(p))
            || (previous.is_none() && self.runtime.models().get_provider(&input.id).is_some())
        {
            return Err(conflict());
        }
        if (previous.is_some() && input.revision.is_none())
            || input
                .revision
                .as_ref()
                .is_some_and(|r| *r != revision(previous.as_ref().unwrap_or(&Value::Null)))
        {
            return Err(conflict());
        }
        let mut updated = previous.clone().unwrap_or_else(|| json!({}));
        let has_key = updated["apiKey"].as_str().is_some_and(|s| !s.is_empty());
        if input.api_key.is_none()
            && !has_key
            && !self.runtime.has_configured_auth(&input.id)
            && !local
        {
            return Err("请输入 API key。".into());
        }
        updated["name"] = json!(input.name.trim());
        updated["baseUrl"] = json!(url.as_str().trim_end_matches('/'));
        updated["api"] = serde_json::to_value(input.api).map_err(|_| "接口协议无效。")?;
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
        tx.write("models.json", &value)?;
        drop(tx);
        if !self.reload_custom(&input.id, true).await {
            self.rollback_custom(&input.id, Some(&updated), previous.as_ref())
                .await?;
            return Err("模型服务无法加载，已保留原配置，请检查参数后重试。".into());
        }
        self.invalidate_account();
        Ok(json!({"id":input.id,"revision":revision(&updated)}))
    }
    pub async fn delete_custom_provider(&self, id: &str, expected: &str) -> Result<Value, String> {
        if !valid_id(id) || reserved(id) || !valid_revision(expected) {
            return Err("服务 ID 或配置版本无效。".into());
        }
        let _serial = self.config_writes.lock().await;
        let tx = self.files.transaction().await?;
        let mut value = config(Some(tx.read("models.json")?))?;
        let previous = value["providers"]
            .get(id)
            .filter(|p| is_custom(p))
            .cloned()
            .ok_or("自定义服务不存在。")?;
        if revision(&previous) != expected {
            return Err(conflict());
        }
        value["providers"]
            .as_object_mut()
            .expect("validated")
            .remove(id);
        tx.write("models.json", &value)?;
        drop(tx);
        if !self.reload_custom(id, false).await {
            self.rollback_custom(id, None, Some(&previous)).await?;
            return Err("模型服务无法刷新，已保留原配置。".into());
        }
        self.invalidate_account();
        Ok(json!({"id":id}))
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
        self.runtime.get_error().is_none()
            && result.errors.is_empty()
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
        let tx = self.files.transaction().await?;
        let mut value = config(Some(tx.read("models.json")?))?;
        if value["providers"].get(id) == committed {
            if let Some(previous) = previous {
                value["providers"][id] = previous.clone();
            } else {
                value["providers"]
                    .as_object_mut()
                    .expect("validated")
                    .remove(id);
            }
            tx.write("models.json", &value)?;
        }
        drop(tx);
        let _ = self.reload_custom(id, previous.is_some()).await;
        Ok(())
    }
}

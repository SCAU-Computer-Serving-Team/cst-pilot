use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use pi_ai::{
    AnyModel, ApiKeyAuth, AuthResult, CreateProviderOptions, Model, ModelCost, ModelCostTier,
    ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits, ModelInputModality,
    ModelPromptCache, ModelThinkingLevel, Provider, ProviderApiConfig, ProviderAuth,
    ProviderCompat, ProviderHeaders, RefreshModelsInput, create_provider, merge_provider_headers,
    standard_chat_api_config,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ConfigValueResolver;
use crate::config_value::{ConfigValueSource, is_command_config_value, is_config_value_configured};
use crate::settings_manager::strip_json_comments;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialModelCost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<BTreeMap<ModelThinkingLevel, Option<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ModelInputModality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<BTreeMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level:
        Option<BTreeMap<ModelThinkingLevel, BTreeMap<String, Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<ProviderCompat>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<BTreeMap<ModelThinkingLevel, Option<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ModelInputModality>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<PartialModelCost>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<BTreeMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level:
        Option<BTreeMap<ModelThinkingLevel, BTreeMap<String, Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<ProviderCompat>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonProvider {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<ProviderCompat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ModelsJsonModel>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<BTreeMap<String, ModelsJsonModelOverride>>,
}

impl std::fmt::Debug for ModelsJsonModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsJsonModel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ModelsJsonModelOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsJsonModelOverride")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ModelsJsonProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsJsonProvider")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("api", &self.api)
            .field(
                "header_keys",
                &self.headers.as_ref().map(|h| h.keys().collect::<Vec<_>>()),
            )
            .field("auth_header", &self.auth_header)
            .field(
                "model_count",
                &self.models.as_ref().map_or(0, std::vec::Vec::len),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelConfig {
    providers: BTreeMap<String, ModelsJsonProvider>,
    error: Option<String>,
}

impl ModelConfig {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(models_json_path: Option<&Path>) -> Self {
        let Some(path) = models_json_path else {
            return Self::empty();
        };
        if !path.exists() {
            return Self::empty();
        }
        let mut raw = String::new();
        if let Err(err) = File::open(path).and_then(|mut f| f.read_to_string(&mut raw)) {
            return Self {
                providers: BTreeMap::new(),
                error: Some(format!(
                    "Failed to load models.json: {err}\n\nFile: {}",
                    path.display()
                )),
            };
        }
        Self::from_json_str(&raw, Some(path))
    }

    pub fn from_json_str(raw: &str, path_hint: Option<&Path>) -> Self {
        let suffix = path_hint
            .map(|p| format!("\n\nFile: {}", p.display()))
            .unwrap_or_default();
        let stripped = strip_json_comments(raw);
        let parsed: Value = match serde_json::from_str(&stripped) {
            Ok(v) => v,
            Err(_) => {
                return Self {
                    providers: BTreeMap::new(),
                    error: Some(format!(
                        "Failed to parse models.json: invalid JSON syntax{suffix}"
                    )),
                };
            }
        };

        let Some(root_obj) = parsed.as_object() else {
            return Self {
                providers: BTreeMap::new(),
                error: Some(format!(
                    "Invalid models.json schema:\n  - root: expected an object{suffix}"
                )),
            };
        };
        let Some(prov_val) = root_obj.get("providers") else {
            return Self {
                providers: BTreeMap::new(),
                error: Some(format!(
                    "Invalid models.json schema:\n  - providers: required property missing{suffix}"
                )),
            };
        };
        let Some(prov_obj) = prov_val.as_object() else {
            return Self {
                providers: BTreeMap::new(),
                error: Some(format!(
                    "Invalid models.json schema:\n  - providers: expected an object{suffix}"
                )),
            };
        };

        let mut providers = BTreeMap::new();
        let mut schema_errors = Vec::new();

        for (index, (provider_id, val)) in prov_obj.iter().enumerate() {
            match serde_json::from_value::<ModelsJsonProvider>(val.clone()) {
                Ok(prov) => {
                    validate_provider_schema(&format!("[{index}]"), &prov, &mut schema_errors);
                    providers.insert(provider_id.clone(), prov);
                }
                Err(_) => {
                    schema_errors.push(format!(
                        "  - providers[{index}]: invalid field type or value"
                    ));
                }
            }
        }

        if !schema_errors.is_empty() {
            return Self {
                providers: BTreeMap::new(),
                error: Some(format!(
                    "Invalid models.json schema:\n{}{suffix}",
                    schema_errors.join("\n")
                )),
            };
        }

        Self {
            providers,
            error: None,
        }
    }

    pub fn get_provider(&self, provider_id: &str) -> Option<&ModelsJsonProvider> {
        self.providers.get(provider_id)
    }

    pub fn get_provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

fn valid_api(api: &str) -> bool {
    matches!(
        api,
        pi_ai::known_api::OPENAI_COMPLETIONS
            | pi_ai::known_api::OPENAI_RESPONSES
            | pi_ai::known_api::ANTHROPIC_MESSAGES
            | pi_ai::known_api::GOOGLE_GENERATIVE_AI
    )
}

fn valid_base_url(value: &str) -> bool {
    url::Url::parse(value)
        .is_ok_and(|url| matches!(url.scheme(), "https" | "http") && url.host_str().is_some())
}

fn validate_provider_schema(
    provider_id: &str,
    prov: &ModelsJsonProvider,
    errors: &mut Vec<String>,
) {
    if prov.api.as_deref().is_some_and(|api| !valid_api(api)) {
        errors.push(format!("  - providers.{provider_id}.api: unsupported API"));
    }
    if prov
        .base_url
        .as_deref()
        .is_some_and(|url| !valid_base_url(url))
    {
        errors.push(format!(
            "  - providers.{provider_id}.baseUrl: expected an HTTP(S) URL"
        ));
    }
    if let Some(name) = &prov.name
        && name.is_empty()
    {
        errors.push(format!(
            "  - providers.{provider_id}.name: must not be empty"
        ));
    }
    if let Some(base_url) = &prov.base_url
        && base_url.is_empty()
    {
        errors.push(format!(
            "  - providers.{provider_id}.baseUrl: must not be empty"
        ));
    }
    if let Some(api_key) = &prov.api_key
        && api_key.is_empty()
    {
        errors.push(format!(
            "  - providers.{provider_id}.apiKey: must not be empty"
        ));
    }
    if let Some(models) = &prov.models {
        for (idx, m) in models.iter().enumerate() {
            if m.api.as_deref().is_some_and(|api| !valid_api(api)) {
                errors.push(format!(
                    "  - providers.{provider_id}.models.{idx}.api: unsupported API"
                ));
            }
            if m.base_url
                .as_deref()
                .is_some_and(|url| !valid_base_url(url))
            {
                errors.push(format!(
                    "  - providers.{provider_id}.models.{idx}.baseUrl: expected an HTTP(S) URL"
                ));
            }
            if m.id.is_empty() {
                errors.push(format!(
                    "  - providers.{provider_id}.models.{idx}.id: must not be empty"
                ));
            }
            if m.context_window == Some(0) {
                errors.push(format!(
                    "  - providers.{provider_id}.models.{idx}.contextWindow: must be > 0"
                ));
            }
            if m.max_tokens == Some(0) {
                errors.push(format!(
                    "  - providers.{provider_id}.models.{idx}.maxTokens: must be > 0"
                ));
            }
        }
    }
    if let Some(overrides) = &prov.model_overrides {
        for (index, (_, ov)) in overrides.iter().enumerate() {
            let mid = format!("[{index}]");
            if ov.context_window == Some(0) {
                errors.push(format!(
                    "  - providers.{provider_id}.modelOverrides.{mid}.contextWindow: must be > 0"
                ));
            }
            if ov.max_tokens == Some(0) {
                errors.push(format!(
                    "  - providers.{provider_id}.modelOverrides.{mid}.maxTokens: must be > 0"
                ));
            }
        }
    }
}

/// 大小写不敏感合并请求头（后覆盖前，保留覆盖方的大小写形式）。
pub fn merge_string_headers_case_insensitive(
    base: Option<&BTreeMap<String, String>>,
    overrides: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    if base.is_none() && overrides.is_none() {
        return None;
    }
    let mut merged: BTreeMap<String, String> = base.cloned().unwrap_or_default();
    if let Some(ov) = overrides {
        for (name, val) in ov {
            let lower = name.to_ascii_lowercase();
            merged.retain(|k, _| k.to_ascii_lowercase() != lower);
            merged.insert(name.clone(), val.clone());
        }
    }
    (!merged.is_empty()).then_some(merged)
}

fn merge_json_value_objects(base: Option<&Value>, over: Option<&Value>) -> Option<Value> {
    match (base, over) {
        (Some(Value::Object(b)), Some(Value::Object(o))) => {
            let mut m: Map<String, Value> = b.clone();
            for (k, v) in o {
                m.insert(
                    k.clone(),
                    merge_json_value_objects(m.get(k), Some(v)).expect("override value"),
                );
            }
            Some(Value::Object(m))
        }
        (_, Some(o)) => Some(o.clone()),
        (Some(b), None) => Some(b.clone()),
        (None, None) => None,
    }
}

fn merge_btree_maps<K: Ord + Clone, V: Clone>(
    base: Option<&BTreeMap<K, V>>,
    over: Option<&BTreeMap<K, V>>,
) -> Option<BTreeMap<K, V>> {
    match (base, over) {
        (Some(b), Some(o)) => {
            let mut m = b.clone();
            for (k, v) in o {
                m.insert(k.clone(), v.clone());
            }
            Some(m)
        }
        (_, Some(o)) => Some(o.clone()),
        (Some(b), None) => Some(b.clone()),
        (None, None) => None,
    }
}

fn merge_value_maps(
    base: Option<&BTreeMap<String, Value>>,
    over: Option<&BTreeMap<String, Value>>,
) -> Option<BTreeMap<String, Value>> {
    if base.is_none() && over.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    if let Some(over) = over {
        for (key, value) in over {
            merged.insert(
                key.clone(),
                merge_json_value_objects(merged.get(key), Some(value)).expect("override value"),
            );
        }
    }
    Some(merged)
}

fn merge_compat_objects(base: &mut Value, over: &Value) {
    if let (Some(base), Some(over)) = (base.as_object_mut(), over.as_object()) {
        for (key, value) in over {
            if !value.is_null() {
                base.insert(
                    key.clone(),
                    merge_json_value_objects(base.get(key), Some(value)).expect("override value"),
                );
            }
        }
    }
}

pub fn merge_provider_compat(
    base: Option<&ProviderCompat>,
    over: Option<&ProviderCompat>,
) -> Option<ProviderCompat> {
    let Some(ov) = over else {
        return base.cloned();
    };
    let Some(b) = base else {
        return Some(ov.clone());
    };

    let mut base_val = serde_json::to_value(b).unwrap_or(Value::Object(Map::new()));
    let over_val = serde_json::to_value(ov).unwrap_or(Value::Object(Map::new()));
    merge_compat_objects(&mut base_val, &over_val);
    let mut merged: ProviderCompat =
        serde_json::from_value(base_val).unwrap_or_else(|_| ov.clone());
    merged.open_router_routing = merge_json_value_objects(
        b.open_router_routing.as_ref(),
        ov.open_router_routing.as_ref(),
    );
    merged.vercel_gateway_routing = merge_json_value_objects(
        b.vercel_gateway_routing.as_ref(),
        ov.vercel_gateway_routing.as_ref(),
    );
    merged.chat_template_kwargs = merge_value_maps(
        b.chat_template_kwargs.as_ref(),
        ov.chat_template_kwargs.as_ref(),
    );
    merged.chat_template_args = merge_value_maps(
        b.chat_template_args.as_ref(),
        ov.chat_template_args.as_ref(),
    );
    Some(merged)
}

pub fn merge_input_limits(
    base: Option<&ModelInputLimits>,
    over: Option<&ModelInputLimits>,
) -> Option<ModelInputLimits> {
    let Some(ov) = over else {
        return base.cloned();
    };
    let Some(b) = base else {
        return Some(ov.clone());
    };
    let images = match (&b.images, &ov.images) {
        (Some(bi), Some(oi)) => {
            let resize = match (&bi.resize, &oi.resize) {
                (Some(br), Some(or_opt)) => Some(ModelImageResizeOptions {
                    max_width: or_opt.max_width.or(br.max_width),
                    max_height: or_opt.max_height.or(br.max_height),
                    max_bytes: or_opt.max_bytes.or(br.max_bytes),
                    jpeg_quality: or_opt.jpeg_quality.or(br.jpeg_quality),
                }),
                (_, Some(or_opt)) => Some(or_opt.clone()),
                (Some(br), None) => Some(br.clone()),
                (None, None) => None,
            };
            Some(ModelImageInputLimits {
                resize,
                max_per_message: oi.max_per_message.or(bi.max_per_message),
                max_per_request: oi.max_per_request.or(bi.max_per_request),
            })
        }
        (_, Some(oi)) => Some(oi.clone()),
        (Some(bi), None) => Some(bi.clone()),
        (None, None) => None,
    };
    Some(ModelInputLimits {
        max_request_bytes: ov.max_request_bytes.or(b.max_request_bytes),
        images,
    })
}

pub fn merge_sampling_params_by_thinking_level(
    base: Option<&BTreeMap<ModelThinkingLevel, BTreeMap<String, Value>>>,
    over: Option<&BTreeMap<ModelThinkingLevel, BTreeMap<String, Value>>>,
) -> Option<BTreeMap<ModelThinkingLevel, BTreeMap<String, Value>>> {
    let Some(ov) = over else {
        return base.cloned();
    };
    let mut merged = base.cloned().unwrap_or_default();
    for (level, params) in ov {
        let entry = merged.entry(*level).or_default();
        for (k, v) in params {
            entry.insert(
                k.clone(),
                merge_json_value_objects(entry.get(k), Some(v)).expect("override value"),
            );
        }
    }
    Some(merged)
}

pub fn apply_model_override(model: &Model, ov: &ModelsJsonModelOverride) -> Model {
    Model {
        id: model.id.clone(),
        name: ov.name.clone().unwrap_or_else(|| model.name.clone()),
        model_type: model.model_type.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        reasoning: ov.reasoning.unwrap_or(model.reasoning),
        thinking_level_map: merge_btree_maps(
            model.thinking_level_map.as_ref(),
            ov.thinking_level_map.as_ref(),
        ),
        input: ov.input.clone().unwrap_or_else(|| model.input.clone()),
        input_limits: merge_input_limits(model.input_limits.as_ref(), ov.input_limits.as_ref()),
        cost: match &ov.cost {
            Some(c) => ModelCost {
                input: c.input.unwrap_or(model.cost.input),
                output: c.output.unwrap_or(model.cost.output),
                cache_read: c.cache_read.unwrap_or(model.cost.cache_read),
                cache_write: c.cache_write.unwrap_or(model.cost.cache_write),
                tiers: c.tiers.clone().or_else(|| model.cost.tiers.clone()),
            },
            None => model.cost.clone(),
        },
        prompt_cache: match (&model.prompt_cache, &ov.prompt_cache) {
            (Some(b), Some(o)) => Some(ModelPromptCache {
                short: o.short.or(b.short),
                long: o.long.or(b.long),
            }),
            (_, Some(o)) => Some(o.clone()),
            (Some(b), None) => Some(b.clone()),
            (None, None) => None,
        },
        context_window: ov.context_window.unwrap_or(model.context_window),
        max_tokens: ov.max_tokens.unwrap_or(model.max_tokens),
        sampling_params: merge_btree_maps(
            model.sampling_params.as_ref(),
            ov.sampling_params.as_ref(),
        ),
        sampling_params_by_thinking_level: merge_sampling_params_by_thinking_level(
            model.sampling_params_by_thinking_level.as_ref(),
            ov.sampling_params_by_thinking_level.as_ref(),
        ),
        headers: merge_string_headers_case_insensitive(model.headers.as_ref(), ov.headers.as_ref()),
        compat: merge_provider_compat(model.compat.as_ref(), ov.compat.as_ref()),
    }
}

pub fn model_from_json(
    provider_id: &str,
    def: &ModelsJsonModel,
    provider_cfg: &ModelsJsonProvider,
    default_model: Option<&Model>,
    builtin_base_url: Option<&str>,
) -> Result<Model, String> {
    let api = def
        .api
        .as_deref()
        .or(provider_cfg.api.as_deref())
        .or_else(|| default_model.map(|m| m.api.as_str()))
        .ok_or_else(|| {
            format!(
                "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
                def.id
            )
        })?;
    let base_url = def
        .base_url
        .as_deref()
        .or(provider_cfg.base_url.as_deref())
        .or(builtin_base_url)
        .or_else(|| default_model.map(|m| m.base_url.as_str()))
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            format!("Provider {provider_id}: \"baseUrl\" is required when defining custom models.")
        })?;
    if !valid_api(api) || !valid_base_url(base_url) {
        return Err("Custom model requires a supported API and an HTTP(S) base URL".to_owned());
    }
    if def.context_window == Some(0) {
        return Err(format!(
            "Provider {provider_id}, model {}: invalid contextWindow",
            def.id
        ));
    }
    if def.max_tokens == Some(0) {
        return Err(format!(
            "Provider {provider_id}, model {}: invalid maxTokens",
            def.id
        ));
    }
    Ok(Model {
        id: def.id.clone(),
        name: def.name.clone().unwrap_or_else(|| def.id.clone()),
        model_type: None,
        api: api.to_owned(),
        provider: provider_id.to_owned(),
        base_url: base_url.to_owned(),
        reasoning: def.reasoning.unwrap_or(false),
        thinking_level_map: def.thinking_level_map.clone(),
        input: def
            .input
            .clone()
            .unwrap_or_else(|| vec![ModelInputModality::Text]),
        input_limits: def.input_limits.clone(),
        cost: def.cost.clone().unwrap_or_default(),
        prompt_cache: def.prompt_cache.clone(),
        context_window: def.context_window.unwrap_or(128_000),
        max_tokens: def.max_tokens.unwrap_or(16_384),
        sampling_params: def.sampling_params.clone(),
        sampling_params_by_thinking_level: def.sampling_params_by_thinking_level.clone(),
        headers: merge_string_headers_case_insensitive(
            provider_cfg.headers.as_ref(),
            def.headers.as_ref(),
        ),
        compat: merge_provider_compat(provider_cfg.compat.as_ref(), def.compat.as_ref()),
    })
}

fn transform_models_list(
    provider_id: &str,
    items: Vec<AnyModel>,
    provider_cfg: &ModelsJsonProvider,
    builtin_base_url: Option<&str>,
) -> Result<(Vec<AnyModel>, BTreeSet<String>), String> {
    let default_chat = items.iter().find_map(|m| match m {
        AnyModel::Chat(c) => Some(c.clone()),
        AnyModel::Classifier(_) => None,
    });

    let mut out: Vec<AnyModel> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            AnyModel::Chat(mut chat) => {
                if let Some(prov_url) = &provider_cfg.base_url {
                    chat.base_url = prov_url.clone();
                }
                if let Some(prov_api) = &provider_cfg.api {
                    chat.api = prov_api.clone();
                }
                if provider_cfg.headers.is_some() {
                    chat.headers = merge_string_headers_case_insensitive(
                        chat.headers.as_ref(),
                        provider_cfg.headers.as_ref(),
                    );
                }
                if provider_cfg.compat.is_some() {
                    chat.compat =
                        merge_provider_compat(chat.compat.as_ref(), provider_cfg.compat.as_ref());
                }
                if let Some(ov) = provider_cfg
                    .model_overrides
                    .as_ref()
                    .and_then(|m| m.get(&chat.id))
                {
                    chat = apply_model_override(&chat, ov);
                }
                out.push(AnyModel::Chat(chat));
            }
            AnyModel::Classifier(mut clf) => {
                if let Some(prov_url) = &provider_cfg.base_url {
                    clf.base_url = prov_url.clone();
                }
                out.push(AnyModel::Classifier(clf));
            }
        }
    }

    let mut custom_ids = BTreeSet::new();
    if let Some(custom_defs) = &provider_cfg.models {
        for def in custom_defs {
            let mut custom_model = model_from_json(
                provider_id,
                def,
                provider_cfg,
                default_chat.as_ref(),
                builtin_base_url,
            )?;
            if let Some(ov) = provider_cfg
                .model_overrides
                .as_ref()
                .and_then(|m| m.get(&custom_model.id))
            {
                custom_model = apply_model_override(&custom_model, ov);
            }
            custom_ids.insert(custom_model.id.clone());
            if let Some(pos) = out
                .iter()
                .position(|e| matches!(e, AnyModel::Chat(c) if c.id == custom_model.id))
            {
                out[pos] = AnyModel::Chat(custom_model);
            } else {
                out.push(AnyModel::Chat(custom_model));
            }
        }
    }

    Ok((out, custom_ids))
}

/// 将内置（或扩展）`Provider` 与 `models.json` 中的 `ModelsJsonProvider` 合成为完整 `Provider`：
/// 保留原生分类器、模型过滤与动态刷新能力，并接入配置凭据、`auth_header` 与大小写无关请求头。
pub fn compose_provider_with_model_config(
    provider_id: &str,
    base_provider: Option<&Provider>,
    provider_cfg: Option<&ModelsJsonProvider>,
    resolver: &ConfigValueResolver,
) -> Result<Option<Provider>, String> {
    let Some(cfg) = provider_cfg else {
        return Ok(base_provider.cloned());
    };

    let mut opts = match base_provider {
        Some(base) => base.to_create_options(),
        None => CreateProviderOptions {
            id: provider_id.to_owned(),
            name: Some(cfg.name.clone().unwrap_or_else(|| provider_id.to_owned())),
            base_url: cfg.base_url.clone(),
            headers: None,
            auth: ProviderAuth::api_key(ApiKeyAuth::from_env(provider_id, provider_id)),
            models: Vec::new(),
            fetch_models: None,
            filter_models: None,
            api: Some(standard_chat_api_config()),
            classifiers: None,
        },
    };

    if let Some(name) = &cfg.name {
        opts.name = Some(name.clone());
    }
    let builtin_base_url = opts.base_url.clone();
    if let Some(base_url) = &cfg.base_url {
        opts.base_url = Some(base_url.clone());
    }

    let (transformed_models, custom_ids) =
        transform_models_list(provider_id, opts.models, cfg, builtin_base_url.as_deref())?;
    opts.models = transformed_models;

    // 若内置 Provider 具有 filter_models，使 models.json 显式声明的 custom_ids 绕过内置过滤（如 openrouter 自定义模型）
    if let Some(orig_filter) = opts.filter_models.take() {
        let custom_ids_clone = custom_ids.clone();
        opts.filter_models = Some(Arc::new(move |models, cred| {
            let native: Vec<Model> = models
                .iter()
                .filter(|m| !custom_ids_clone.contains(&m.id))
                .cloned()
                .collect();
            let mut filtered = orig_filter(&native, cred);
            filtered.extend(
                models
                    .iter()
                    .filter(|m| custom_ids_clone.contains(&m.id))
                    .cloned(),
            );
            filtered
        }));
    }

    // 若内置 Provider 具有动态刷新 fetch_models，对刷新返回的模型同样应用 models.json 覆写与自定义模型合并
    if let Some(orig_fetch) = opts.fetch_models.take() {
        let pid = provider_id.to_owned();
        let cfg_clone = cfg.clone();
        let b_url = builtin_base_url.clone();
        opts.fetch_models = Some(Arc::new(move |input: RefreshModelsInput| {
            let orig = orig_fetch.clone();
            let pid = pid.clone();
            let cfg_c = cfg_clone.clone();
            let b_u = b_url.clone();
            Box::pin(async move {
                let fetched = orig(input).await?;
                let (merged, _) = transform_models_list(&pid, fetched, &cfg_c, b_u.as_deref())?;
                Ok(merged)
            })
        }));
    }

    if opts.api.is_none() {
        opts.api = Some(standard_chat_api_config());
    } else if let Some(ProviderApiConfig::Single(single)) = opts.api.clone() {
        let native_apis: BTreeSet<String> = base_provider
            .into_iter()
            .flat_map(Provider::get_models)
            .map(|model| model.api)
            .collect();
        let needs_additional_api = opts
            .models
            .iter()
            .any(|model| matches!(model, AnyModel::Chat(chat) if !native_apis.contains(&chat.api)));
        if needs_additional_api {
            let mut by_api = match standard_chat_api_config() {
                ProviderApiConfig::ByApi(map) => map,
                ProviderApiConfig::Single(_) => BTreeMap::new(),
            };
            for api in native_apis {
                by_api.insert(api, single.clone());
            }
            opts.api = Some(ProviderApiConfig::ByApi(by_api));
        }
    }

    // 合成認證与请求头解析
    if cfg.api_key.is_some() || cfg.headers.is_some() || cfg.auth_header == Some(true) {
        let base_auth = opts.auth.clone();
        let cfg_key = cfg.api_key.clone();
        let cfg_headers = cfg.headers.clone();
        let auth_header = cfg.auth_header.unwrap_or(false);
        let resolver_clone = resolver.clone();
        let pid = provider_id.to_owned();

        let resolve_composed = Arc::new(
            move |cred: Option<pi_ai::ApiKeyCredential>,
                  env: Option<pi_ai::ProviderEnv>,
                  cancel: Option<tokio_util::sync::CancellationToken>| {
                // 1. 优先尝试存储凭据或运行时覆盖凭据
                let base_res = if let Some(ref api_auth) = base_auth.api_key {
                    (api_auth.resolve)(cred.clone(), env.clone())?
                } else if base_auth.oauth.is_some() {
                    if let Some(c) = &cred
                        && let Some(k) = &c.key
                        && !k.is_empty()
                    {
                        Some(AuthResult {
                            auth: pi_ai::ModelAuth {
                                api_key: Some(k.clone()),
                                headers: None,
                                base_url: None,
                            },
                            env: c.env.clone(),
                            source: Some("stored".to_owned()),
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };

                let env = base_res
                    .as_ref()
                    .and_then(|result| result.env.clone())
                    .or(env);
                let mut final_api_key = base_res.as_ref().and_then(|r| r.auth.api_key.clone());
                let mut source = base_res.as_ref().and_then(|r| r.source.clone());

                if final_api_key.is_none()
                    && let Some(raw_cfg_key) = &cfg_key
                {
                    let resolved_key = resolver_clone.resolve_or_throw(
                        raw_cfg_key,
                        &format!("API key for provider \"{pid}\""),
                        env.as_ref(),
                        ConfigValueSource::Global,
                        cancel.as_ref(),
                    )?;
                    final_api_key = Some(resolved_key);
                    source = Some(if is_command_config_value(raw_cfg_key) {
                        "models_json_command".to_owned()
                    } else {
                        "models_json_key".to_owned()
                    });
                }

                let mut resolved_headers: Option<ProviderHeaders> =
                    base_res.as_ref().and_then(|r| r.auth.headers.clone());

                if let Some(h_map) = &cfg_headers {
                    let resolved_map = resolver_clone.resolve_headers_or_throw(
                        Some(h_map),
                        env.as_ref(),
                        ConfigValueSource::Global,
                        cancel.as_ref(),
                    )?;
                    if let Some(rm) = resolved_map {
                        let ph: ProviderHeaders =
                            rm.into_iter().map(|(k, v)| (k, Some(v))).collect();
                        resolved_headers = merge_provider_headers(resolved_headers, Some(&ph));
                    }
                }

                if auth_header && let Some(key) = &final_api_key {
                    let mut bearer = ProviderHeaders::new();
                    bearer.insert("Authorization".to_owned(), Some(format!("Bearer {key}")));
                    resolved_headers = merge_provider_headers(resolved_headers, Some(&bearer));
                }

                if final_api_key.is_none() && resolved_headers.is_none() {
                    return Ok(None);
                }

                Ok(Some(AuthResult {
                    auth: pi_ai::ModelAuth {
                        api_key: final_api_key,
                        headers: resolved_headers,
                        base_url: base_res.and_then(|r| r.auth.base_url),
                    },
                    env,
                    source,
                }))
            },
        );

        let sync_resolver = resolve_composed.clone();
        let resolve_fn: pi_ai::ApiKeyResolverFn =
            Arc::new(move |credential, env| sync_resolver(credential, env, None));
        let resolve_async: pi_ai::ApiKeyAsyncResolverFn = Arc::new(
            move |credential, env, cancel| {
                let resolve = resolve_composed.clone();
                Box::pin(async move {
                    let cancel = cancel.map(|token| token.child_token()).unwrap_or_default();
                    let _cancel_on_drop = cancel.clone().drop_guard();
                    let worker_cancel = cancel.clone();
                    let worker = tokio::task::spawn_blocking(move || {
                        resolve(credential, env, Some(worker_cancel))
                    });
                    tokio::select! { biased;
                        _ = cancel.cancelled() => Err("Request was aborted".to_owned()),
                        result = worker => result.map_err(|_| "Provider configuration resolution task failed".to_owned())?,
                    }
                })
            },
        );
        let keyless = opts
            .auth
            .api_key
            .as_ref()
            .is_some_and(|auth| auth.configured_without_credentials);
        opts.auth = ProviderAuth {
            api_key: Some(ApiKeyAuth {
                name: opts.name.clone().unwrap_or_else(|| provider_id.to_owned()),
                resolve: resolve_fn,
                resolve_async: Some(resolve_async),
                configured_without_credentials: keyless,
                login_fn: opts.auth.api_key.and_then(|a| a.login_fn),
            }),
            oauth: opts.auth.oauth,
        };
    }

    create_provider(opts).map(Some)
}

/// 只读检查 `models.json` 中的某服务商是否配置了可用凭据（零 Shell 命令执行）。
pub fn has_configured_models_json_auth(prov: &ModelsJsonProvider) -> Option<&'static str> {
    let key = prov.api_key.as_deref()?;
    if is_command_config_value(key) {
        Some("models_json_command")
    } else if is_config_value_configured(key, None) {
        Some("models_json_key")
    } else {
        None
    }
}

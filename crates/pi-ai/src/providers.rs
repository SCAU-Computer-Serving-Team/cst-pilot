pub mod ant_ling;
pub mod anthropic;
pub mod deepseek;
pub mod faux;
pub mod github_copilot;
pub mod google;
pub mod huggingface;
pub mod minimax;
pub mod moonshot;
pub mod openai;
pub mod opencode;
pub mod openrouter;
pub mod qwen;
pub mod typesafe;
pub mod vercel_ai_gateway;
pub mod xai;
pub mod xiaomi;
pub mod zai;

use std::collections::{BTreeMap, BTreeSet};

use crate::auth::{ApiKeyAuth, OAuthAuth, ProviderAuth};
use crate::models::{
    CreateModelsOptions, CreateProviderOptions, FilterModelsFn, Models, Provider,
    ProviderApiConfig, create_models, create_provider, default_api_map, default_classifier_map,
};
use crate::types::AnyModel;

pub(crate) fn load_catalog_json(raw_json: &str) -> Vec<AnyModel> {
    let grouped: BTreeMap<String, BTreeMap<String, AnyModel>> =
        serde_json::from_str(raw_json).expect("embedded provider catalog JSON must be valid");
    let mut models = Vec::new();
    for group in grouped.into_values() {
        for model in group.into_values() {
            models.push(model);
        }
    }
    models
}

pub(crate) fn build_standard_provider(
    id: &str,
    name: &str,
    raw_json: &str,
    supports_oauth: bool,
    is_subscription: bool,
) -> Provider {
    build_standard_provider_with_filter(id, name, raw_json, supports_oauth, is_subscription, None)
}

pub(crate) fn build_standard_provider_with_filter(
    id: &str,
    name: &str,
    raw_json: &str,
    supports_oauth: bool,
    is_subscription: bool,
    filter_models: Option<FilterModelsFn>,
) -> Provider {
    let models = load_catalog_json(raw_json);
    let unique_urls: BTreeSet<&str> = models
        .iter()
        .map(|m| match m {
            AnyModel::Chat(c) => c.base_url.as_str(),
            AnyModel::Classifier(c) => c.base_url.as_str(),
        })
        .collect();
    // 仅当该服务商下所有模型共享同一默认 base_url 时才设置 provider.base_url；
    // 多协议聚合服务商（如 opencode、openrouter、vercel-ai-gateway）保留 None 以使用各模型自身的协议端点。
    let base_url = if unique_urls.len() == 1 {
        unique_urls.into_iter().next().map(|s| s.to_owned())
    } else {
        None
    };

    let api_key_auth = ApiKeyAuth::from_env(format!("{name} API key"), id);
    let auth = if supports_oauth {
        ProviderAuth::both(
            api_key_auth,
            OAuthAuth::for_provider(id, name, is_subscription),
        )
    } else {
        ProviderAuth::api_key(api_key_auth)
    };

    create_provider(CreateProviderOptions {
        id: id.to_owned(),
        name: Some(name.to_owned()),
        base_url,
        headers: None,
        auth,
        models,
        fetch_models: None,
        filter_models,
        api: Some(ProviderApiConfig::ByApi(default_api_map())),
        classifiers: Some(default_classifier_map()),
    })
    .expect("built-in provider configuration is valid")
}

/// 返回全部 26 个内置服务商实例。
pub fn builtin_providers() -> Vec<Provider> {
    vec![
        deepseek::deepseek_provider(),
        moonshot::moonshotai_cn_provider(),
        moonshot::moonshotai_provider(),
        moonshot::kimi_coding_provider(),
        zai::zai_coding_cn_provider(),
        zai::zai_provider(),
        minimax::minimax_cn_provider(),
        minimax::minimax_provider(),
        qwen::qwen_token_plan_cn_provider(),
        qwen::qwen_token_plan_provider(),
        qwen::qwen_token_plan_individual_provider(),
        xiaomi::xiaomi_token_plan_cn_provider(),
        xiaomi::xiaomi_provider(),
        ant_ling::ant_ling_provider(),
        openai::openai_provider(),
        openai::openai_codex_provider(),
        anthropic::anthropic_provider(),
        google::google_provider(),
        xai::xai_provider(),
        opencode::opencode_provider(),
        opencode::opencode_go_provider(),
        typesafe::typesafe_provider(),
        openrouter::openrouter_provider(),
        vercel_ai_gateway::vercel_ai_gateway_provider(),
        huggingface::huggingface_provider(),
        github_copilot::github_copilot_provider(),
    ]
}

/// 创建已预装全部 26 个内置服务商的 Models 容器。
pub fn builtin_models(options: Option<CreateModelsOptions>) -> Models {
    let models = create_models(options);
    for provider in builtin_providers() {
        models.set_provider(provider);
    }
    models
}

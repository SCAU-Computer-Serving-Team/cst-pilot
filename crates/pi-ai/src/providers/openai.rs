use crate::auth::{OAuthAuth, ProviderAuth};
use crate::models::{
    CreateProviderOptions, Provider, ProviderApiConfig, create_provider, default_api_map,
};
use crate::providers::{build_standard_provider, load_catalog_json};

const DATA_OPENAI: &str = include_str!("data/openai.json");
const DATA_CODEX: &str = include_str!("data/openai-codex.json");

pub fn openai_provider() -> Provider {
    build_standard_provider("openai", "OpenAI", DATA_OPENAI, true, true)
}

pub fn openai_codex_provider() -> Provider {
    create_provider(CreateProviderOptions {
        id: "openai-codex".to_owned(),
        name: Some("OpenAI Codex".to_owned()),
        base_url: Some("https://chatgpt.com/backend-api".to_owned()),
        headers: None,
        auth: ProviderAuth::oauth(OAuthAuth::for_provider(
            "openai-codex",
            "OpenAI (ChatGPT Plus/Pro)",
            true,
        )),
        models: load_catalog_json(DATA_CODEX),
        fetch_models: None,
        filter_models: None,
        api: Some(ProviderApiConfig::ByApi(default_api_map())),
        classifiers: None,
    })
    .expect("built-in OpenAI Codex provider configuration is valid")
}

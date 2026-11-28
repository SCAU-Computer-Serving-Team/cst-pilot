use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::get_provider_env;
use crate::auth::{ApiKeyAuth, ApiKeyCredential, AuthPrompt, AuthResult, ModelAuth, ProviderAuth};
use crate::models::{Provider, ProviderApiConfig, ProviderStreams, create_provider, stream_error};
use crate::providers::build_standard_provider;
use crate::types::{Model, ProviderEnv};

const DATA: &str = include_str!("data/cloudflare-ai-gateway.json");
pub(crate) const ACCOUNT: &str = "CLOUDFLARE_ACCOUNT_ID";
pub(crate) const GATEWAY: &str = "CLOUDFLARE_GATEWAY_ID";

pub(crate) fn is_valid_cloudflare_segment(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn resolve_model(model: &Model, env: Option<&ProviderEnv>) -> Result<Model, &'static str> {
    let mut model = model.clone();
    for (key, placeholder) in [
        (ACCOUNT, "{CLOUDFLARE_ACCOUNT_ID}"),
        (GATEWAY, "{CLOUDFLARE_GATEWAY_ID}"),
    ] {
        if model.base_url.contains(placeholder) {
            let id = get_provider_env(key, env)
                .filter(|id| is_valid_cloudflare_segment(id))
                .ok_or("Cloudflare AI Gateway requires valid account and gateway IDs")?;
            model.base_url = model.base_url.replace(placeholder, &id);
        }
    }
    Ok(model)
}

pub fn cloudflare_ai_gateway_provider() -> Provider {
    let id = "cloudflare-ai-gateway";
    let mut options = build_standard_provider(id, "Cloudflare AI Gateway", DATA, false, false)
        .to_create_options();
    let mut auth =
        ApiKeyAuth::from_env("Cloudflare API key", id).with_login(Arc::new(|interaction| {
            Box::pin(async move {
                let key = interaction
                    .prompt_user(AuthPrompt::Secret {
                        message: "Enter Cloudflare API key".to_owned(),
                        placeholder: None,
                    })
                    .await?;
                let key = key.trim().to_owned();
                if key.is_empty() {
                    return Err("Cloudflare API key cannot be empty".to_owned());
                }
                let account_id = interaction
                    .prompt_user(AuthPrompt::Text {
                        message: "Enter Cloudflare account ID".to_owned(),
                        placeholder: None,
                    })
                    .await?;
                let account_id = account_id.trim().to_owned();
                if !is_valid_cloudflare_segment(&account_id) {
                    return Err("Invalid Cloudflare account ID".to_owned());
                }
                let gateway_id = interaction
                    .prompt_user(AuthPrompt::Text {
                        message: "Enter Cloudflare AI Gateway ID".to_owned(),
                        placeholder: None,
                    })
                    .await?;
                let gateway_id = gateway_id.trim().to_owned();
                if !is_valid_cloudflare_segment(&gateway_id) {
                    return Err("Invalid Cloudflare AI Gateway ID".to_owned());
                }
                Ok(ApiKeyCredential {
                    key: Some(key),
                    env: Some(BTreeMap::from([
                        (ACCOUNT.to_owned(), account_id),
                        (GATEWAY.to_owned(), gateway_id),
                    ])),
                })
            })
        }));
    let resolve_key = auth.resolve.clone();
    auth.resolve = Arc::new(move |credential, env| {
        let mut merged = env.unwrap_or_default();
        if let Some(stored_env) = credential.as_ref().and_then(|c| c.env.as_ref()) {
            merged.extend(stored_env.clone());
        }
        let Some(account_id) =
            get_provider_env(ACCOUNT, Some(&merged)).filter(|v| is_valid_cloudflare_segment(v))
        else {
            return Ok(None);
        };
        let Some(gateway_id) =
            get_provider_env(GATEWAY, Some(&merged)).filter(|v| is_valid_cloudflare_segment(v))
        else {
            return Ok(None);
        };
        let Some(result) = resolve_key(credential, Some(merged.clone()))? else {
            return Ok(None);
        };
        let Some(key) = result.auth.api_key else {
            return Ok(None);
        };
        merged.insert(ACCOUNT.to_owned(), account_id);
        merged.insert(GATEWAY.to_owned(), gateway_id);
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: None,
                base_url: None,
                headers: Some(BTreeMap::from([
                    ("cf-aig-authorization".into(), Some(format!("Bearer {key}"))),
                    ("Authorization".into(), None),
                    ("x-api-key".into(), None),
                ])),
            },
            env: Some(merged),
            source: result.source,
        }))
    });
    options.auth = ProviderAuth::api_key(auth);

    if let Some(ProviderApiConfig::ByApi(map)) = &mut options.api {
        for streams in map.values_mut() {
            let ProviderStreams {
                stream,
                stream_simple,
            } = streams.clone();
            *streams = ProviderStreams::new(
                Arc::new(move |model, context, options| {
                    match resolve_model(model, options.and_then(|opts| opts.env.as_ref())) {
                        Ok(model) => stream(&model, context, options),
                        Err(err) => stream_error(model, err),
                    }
                }),
                Arc::new(move |model, context, options| {
                    match resolve_model(model, options.and_then(|opts| opts.base.env.as_ref())) {
                        Ok(model) => stream_simple(&model, context, options),
                        Err(err) => stream_error(model, err),
                    }
                }),
            );
        }
    }

    create_provider(options).expect("built-in Cloudflare Gateway provider is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_resolves_valid_path_segments_only() {
        let model = cloudflare_ai_gateway_provider().get_models().remove(0);
        let mut env = ProviderEnv::from([
            (ACCOUNT.into(), "account-id".into()),
            (GATEWAY.into(), "gateway_id".into()),
        ]);
        let actual = resolve_model(&model, Some(&env)).unwrap();
        assert!(actual.base_url.contains("/account-id/gateway_id/"));
        env.insert(GATEWAY.into(), "../other".into());
        assert!(resolve_model(&model, Some(&env)).is_err());
    }
}

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::get_provider_env;
use crate::auth::{ApiKeyAuth, ApiKeyCredential, AuthPrompt, AuthResult, ModelAuth, ProviderAuth};
use crate::models::{Provider, ProviderApiConfig, ProviderStreams, create_provider, stream_error};
use crate::providers::build_standard_provider;
use crate::providers::cloudflare_ai_gateway::{ACCOUNT, is_valid_cloudflare_segment};
use crate::types::{Model, ProviderEnv};

const DATA: &str = include_str!("data/cloudflare-workers-ai.json");

fn resolve_model(model: &Model, env: Option<&ProviderEnv>) -> Result<Model, &'static str> {
    let mut model = model.clone();
    if model.base_url.contains("{CLOUDFLARE_ACCOUNT_ID}") {
        let id = get_provider_env(ACCOUNT, env)
            .filter(|id| is_valid_cloudflare_segment(id))
            .ok_or("Cloudflare Workers AI requires a valid account ID")?;
        model.base_url = model.base_url.replace("{CLOUDFLARE_ACCOUNT_ID}", &id);
    }
    Ok(model)
}

pub fn cloudflare_workers_ai_provider() -> Provider {
    let id = "cloudflare-workers-ai";
    let mut options = build_standard_provider(id, "Cloudflare Workers AI", DATA, false, false)
        .to_create_options();
    options.base_url = None;

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
                Ok(ApiKeyCredential {
                    key: Some(key),
                    env: Some(BTreeMap::from([(ACCOUNT.to_owned(), account_id)])),
                })
            })
        }));
    let resolve_key = auth.resolve.clone();
    auth.resolve = Arc::new(move |credential, env| {
        let mut merged = credential
            .as_ref()
            .and_then(|c| c.env.clone())
            .unwrap_or_default();
        if let Some(req_env) = env {
            merged.extend(req_env);
        }
        let Some(account_id) =
            get_provider_env(ACCOUNT, Some(&merged)).filter(|v| is_valid_cloudflare_segment(v))
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
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                base_url: None,
                headers: None,
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

    create_provider(options).expect("built-in Cloudflare Workers AI provider is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_ai_resolves_account_id_and_rejects_missing_or_traversal_segments() {
        let p = cloudflare_workers_ai_provider();
        assert!(p.base_url().is_none());
        let model = p.get_models().remove(0);
        let mut env = ProviderEnv::from([(ACCOUNT.into(), "acc_123".into())]);
        let resolved = resolve_model(&model, Some(&env)).unwrap();
        assert_eq!(
            resolved.base_url,
            "https://api.cloudflare.com/client/v4/accounts/acc_123/ai/v1"
        );
        env.insert(ACCOUNT.into(), "../bad".into());
        assert!(resolve_model(&model, Some(&env)).is_err());

        let api_auth = p.auth().api_key.as_ref().unwrap();
        let only_key = Some(ApiKeyCredential {
            key: Some("cf-tok".into()),
            env: None,
        });
        assert!(
            (api_auth.resolve)(only_key.clone(), None)
                .unwrap()
                .is_none()
        );
        let valid_env = ProviderEnv::from([(ACCOUNT.into(), "acc_123".into())]);
        let ok = (api_auth.resolve)(only_key, Some(valid_env))
            .unwrap()
            .expect("account id + key resolves");
        assert_eq!(ok.auth.api_key.as_deref(), Some("cf-tok"));
    }
}

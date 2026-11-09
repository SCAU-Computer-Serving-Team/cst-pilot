use std::sync::Arc;

use pi_ai::{
    ApiKeyAuth, ApiKeyCredential, AuthPrompt, AuthResult, CreateProviderOptions, Credential,
    ModelAuth, OAuthAuth, Provider, ProviderApiConfig, ProviderAuth, create_provider,
    openai_completions_api,
};
use tokio_util::sync::CancellationToken;

use super::{Channel, models::Catalog, storage::JsonFiles};

pub(crate) fn create(
    files: JsonFiles,
    catalog: Arc<Catalog>,
    channel: Channel,
) -> Result<Provider, String> {
    let login_catalog = catalog.clone();
    let oauth_catalog = catalog.clone();
    let oauth_client = catalog.client.clone();
    let refresh_client = catalog.client.clone();
    let to_auth_client = catalog.client.clone();
    let base = catalog.client.host().to_owned();
    let filter_catalog = catalog.clone();
    let fetch_catalog = catalog;
    let api_key = ApiKeyAuth {
        name: "CSTOA APIKEY".into(),
        resolve_async: None,
        configured_without_credentials: false,
        resolve: Arc::new(move |credential, _| {
            Ok(credential
                .and_then(|c| c.key)
                .filter(|k| !k.is_empty())
                .map(|key| AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key),
                        base_url: Some(format!("{base}/v1")),
                        headers: None,
                    },
                    env: None,
                    source: Some("APIKEY".into()),
                }))
        }),
        login_fn: Some(Arc::new(move |interaction| {
            let catalog = login_catalog.clone();
            Box::pin(async move {
                let raw = interaction
                    .prompt_user(AuthPrompt::Secret {
                        message: "输入 CSTOA APIKEY".into(),
                        placeholder: None,
                    })
                    .await?;
                let key = raw.trim().to_owned();
                if key.is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
                    return Err("CSTOA APIKEY 格式无效。".into());
                }
                let cancel = interaction.cancel_token.clone().unwrap_or_default();
                catalog.client.validate_key(&key, cancel.clone()).await?;
                let credential = ApiKeyCredential {
                    key: Some(key),
                    env: None,
                };
                prepare(
                    &catalog,
                    &Credential::ApiKey(credential.clone()),
                    &interaction,
                    cancel,
                )
                .await?;
                Ok(credential)
            })
        })),
    };
    let oauth = OAuthAuth {
        name: "CSTOA API".into(),
        is_subscription: false,
        login_fn: Some(Arc::new(move |interaction, _| {
            let client = oauth_client.clone();
            let files = files.clone();
            let catalog = oauth_catalog.clone();
            Box::pin(async move {
                let credential = client.login(&files, channel, &interaction).await?;
                let cancel = interaction.cancel_token.clone().unwrap_or_default();
                prepare(
                    &catalog,
                    &Credential::OAuth(credential.clone()),
                    &interaction,
                    cancel,
                )
                .await?;
                Ok(credential)
            })
        })),
        refresh: Arc::new(move |credential| {
            let client = refresh_client.clone();
            Box::pin(async move { client.refresh(credential, CancellationToken::new()).await })
        }),
        to_auth: Arc::new(move |credential| {
            let base_url = to_auth_client.model_base(&Credential::OAuth(credential.clone()));
            Ok(ModelAuth {
                api_key: Some(credential.access),
                base_url: Some(base_url),
                headers: None,
            })
        }),
    };
    create_provider(CreateProviderOptions {
        id: "cstoa".into(),
        name: Some("CSTOA API".into()),
        base_url: None,
        headers: None,
        auth: ProviderAuth::both(api_key, oauth),
        models: vec![],
        filter_models: Some(Arc::new(move |models, credential| {
            filter_catalog.filter(models, credential)
        })),
        fetch_models: Some(Arc::new(move |input| {
            let catalog = fetch_catalog.clone();
            Box::pin(async move { catalog.fetch(input).await })
        })),
        api: Some(ProviderApiConfig::Single(openai_completions_api())),
        classifiers: None,
    })
}
async fn prepare(
    catalog: &Catalog,
    credential: &Credential,
    interaction: &pi_ai::AuthInteraction,
    cancel: CancellationToken,
) -> Result<(), String> {
    if catalog.prepare(credential, cancel.clone()).await.is_err() {
        if cancel.is_cancelled() {
            return Err("Login cancelled".into());
        }
        interaction.notify_user(pi_ai::AuthEvent::Info {
            message: "登录凭据已取得，模型清单暂时无法更新，稍后将自动重试。".into(),
            links: None,
        });
    }
    Ok(())
}

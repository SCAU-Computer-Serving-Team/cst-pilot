use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::auth::{ApiKeyAuth, Credential, OAuthAuth, ProviderAuth};
use crate::models::{
    Provider, ProviderClassifier, create_provider, typesafe_system_one_classifier,
};
use crate::providers::build_standard_provider_with_filter;
use crate::types::known_classifier_api;

const DATA: &str = include_str!("data/github-copilot.json");

fn copilot_system_one(classifier: ProviderClassifier) -> ProviderClassifier {
    ProviderClassifier::new(Arc::new(move |model, context, options| {
        let mut adjusted = model.clone();
        adjusted.base_url = format!("{}/v1", model.base_url.trim_end_matches('/'));
        let ctx = context.clone();
        let opts = options.cloned();
        let inner = classifier.clone();
        Box::pin(async move { (inner.classify)(&adjusted, &ctx, opts.as_ref()).await })
    }))
}

pub fn github_copilot_provider() -> Provider {
    let filter = Arc::new(
        |models: &[crate::types::Model], cred: Option<&Credential>| {
            let Some(Credential::OAuth(oauth)) = cred else {
                return models.to_vec();
            };
            let Some(ids_val) = oauth
                .extra
                .get("availableModelIds")
                .and_then(|v| v.as_array())
            else {
                return models.to_vec();
            };
            if !ids_val.iter().all(|v| v.is_string()) {
                return models.to_vec();
            }
            let allowed: BTreeSet<&str> = ids_val.iter().filter_map(|v| v.as_str()).collect();
            models
                .iter()
                .filter(|m| allowed.contains(m.id.as_str()))
                .cloned()
                .collect()
        },
    );
    let mut options = build_standard_provider_with_filter(
        "github-copilot",
        "GitHub Copilot",
        DATA,
        true,
        true,
        Some(filter),
    )
    .to_create_options();
    options.base_url = Some("https://api.individual.githubcopilot.com".to_owned());
    options.auth = ProviderAuth::both(
        ApiKeyAuth::env_only("GitHub Copilot token", "github-copilot"),
        OAuthAuth::for_provider("github-copilot", "GitHub Copilot", true),
    );
    options.classifiers = Some(BTreeMap::from([(
        known_classifier_api::TYPESAFE_SYSTEM_ONE.to_owned(),
        copilot_system_one(typesafe_system_one_classifier()),
    )]));
    create_provider(options).expect("built-in GitHub Copilot provider is valid")
}

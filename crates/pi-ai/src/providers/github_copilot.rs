use std::collections::BTreeSet;
use std::sync::Arc;

use crate::auth::Credential;
use crate::models::Provider;
use crate::providers::build_standard_provider_with_filter;

const DATA: &str = include_str!("data/github-copilot.json");

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
            let allowed: BTreeSet<&str> = ids_val.iter().filter_map(|v| v.as_str()).collect();
            models
                .iter()
                .filter(|m| allowed.contains(m.id.as_str()))
                .cloned()
                .collect()
        },
    );
    build_standard_provider_with_filter(
        "github-copilot",
        "GitHub Copilot",
        DATA,
        true,
        true,
        Some(filter),
    )
}

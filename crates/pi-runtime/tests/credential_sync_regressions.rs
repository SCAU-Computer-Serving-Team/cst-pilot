use pi_ai::StopReason;

#[tokio::test]
async fn cancellation_during_runtime_prepare_must_be_aborted() {
    let runtime = pi_runtime::ModelRuntime::create(pi_runtime::CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let stream = runtime
        .stream_simple(
            pi_ai::faux::create_faux_provider().model,
            pi_ai::normalize_context(&pi_ai::Context::default()),
            pi_ai::SimpleStreamOptions {
                base: pi_ai::StreamOptions {
                    cancel_token: Some(token.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    let result = stream.result().await;
    assert_eq!(
        result.stop_reason,
        StopReason::Aborted,
        "cancelled request classified as a provider error"
    );
    let stream = runtime
        .stream(
            pi_ai::faux::create_faux_provider().model,
            pi_ai::normalize_context(&pi_ai::Context::default()),
            pi_ai::StreamOptions {
                cancel_token: Some(token),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(stream.result().await.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn credential_change_must_restore_dynamic_catalog_cache() {
    use pi_ai::{
        AnyModel, ApiKeyAuth, CreateProviderOptions, ModelsStore, ModelsStoreEntry, ProviderAuth,
        create_provider,
    };
    use std::sync::Arc;
    let cache = Arc::new(pi_ai::InMemoryModelsStore::new());
    let mut model = pi_ai::faux::create_faux_provider().model;
    model.provider = "cache-review".into();
    model.id = "cached-model".into();
    cache
        .write(
            "cache-review",
            ModelsStoreEntry {
                models: vec![AnyModel::Chat(model)],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let runtime = pi_runtime::ModelRuntime::create(pi_runtime::CreateModelRuntimeOptions {
        models_store: Some(cache),
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    runtime.register_provider(
        create_provider(CreateProviderOptions {
            id: "cache-review".into(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::api_key(ApiKeyAuth::from_env("cache-review", "REVIEW_UNUSED_KEY")),
            models: vec![],
            fetch_models: Some(Arc::new(|_| {
                Box::pin(async { panic!("network must not be used") })
            })),
            filter_models: None,
            api: Some(pi_ai::standard_chat_api_config()),
            classifiers: None,
        })
        .unwrap(),
    );
    runtime
        .set_runtime_api_key("cache-review", "local-probe-key")
        .await
        .unwrap();
    assert!(
        runtime.get_model("cache-review", "cached-model").is_some(),
        "credential synchronization did not restore catalog cache"
    );
    assert_eq!(runtime.get_available_models().len(), 1);
    runtime
        .remove_runtime_api_key("cache-review")
        .await
        .unwrap();
    assert!(runtime.get_available_models().is_empty());
    assert!(runtime.get_model("cache-review", "cached-model").is_some());
}

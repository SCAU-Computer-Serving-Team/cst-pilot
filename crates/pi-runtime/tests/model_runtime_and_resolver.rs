use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use pi_ai::*;
use pi_runtime::*;

fn model(pid: &str, id: &str) -> Model {
    let mut m = pi_session::unconfigured_model();
    m.provider = pid.into();
    m.id = id.into();
    m.name = id.into();
    m.api = known_api::OPENAI_COMPLETIONS.into();
    m.base_url = "https://mock.invalid/v1".into();
    m.context_window = 8192;
    m.max_tokens = 1024;
    m
}

fn provider(pid: &str, filter: Option<FilterModelsFn>) -> Provider {
    create_provider(CreateProviderOptions {
        id: pid.into(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth::api_key(ApiKeyAuth::from_env(pid, pid)),
        models: vec![
            AnyModel::Chat(model(pid, "allowed")),
            AnyModel::Chat(model(pid, "hidden")),
        ],
        fetch_models: None,
        filter_models: filter,
        api: Some(standard_chat_api_config()),
        classifiers: None,
    })
    .unwrap()
}

async fn runtime(credentials: Arc<dyn CredentialStore>) -> ModelRuntime {
    ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
}

#[tokio::test]
async fn custom_memory_credentials_are_visible_to_readonly_snapshot() {
    let creds = InMemoryCredentialStore::new();
    creds
        .set(
            "mock",
            Credential::ApiKey(ApiKeyCredential {
                key: Some("mock-key".into()),
                env: None,
            }),
        )
        .await;
    let rt = runtime(Arc::new(creds)).await;
    rt.register_provider(provider("mock", None));
    assert!(rt.has_configured_auth("mock"));
    assert_eq!(rt.get_available_models().len(), 2);
}

#[tokio::test]
async fn readonly_availability_applies_native_filter_with_oauth_metadata() {
    let creds = InMemoryAuthStorage::new(
        BTreeMap::from([(
            "mock".into(),
            Credential::OAuth(OAuthCredential {
                access: "mock-access".into(),
                refresh: "mock-refresh".into(),
                expires: 0,
                extra: BTreeMap::from([("plan".into(), serde_json::json!("allowed"))]),
            }),
        )]),
        ConfigValueResolver::default(),
    );
    let rt = runtime(Arc::new(creds)).await;
    rt.register_provider(provider(
        "mock",
        Some(Arc::new(|models, credential| {
            let Some(Credential::OAuth(o)) = credential else {
                return vec![];
            };
            models
                .iter()
                .filter(|m| Some(m.id.as_str()) == o.extra.get("plan").and_then(|v| v.as_str()))
                .cloned()
                .collect()
        })),
    ));
    assert_eq!(
        rt.get_available_models()
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["allowed"]
    );
}

#[cfg(windows)]
#[tokio::test]
async fn actual_models_auth_never_receives_command_status_sentinel() {
    let root = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("model-auth-{}", uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let auth_path = root.join("auth.json");
    std::fs::write(
        &auth_path,
        r#"{"mock":{"type":"api_key","key":"!echo mock-command-key"}}"#,
    )
    .unwrap();
    let rt = ModelRuntime::create(CreateModelRuntimeOptions {
        auth_path: Some(auth_path),
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    rt.register_provider(provider("mock", None));
    assert!(rt.has_configured_auth("mock"));
    let auth = rt.models().get_auth("mock", None).await.unwrap().unwrap();
    assert_eq!(auth.auth.api_key.as_deref(), Some("mock-command-key"));
}

#[test]
fn synchronization_error_never_displays_arbitrary_secret_cause() {
    let err = CredentialSynchronizationError {
        provider_id: "mock".into(),
        operation: CredentialSyncOperation::Login,
        credential: Some(Box::new(Credential::ApiKey(ApiKeyCredential {
            key: Some("mock-secret".into()),
            env: None,
        }))),
        cause: "arbitrary backend: mock-secret !command-private".into(),
    };
    for rendered in [err.to_string(), format!("{err:?}")] {
        assert!(!rendered.contains("mock-secret"));
        assert!(!rendered.contains("command-private"));
    }
}

#[test]
fn models_schema_errors_and_debug_redact_values() {
    for raw in [
        r#"{"providers":{"mock":{"apiKey":123,"baseUrl":"mock-secret"}}}"#,
        r#"{"providers":{"mock":{"models":[{"id":"m","contextWindow":"mock-secret"}]}}}"#,
    ] {
        let cfg = ModelConfig::from_json_str(raw, None);
        assert!(!cfg.get_error().unwrap().contains("mock-secret"));
    }
    let cfg = ModelConfig::from_json_str(
        r#"{"providers":{"mock":{"headers":{"X-Key":"mock-secret"},"models":[{"id":"m","headers":{"X-Other":"mock-secret"}}]}}}"#,
        None,
    );
    assert!(!format!("{cfg:?}").contains("mock-secret"));
    assert!(!format!("{:?}", cfg.get_provider("mock").unwrap().models).contains("mock-secret"));
}

#[test]
fn model_override_deep_merges_compat_and_sampling() {
    let mut base = model("mock", "m");
    base.compat = Some(
        serde_json::from_value(serde_json::json!({
            "openRouterRouting": {"nested": {"keep": 1, "change": 2}},
            "chatTemplateKwargs": {"nested": {"keep": 1, "change": 2}}
        }))
        .unwrap(),
    );
    base.sampling_params_by_thinking_level = Some(BTreeMap::from([(
        ModelThinkingLevel::High,
        BTreeMap::from([("nested".into(), serde_json::json!({"keep":1,"change":2}))]),
    )]));
    let ov: ModelsJsonModelOverride = serde_json::from_value(serde_json::json!({
        "compat": {"openRouterRouting":{"nested":{"change":3}}, "chatTemplateKwargs":{"nested":{"change":3}}},
        "samplingParamsByThinkingLevel": {"high":{"nested":{"change":3}}}
    })).unwrap();
    let merged = apply_model_override(&base, &ov);
    let c = serde_json::to_value(merged.compat.unwrap()).unwrap();
    assert_eq!(
        c["openRouterRouting"]["nested"],
        serde_json::json!({"keep":1,"change":3})
    );
    assert_eq!(
        c["chatTemplateKwargs"]["nested"],
        serde_json::json!({"keep":1,"change":3})
    );
    assert_eq!(
        merged.sampling_params_by_thinking_level.unwrap()[&ModelThinkingLevel::High]["nested"],
        serde_json::json!({"keep":1,"change":3})
    );
}

#[test]
fn custom_models_reject_invalid_api_url_and_token_limits() {
    for fields in [
        serde_json::json!({"api":"invalid-api","baseUrl":"https://mock.invalid","contextWindow":1,"maxTokens":1}),
        serde_json::json!({"api":"openai-completions","baseUrl":"not-a-url","contextWindow":1,"maxTokens":1}),
        serde_json::json!({"api":"openai-completions","baseUrl":"https://mock.invalid","contextWindow":0,"maxTokens":1}),
    ] {
        let mut fields = fields.as_object().unwrap().clone();
        fields.insert("id".into(), serde_json::json!("m"));
        let cfg = ModelConfig::from_json_str(
            &serde_json::json!({"providers":{"mock":{"models":[fields]}}}).to_string(),
            None,
        );
        assert!(cfg.get_error().is_some(), "{fields:?}");
    }
}

#[tokio::test]
async fn refresh_removes_deleted_config_provider_and_keeps_extension_override() {
    let root = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("model-refresh-{}", uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("models.json");
    std::fs::write(&path, r#"{"providers":{"config":{"api":"openai-completions","baseUrl":"https://mock.invalid","models":[{"id":"m"}]}}}"#).unwrap();
    let rt = ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: Some(path.clone()),
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await;
    let runner = pi_session::ExtensionRunner::from_extensions(".", vec![]).unwrap();
    rt.bind_extension_runner(&runner);
    runner
        .runtime()
        .register_provider(provider("config", None))
        .unwrap();
    std::fs::write(&path, r#"{"providers":{}}"#).unwrap();
    rt.refresh(Some(ModelsRefreshOptions {
        allow_network: Some(false),
        ..Default::default()
    }))
    .await;
    assert!(rt.get_model("config", "allowed").is_some());
    runner.runtime().unregister_provider("config").unwrap();
    assert!(rt.models().get_provider("config").is_none());
    rt.refresh_snapshot_sync();
    assert!(rt.get_all_models().is_empty());
}

#[test]
fn resolver_full_ids_colons_ambiguity_and_longest_provider_prefix() {
    let models = vec![
        model("a", "same"),
        model("b", "same"),
        model("a/b", "m:exacto"),
        model("a", "b/m:exacto"),
        model("x", "a/b/m:exacto"),
    ];
    let resolve = |text: &str, usable: fn(&str) -> bool| {
        resolve_model_reference(
            &ModelReferenceRequest {
                text: text.into(),
                provider: None,
                thinking_suffix: ThinkingSuffixMode::Strict,
                fuzzy: true,
            },
            &models,
            usable,
        )
    };
    assert!(matches!(
        resolve("same", |_| false),
        ModelReferenceResult::Ambiguous {
            usable_count: 0,
            ..
        }
    ));
    assert!(
        matches!(resolve("same", |p| p=="b"), ModelReferenceResult::Resolved {model:m,..} if m.provider=="b")
    );
    assert!(
        matches!(resolve("a/b/m:exacto", |p| p=="a/b"), ModelReferenceResult::Resolved {model:m,thinking_level:None,..} if m.provider=="a/b" && m.id=="m:exacto")
    );
    assert!(
        matches!(resolve("a/b/m:exacto:high", |p| p=="a/b"), ModelReferenceResult::Resolved {model:m,thinking_level:Some(ModelThinkingLevel::High),..} if m.provider=="a/b")
    );
}

#[tokio::test]
async fn generic_and_fixed_identity_selection_stay_separate() {
    let rt = runtime(Arc::new(InMemoryCredentialStore::new())).await;
    rt.register_provider(provider("preferred", None));
    rt.register_provider(provider("other", None));
    rt.set_runtime_api_key("other", "mock-key").await.unwrap();
    let levels = BTreeMap::new();
    let generic = InitialModelPolicy::GenericSdk;
    let fixed = InitialModelPolicy::FixedIdentity {
        default_provider: "preferred".into(),
        default_model_id: "allowed".into(),
        preserve_unauthenticated: true,
    };
    for (policy, expected) in [(&generic, "other"), (&fixed, "preferred")] {
        let result = find_initial_model(FindInitialModelOptions {
            scoped_models: &[],
            is_continuing: false,
            default_provider: None,
            default_model_id: None,
            default_thinking_level: None,
            model_thinking_levels: &levels,
            model_runtime: &rt,
            policy,
        });
        assert_eq!(result.model.unwrap().provider, expected);
    }
}

struct ProbeStore(Arc<std::sync::atomic::AtomicUsize>);
impl CredentialStore for ProbeStore {
    fn read<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err("must not execute".into()) })
    }
    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err("must not execute".into()) })
    }
    fn modify<'a>(
        &'a self,
        _: &'a str,
        _: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async { Err("unused".into()) })
    }
    fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("unused".into()) })
    }
}

#[tokio::test]
async fn readonly_refresh_never_calls_arbitrary_store_or_auth_resolver() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let rt = runtime(Arc::new(ProbeStore(calls.clone()))).await;
    let mut opts = provider("mock", None).to_create_options();
    let probe = calls.clone();
    opts.auth = ProviderAuth::api_key(ApiKeyAuth {
        resolve: Arc::new(move |_, _| {
            probe.fetch_add(1, Ordering::SeqCst);
            Err("must not execute".into())
        }),
        ..ApiKeyAuth::from_env("mock", "mock")
    });
    rt.register_provider(create_provider(opts).unwrap());
    rt.refresh(Some(ModelsRefreshOptions {
        allow_network: Some(false),
        ..Default::default()
    }))
    .await;
    rt.get_all_models();
    rt.get_available_models();
    rt.has_configured_auth("mock");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn configured_command_credential_reaches_native_dynamic_fetch() {
    let creds = InMemoryAuthStorage::new(
        BTreeMap::from([(
            "mock".into(),
            Credential::ApiKey(ApiKeyCredential {
                key: Some("mock-real-key".into()),
                env: None,
            }),
        )]),
        ConfigValueResolver::default(),
    );
    let rt = runtime(Arc::new(creds)).await;
    let mut opts = provider("mock", None).to_create_options();
    opts.fetch_models = Some(Arc::new(|input| {
        Box::pin(async move {
            assert!(
                matches!(input.credential, Some(Credential::ApiKey(c)) if c.key.as_deref()==Some("mock-real-key"))
            );
            Ok(vec![AnyModel::Chat(model("mock", "fetched"))])
        })
    }));
    rt.register_provider(create_provider(opts).unwrap());
    let result = rt
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(true),
            force: Some(true),
            ..Default::default()
        }))
        .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(rt.get_model("mock", "fetched").is_some());
}

#[tokio::test]
async fn composed_native_stream_and_explicit_header_deletion_are_preserved() {
    use futures_util::StreamExt;
    let seen = Arc::new(std::sync::Mutex::new(None));
    let capture = seen.clone();
    let native = ProviderStreams::new(
        Arc::new(move |model, _, opts| {
            *capture.lock().unwrap() = Some((model.clone(), opts.cloned().unwrap()));
            let stream = create_assistant_message_event_stream();
            stream.end(None);
            stream
        }),
        Arc::new(|_, _, _| panic!("unused")),
    );
    let mut opts = provider("mock", None).to_create_options();
    opts.api = Some(ProviderApiConfig::Single(native));
    let base = create_provider(opts).unwrap();
    let config: ModelsJsonProvider = serde_json::from_value(serde_json::json!({"apiKey":"mock-key","headers":{"X-Delete":"configured","X-Template":"$PHASE4_MODEL_HEADER"}})).unwrap();
    let composed = compose_provider_with_model_config(
        "mock",
        Some(&base),
        Some(&config),
        &ConfigValueResolver::default(),
    )
    .unwrap()
    .unwrap();
    assert!(
        matches!(
            composed.to_create_options().api,
            Some(ProviderApiConfig::Single(_))
        ),
        "native stream must be retained before invoking the mock"
    );
    let rt = runtime(Arc::new(InMemoryCredentialStore::new())).await;
    rt.register_provider(composed);
    // Exercise native composition without network, then the runtime's model-header resolution.
    let mut model = rt.get_model("mock", "allowed").unwrap();
    model.headers = Some(BTreeMap::from([
        ("X-Delete".into(), "model".into()),
        ("X-Template".into(), "$PHASE4_MODEL_HEADER".into()),
    ]));
    let options = StreamOptions {
        api_key: Some("mock-key".into()),
        env: Some(BTreeMap::from([(
            "PHASE4_MODEL_HEADER".into(),
            "resolved".into(),
        )])),
        headers: Some(BTreeMap::from([("x-delete".into(), None)])),
        ..Default::default()
    };
    let mut stream = rt
        .stream(model, TranscriptContext::default(), options)
        .await;
    while stream.next().await.is_some() {}
    let guard = seen.lock().unwrap();
    let (model, opts) = guard.as_ref().expect("native stream retained");
    assert!(model.headers.is_none());
    assert_eq!(opts.headers.as_ref().unwrap().get("x-delete"), Some(&None));
    assert!(!opts.headers.as_ref().unwrap().contains_key("X-Delete"));
    assert_eq!(
        opts.headers
            .as_ref()
            .unwrap()
            .get("X-Template")
            .and_then(|v| v.as_deref()),
        Some("resolved")
    );
}

#[tokio::test]
async fn keyless_auth_snapshot_uses_declaration_without_resolver_execution() {
    let rt = runtime(Arc::new(InMemoryCredentialStore::new())).await;
    let mut opts = provider("local", None).to_create_options();
    opts.auth = ProviderAuth::api_key(ApiKeyAuth::keyless("local", "unused"));
    rt.register_provider(create_provider(opts).unwrap());
    assert!(rt.has_configured_auth("local"));
    assert_eq!(rt.get_available_models().len(), 2);
}

#[tokio::test]
async fn credential_changes_update_other_isolated_view_without_recomposition() {
    let rt = runtime(Arc::new(InMemoryCredentialStore::new())).await;
    // Both views use the same persisted config/native baseline, independent catalogs.
    rt.register_provider(provider("mock", None));
    let isolated = rt.fork_isolated_view();
    assert!(isolated.models().get_provider("mock").is_some());
    isolated.register_provider(provider("only-in-fork", None));
    rt.rebuild_baseline_providers();
    assert!(rt.models().get_provider("only-in-fork").is_none());
    rt.set_runtime_api_key("mock", "mock-key").await.unwrap();
    assert!(isolated.has_configured_auth("mock"));
    rt.remove_runtime_api_key("mock").await.unwrap();
    assert!(!isolated.has_configured_auth("mock"));
    assert!(!Arc::ptr_eq(rt.models(), isolated.models()));
}

#[cfg(windows)]
#[tokio::test]
async fn models_json_auth_command_is_async_cancellable_and_cached() {
    use std::time::Duration;
    let root = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
        .join(format!("model-cancel-{}", uuid_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let marker = root.join("started");
    let command = format!(
        "!powershell.exe -NoProfile -Command \"Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 30; Write-Output mock-late\"",
        marker.display()
    );
    let config = ModelsJsonProvider {
        api_key: Some(command),
        ..Default::default()
    };
    let base = provider("mock", None);
    let resolver = ConfigValueResolver::new(&root, &root, true);
    let rt = runtime(Arc::new(InMemoryCredentialStore::new())).await;
    rt.register_provider(
        compose_provider_with_model_config("mock", Some(&base), Some(&config), &resolver)
            .unwrap()
            .unwrap(),
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = {
        let rt = rt.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            rt.models()
                .get_auth_cancellable("mock", None, Some(&cancel))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(4), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(!error.to_string().contains("mock-late"));
    assert!(!error.to_string().contains("Set-Content"));
}

#[tokio::test]
async fn login_logout_are_serialized_and_sync_failure_keeps_committed_secret_redacted() {
    let dir = std::env::temp_dir().join(format!("credential-sync-{}", uuid_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let cache_path = dir.join("models-store.json");
    let rt = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(false),
        refresh_on_create: Some(false),
        models_store_path: Some(cache_path.clone()),
        ..Default::default()
    })
    .await;
    let mut options = provider("mock", None).to_create_options();
    options.fetch_models = Some(Arc::new(|_| {
        Box::pin(async { panic!("credential sync must stay offline") })
    }));
    rt.register_provider(create_provider(options).unwrap());
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let interaction = AuthInteraction::new(
        {
            let entered = entered.clone();
            let release = release.clone();
            Arc::new(move |_| {
                let entered = entered.clone();
                let release = release.clone();
                Box::pin(async move {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    Ok("mock-login-secret".into())
                })
            })
        },
        Arc::new(|_| {}),
    );
    let login = {
        let rt = rt.clone();
        tokio::spawn(async move { rt.login("mock", AuthType::ApiKey, interaction, None).await })
    };
    entered.acquire().await.unwrap().forget();
    let logout = {
        let rt = rt.clone();
        tokio::spawn(async move { rt.logout("mock").await })
    };
    tokio::task::yield_now().await;
    assert!(!logout.is_finished());
    release.add_permits(1);
    login.await.unwrap().unwrap();
    logout.await.unwrap().unwrap();
    assert!(!rt.has_configured_auth("mock"));
    std::fs::write(&cache_path, "invalid JSON").unwrap();
    let error = rt
        .set_runtime_api_key("mock", "mock-committed-secret")
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("mock-committed-secret"));
    assert!(error.cause.contains("Invalid model catalog cache"));
    assert_eq!(
        rt.credentials()
            .read("mock")
            .await
            .unwrap()
            .unwrap()
            .auth_type(),
        AuthType::ApiKey
    );
    assert!(rt.has_configured_auth("mock"));
    std::fs::write(&cache_path, "{}").unwrap();
    rt.remove_runtime_api_key("mock").await.unwrap();
    assert!(!rt.has_configured_auth("mock"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn cloudflare_requires_saved_ids_and_codex_models_json_never_reopens_interactive_api_key() {
    let dir = std::env::temp_dir().join(format!("cf-and-codex-{}", uuid_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let auth_path = dir.join("auth.json");
    let models_path = dir.join("models.json");

    // 1. Cloudflare Workers AI 仅有密钥而缺少 Account ID 时不得被视为已配置
    std::fs::write(
        &auth_path,
        r#"{"cloudflare-workers-ai":{"type":"api_key","key":"cf-only-key"}}"#,
    )
    .unwrap();
    // 2. models.json 为 openai-codex 配置 headers 时不得伪造交互式 API Key 登录
    std::fs::write(
        &models_path,
        r#"{"providers":{"openai-codex":{"headers":{"X-Codex-Custom":"yes"}}}}"#,
    )
    .unwrap();

    let rt = ModelRuntime::create(CreateModelRuntimeOptions {
        include_builtins: Some(true),
        refresh_on_create: Some(false),
        auth_path: Some(auth_path.clone()),
        models_path: Some(models_path.clone()),
        ..Default::default()
    })
    .await;

    assert!(!rt.has_configured_auth("cloudflare-workers-ai"));
    assert!(
        rt.get_available_models()
            .iter()
            .all(|m| m.provider != "cloudflare-workers-ai")
    );

    let codex = rt.models().get_provider("openai-codex").unwrap();
    assert!(
        codex.auth().api_key.is_none(),
        "headers-only models.json on openai-codex must not fabricate ApiKeyAuth"
    );
    let oauth_to_auth = &codex.auth().oauth.as_ref().unwrap().to_auth;
    let derived = oauth_to_auth(pi_ai::OAuthCredential {
        access: "acc-tok".into(),
        refresh: "ref-tok".into(),
        expires: pi_ai::now_ms() + 60_000,
        extra: std::collections::BTreeMap::new(),
    })
    .unwrap();
    assert_eq!(
        derived
            .headers
            .as_ref()
            .and_then(|h| h.get("X-Codex-Custom"))
            .and_then(|v| v.as_deref()),
        Some("yes")
    );

    // 即使 models.json 显式给 openai-codex 配了 apiKey，也必须保持 api_key: None（纯 OAuth，不得绕过登录限制）
    std::fs::write(
        &models_path,
        r#"{"providers":{"openai-codex":{"apiKey":"literal-cfg-key"},"github-copilot":{"apiKey":"literal-copilot-key","headers":{"X-Copilot-Req":"$PER_REQ_VAL"}}}}"#,
    )
    .unwrap();
    rt.refresh(Some(pi_ai::ModelsRefreshOptions {
        allow_network: Some(false),
        providers: Some(vec!["openai-codex".into(), "github-copilot".into()]),
        force: Some(false),
    }))
    .await;
    let codex_with_key = rt.models().get_provider("openai-codex").unwrap();
    assert!(codex_with_key.auth().api_key.is_none());
    assert!(!rt.has_configured_auth("openai-codex"));
    assert!(
        rt.models()
            .get_auth("openai-codex", None)
            .await
            .unwrap()
            .is_none()
    );
    // Copilot 同样忽略 models.json 的 apiKey，且 OAuth 派生请求头可解析单次请求传入的环境变量
    assert!(
        rt.models()
            .get_auth("github-copilot", None)
            .await
            .unwrap()
            .is_none()
    );
    rt.credentials()
        .modify(
            "github-copilot",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(pi_ai::Credential::OAuth(pi_ai::OAuthCredential {
                        access: "copilot-oauth-tok".into(),
                        refresh: "copilot-oauth-ref".into(),
                        expires: pi_ai::now_ms() + 3_600_000,
                        extra: std::collections::BTreeMap::new(),
                    })))
                })
            }),
        )
        .await
        .unwrap();
    let copilot_req_auth = rt
        .models()
        .get_auth(
            "github-copilot",
            Some(&pi_ai::AuthResolutionOverrides {
                api_key: None,
                env: Some(std::collections::BTreeMap::from([(
                    "PER_REQ_VAL".into(),
                    "req-ok".into(),
                )])),
                min_oauth_validity_ms: None,
            }),
        )
        .await
        .unwrap()
        .expect("OAuth header resolution must receive per-request env");
    assert_eq!(
        copilot_req_auth
            .auth
            .headers
            .as_ref()
            .and_then(|h| h.get("X-Copilot-Req"))
            .and_then(|v| v.as_deref()),
        Some("req-ok")
    );

    // 3. 登录 Cloudflare Workers AI 保存密钥 + Account ID 后立即可用，且每个内置对话服务商的默认模型均存在
    let prompts = std::sync::Mutex::new(0usize);
    let cf_interaction = AuthInteraction::new(
        Arc::new(move |_| {
            let mut n = prompts.lock().unwrap();
            *n += 1;
            let step = *n;
            Box::pin(async move {
                if step == 1 {
                    Ok("cf-real-token".to_owned())
                } else {
                    Ok("acc_runtime_99".to_owned())
                }
            })
        }),
        Arc::new(|_| {}),
    );
    rt.login(
        "cloudflare-workers-ai",
        AuthType::ApiKey,
        cf_interaction,
        None,
    )
    .await
    .unwrap();
    assert!(rt.has_configured_auth("cloudflare-workers-ai"));
    let cf_resolved = rt
        .models()
        .get_auth("cloudflare-workers-ai", None)
        .await
        .unwrap()
        .expect("saved Cloudflare credential must resolve");
    assert_eq!(
        cf_resolved
            .env
            .as_ref()
            .and_then(|e| e.get("CLOUDFLARE_ACCOUNT_ID"))
            .map(String::as_str),
        Some("acc_runtime_99")
    );
    // P1-1: set_runtime_api_key 运行时换密钥后仍保留底层已存的 Cloudflare Account ID
    rt.set_runtime_api_key("cloudflare-workers-ai", "cf-runtime-rotated-token")
        .await
        .unwrap();
    assert!(rt.has_configured_auth("cloudflare-workers-ai"));
    let cf_after_runtime_key = rt
        .models()
        .get_auth("cloudflare-workers-ai", None)
        .await
        .unwrap()
        .expect("set_runtime_api_key must retain stored Cloudflare Account ID");
    assert_eq!(
        cf_after_runtime_key.auth.api_key.as_deref(),
        Some("cf-runtime-rotated-token")
    );
    assert_eq!(
        cf_after_runtime_key
            .env
            .as_ref()
            .and_then(|e| e.get("CLOUDFLARE_ACCOUNT_ID"))
            .map(String::as_str),
        Some("acc_runtime_99")
    );

    // P1-4 & P2-2: 仅靠 headers 认证的自定义服务可正常解析，且自定义服务可注册 openai-codex-responses 协议
    std::fs::write(
        &models_path,
        r#"{
            "providers": {
                "hdr-only-gw": {
                    "baseUrl": "https://example.com/v1",
                    "api": "openai-codex-responses",
                    "headers": { "Authorization": "Bearer hdr-only-tok" },
                    "models": [{ "id": "codex-proxy", "name": "Codex Proxy" }]
                }
            }
        }"#,
    )
    .unwrap();
    rt.refresh(Some(pi_ai::ModelsRefreshOptions {
        allow_network: Some(false),
        providers: None,
        force: Some(false),
    }))
    .await;
    assert_eq!(rt.get_error(), None);
    let hdr_auth = rt
        .models()
        .get_auth("hdr-only-gw", None)
        .await
        .unwrap()
        .expect("headers-only custom provider must resolve auth");
    assert_eq!(
        hdr_auth
            .auth
            .headers
            .as_ref()
            .and_then(|h| h.get("Authorization"))
            .and_then(|v| v.as_deref()),
        Some("Bearer hdr-only-tok")
    );
    let hdr_prov = rt.models().get_provider("hdr-only-gw").unwrap();
    if let Some(pi_ai::ProviderApiConfig::ByApi(api_map)) = hdr_prov.to_create_options().api {
        assert!(api_map.contains_key(pi_ai::known_api::OPENAI_CODEX_RESPONSES));
    } else {
        panic!("expected ByApi map on custom provider");
    }

    for prov in pi_runtime::DEFAULT_PROVIDER_PRIORITY {
        let def_id = pi_runtime::default_model_for_provider(prov)
            .unwrap_or_else(|| panic!("missing default model mapping for {prov}"));
        assert!(
            rt.get_model(prov, def_id).is_some(),
            "default model {prov}/{def_id} missing from catalog"
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}

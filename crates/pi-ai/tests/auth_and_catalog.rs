use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pi_ai::providers::faux::{
    faux_assistant_message, faux_text, faux_tool_call, register_faux_provider,
};
use pi_ai::{
    AnyModel, ApiKeyAuth, ApiKeyCredential, AssistantMessage, AssistantMessageEvent, AuthEvent,
    AuthInteraction, AuthPrompt, AuthResolutionOverrides, AuthResult, AuthType, BoxFuture,
    ClassifierContext, ClassifierOptions, ClassifierResult, ClassifierStopReason, Context,
    CreateModelsOptions, CreateProviderOptions, Credential, CredentialStore, FetchModelsFn,
    InMemoryCredentialStore, InMemoryModelsStore, Message, Model, ModelAuth, ModelCost,
    ModelInputModality, ModelsErrorCode, ModelsRefreshOptions, ModelsStore, ModelsStoreEntry,
    OAuthAuth, OAuthCredential, ProviderApiConfig, ProviderAuth, ProviderClassifier,
    ProviderStreams, SimpleStreamOptions, StopReason, StreamFn, StreamOptions, UserMessage,
    builtin_models, builtin_providers, create_assistant_message_event_stream, create_models,
    create_provider, default_api_map, known_api, known_classifier_api, now_ms,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[test]
fn all_26_builtin_providers_load_and_reject_contradictory_model_types() {
    let providers = builtin_providers();
    assert_eq!(providers.len(), 26);

    let models = builtin_models(None);
    let all = models.get_all_models(None);
    assert!(
        all.len() > 1000,
        "expected >1000 builtin models, got {}",
        all.len()
    );

    let ds_flash = models
        .get_model("deepseek", "deepseek-flash")
        .expect("deepseek-flash must exist");
    assert_eq!(ds_flash.api, known_api::OPENAI_RESPONSES);
    assert_eq!(ds_flash.base_url, "https://api.deepseek.com");

    // S6: 标记为 type: "chat" 但缺少必填 maxTokens 的损坏 JSON 必须反序列化报错，禁止被误判为 ClassifierModel
    let mut broken_chat = serde_json::to_value(&ds_flash).unwrap();
    broken_chat["type"] = json!("chat");
    broken_chat.as_object_mut().unwrap().remove("maxTokens");
    let deser_res = serde_json::from_value::<AnyModel>(broken_chat);
    assert!(
        deser_res.is_err(),
        "contradictory chat model without maxTokens must fail deserialization"
    );

    // 验证多端点服务商（如 opencode、openrouter、vercel-ai-gateway）保留各模型自身的 base_url
    let oc = models.get_provider("opencode").unwrap();
    assert!(oc.base_url().is_none());

    let jev = models
        .get_classifier_model("typesafe", "jev-latest")
        .expect("typesafe/jev-latest classifier must exist");
    assert_eq!(jev.api, known_classifier_api::TYPESAFE_SYSTEM_ONE);
}

#[test]
fn credentials_and_options_debug_output_redact_secrets() {
    let sentinel_key = "sk-secret-sentinel-99999";
    let sentinel_refresh = "rt-secret-sentinel-88888";
    let sentinel_header = "Bearer super-secret-header-77777";
    let sentinel_env = "env-secret-value-66666";

    let api_cred = Credential::ApiKey(ApiKeyCredential {
        key: Some(sentinel_key.to_owned()),
        env: Some(BTreeMap::from([(
            "SECRET_ENV".to_owned(),
            sentinel_env.to_owned(),
        )])),
    });
    let oauth_cred = Credential::OAuth(OAuthCredential {
        refresh: sentinel_refresh.to_owned(),
        access: sentinel_key.to_owned(),
        expires: 12345,
        extra: BTreeMap::from([("secretExtra".to_owned(), json!(sentinel_env))]),
    });
    let auth_res = AuthResult {
        auth: ModelAuth {
            api_key: Some(sentinel_key.to_owned()),
            headers: Some(BTreeMap::from([(
                "Authorization".to_owned(),
                Some(sentinel_header.to_owned()),
            )])),
            base_url: Some("https://api.example.com".to_owned()),
        },
        env: Some(BTreeMap::from([(
            "SECRET_ENV".to_owned(),
            sentinel_env.to_owned(),
        )])),
        source: Some("credentials".to_owned()),
    };
    let clf_opts = ClassifierOptions {
        api_key: Some(sentinel_key.to_owned()),
        headers: Some(BTreeMap::from([(
            "Authorization".to_owned(),
            Some(sentinel_header.to_owned()),
        )])),
        env: Some(BTreeMap::from([(
            "SECRET_ENV".to_owned(),
            sentinel_env.to_owned(),
        )])),
        ..ClassifierOptions::default()
    };

    let overrides = AuthResolutionOverrides {
        api_key: Some(sentinel_key.to_owned()),
        env: Some(BTreeMap::from([(
            "SECRET_ENV".to_owned(),
            sentinel_env.to_owned(),
        )])),
        min_oauth_validity_ms: None,
    };
    let mut model_with_hdr = builtin_models(None)
        .get_model("deepseek", "deepseek-flash")
        .unwrap();
    model_with_hdr.headers = Some(BTreeMap::from([(
        "Authorization".to_owned(),
        sentinel_header.to_owned(),
    )]));

    let dumped = format!(
        "{api_cred:?} {oauth_cred:?} {auth_res:?} {clf_opts:?} {overrides:?} {model_with_hdr:?}"
    );
    assert!(!dumped.contains(sentinel_key));
    assert!(!dumped.contains(sentinel_refresh));
    assert!(!dumped.contains(sentinel_header));
    assert!(!dumped.contains(sentinel_env));
    assert!(dumped.contains("[REDACTED]"));
}

#[tokio::test]
async fn complete_discards_intermediate_event_snapshots() {
    let stream = create_assistant_message_event_stream();
    let writer = stream.clone();
    let m = builtin_models(None)
        .get_model("deepseek", "deepseek-flash")
        .unwrap();

    stream.discard_queued_events();
    let m_clone = m.clone();
    tokio::spawn(async move {
        let mut partial = AssistantMessage::new_pending(&m_clone);
        writer.push(AssistantMessageEvent::Start {
            partial: partial.clone(),
        });
        for i in 0..100 {
            writer.push(AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: format!("chunk-{i}"),
                partial: partial.clone(),
            });
        }
        partial.stop_reason = StopReason::Stop;
        writer.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: partial,
        });
        writer.end(None);
    });

    let res = stream.result().await;
    assert_eq!(res.stop_reason, StopReason::Stop);
    assert_eq!(
        stream.queued_event_count(),
        0,
        "result-only mode must not retain intermediate event snapshots"
    );
}

#[tokio::test]
async fn credential_priority_and_oauth_refresh_preserve_stored_token_on_failure() {
    let cred_store = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(cred_store.clone()),
        models_store: None,
        identity: None,
    }));

    let failing_oauth = OAuthAuth {
        name: "Test OAuth".to_owned(),
        login_fn: None,
        is_subscription: true,
        refresh: Arc::new(|_cred| {
            Box::pin(async move { Err("upstream auth server 503".to_owned()) })
        }),
        to_auth: Arc::new(|cred| {
            Ok(ModelAuth {
                api_key: Some(cred.access),
                headers: None,
                base_url: None,
            })
        }),
    };

    let provider = create_provider(CreateProviderOptions {
        id: "deepseek".to_owned(),
        name: Some("DeepSeek".to_owned()),
        base_url: Some("https://api.deepseek.com".to_owned()),
        headers: None,
        auth: ProviderAuth::both(
            ApiKeyAuth::from_env("DeepSeek API key", "deepseek"),
            failing_oauth,
        ),
        models: vec![],
        fetch_models: None,
        filter_models: None,
        api: Some(ProviderApiConfig::ByApi(default_api_map())),
        classifiers: None,
    })
    .unwrap();
    models.set_provider(provider);

    let env_map = BTreeMap::from([("DEEPSEEK_API_KEY".to_owned(), "env-key-111".to_owned())]);
    let res_env = models
        .get_auth(
            "deepseek",
            Some(&AuthResolutionOverrides {
                api_key: None,
                env: Some(env_map.clone()),
                min_oauth_validity_ms: None,
            }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res_env.auth.api_key.as_deref(), Some("env-key-111"));

    cred_store
        .set(
            "deepseek",
            Credential::ApiKey(ApiKeyCredential {
                key: Some("stored-key-222".to_owned()),
                env: None,
            }),
        )
        .await;
    let res_stored = models
        .get_auth(
            "deepseek",
            Some(&AuthResolutionOverrides {
                api_key: None,
                env: Some(env_map.clone()),
                min_oauth_validity_ms: None,
            }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res_stored.auth.api_key.as_deref(), Some("stored-key-222"));

    let res_explicit = models
        .get_auth(
            "deepseek",
            Some(&AuthResolutionOverrides {
                api_key: Some("explicit-key-333".to_owned()),
                env: Some(env_map.clone()),
                min_oauth_validity_ms: None,
            }),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        res_explicit.auth.api_key.as_deref(),
        Some("explicit-key-333")
    );

    let expired_oauth = OAuthCredential {
        refresh: "refresh-secret-keep-me".to_owned(),
        access: "old-access".to_owned(),
        expires: now_ms().saturating_sub(10_000),
        extra: BTreeMap::new(),
    };
    cred_store
        .set("deepseek", Credential::OAuth(expired_oauth.clone()))
        .await;

    let err = models
        .get_auth(
            "deepseek",
            Some(&AuthResolutionOverrides {
                api_key: None,
                env: Some(env_map),
                min_oauth_validity_ms: None,
            }),
        )
        .await
        .expect_err("expired oauth with failing refresh must error");
    assert_eq!(err.code, ModelsErrorCode::OAuth);

    let still_stored = cred_store.read("deepseek").await.unwrap();
    assert_eq!(still_stored, Some(Credential::OAuth(expired_oauth)));
}

#[tokio::test]
async fn builtin_oauth_providers_refresh_against_mock_server_and_derive_auth() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let body = r#"{"access_token":"fresh-openai-token","refresh_token":"fresh-refresh","expires_in":3600,"scope":"chatgpt.tokens.use.direct"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
    });

    let p = pi_ai::providers::openai::openai_provider();
    let oauth = p.auth().oauth.as_ref().unwrap();
    let refreshed = (oauth.refresh)(OAuthCredential {
        access: "expired-access".to_owned(),
        refresh: "old-refresh".to_owned(),
        expires: 0,
        extra: BTreeMap::from([
            (
                "tokenUrl".to_owned(),
                json!(format!("http://{addr}/oauth/token")),
            ),
            ("clientId".to_owned(), json!("issued-mock-client")),
        ]),
    })
    .await
    .expect("built-in OpenAI OAuth refresher should succeed against mock token server");
    assert_eq!(refreshed.access, "fresh-openai-token");
    assert_eq!(refreshed.refresh, "fresh-refresh");

    let copilot = pi_ai::providers::github_copilot::github_copilot_provider();
    let copilot_oauth = copilot.auth().oauth.as_ref().unwrap();
    let copilot_cred = OAuthCredential {
        access: "tid=1;exp=9999999999;proxy-ep=proxy.enterprise.githubcopilot.com".to_owned(),
        refresh: "gho_test".to_owned(),
        expires: now_ms() + 3_600_000,
        extra: BTreeMap::from([("availableModelIds".to_owned(), json!(["gpt-4o"]))]),
    };
    let derived = (copilot_oauth.to_auth)(copilot_cred.clone()).unwrap();
    assert_eq!(
        derived.base_url.as_deref(),
        Some("https://api.enterprise.githubcopilot.com")
    );
    let filtered = copilot.filter_chat_models(
        &copilot.get_models(),
        Some(&Credential::OAuth(copilot_cred)),
    );
    assert!(filtered.iter().all(|m| m.id == "gpt-4o"));
}

#[tokio::test]
async fn provider_endpoint_headers_and_classifier_auth_headers_are_applied() {
    let capture = Arc::new(Mutex::new(None));
    let capture_clone = capture.clone();
    let m = builtin_models(None)
        .get_model("deepseek", "deepseek-flash")
        .unwrap();

    let stream_fn: StreamFn = Arc::new(move |req_m, _ctx, req_o| {
        *capture_clone.lock().unwrap() = Some((req_m.clone(), req_o.cloned().unwrap_or_default()));
        let s = create_assistant_message_event_stream();
        let mut msg = AssistantMessage::new_pending(req_m);
        msg.stop_reason = StopReason::Stop;
        s.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: msg,
        });
        s
    });
    let stream_fn_simple = stream_fn.clone();
    let streams = ProviderStreams::new(
        stream_fn,
        Arc::new(move |m, c, o| stream_fn_simple(m, c, o.map(|s| &s.base))),
    );

    let custom_provider = create_provider(CreateProviderOptions {
        id: "deepseek".to_owned(),
        name: None,
        base_url: Some("https://proxy.example/v1".to_owned()),
        headers: Some(BTreeMap::from([(
            "x-tenant".to_owned(),
            Some("tenant-1".to_owned()),
        )])),
        auth: ProviderAuth::api_key(ApiKeyAuth::keyless("test", "dummy")),
        models: vec![AnyModel::Chat(m.clone())],
        fetch_models: None,
        filter_models: None,
        api: Some(ProviderApiConfig::Single(streams)),
        classifiers: None,
    })
    .unwrap();

    let models = create_models(None);
    models.set_provider(custom_provider);
    let ctx = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new("hi"))],
        tools: None,
    };
    models.complete(&m, &ctx, None).await;

    let (seen_model, seen_opts) = capture.lock().unwrap().clone().unwrap();
    assert_eq!(seen_model.base_url, "https://proxy.example/v1");
    assert_eq!(
        seen_opts
            .headers
            .as_ref()
            .and_then(|h| h.get("x-tenant"))
            .cloned()
            .flatten()
            .as_deref(),
        Some("tenant-1")
    );

    let clf_model = builtin_models(None)
        .get_classifier_model("typesafe", "jev-latest")
        .unwrap();
    let seen_clf = Arc::new(Mutex::new(None));
    let seen_clf_clone = seen_clf.clone();
    let clf = ProviderClassifier::new(Arc::new(move |cm, _c, co| {
        *seen_clf_clone.lock().unwrap() = Some((cm.base_url.clone(), co.cloned()));
        let cm = cm.clone();
        Box::pin(async move {
            ClassifierResult {
                api: cm.api,
                provider: cm.provider,
                model: cm.id,
                answers: BTreeMap::new(),
                usage: None,
                stop_reason: ClassifierStopReason::Stop,
                error_message: None,
                timestamp: 0,
            }
        })
    }));
    let header_only_auth = ProviderAuth::api_key(ApiKeyAuth {
        name: "header-only".to_owned(),
        login_fn: None,
        resolve: Arc::new(|_, _| {
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: None,
                    headers: Some(BTreeMap::from([(
                        "authorization".to_owned(),
                        Some("Bearer custom-clf".to_owned()),
                    )])),
                    base_url: None,
                },
                env: Some(BTreeMap::from([("CLF_ENV".to_owned(), "1".to_owned())])),
                source: None,
            }))
        }),
    });
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: clf_model.provider.clone(),
            name: None,
            base_url: Some("https://clf-proxy.example/v1".to_owned()),
            headers: Some(BTreeMap::from([(
                "x-org".to_owned(),
                Some("org-9".to_owned()),
            )])),
            auth: header_only_auth,
            models: vec![AnyModel::Classifier(clf_model.clone())],
            fetch_models: None,
            filter_models: None,
            api: None,
            classifiers: Some(BTreeMap::from([(clf_model.api.clone(), clf)])),
        })
        .unwrap(),
    );
    let clf_ctx = ClassifierContext {
        state: serde_json::Map::new(),
        questions: BTreeMap::new(),
        images: None,
    };
    models.classify(&clf_model, &clf_ctx, None).await;
    let (seen_clf_url, seen_clf_opts) = seen_clf.lock().unwrap().clone().unwrap();
    assert_eq!(seen_clf_url, "https://clf-proxy.example/v1");
    let hdrs = seen_clf_opts.unwrap().headers.unwrap();
    assert_eq!(
        hdrs.get("authorization").cloned().flatten().as_deref(),
        Some("Bearer custom-clf")
    );
    assert_eq!(
        hdrs.get("x-org").cloned().flatten().as_deref(),
        Some("org-9")
    );
}

struct FailingWriteModelsStore {
    inner: InMemoryModelsStore,
}

impl ModelsStore for FailingWriteModelsStore {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, String>> {
        self.inner.read(provider_id)
    }

    fn write<'a>(
        &'a self,
        _provider_id: &'a str,
        _entry: ModelsStoreEntry,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { Err("disk full on USB drive".to_owned()) })
    }

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.inner.delete(provider_id)
    }
}

#[tokio::test]
async fn dynamic_provider_restores_cache_and_reports_store_errors() {
    let cred_store = Arc::new(InMemoryCredentialStore::new());
    let base_store = InMemoryModelsStore::new();

    let cached_model = Model {
        id: "cstoa-cached-v1".to_owned(),
        name: "CSTOA Cached".to_owned(),
        api: known_api::OPENAI_RESPONSES.to_owned(),
        provider: "cstoa".to_owned(),
        base_url: "https://api.cstoa.example/v1".to_owned(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        model_type: Some("chat".to_owned()),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 128_000,
        max_tokens: 8192,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    };

    base_store
        .write(
            "cstoa",
            ModelsStoreEntry {
                models: vec![AnyModel::Chat(cached_model.clone())],
                last_modified: None,
                checked_at: Some(100),
                etag: None,
            },
        )
        .await
        .unwrap();

    cred_store
        .set(
            "cstoa",
            Credential::ApiKey(ApiKeyCredential {
                key: Some("cstoa-token".to_owned()),
                env: None,
            }),
        )
        .await;

    let live_model = Model {
        id: "cstoa-live-v2".to_owned(),
        name: "CSTOA Live V2".to_owned(),
        ..cached_model.clone()
    };
    let live_model_clone = live_model.clone();

    let cstoa_provider = create_provider(CreateProviderOptions {
        id: "cstoa".to_owned(),
        name: Some("CSTOA".to_owned()),
        base_url: Some("https://api.cstoa.example/v1".to_owned()),
        headers: None,
        auth: ProviderAuth::api_key(ApiKeyAuth::keyless("CSTOA Token", "cstoa-token")),
        models: vec![],
        fetch_models: Some(Arc::new(move |_input| {
            let m = live_model_clone.clone();
            Box::pin(async move { Ok(vec![AnyModel::Chat(m)]) })
        })),
        filter_models: None,
        api: Some(ProviderApiConfig::ByApi(default_api_map())),
        classifiers: None,
    })
    .unwrap();

    let failing_store = Arc::new(FailingWriteModelsStore { inner: base_store });
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(cred_store),
        models_store: Some(failing_store),
        identity: None,
    }));
    models.set_provider(cstoa_provider);

    let r1 = models
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["cstoa".to_owned()]),
            force: None,
        }))
        .await;
    assert!(r1.errors.is_empty());
    assert!(models.get_model("cstoa", "cstoa-cached-v1").is_some());

    let r2 = models
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec!["cstoa".to_owned()]),
            force: Some(true),
        }))
        .await;
    assert!(models.get_model("cstoa", "cstoa-live-v2").is_some());
    let store_err = r2
        .errors
        .get("cstoa")
        .expect("models_store write error must be reported");
    assert_eq!(store_err.code, ModelsErrorCode::ModelSource);
    assert!(store_err.message.contains("disk full"));
}

#[tokio::test]
async fn faux_provider_streams_through_models_collection() {
    let models = create_models(None);
    let faux = register_faux_provider(&models);

    faux.set_responses([
        faux_assistant_message(
            vec![
                faux_text("Let me run the check"),
                faux_tool_call(
                    "check_network",
                    serde_json::from_value(json!({"host": "8.8.8.8"})).unwrap(),
                    Some("call_1".to_owned()),
                ),
            ],
            Some(StopReason::ToolUse),
        )
        .into(),
        faux_assistant_message(
            vec![faux_text("Network is healthy!")],
            Some(StopReason::Stop),
        )
        .into(),
    ]);

    let ctx = Context {
        system_prompt: Some("Diagnostic agent".to_owned()),
        messages: vec![Message::User(UserMessage::new("Check network"))],
        tools: None,
    };

    let msg1 = models.complete_simple(&faux.model, &ctx, None).await;
    assert_eq!(msg1.stop_reason, StopReason::ToolUse);
    assert_eq!(msg1.tool_calls().len(), 1);
    assert_eq!(msg1.tool_calls()[0].name, "check_network");

    let msg2 = models.complete_simple(&faux.model, &ctx, None).await;
    assert_eq!(msg2.stop_reason, StopReason::Stop);
    assert_eq!(msg2.text(), "Network is healthy!");
    assert_eq!(faux.call_count(), 2);
}

#[tokio::test]
async fn models_collection_streams_mock_deepseek_via_credential_store() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let sse = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_m1\"}}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_m1\",\"role\":\"assistant\",\"status\":\"in_progress\",\"content\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"7\"}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_m1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"7\"}]}}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_m1\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":20,\"output_tokens\":2,\"total_tokens\":22}}}\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                sse.len(),
                sse
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
    });

    let cred_store = Arc::new(InMemoryCredentialStore::new());
    cred_store
        .set(
            "deepseek",
            Credential::ApiKey(ApiKeyCredential {
                key: Some("mock-stored-key".to_owned()),
                env: None,
            }),
        )
        .await;

    let models = builtin_models(Some(CreateModelsOptions {
        credentials: Some(cred_store),
        models_store: None,
        identity: None,
    }));

    let mut ds_flash = models
        .get_model("deepseek", "deepseek-flash")
        .expect("deepseek-flash must exist");
    ds_flash.base_url = format!("http://{addr}/v1");
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "deepseek".to_owned(),
            name: Some("DeepSeek".to_owned()),
            base_url: Some(format!("http://{addr}/v1")),
            headers: None,
            auth: ProviderAuth::api_key(ApiKeyAuth::from_env("DeepSeek API key", "deepseek")),
            models: vec![AnyModel::Chat(ds_flash.clone())],
            fetch_models: None,
            filter_models: None,
            api: Some(ProviderApiConfig::ByApi(default_api_map())),
            classifiers: None,
        })
        .unwrap(),
    );

    let ctx = Context {
        system_prompt: Some("只回复数字本身。".to_owned()),
        messages: vec![Message::User(UserMessage::new("3+4等于几？"))],
        tools: None,
    };

    let reply = models
        .complete_simple(&ds_flash, &ctx, Some(&SimpleStreamOptions::default()))
        .await;
    assert_eq!(reply.stop_reason, StopReason::Stop);
    assert_eq!(reply.text(), "7");
    assert_eq!(reply.usage.total_tokens, 22);
    assert!(reply.usage.cost.total > 0.0);
}

#[tokio::test]
async fn models_login_persists_api_key_and_oauth_credentials_and_redacts_oauth_errors() {
    let cred_store = Arc::new(InMemoryCredentialStore::new());
    let models = builtin_models(Some(CreateModelsOptions {
        credentials: Some(cred_store.clone()),
        models_store: None,
        identity: None,
    }));

    // 1. API Key login flow
    let interaction = AuthInteraction::new(
        Arc::new(|prompt| {
            Box::pin(async move {
                match prompt {
                    AuthPrompt::Secret { .. } => Ok("  sk-logged-in-deepseek-key  ".to_owned()),
                    _ => Err("unexpected prompt".to_owned()),
                }
            })
        }),
        Arc::new(|_ev| {}),
    );
    let logged_in = models
        .login("deepseek", AuthType::ApiKey, &interaction, None)
        .await
        .expect("API key login must succeed");
    assert_eq!(
        logged_in,
        Credential::ApiKey(ApiKeyCredential {
            key: Some("sk-logged-in-deepseek-key".to_owned()),
            env: None,
        })
    );
    assert_eq!(cred_store.read("deepseek").await.unwrap(), Some(logged_in));

    // 2. OAuth login flow via with_login + persistence
    let custom_oauth = OAuthAuth::anthropic().with_login(Arc::new(|interaction, _opts| {
        Box::pin(async move {
            interaction.notify_user(AuthEvent::Progress {
                message: "exchanging token".to_owned(),
            });
            let code = interaction
                .prompt_user(AuthPrompt::ManualCode {
                    message: "paste code".to_owned(),
                    placeholder: None,
                })
                .await?;
            Ok(OAuthCredential {
                access: format!("oat-access-{code}"),
                refresh: "oat-refresh".to_owned(),
                expires: now_ms() + 3_600_000,
                extra: BTreeMap::new(),
            })
        })
    }));
    let anth_models = builtin_models(None).get_models(Some("anthropic"));
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "anthropic".to_owned(),
            name: Some("Anthropic".to_owned()),
            base_url: Some("https://api.anthropic.com".to_owned()),
            headers: None,
            auth: ProviderAuth::oauth(custom_oauth),
            models: anth_models.into_iter().map(AnyModel::Chat).collect(),
            fetch_models: None,
            filter_models: None,
            api: Some(ProviderApiConfig::ByApi(default_api_map())),
            classifiers: None,
        })
        .unwrap(),
    );

    let oauth_interaction = AuthInteraction::new(
        Arc::new(|_prompt| Box::pin(async move { Ok("authcode123".to_owned()) })),
        Arc::new(|_ev| {}),
    );
    let oauth_cred = models
        .login("anthropic", AuthType::OAuth, &oauth_interaction, None)
        .await
        .expect("OAuth login must succeed and persist");
    match oauth_cred {
        Credential::OAuth(ref c) => assert_eq!(c.access, "oat-access-authcode123"),
        _ => panic!("expected OAuth credential"),
    }
    assert_eq!(
        cred_store.read("anthropic").await.unwrap(),
        Some(oauth_cred)
    );

    // 3. OAuth refresh error must not expose echoed refresh_token from HTTP 400 body
    let err_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let err_addr = err_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = err_listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf).await.unwrap();
        let body = r#"{"error":"invalid_grant","refresh_token":"echoed-secret-sentinel-777"}"#;
        let resp = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = s.write_all(resp.as_bytes()).await;
    });
    let bad_cred = OAuthCredential {
        access: "old".to_owned(),
        refresh: "echoed-secret-sentinel-777".to_owned(),
        expires: 0,
        extra: BTreeMap::from([(
            "tokenUrl".to_owned(),
            json!(format!("http://{err_addr}/token")),
        )]),
    };
    let refresh_err = (OAuthAuth::openai_codex().refresh)(bad_cred)
        .await
        .unwrap_err();
    assert!(!refresh_err.contains("echoed-secret-sentinel-777"));
    assert!(refresh_err.contains("invalid_grant"));
}

#[tokio::test]
async fn per_provider_credential_locks_and_cancellable_auth_prep_work() {
    // 1. Provider A 挂起刷新不会阻塞 Provider B 读取
    let store = InMemoryCredentialStore::new();
    store
        .set(
            "b",
            Credential::ApiKey(ApiKeyCredential {
                key: Some("b-key".to_owned()),
                env: None,
            }),
        )
        .await;
    let entered = Arc::new(Semaphore::new(0));
    let entered_clone = entered.clone();
    let store_clone = store.clone();
    let pending_task = tokio::spawn(async move {
        store_clone
            .modify(
                "a",
                Box::new(move |_| {
                    Box::pin(async move {
                        entered_clone.add_permits(1);
                        std::future::pending::<Result<Option<Credential>, String>>().await
                    })
                }),
            )
            .await
    });
    entered.acquire().await.unwrap().forget();
    let b_read = tokio::time::timeout(std::time::Duration::from_millis(100), store.read("b"))
        .await
        .expect("reading provider B must not be blocked by provider A modify")
        .unwrap();
    assert!(b_read.is_some());
    pending_task.abort();

    // 2. OAuth 刷新挂起时取消请求能立即中止并返回 StopReason::Aborted
    let refresh_entered = Arc::new(Semaphore::new(0));
    let re_clone = refresh_entered.clone();
    let pending_oauth = OAuthAuth {
        name: "pending".to_owned(),
        login_fn: None,
        is_subscription: true,
        refresh: Arc::new(move |_| {
            let re = re_clone.clone();
            Box::pin(async move {
                re.add_permits(1);
                std::future::pending::<Result<OAuthCredential, String>>().await
            })
        }),
        to_auth: Arc::new(|c| {
            Ok(ModelAuth {
                api_key: Some(c.access),
                headers: None,
                base_url: None,
            })
        }),
    };
    let c_store = Arc::new(InMemoryCredentialStore::new());
    c_store
        .set(
            "deepseek",
            Credential::OAuth(OAuthCredential {
                access: "exp".to_owned(),
                refresh: "ref".to_owned(),
                expires: 0,
                extra: BTreeMap::new(),
            }),
        )
        .await;
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(c_store),
        models_store: None,
        identity: None,
    }));
    let ds_model = builtin_models(None)
        .get_model("deepseek", "deepseek-flash")
        .unwrap();
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "deepseek".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::oauth(pending_oauth),
            models: vec![AnyModel::Chat(ds_model.clone())],
            fetch_models: None,
            filter_models: None,
            api: Some(ProviderApiConfig::ByApi(default_api_map())),
            classifiers: None,
        })
        .unwrap(),
    );

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let ctx = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new("hi"))],
        tools: None,
    };
    let req_task = tokio::spawn(async move {
        models
            .complete(
                &ds_model,
                &ctx,
                Some(&StreamOptions {
                    cancel_token: Some(cancel_clone),
                    ..StreamOptions::default()
                }),
            )
            .await
    });
    refresh_entered.acquire().await.unwrap().forget();
    cancel.cancel();
    let aborted_msg = tokio::time::timeout(std::time::Duration::from_millis(150), req_task)
        .await
        .expect("cancellation must interrupt pending OAuth refresh")
        .unwrap();
    assert_eq!(aborted_msg.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn concurrent_dynamic_refresh_and_generation_guard_work() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let count = Arc::new(AtomicUsize::new(0));
    let e_clone = entered.clone();
    let r_clone = release.clone();
    let base_model = builtin_models(None)
        .get_model("deepseek", "deepseek-flash")
        .unwrap();

    let fetch: FetchModelsFn = Arc::new(move |_| {
        let n = count.fetch_add(1, Ordering::SeqCst);
        let e = e_clone.clone();
        let r = r_clone.clone();
        let mut m = base_model.clone();
        m.provider = "dyn".to_owned();
        Box::pin(async move {
            if n == 0 {
                e.add_permits(1);
                r.acquire().await.unwrap().forget();
                m.id = "stale-model".to_owned();
            } else {
                m.id = "fresh-model".to_owned();
            }
            Ok(vec![AnyModel::Chat(m)])
        })
    });

    let models = create_models(None);
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "dyn".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth::api_key(ApiKeyAuth::keyless("k", "unused")),
            models: vec![],
            fetch_models: Some(fetch),
            filter_models: None,
            api: Some(ProviderApiConfig::ByApi(default_api_map())),
            classifiers: None,
        })
        .unwrap(),
    );

    let m1 = models.clone();
    let slow_refresh = tokio::spawn(async move {
        m1.refresh(Some(ModelsRefreshOptions {
            allow_network: Some(true),
            force: Some(true),
            providers: None,
        }))
        .await
    });
    entered.acquire().await.unwrap().forget();

    // 第二次刷新先完成并发布 fresh-model
    models
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(true),
            force: Some(true),
            providers: None,
        }))
        .await;
    assert!(models.get_model("dyn", "fresh-model").is_some());

    // 放行第一次慢刷新，旧代次结果不得覆盖 fresh-model
    release.add_permits(1);
    slow_refresh.await.unwrap();
    assert!(models.get_model("dyn", "fresh-model").is_some());
    assert!(models.get_model("dyn", "stale-model").is_none());

    // 验证 Copilot 空 availableModelIds 返回空列表
    let copilot = pi_ai::providers::github_copilot::github_copilot_provider();
    let empty_cred = Credential::OAuth(OAuthCredential {
        access: "tok".to_owned(),
        refresh: "ref".to_owned(),
        expires: now_ms() + 60_000,
        extra: BTreeMap::from([("availableModelIds".to_owned(), json!([]))]),
    });
    assert!(
        copilot
            .filter_chat_models(&copilot.get_models(), Some(&empty_cred))
            .is_empty()
    );
}

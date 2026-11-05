use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pi_ai::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

type Requests = Arc<Mutex<Vec<String>>>;

#[test]
fn simple_options_reserve_context_space_before_requesting_output() {
    let fake = faux::create_faux_provider();
    let mut model = fake.model;
    model.context_window = 8_192;
    model.max_tokens = 16_384;
    let context = normalize_context(&Context {
        messages: vec![Message::User(UserMessage::new("x".repeat(4_096)))],
        ..Default::default()
    });

    let options = api::build_base_options(&model, &context, None);
    assert_eq!(options.max_tokens, Some(2_925));

    let requested = SimpleStreamOptions {
        base: StreamOptions {
            max_tokens: Some(2_000),
            ..Default::default()
        },
        ..Default::default()
    };
    let options = api::build_base_options(&model, &context, Some(&requested));
    assert_eq!(options.max_tokens, Some(2_000));
}

async fn mock_json(responses: Vec<(u16, Value)>) -> (String, Requests) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let out = captured.clone();
    tokio::spawn(async move {
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(i) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..i]);
                    let length: usize = header
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= i + 4 + length {
                        break;
                    }
                }
            }
            out.lock()
                .unwrap()
                .push(String::from_utf8(request).unwrap());
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (base, captured)
}

fn oauth_credential(access: &str, expires: u64) -> OAuthCredential {
    OAuthCredential {
        access: access.to_owned(),
        refresh: "refresh-old".to_owned(),
        expires,
        extra: BTreeMap::new(),
    }
}

fn empty_interaction() -> AuthInteraction {
    AuthInteraction::new(
        Arc::new(|_| Box::pin(async { Err("Unexpected prompt".to_owned()) })),
        Arc::new(|_| {}),
    )
}

fn dynamic_provider(fetch: FetchModelsFn) -> Provider {
    create_provider(CreateProviderOptions {
        id: "dynamic".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth::api_key(ApiKeyAuth::keyless("test", "test-key")),
        models: vec![],
        fetch_models: Some(fetch),
        filter_models: None,
        api: Some(ProviderApiConfig::ByApi(default_api_map())),
        classifiers: None,
    })
    .unwrap()
}

#[tokio::test]
async fn custom_login_is_instance_owned_and_released() {
    let original = ApiKeyAuth::from_env("test", "deepseek");
    let marker = Arc::new(());
    let weak = Arc::downgrade(&marker);
    let custom = original.clone().with_login(Arc::new(move |_| {
        let _marker = marker.clone();
        Box::pin(async {
            Ok(ApiKeyCredential {
                key: Some("custom".to_owned()),
                env: None,
            })
        })
    }));
    let interaction = AuthInteraction::new(
        Arc::new(|_| Box::pin(async { Ok("default".to_owned()) })),
        Arc::new(|_| {}),
    );
    assert_eq!(
        original.login(&interaction).await.unwrap().key.as_deref(),
        Some("default")
    );
    assert_eq!(
        custom
            .clone()
            .login(&interaction)
            .await
            .unwrap()
            .key
            .as_deref(),
        Some("custom")
    );
    drop(custom);
    assert!(weak.upgrade().is_none());

    let original = OAuthAuth::bearer("custom provider", false);
    let custom = original.clone().with_login(Arc::new(|_, _| {
        Box::pin(async { Ok(oauth_credential("custom", u64::MAX)) })
    }));
    assert!(
        original
            .login(&interaction, None)
            .await
            .unwrap_err()
            .contains("does not provide")
    );
    assert_eq!(
        custom.login(&interaction, None).await.unwrap().access,
        "custom"
    );
}

#[tokio::test]
async fn arbitrary_oauth_error_codes_never_echo_secrets() {
    for value in [
        json!("secret-sentinel"),
        json!({"code":"secret-sentinel"}),
        json!({"type":"secret-sentinel"}),
    ] {
        let (base, _) = mock_json(vec![(400, json!({"error":value}))]).await;
        let mut cred = oauth_credential("expired", 0);
        cred.extra
            .insert("tokenUrl".to_owned(), json!(format!("{base}/token")));
        let err = (OAuthAuth::openai_codex().refresh)(cred).await.unwrap_err();
        assert!(!err.contains("secret-sentinel"), "{err}");
        assert!(err.contains("400"));
    }
}

#[tokio::test]
async fn cancellation_returns_promptly_but_rotated_token_is_persisted() {
    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .set("test", Credential::OAuth(oauth_credential("expired", 0)))
        .await;
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let entered = started.clone();
    let gate = release.clone();
    let oauth = OAuthAuth::bearer("rotation", false);
    let oauth = OAuthAuth {
        refresh: Arc::new(move |_| {
            let entered = entered.clone();
            let gate = gate.clone();
            Box::pin(async move {
                entered.add_permits(1);
                gate.acquire().await.unwrap().forget();
                Ok(OAuthCredential {
                    refresh: "refresh-rotated".to_owned(),
                    ..oauth_credential("rotated", u64::MAX)
                })
            })
        }),
        ..oauth
    };
    let cancel = CancellationToken::new();
    let task = {
        let store = store.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            refresh_stored_oauth_credential_cancellable(store, "test", &oauth, 0, Some(&cancel))
                .await
        })
    };
    started.acquire().await.unwrap().forget();
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    release.add_permits(1);
    let saved = tokio::time::timeout(
        Duration::from_secs(1),
        store.modify("test", Box::new(|_| Box::pin(async { Ok(None) }))),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let Credential::OAuth(saved) = saved else {
        panic!("OAuth expected");
    };
    assert_eq!(saved.refresh, "refresh-rotated");
}

#[tokio::test]
async fn cancelled_login_does_not_persist_after_waiting_for_store_lock() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let blocker = {
        let store = store.clone();
        let entered = entered.clone();
        let release = release.clone();
        tokio::spawn(async move {
            store
                .modify(
                    "deepseek",
                    Box::new(move |_| {
                        Box::pin(async move {
                            entered.add_permits(1);
                            release.acquire().await.unwrap().forget();
                            Ok(None)
                        })
                    }),
                )
                .await
        })
    };
    entered.acquire().await.unwrap().forget();
    let models = builtin_models(Some(CreateModelsOptions {
        credentials: Some(store.clone()),
        ..Default::default()
    }));
    let prompted = Arc::new(Semaphore::new(0));
    let prompt_signal = prompted.clone();
    let cancel = CancellationToken::new();
    let interaction = AuthInteraction {
        cancel_token: Some(cancel.clone()),
        ..AuthInteraction::new(
            Arc::new(move |_| {
                prompt_signal.add_permits(1);
                Box::pin(async { Ok("new-key".to_owned()) })
            }),
            Arc::new(|_| {}),
        )
    };
    let task = tokio::spawn(async move {
        models
            .login("deepseek", AuthType::ApiKey, &interaction, None)
            .await
    });
    prompted.acquire().await.unwrap().forget();
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    release.add_permits(1);
    blocker.await.unwrap().unwrap();
    assert!(store.read("deepseek").await.unwrap().is_none());
}

#[tokio::test]
async fn replacing_or_removing_a_provider_invalidates_pending_refresh() {
    for operation in ["replace", "delete", "clear"] {
        let store = Arc::new(InMemoryModelsStore::new());
        let models = create_models(Some(CreateModelsOptions {
            models_store: Some(store.clone()),
            ..Default::default()
        }));
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let mut model = builtin_models(None).get_models(Some("deepseek")).remove(0);
        model.provider = "dynamic".to_owned();
        models.set_provider(dynamic_provider(Arc::new({
            let entered = entered.clone();
            let release = release.clone();
            move |_| {
                let entered = entered.clone();
                let release = release.clone();
                let model = model.clone();
                Box::pin(async move {
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    Ok(vec![AnyModel::Chat(model)])
                })
            }
        })));
        let task = {
            let models = models.clone();
            tokio::spawn(async move { models.refresh(None).await })
        };
        entered.acquire().await.unwrap().forget();
        match operation {
            "replace" => models.set_provider(dynamic_provider(Arc::new(|_| {
                Box::pin(async { Ok(vec![]) })
            }))),
            "delete" => models.delete_provider("dynamic"),
            _ => models.clear_providers(),
        }
        release.add_permits(1);
        task.await.unwrap();
        assert!(
            store.read("dynamic").await.unwrap().is_none(),
            "{operation}"
        );
        assert!(models.get_models(Some("dynamic")).is_empty());
    }
}

#[tokio::test]
async fn device_login_uses_each_providers_grant_and_copilot_catalog() {
    for (provider, client_id) in [
        (
            OAuthAuth::kimi_coding(),
            "17e5f671-d194-4dfb-9706-5516cb48c098",
        ),
        (OAuthAuth::xai(), "b1a00492-073a-47ea-816f-4c329264a828"),
        (OAuthAuth::github_copilot(), "Iv1.b507a08c87ecfe98"),
    ] {
        let copilot = provider.name == "GitHub Copilot";
        let mut responses = vec![
            (
                200,
                json!({"device_code":"device-secret","user_code":"DISPLAY","verification_uri":"https://auth.example/verify","expires_in":60,"interval":1}),
            ),
            (
                200,
                json!({"access_token":"granted-access","refresh_token":"granted-refresh","expires_in":3600}),
            ),
        ];
        if copilot {
            responses.extend([
            (200, json!({"token":"copilot-access","expires_at":9999999999u64})),
            (200, json!({"data":[{"id":"gpt-4o","model_picker_enabled":true,"policy":{"state":"enabled"}},{"id":"disabled-model","model_picker_enabled":true,"policy":{"state":"disabled"}}]})),
        ]);
        }
        let (base, requests) = mock_json(responses).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let interaction = AuthInteraction::new(
            Arc::new(|p| {
                Box::pin(async move {
                    match p {
                        AuthPrompt::Text { .. } => Ok(String::new()),
                        _ => Err("Must use device grant".to_owned()),
                    }
                })
            }),
            Arc::new(move |event| sink.lock().unwrap().push(event)),
        );
        let options = LoginOptions {
            endpoints: Some(OAuthEndpoints {
                device_code_url: Some(format!("{base}/device")),
                token_url: Some(format!("{base}/token")),
                copilot_token_url: Some(format!("{base}/copilot-token")),
                copilot_models_url: Some(format!("{base}/models")),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cred = provider.login(&interaction, Some(&options)).await.unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains(client_id));
        assert!(
            requests[1]
                .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code")
        );
        assert!(requests[1].contains("device_code=device-secret"));
        assert!(events.lock().unwrap().iter().any(
            |e| matches!(e, AuthEvent::DeviceCode { user_code, .. } if user_code == "DISPLAY")
        ));
        if copilot {
            assert_eq!(cred.refresh, "granted-access");
            assert_eq!(cred.access, "copilot-access");
            assert_eq!(cred.extra["availableModelIds"], json!(["gpt-4o"]));
            assert!(
                requests[2]
                    .to_lowercase()
                    .contains("authorization: bearer granted-access")
            );
        } else {
            assert_eq!(cred.access, "granted-access");
        }
    }
}

#[tokio::test]
async fn device_login_rejects_unsafe_uri_and_honors_cancellation() {
    let (base, _) = mock_json(vec![(200, json!({"device_code":"secret","user_code":"code","verification_uri":"javascript:alert(1)","expires_in":60}))]).await;
    let options = LoginOptions {
        endpoints: Some(OAuthEndpoints {
            device_code_url: Some(base),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        OAuthAuth::xai()
            .login(&empty_interaction(), Some(&options))
            .await
            .unwrap_err()
            .contains("Untrusted")
    );
    let (base, _) = mock_json(vec![(200, json!({"device_code":"secret","user_code":"code","verification_uri":"https://example.com","expires_in":60}))]).await;
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let interaction = AuthInteraction {
        cancel_token: Some(cancel),
        ..AuthInteraction::new(
            Arc::new(|_| Box::pin(async { Err("Unexpected prompt".to_owned()) })),
            Arc::new(move |event| {
                if matches!(event, AuthEvent::DeviceCode { .. }) {
                    trigger.cancel();
                }
            }),
        )
    };
    let options = LoginOptions {
        endpoints: Some(OAuthEndpoints {
            device_code_url: Some(base),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = tokio::time::timeout(
        Duration::from_secs(1),
        OAuthAuth::kimi_coding().login(&interaction, Some(&options)),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(err.contains("cancelled"));
}

#[tokio::test]
async fn chatgpt_dynamic_registration_and_codex_use_distinct_auth_contracts() {
    // Sequential in one test: both callback flows own port 1455.
    for dynamic in [true, false] {
        let access = if dynamic {
            "direct-access".to_owned()
        } else {
            let payload = URL_SAFE_NO_PAD.encode(
                json!({"https://api.openai.com/auth":{"chatgpt_account_id":"acct-mock"}})
                    .to_string(),
            );
            format!("e30.{payload}.signature")
        };
        let (base, requests) = mock_json(vec![(200, json!({"access_token":access,"refresh_token":"new-refresh","expires_in":3600,"id_token":"id","scope":"openid chatgpt.tokens.use.direct"}))]).await;
        let auth_url = Arc::new(Mutex::new(String::new()));
        let notified = auth_url.clone();
        let prompt_url = auth_url.clone();
        let interaction = AuthInteraction::new(
            Arc::new(move |_| {
                let url = url::Url::parse(&prompt_url.lock().unwrap()).unwrap();
                let state = url
                    .query_pairs()
                    .find(|(k, _)| k == "state")
                    .unwrap()
                    .1
                    .into_owned();
                let redirect = url
                    .query_pairs()
                    .find(|(k, _)| k == "redirect_uri")
                    .unwrap()
                    .1
                    .into_owned();
                Box::pin(async move {
                    Ok(format!(
                        "{redirect}?code=mock-code&state={state}&client_id=issued-client-123"
                    ))
                })
            }),
            Arc::new(move |e| {
                if let AuthEvent::AuthUrl { url, .. } = e {
                    *notified.lock().unwrap() = url;
                }
            }),
        );
        let options = LoginOptions {
            get_device_id: Some(Arc::new(|| {
                "caa1cda5-543d-448d-8e49-37cd30073575".to_owned()
            })),
            agent_name: Some("Test Agent".to_owned()),
            endpoints: Some(OAuthEndpoints {
                token_url: Some(format!("{base}/token")),
                ..Default::default()
            }),
        };
        let provider = if dynamic {
            OAuthAuth::openai_chatgpt()
        } else {
            OAuthAuth::openai_codex()
        };
        let cred = provider.login(&interaction, Some(&options)).await.unwrap();
        let auth = auth_url.lock().unwrap();
        let requests = requests.lock().unwrap();
        if dynamic {
            assert!(auth.contains("/api/accounts/authorize?"));
            assert!(auth.contains("dynamic_agent_client"));
            assert!(auth.contains("chatgpt.tokens.use.direct"));
            assert!(requests[0].contains("client_id=issued-client-123"));
            assert!(requests[0].contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"));
            assert_eq!(cred.extra["clientId"], json!("issued-client-123"));
        } else {
            assert!(auth.contains("codex_cli_simplified_flow=true"));
            assert!(!auth.contains("dynamic_agent_client"));
            assert_eq!(cred.extra["accountId"], json!("acct-mock"));
        }
    }

    // A real loopback callback must win over a pending manual prompt.
    let (base, _) = mock_json(vec![(200, json!({"access_token":"callback-access","refresh_token":"callback-refresh","expires_in":3600,"id_token":"id","scope":"chatgpt.tokens.use.direct"}))]).await;
    let options = LoginOptions {
        get_device_id: Some(Arc::new(|| {
            "caa1cda5-543d-448d-8e49-37cd30073575".to_owned()
        })),
        endpoints: Some(OAuthEndpoints {
            token_url: Some(base),
            ..Default::default()
        }),
        ..Default::default()
    };
    let interaction = AuthInteraction::new(
        Arc::new(|_| Box::pin(std::future::pending())),
        Arc::new(|event| {
            if let AuthEvent::AuthUrl { url, .. } = event {
                let auth = url::Url::parse(&url).unwrap();
                let state = auth
                    .query_pairs()
                    .find(|(k, _)| k == "state")
                    .unwrap()
                    .1
                    .into_owned();
                let redirect = auth
                    .query_pairs()
                    .find(|(k, _)| k == "redirect_uri")
                    .unwrap()
                    .1
                    .into_owned();
                tokio::spawn(async move {
                    let response = reqwest::get(format!(
                        "{redirect}?code=callback-code&state={state}&client_id=callback-client"
                    ))
                    .await
                    .unwrap();
                    assert!(response.status().is_success());
                });
            }
        }),
    );
    let provider = OAuthAuth::openai_chatgpt();
    let cred = tokio::time::timeout(
        Duration::from_secs(2),
        provider.login(&interaction, Some(&options)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cred.extra["clientId"], json!("callback-client"));

    // Only a denial with matching state ends the login and releases the listener.
    let denial = AuthInteraction::new(
        Arc::new(|_| Box::pin(std::future::pending())),
        Arc::new(|event| {
            if let AuthEvent::AuthUrl { url, .. } = event {
                let auth = url::Url::parse(&url).unwrap();
                let state = auth
                    .query_pairs()
                    .find(|(k, _)| k == "state")
                    .unwrap()
                    .1
                    .into_owned();
                let redirect = auth
                    .query_pairs()
                    .find(|(k, _)| k == "redirect_uri")
                    .unwrap()
                    .1
                    .into_owned();
                tokio::spawn(async move {
                    let client = reqwest::Client::new();
                    assert_eq!(
                        client
                            .get(format!("{redirect}?error=access_denied&state=wrong-state"))
                            .send()
                            .await
                            .unwrap()
                            .status()
                            .as_u16(),
                        400
                    );
                    let _ = client
                        .get(format!("{redirect}?error=access_denied&state={state}"))
                        .send()
                        .await;
                });
            }
        }),
    );
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        provider.login(&denial, Some(&options)),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("authorization denied"));

    // Cancelled browser waits must release the callback port.
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let interaction = AuthInteraction {
        cancel_token: Some(cancel),
        ..AuthInteraction::new(
            Arc::new(|_| Box::pin(std::future::pending())),
            Arc::new(move |e| {
                if matches!(e, AuthEvent::AuthUrl { .. }) {
                    trigger.cancel();
                }
            }),
        )
    };
    assert!(
        provider
            .login(&interaction, Some(&options))
            .await
            .unwrap_err()
            .contains("cancelled")
    );
    let _listener = TcpListener::bind("127.0.0.1:1455")
        .await
        .expect("cancelled login must release callback port");
}

struct SlowModelsStore {
    inner: InMemoryModelsStore,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

impl ModelsStore for SlowModelsStore {
    fn read<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, String>> {
        self.inner.read(id)
    }
    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        self.inner.delete(id)
    }
    fn write<'a>(
        &'a self,
        id: &'a str,
        entry: ModelsStoreEntry,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            self.inner.write(id, entry).await
        })
    }
}

#[tokio::test]
async fn replacement_during_cache_write_cannot_restore_stale_models() {
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let store = Arc::new(SlowModelsStore {
        inner: InMemoryModelsStore::new(),
        entered: entered.clone(),
        release: release.clone(),
    });
    let models = create_models(Some(CreateModelsOptions {
        models_store: Some(store.clone()),
        ..Default::default()
    }));
    let mut stale = builtin_models(None).get_models(Some("deepseek")).remove(0);
    stale.provider = "dynamic".to_owned();
    models.set_provider(dynamic_provider(Arc::new(move |_| {
        let stale = stale.clone();
        Box::pin(async move { Ok(vec![AnyModel::Chat(stale)]) })
    })));
    let old = {
        let models = models.clone();
        tokio::spawn(async move { models.refresh(None).await })
    };
    entered.acquire().await.unwrap().forget();
    models.set_provider(dynamic_provider(Arc::new(|_| {
        Box::pin(async { Ok(vec![]) })
    })));
    let offline = {
        let models = models.clone();
        tokio::spawn(async move {
            models
                .refresh(Some(ModelsRefreshOptions {
                    allow_network: Some(false),
                    ..Default::default()
                }))
                .await
        })
    };
    release.add_permits(1);
    old.await.unwrap();
    offline.await.unwrap();
    assert!(store.read("dynamic").await.unwrap().is_none());
    assert!(models.get_models(Some("dynamic")).is_empty());
}

#[test]
fn google_uses_json_schema_and_only_rewrites_schema_consts() {
    let tools = vec![Tool {
        name: "pick".to_owned(),
        description: "pick".to_owned(),
        parameters: json!({
            "type":"object", "properties": {"const":{"const":"literal"},"choice":{"oneOf":[{"const":"a"},{"$ref":"#/$defs/B"}]}},
            "$defs":{"B":{"const":2}}, "examples":[{"const":"keep-me"}], "default":{"const":"also-keep"}
        }),
        constrained_sampling: None,
    }];
    let wire = api::google_generative_ai::convert_google_tools(&tools);
    let declaration = &wire[0]["functionDeclarations"][0];
    assert!(declaration.get("parameters").is_none());
    let schema = &declaration["parametersJsonSchema"];
    assert_eq!(schema["properties"]["const"]["enum"], json!(["literal"]));
    assert_eq!(
        schema["properties"]["choice"]["oneOf"][0]["enum"],
        json!(["a"])
    );
    assert_eq!(schema["$defs"]["B"]["enum"], json!([2]));
    assert_eq!(schema["examples"][0]["const"], json!("keep-me"));
    assert_eq!(schema["default"]["const"], json!("also-keep"));
}

#[test]
fn foreign_responses_tool_item_ids_are_hashed_without_collision() {
    let target = builtin_models(None).get_models(Some("openai")).remove(0);
    let mut source = builtin_models(None).get_models(Some("deepseek")).remove(0);
    source.api = known_api::OPENAI_RESPONSES.to_owned();
    let mut message = AssistantMessage::new_pending(&source);
    message.stop_reason = StopReason::ToolUse;
    for i in [1, 2] {
        message.content.push(ContentBlock::ToolCall(ToolCall {
            id: format!("call_{i}|{}{i}", "A".repeat(70)),
            name: "test".to_owned(),
            arguments: Default::default(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }));
    }
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::Assistant(message)],
        tools: None,
    });
    let wire = api::openai_responses::convert_responses_messages(&target, &context);
    let ids: Vec<&str> = wire
        .iter()
        .filter(|m| m["type"] == "function_call")
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(ids.iter().all(|id| id.starts_with("fc_") && id.len() <= 64));
}

#[tokio::test]
async fn terminal_responses_event_completes_without_waiting_for_http_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let release = Arc::new(Semaphore::new(0));
    let gate = release.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        let event = "data: {\"type\":\"response.done\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":1,\"total_tokens\":5}}}\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n",
            event.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        gate.acquire().await.unwrap().forget();
    });
    let mut model = builtin_models(None)
        .get_models(Some("openai-codex"))
        .remove(0);
    model.base_url = format!("http://{addr}");
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new("hello"))],
        tools: None,
    });
    let options = StreamOptions {
        api_key: Some("mock-key".to_owned()),
        ..Default::default()
    };
    let stream = api::openai_responses::stream(&model, &context, Some(&options));
    let result = tokio::time::timeout(Duration::from_secs(2), stream.result()).await;
    release.add_permits(1);
    server.await.unwrap();
    let result = result.expect("terminal event must finish before EOF");
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.usage.total_tokens, 5);
}

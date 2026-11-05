use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use pi_ai::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn provider(fetch: FetchModelsFn) -> Provider {
    create_provider(CreateProviderOptions {
        id: "dynamic".into(),
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
async fn superseding_refresh_releases_stalled_fetch() {
    let models = Models::default();
    let started = Arc::new(Semaphore::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let calls = AtomicUsize::new(0);
    models.set_provider(provider(Arc::new({
        let started = started.clone();
        let dropped = dropped.clone();
        move |_| {
            let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
            let started = started.clone();
            let dropped = dropped.clone();
            Box::pin(async move {
                if first {
                    let _guard = Dropped(dropped);
                    started.add_permits(1);
                    std::future::pending::<()>().await;
                }
                Ok(vec![])
            })
        }
    })));
    let mut old = tokio::spawn({
        let models = models.clone();
        async move { models.refresh(None).await }
    });
    started.acquire().await.unwrap().forget();
    let fresh = models.refresh(None).await;
    assert!(fresh.errors.is_empty());
    let result = tokio::time::timeout(Duration::from_secs(1), &mut old).await;
    old.abort();
    assert!(
        result.is_ok(),
        "superseded refresh is still waiting for fetch"
    );
    assert!(result.unwrap().unwrap().errors.is_empty());
    assert!(dropped.load(Ordering::SeqCst));
}

struct StalledReadStore {
    calls: AtomicUsize,
    started: Semaphore,
    dropped: Arc<AtomicBool>,
}
impl ModelsStore for StalledReadStore {
    fn read<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, String>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let _guard = Dropped(self.dropped.clone());
                self.started.add_permits(1);
                std::future::pending::<()>().await;
            }
            Ok(None)
        })
    }
    fn write<'a>(&'a self, _: &'a str, _: ModelsStoreEntry) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn delete<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn superseding_refresh_releases_stalled_cache_read_and_publication_lock() {
    let store = Arc::new(StalledReadStore {
        calls: AtomicUsize::new(0),
        started: Semaphore::new(0),
        dropped: Arc::new(AtomicBool::new(false)),
    });
    let models = Models::new(Some(CreateModelsOptions {
        models_store: Some(store.clone()),
        ..Default::default()
    }));
    models.set_provider(provider(Arc::new(|_| Box::pin(async { Ok(vec![]) }))));
    let mut old = tokio::spawn({
        let models = models.clone();
        async move { models.refresh(None).await }
    });
    store.started.acquire().await.unwrap().forget();
    let fresh = tokio::time::timeout(Duration::from_secs(1), models.refresh(None)).await;
    old.abort();
    assert!(
        fresh.is_ok(),
        "new refresh is blocked by the old cache read"
    );
    assert!(fresh.unwrap().errors.is_empty());
    let _ = (&mut old).await;
    assert!(store.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn provider_changes_release_stalled_refresh() {
    for operation in ["replace", "delete", "clear"] {
        let models = Models::default();
        let started = Arc::new(Semaphore::new(0));
        models.set_provider(provider(Arc::new({
            let started = started.clone();
            move |_| {
                let started = started.clone();
                Box::pin(async move {
                    started.add_permits(1);
                    std::future::pending().await
                })
            }
        })));
        let mut task = tokio::spawn({
            let models = models.clone();
            async move { models.refresh(None).await }
        });
        started.acquire().await.unwrap().forget();
        match operation {
            "replace" => {
                models.set_provider(provider(Arc::new(|_| Box::pin(async { Ok(vec![]) }))))
            }
            "delete" => models.delete_provider("dynamic"),
            _ => models.clear_providers(),
        }
        let result = tokio::time::timeout(Duration::from_secs(1), &mut task).await;
        task.abort();
        assert!(result.is_ok(), "{operation} left refresh running");
        assert!(result.unwrap().unwrap().errors.is_empty());
    }
}

#[tokio::test]
async fn superseding_catalog_refresh_preserves_rotated_oauth_credential() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let credential = OAuthCredential {
        access: "expired".into(),
        refresh: "old-refresh".into(),
        expires: 0,
        extra: Default::default(),
    };
    credentials
        .set("dynamic", Credential::OAuth(credential))
        .await;
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut oauth = OAuthAuth::bearer("test", false);
    oauth.refresh = Arc::new({
        let started = started.clone();
        let release = release.clone();
        let calls = calls.clone();
        move |mut credential| {
            let started = started.clone();
            let release = release.clone();
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                started.add_permits(1);
                release.acquire().await.unwrap().forget();
                credential.access = "rotated-access".into();
                credential.refresh = "rotated-refresh".into();
                credential.expires = u64::MAX;
                Ok(credential)
            })
        }
    });
    let models = Models::new(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    let mut options = provider(Arc::new(|input| Box::pin(async move {
        assert!(matches!(input.credential, Some(Credential::OAuth(ref c)) if c.access == "rotated-access"));
        Ok(vec![])
    }))).to_create_options();
    options.auth = ProviderAuth {
        oauth: Some(oauth),
        api_key: None,
    };
    models.set_provider(create_provider(options).unwrap());
    let mut old = tokio::spawn({
        let models = models.clone();
        async move { models.refresh(None).await }
    });
    started.acquire().await.unwrap().forget();
    let fresh = tokio::spawn({
        let models = models.clone();
        async move { models.refresh(None).await }
    });
    let result = tokio::time::timeout(Duration::from_secs(1), &mut old).await;
    old.abort();
    release.add_permits(1);
    assert!(
        result.is_ok(),
        "catalog refresh still waits for OAuth rotation"
    );
    assert!(result.unwrap().unwrap().errors.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), fresh)
            .await
            .unwrap()
            .unwrap()
            .errors
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let saved = credentials.read("dynamic").await.unwrap().unwrap();
    assert!(matches!(saved, Credential::OAuth(c) if c.refresh == "rotated-refresh"));
}

#[tokio::test]
async fn faux_callback_panics_end_stream_with_error() {
    for hook in ["on_payload", "on_response"] {
        let models = Models::default();
        let model = faux::register_faux_provider(&models).model;
        let mut options = StreamOptions::default();
        if hook == "on_payload" {
            options.on_payload = Some(Arc::new(|_, _| Box::pin(async { panic!("payload panic") })));
        } else {
            options.on_response = Some(Arc::new(|_, _| panic!("response panic")));
        }
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            models.complete(&model, &Context::default(), Some(&options)),
        )
        .await
        .unwrap();
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(
            result.error_message.as_deref(),
            Some(format!("{hook} callback panicked").as_str())
        );
    }
}

async fn sse_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&buf[..n]);
            if let Some(i) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..i]);
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= i + 4 + length {
                    break;
                }
            }
        }
        let body = "data: {}\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    (url, task)
}

#[tokio::test]
async fn provider_callback_panics_end_stream_with_error() {
    for api_name in [
        known_api::OPENAI_COMPLETIONS,
        known_api::OPENAI_RESPONSES,
        known_api::ANTHROPIC_MESSAGES,
        known_api::GOOGLE_GENERATIVE_AI,
    ] {
        for hook in [
            "on_headers",
            "on_payload",
            "on_response",
            "on_provider_stream_event",
        ] {
            for before_future in [true, false] {
                let (url, server) = sse_server().await;
                let mut model = faux::create_faux_provider().model;
                model.api = api_name.into();
                model.base_url = url;
                let mut options = StreamOptions {
                    api_key: Some("test".into()),
                    ..Default::default()
                };
                match hook {
                    "on_headers" => {
                        options.on_headers = Some(Arc::new(move |_, _| {
                            assert!(!before_future, "synchronous callback panic");
                            Box::pin(async { panic!("async callback panic") })
                        }))
                    }
                    "on_payload" => {
                        options.on_payload = Some(Arc::new(move |_, _| {
                            assert!(!before_future, "synchronous callback panic");
                            Box::pin(async { panic!("async callback panic") })
                        }))
                    }
                    "on_response" => {
                        options.on_response = Some(Arc::new(move |_, _| {
                            assert!(!before_future, "synchronous callback panic");
                            Box::pin(async { panic!("async callback panic") })
                        }))
                    }
                    _ => {
                        options.on_provider_stream_event = Some(Arc::new(move |_, _| {
                            assert!(!before_future, "synchronous callback panic");
                            Box::pin(async { panic!("async callback panic") })
                        }))
                    }
                }
                let context = TranscriptContext::default();
                let stream = match api_name {
                    known_api::OPENAI_COMPLETIONS => {
                        api::openai_completions::stream(&model, &context, Some(&options))
                    }
                    known_api::OPENAI_RESPONSES => {
                        api::openai_responses::stream(&model, &context, Some(&options))
                    }
                    known_api::ANTHROPIC_MESSAGES => {
                        api::anthropic_messages::stream(&model, &context, Some(&options))
                    }
                    _ => api::google_generative_ai::stream(&model, &context, Some(&options)),
                };
                let result = tokio::time::timeout(Duration::from_secs(2), stream.result()).await;
                server.abort();
                let result =
                    result.unwrap_or_else(|_| panic!("{api_name} {hook} left stream unfinished"));
                assert_eq!(result.stop_reason, StopReason::Error);
                assert!(
                    result.error_message.as_deref().unwrap().contains(hook),
                    "{result:?}"
                );
                assert!(result.error_message.unwrap().contains("panicked"));
            }
        }
    }
}

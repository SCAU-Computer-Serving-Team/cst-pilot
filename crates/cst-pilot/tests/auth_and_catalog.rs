use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_pilot::auth::{
    AuthRuntime, Channel,
    oa::{Clock, OaClient, requires_login},
};
use cst_pilot::tools::ssrf::{HttpRequest, HttpResponse, HttpTransport};
use pi_ai::{AuthEvent, AuthInteraction, AuthPrompt, AuthType, BoxFuture, OAuthCredential};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cst-auth-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("agent/home")).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join("agent/home").join(name)
    }
    fn json(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.file(name)).unwrap()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct TestClock {
    now: AtomicU64,
    sleeps: Mutex<Vec<u64>>,
}
impl Default for TestClock {
    fn default() -> Self {
        Self {
            now: AtomicU64::new(pi_ai::now_ms()),
            sleeps: Mutex::new(vec![]),
        }
    }
}
impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
    fn sleep(
        &self,
        duration: Duration,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err("Login cancelled".into());
            }
            let ms = duration.as_millis() as u64;
            self.sleeps.lock().unwrap().push(ms);
            self.now.fetch_add(ms, Ordering::SeqCst);
            Ok(())
        })
    }
}
#[derive(Default)]
struct MockHttp {
    replies: Mutex<VecDeque<(u16, Value)>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl MockHttp {
    fn reply(&self, status: u16, body: Value) {
        self.replies.lock().unwrap().push_back((status, body));
    }
}
impl HttpTransport for MockHttp {
    fn request(
        &self,
        request: HttpRequest,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>> {
        self.requests.lock().unwrap().push(request);
        let next = self.replies.lock().unwrap().pop_front();
        Box::pin(async move {
            let (status, value) = next.ok_or("test response not configured")?;
            Ok(HttpResponse {
                status,
                body: value.to_string(),
            })
        })
    }
}
fn client(http: Arc<MockHttp>, clock: Arc<TestClock>) -> OaClient {
    OaClient::with_transport("https://oa.example", http, clock).unwrap()
}
fn interaction(key: &str, notifications: Arc<Mutex<Vec<AuthEvent>>>) -> AuthInteraction {
    let key = key.to_owned();
    AuthInteraction::new(
        Arc::new(move |prompt| {
            assert!(matches!(prompt, AuthPrompt::Secret { .. }));
            let key = key.clone();
            Box::pin(async move { Ok(key) })
        }),
        Arc::new(move |event| notifications.lock().unwrap().push(event)),
    )
}
fn profile() -> Value {
    json!({"result":true,"data":{"memberId":"202601","name":"测试队员","balanceAvailable":true,"balance":0}})
}
fn oauth() -> Value {
    json!({"access_token":"oauth-test-access","refresh_token":"oauth-test-refresh","expires_in":86400})
}
fn device() -> Value {
    json!({"device_code":"private-device-code","user_code":"CODE12","verification_uri_complete":"https://oa.example/oauth/device?user_code=CODE12","interval":2,"expires_in":300})
}

#[tokio::test]
async fn api_key_login_preserves_other_services_and_official_specs_without_remote_overrides() {
    let fixture = Fixture::new();
    fs::write(
        fixture.file("auth.json"),
        json!({"deepseek":{"type":"api_key","key":"existing-test-key"}}).to_string(),
    )
    .unwrap();
    let http = Arc::new(MockHttp::default());
    let clock = Arc::new(TestClock::default());
    http.reply(200, profile());
    http.reply(200, json!({"data":[{"id":"deepseek-flash","baseUrl":"https://evil.example","headers":{"Authorization":"evil"}},{"id":"MiniMax-M3.1-Flash"},{"id":"deepseek-v4.1-flash"},{"id":"new-model"}]}));
    let auth = AuthRuntime::create(&fixture.0, Channel::Tui, client(http.clone(), clock))
        .await
        .unwrap();
    auth.login(
        AuthType::ApiKey,
        interaction("  test-api-key  ", Arc::default()),
    )
    .await
    .unwrap();
    let models = auth
        .runtime()
        .get_available_models()
        .into_iter()
        .filter(|m| m.provider == "cstoa")
        .collect::<Vec<_>>();
    assert_eq!(models.len(), 4);
    for id in [
        "deepseek-flash",
        "MiniMax-M3.1-Flash",
        "deepseek-v4.1-flash",
    ] {
        let model = models.iter().find(|m| m.id == id).unwrap();
        let metadata_id = if id == "deepseek-v4.1-flash" {
            "deepseek-flash"
        } else {
            id
        };
        let official = pi_ai::builtin_providers()
            .into_iter()
            .flat_map(|p| p.get_models())
            .find(|m| m.id == metadata_id)
            .unwrap();
        assert_eq!(model.context_window, official.context_window);
        assert_eq!(model.max_tokens, official.max_tokens);
        assert_eq!(model.input, official.input);
        assert_eq!(model.input_limits, official.input_limits);
        assert_eq!(model.reasoning, official.reasoning);
        assert_eq!(model.api, "openai-completions");
        assert_eq!(model.base_url, "https://oa.example/v1");
        assert!(model.headers.is_none());
        assert_eq!(model.cost.input, 0.0);
        // Non-completions compatibility flags must never be sent to the gateway.
        assert_eq!(model.compat.as_ref().unwrap().force_adaptive_thinking, None);
    }
    assert_eq!(
        models
            .iter()
            .find(|m| m.id == "new-model")
            .unwrap()
            .context_window,
        32768
    );
    let stored = fixture.json("models-store.json");
    assert_eq!(stored["cstoa"]["etag"].as_str().unwrap().len(), 64);
    assert!(!stored.to_string().contains("test-api-key"));
    assert_eq!(
        fixture.json("auth.json")["deepseek"]["key"],
        "existing-test-key"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    assert!(!auth.refresh(false).await.errors.contains_key("cstoa"));
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    http.reply(200, json!({"data":[]}));
    assert!(!auth.refresh(true).await.errors.contains_key("cstoa"));
    assert!(
        auth.runtime()
            .get_all_models()
            .iter()
            .all(|m| m.provider != "cstoa")
    );
}

#[tokio::test]
async fn device_login_uses_full_link_rfc_poll_backoff_and_persistent_portable_device_id() {
    let fixture = Fixture::new();
    let http = Arc::new(MockHttp::default());
    let clock = Arc::new(TestClock::default());
    http.reply(200, device());
    http.reply(400, json!({"error":"authorization_pending"}));
    http.reply(400, json!({"error":"slow_down"}));
    http.reply(200, oauth());
    http.reply(200, json!({"data":[{"id":"deepseek-flash"}]}));
    let auth = AuthRuntime::create(
        &fixture.0,
        Channel::Tui,
        client(http.clone(), clock.clone()),
    )
    .await
    .unwrap();
    let notifications = Arc::new(Mutex::new(vec![]));
    auth.login(
        AuthType::OAuth,
        interaction("unused", notifications.clone()),
    )
    .await
    .unwrap();
    assert_eq!(*clock.sleeps.lock().unwrap(), [2000, 2000, 7000]);
    let events = notifications.lock().unwrap().clone();
    assert!(
        matches!(&events[0], AuthEvent::Info { links:Some(links),.. } if links[0].url.ends_with("user_code=CODE12"))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AuthEvent::AuthUrl { .. } | AuthEvent::DeviceCode { .. }))
    );
    let id = fixture.json("device.json")["device_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(id.starts_with("dev_"));
    {
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests[0].body.as_ref().unwrap()["device_id"], id);
        assert!(
            requests
                .last()
                .unwrap()
                .url
                .ends_with("/api/agent/llm/v1/models")
        );
    }
    let stored = fixture.json("auth.json");
    assert_eq!(stored["cstoa"]["refresh"], "oauth-test-refresh");
    drop(auth);
    http.reply(200, device());
    http.reply(200, oauth());
    http.reply(200, json!({"data":[]}));
    let next = AuthRuntime::create(&fixture.0, Channel::Gui, client(http.clone(), clock))
        .await
        .unwrap();
    let notifications = Arc::new(Mutex::new(vec![]));
    next.login(
        AuthType::OAuth,
        interaction("unused", notifications.clone()),
    )
    .await
    .unwrap();
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests[5].body.as_ref().unwrap()["device_id"], id);
    assert!(
        matches!(&notifications.lock().unwrap()[0],AuthEvent::DeviceCode { user_code,open_browser_on_confirm:Some(false),.. } if user_code.is_empty())
    );
}

#[tokio::test]
async fn oauth_refresh_distinguishes_expiry_revocation_temporary_failure_and_cancellation() {
    let http = Arc::new(MockHttp::default());
    let clock = Arc::new(TestClock::default());
    clock.now.store(1000, Ordering::SeqCst);
    let client = client(http.clone(), clock.clone());
    let credential = OAuthCredential {
        access: "test-access".into(),
        refresh: String::new(),
        expires: 1001,
        extra: Default::default(),
    };
    assert_eq!(
        client
            .refresh(credential.clone(), CancellationToken::new())
            .await
            .unwrap(),
        credential
    );
    clock.now.store(1002, Ordering::SeqCst);
    assert!(requires_login(
        &client
            .refresh(credential.clone(), CancellationToken::new())
            .await
            .unwrap_err()
    ));
    let mut credential = credential;
    credential.expires = 10000;
    credential.refresh = "test-refresh".into();
    http.reply(503, json!({"error":"arbitrary-secret-should-not-escape"}));
    assert_eq!(
        client
            .refresh(credential.clone(), CancellationToken::new())
            .await
            .unwrap(),
        credential
    );
    http.reply(401, json!({"error":"private-secret"}));
    assert!(requires_login(
        &client
            .refresh(credential.clone(), CancellationToken::new())
            .await
            .unwrap_err()
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        client.refresh(credential, cancel).await.unwrap_err(),
        "Login cancelled"
    );
}

#[tokio::test]
async fn invalid_keys_network_failures_and_cancelled_login_preserve_prior_credentials() {
    let fixture = Fixture::new();
    let old = json!({"cstoa":{"type":"api_key","key":"previous-key"}});
    fs::write(fixture.file("auth.json"), old.to_string()).unwrap();
    let http = Arc::new(MockHttp::default());
    let auth = AuthRuntime::create(
        &fixture.0,
        Channel::Tui,
        client(http.clone(), Arc::default()),
    )
    .await
    .unwrap();
    for (status, body, required) in [
        (403, json!({"token":"raw-secret"}), true),
        (200, json!({"result":false,"code":19008}), true),
        (500, json!({"error":"private"}), false),
        (200, json!({"unexpected":"private"}), false),
    ] {
        http.reply(status, body);
        let error = auth
            .login(AuthType::ApiKey, interaction("new-key", Arc::default()))
            .await
            .unwrap_err();
        assert_eq!(requires_login(&error), required);
        assert!(!error.contains("private"));
        assert!(!error.contains("raw-secret"));
        assert_eq!(fixture.json("auth.json"), old);
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut ui = interaction("new-key", Arc::default());
    ui.cancel_token = Some(cancel);
    assert!(auth.login(AuthType::ApiKey, ui).await.is_err());
    assert_eq!(fixture.json("auth.json"), old);
}

#[tokio::test]
async fn cache_restores_only_matching_credentials_retains_on_failure_and_retries_after_30_seconds()
{
    let fixture = Fixture::new();
    let http = Arc::new(MockHttp::default());
    let clock = Arc::new(TestClock::default());
    http.reply(200, profile());
    http.reply(200, json!({"data":[{"id":"deepseek-flash"}]}));
    let auth = AuthRuntime::create(
        &fixture.0,
        Channel::Tui,
        client(http.clone(), clock.clone()),
    )
    .await
    .unwrap();
    auth.login(AuthType::ApiKey, interaction("test-key-a", Arc::default()))
        .await
        .unwrap();
    let baseline = fixture.json("models-store.json");
    drop(auth);
    let auth = AuthRuntime::create(
        &fixture.0,
        Channel::Tui,
        client(http.clone(), clock.clone()),
    )
    .await
    .unwrap();
    assert!(
        auth.runtime()
            .get_available_models()
            .iter()
            .any(|m| m.provider == "cstoa")
    );
    clock.now.fetch_add(301000, Ordering::SeqCst);
    http.reply(503, json!({"raw":"secret"}));
    assert!(auth.refresh(false).await.errors.contains_key("cstoa"));
    assert_eq!(fixture.json("models-store.json"), baseline);
    assert!(
        auth.runtime()
            .get_available_models()
            .iter()
            .any(|m| m.provider == "cstoa")
    );
    let count = http.requests.lock().unwrap().len();
    let _ = auth.refresh(false).await;
    assert_eq!(http.requests.lock().unwrap().len(), count);
    clock.now.fetch_add(30001, Ordering::SeqCst);
    http.reply(200, json!({"data":[]}));
    assert!(auth.refresh(false).await.errors.is_empty());
    auth.runtime()
        .set_runtime_api_key("cstoa", "test-key-b")
        .await
        .unwrap();
    assert!(
        auth.runtime()
            .get_available_models()
            .iter()
            .all(|m| m.provider != "cstoa")
    );
    auth.logout("cstoa").await.unwrap();
    // Runtime keys intentionally outlive logout until their trusted host removes them.
    auth.runtime()
        .remove_runtime_api_key("cstoa")
        .await
        .unwrap();
    assert!(
        auth.runtime()
            .get_available_models()
            .iter()
            .all(|m| m.provider != "cstoa")
    );
}

#[tokio::test]
async fn damaged_catalog_is_preserved_then_rebuilt_after_successful_refresh() {
    for damaged in [b"{invalid-cache".as_slice(), b"[]", &[0xff]] {
        let fixture = Fixture::new();
        fs::write(
            fixture.file("auth.json"),
            json!({"cstoa":{"type":"api_key","key":"test-cache-key"}}).to_string(),
        )
        .unwrap();
        fs::write(fixture.file("models-store.json"), damaged).unwrap();
        let http = Arc::new(MockHttp::default());
        http.reply(200, json!({"data":[{"id":"deepseek-flash"}]}));
        let auth = AuthRuntime::create(&fixture.0, Channel::Tui, client(http, Arc::default()))
            .await
            .unwrap();
        assert_eq!(
            fs::read(fixture.file("models-store.json")).unwrap(),
            damaged
        );
        let result = auth.refresh(true).await;
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            fixture.json("models-store.json")["cstoa"]["models"][0]["id"],
            "deepseek-flash"
        );
        assert!(
            fs::read_dir(fixture.file(""))
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("models-store.corrupt-")
                        && fs::read(entry.path()).unwrap() == damaged
                })
        );
        auth.logout("cstoa").await.unwrap();
        assert!(fixture.json("auth.json").get("cstoa").is_none());
    }
}

#[tokio::test]
async fn damaged_catalog_does_not_turn_successful_logout_into_an_error() {
    let fixture = Fixture::new();
    fs::write(fixture.file("auth.json"), json!({"cstoa":{"type":"api_key","key":"test-cache-key"},"deepseek":{"type":"api_key","key":"other-test-key"}}).to_string()).unwrap();
    fs::write(fixture.file("models-store.json"), b"{invalid-cache").unwrap();
    let auth = AuthRuntime::create(
        &fixture.0,
        Channel::Tui,
        client(Arc::default(), Arc::default()),
    )
    .await
    .unwrap();
    auth.logout("cstoa").await.unwrap();
    assert!(fixture.json("auth.json").get("cstoa").is_none());
    assert!(fixture.json("auth.json").get("deepseek").is_some());
    assert!(fixture.json("models-store.json").get("cstoa").is_none());
}

#[tokio::test]
async fn malformed_device_auth_and_catalog_files_are_preserved_without_secret_error_text() {
    for name in ["auth.json", "models.json"] {
        let fixture = Fixture::new();
        let original = b"{invalid-private-secret";
        fs::write(fixture.file(name), original).unwrap();
        let result = AuthRuntime::create(
            &fixture.0,
            Channel::Tui,
            client(Arc::default(), Arc::default()),
        )
        .await;
        let error = result.err().expect("damaged file must fail");
        assert!(!error.contains("private-secret"));
        assert_eq!(fs::read(fixture.file(name)).unwrap(), original);
    }
    let fixture = Fixture::new();
    fs::write(
        fixture.file("models-store.json"),
        b"{invalid-private-secret",
    )
    .unwrap();
    let recovered = AuthRuntime::create(
        &fixture.0,
        Channel::Tui,
        client(Arc::default(), Arc::default()),
    )
    .await
    .unwrap();
    assert!(
        recovered
            .runtime()
            .get_all_models()
            .iter()
            .any(|m| m.provider == "deepseek")
    );
    assert_eq!(
        fs::read(fixture.file("models-store.json")).unwrap(),
        b"{invalid-private-secret"
    );
    let fixture = Fixture::new();
    let broken = json!({"device_id":"invalid id"}).to_string();
    fs::write(fixture.file("device.json"), &broken).unwrap();
    let auth = AuthRuntime::create(
        &fixture.0,
        Channel::Tui,
        client(Arc::default(), Arc::default()),
    )
    .await
    .unwrap();
    assert!(
        auth.login(AuthType::OAuth, interaction("unused", Arc::default()))
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.file("device.json")).unwrap(),
        broken
    );
}

#[tokio::test]
async fn real_http_client_rejects_redirects_and_runs_device_grant_against_loopback_mock_oa() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let server_hits = hits.clone();
    let origin = host.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 65536];
            let n = socket.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]);
            let first = request.lines().next().unwrap_or("");
            server_hits.fetch_add(1, Ordering::SeqCst);
            let (status, body, location) = if first.contains("device_authorization") {
                (
                    200,
                    json!({"device_code":"local-code","user_code":"CODE12","verification_uri_complete":format!("{origin}/oauth/device?user_code=CODE12"),"interval":1,"expires_in":30}),
                    "",
                )
            } else if first.contains("/api/oauth/token") {
                (200, oauth(), "")
            } else if first.contains("/models") {
                (200, json!({"data":[]}), "")
            } else {
                (302, Value::Null, "Location: http://127.0.0.1:1/private\r\n")
            };
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} OK\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let fixture = Fixture::new();
    let auth = AuthRuntime::create(&fixture.0, Channel::Gui, OaClient::new(&host).unwrap())
        .await
        .unwrap();
    auth.login(AuthType::OAuth, interaction("unused", Arc::default()))
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    let error = auth
        .login(AuthType::ApiKey, interaction("test-key", Arc::default()))
        .await
        .unwrap_err();
    assert!(error.contains("重定向"));
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    server.abort();
    let _ = server.await;
}

#[test]
fn service_addresses_reject_insecure_nonlocal_credentials_query_fragment_and_host_paths() {
    for host in [
        "http://oa.example",
        "https://user:password@oa.example",
        "https://oa.example?token=x",
        "https://oa.example/#frag",
        "https://oa.example/path",
    ] {
        assert!(OaClient::new(host).is_err());
    }
}

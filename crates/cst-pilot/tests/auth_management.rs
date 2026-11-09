use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_pilot::auth::{
    AuthRuntime, Channel,
    custom_provider::CustomProviderInput,
    oa::{Clock, OaClient},
};
use cst_pilot::tools::ssrf::{HttpRequest, HttpResponse, HttpTransport};
use cst_pilot::{Kit, SKILL_NAMES};
use pi_ai::{ApiKeyCredential, BoxFuture, Credential, CredentialStore};
use pi_runtime::CreateAgentSessionOptions;
use pi_session::SessionManager;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("cst-management-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("agent/home")).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join("agent/home").join(name)
    }
    fn write(&self, name: &str, value: Value) {
        fs::write(self.file(name), value.to_string()).unwrap();
    }
    fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.file(name)).unwrap()).unwrap()
    }
    fn resources(&self) {
        fs::write(self.file("APPEND_SYSTEM.md"), "固定诊断规则。").unwrap();
        for name in SKILL_NAMES {
            let dir = self.file(&format!("skills/{name}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: 指南\n---\n诊断规则。"),
            )
            .unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct TestClock(AtomicU64);
impl Default for TestClock {
    fn default() -> Self {
        Self(AtomicU64::new(pi_ai::now_ms()))
    }
}
impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn sleep(&self, duration: Duration, _: CancellationToken) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0
                .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
            Ok(())
        })
    }
}
type QueryGate = (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
#[derive(Default)]
struct MockHttp {
    replies: Mutex<VecDeque<(u16, Value)>>,
    requests: Mutex<Vec<HttpRequest>>,
    gate: Mutex<Option<QueryGate>>,
    active: AtomicUsize,
}
impl MockHttp {
    fn reply(&self, status: u16, body: Value) {
        self.replies.lock().unwrap().push_back((status, body));
    }
    fn block_next(&self) -> QueryGate {
        let pair = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        *self.gate.lock().unwrap() = Some(pair.clone());
        pair
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl HttpTransport for MockHttp {
    fn request(
        &self,
        request: HttpRequest,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>> {
        self.requests.lock().unwrap().push(request);
        let gate = self.gate.lock().unwrap().take();
        let reply = self.replies.lock().unwrap().pop_front();
        Box::pin(async move {
            struct Active<'a>(&'a AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(&self.active);
            if let Some((started, release)) = gate {
                started.notify_one();
                release.notified().await;
            }
            let (status, body) = reply.ok_or("missing mock reply")?;
            Ok(HttpResponse {
                status,
                body: body.to_string(),
            })
        })
    }
}
async fn auth(f: &Fixture, http: Arc<MockHttp>, clock: Arc<TestClock>) -> AuthRuntime {
    AuthRuntime::create(
        &f.0,
        Channel::Tui,
        OaClient::with_transport("https://oa.example", http, clock).unwrap(),
    )
    .await
    .unwrap()
}
fn input(id: &str) -> CustomProviderInput {
    serde_json::from_value(json!({"id":id,"name":"测试服务","baseUrl":"http://127.0.0.1:8765/v1","api":"openai-completions",
        "models":[{"id":"test-model","name":"测试模型","contextWindow":262144,"maxTokens":16384,"reasoning":true,"image":true}]})).unwrap()
}
fn profile(id: &str, balance: Value) -> Value {
    json!({"result":true,"data":{"memberId":id,"name":"测试队员","balanceAvailable":true,"balance":balance}})
}
fn key(value: &str) -> Value {
    json!({"cstoa":{"type":"api_key","key":value}})
}

#[tokio::test]
async fn custom_provider_saves_masks_preserves_unknown_fields_and_enforces_revision() {
    let f = Fixture::new();
    f.write("models.json",json!({"note":"keep-root","providers":{"local-box":{"name":"旧服务","api":"openai-completions","baseUrl":"http://localhost:8765/v1","apiKey":"private-test-key","headerNote":"secret-unknown-field",
        "models":[{"id":"test-model","name":"old","contextWindow":4096,"maxTokens":1024,"reasoning":false,"input":["text"],"cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0},"modelNote":"keep-model"}]}}}));
    let a = auth(&f, Arc::default(), Arc::default()).await;
    let view = a.custom_provider("local-box").unwrap().unwrap();
    assert_eq!(view["apiKeySet"], true);
    assert!(!view.to_string().contains("private-test-key"));
    assert!(!view.to_string().contains("secret-unknown-field"));
    assert!(
        a.save_custom_provider(input("local-box"))
            .await
            .unwrap_err()
            .contains("config_conflict")
    );
    let mut update = input("local-box");
    update.revision = view["revision"].as_str().map(str::to_owned);
    let saved = a.save_custom_provider(update.clone()).await.unwrap();
    let disk = f.read("models.json");
    assert_eq!(disk["note"], "keep-root");
    assert_eq!(disk["providers"]["local-box"]["apiKey"], "private-test-key");
    assert_eq!(
        disk["providers"]["local-box"]["models"][0]["modelNote"],
        "keep-model"
    );
    assert_eq!(
        disk["providers"]["local-box"]["models"][0]["input"],
        json!(["text", "image"])
    );
    assert!(
        a.save_custom_provider(update)
            .await
            .unwrap_err()
            .contains("config_conflict")
    );
    assert!(
        a.delete_custom_provider("local-box", view["revision"].as_str().unwrap())
            .await
            .unwrap_err()
            .contains("config_conflict")
    );
    a.delete_custom_provider("local-box", saved["revision"].as_str().unwrap())
        .await
        .unwrap();
    assert!(
        !a.runtime()
            .get_all_models()
            .iter()
            .any(|m| m.provider == "local-box")
    );
    assert_eq!(f.read("models.json")["note"], "keep-root");
}
#[tokio::test]
async fn custom_provider_validates_ids_protocols_models_and_refresh_rolls_back_only_its_record() {
    let f = Fixture::new();
    let a = auth(&f, Arc::default(), Arc::default()).await;
    for id in [
        "deepseek",
        "cstoa",
        "constructor",
        "A-invalid",
        "a",
        "with space",
    ] {
        assert!(a.save_custom_provider(input(id)).await.is_err());
    }
    for base in [
        "http://private.example/v1",
        "https://user:private@example.com/v1",
        "https://x.example/?key=private",
        "https://x.example/#private",
    ] {
        let mut v = input("test-box");
        v.base_url = base.into();
        assert!(a.save_custom_provider(v).await.is_err());
    }
    let mut remote = input("test-box");
    remote.base_url = "https://test.example/v1".into();
    assert!(a.save_custom_provider(remote).await.is_err());
    let mut v = input("test-box");
    v.models.push(v.models[0].clone());
    assert!(a.save_custom_provider(v).await.is_err());
    let mut v = input("test-box");
    v.models[0].max_tokens = 262145;
    assert!(a.save_custom_provider(v).await.is_err());
    let broken = json!({"note":"untouched","providers":{"test-box":{"baseUrl":"http://localhost:8765/v1","api":"openai-completions","apiKey":"local","headers":{"X-Test":42},"models":[]}}});
    f.write("models.json", broken.clone());
    let mut rollback = input("test-box");
    rollback.revision = a.custom_provider("test-box").unwrap().unwrap()["revision"]
        .as_str()
        .map(str::to_owned);
    assert!(
        a.save_custom_provider(rollback)
            .await
            .unwrap_err()
            .contains("无法加载")
    );
    assert_eq!(f.read("models.json"), broken);
    f.write("models.json", json!({"note":"untouched","providers":{}}));
    for (n, protocol) in [
        "openai-completions",
        "openai-responses",
        "anthropic-messages",
        "google-generative-ai",
    ]
    .iter()
    .enumerate()
    {
        let mut v=serde_json::to_value(json!({"id":format!("service-{n}"),"name":"服务","baseUrl":"http://localhost:8765/v1","api":protocol,
            "models":[{"id":"test","name":"模型","contextWindow":4096,"maxTokens":1024,"reasoning":false,"image":false}]})).unwrap();
        let saved = a
            .save_custom_provider(serde_json::from_value(v.take()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            a.runtime()
                .get_model(saved["id"].as_str().unwrap(), "test")
                .unwrap()
                .api,
            *protocol
        );
    }
    assert_eq!(a.custom_providers().unwrap().len(), 4);
    let defaults = a.custom_model_defaults();
    assert_eq!(defaults["contextWindow"], 262144);
    assert!(defaults["maxTokens"].as_u64().unwrap() > 0);
}
#[tokio::test]
async fn parallel_custom_edits_allow_one_revision_and_preserve_new_api_key() {
    let f = Fixture::new();
    let a = auth(&f, Arc::default(), Arc::default()).await;
    let saved = a.save_custom_provider(input("test-box")).await.unwrap();
    let mut v = input("test-box");
    v.revision = saved["revision"].as_str().map(str::to_owned);
    v.api_key = Some("new-private-key".into());
    let (one, two) = tokio::join!(a.save_custom_provider(v.clone()), a.save_custom_provider(v));
    assert_ne!(one.is_ok(), two.is_ok());
    assert_eq!(
        f.read("models.json")["providers"]["test-box"]["apiKey"],
        "new-private-key"
    );
}
#[tokio::test]
async fn account_zero_balance_caches_for_sixty_seconds_without_sliding_and_retries_failures() {
    let f = Fixture::new();
    f.write("auth.json", key("member-key"));
    let http = Arc::new(MockHttp::default());
    let clock = Arc::new(TestClock::default());
    let a = auth(&f, http.clone(), clock.clone()).await;
    http.reply(200, profile("001", json!(0)));
    assert_eq!(
        a.account(CancellationToken::new()).await.unwrap()["profile"]["studentId"],
        "001"
    );
    clock.0.fetch_add(59000, Ordering::SeqCst);
    assert_eq!(
        a.quota("cstoa", CancellationToken::new()).await.unwrap()["balance"],
        0
    );
    assert_eq!(http.count(), 1);
    clock.0.fetch_add(1001, Ordering::SeqCst);
    http.reply(503, json!({"token":"must-not-leak"}));
    let failed = a.account_snapshot(CancellationToken::new()).await.unwrap();
    assert_eq!(failed["account"]["requiresLogin"], false);
    assert_eq!(failed["quota"]["balance"], Value::Null);
    assert!(!failed.to_string().contains("must-not-leak"));
    http.reply(200, profile("001", json!(3)));
    assert_eq!(
        a.quota("cstoa", CancellationToken::new()).await.unwrap()["balance"],
        3
    );
    assert_eq!(http.count(), 3);
    a.invalidate_account();
    http.reply(
        200,
        json!({"result":false,"code":19008,"message":"private"}),
    );
    assert_eq!(
        a.account(CancellationToken::new()).await.unwrap()["requiresLogin"],
        true
    );
    http.reply(200, profile("001", json!(4)));
    assert_eq!(
        a.quota("cstoa", CancellationToken::new()).await.unwrap()["balance"],
        4
    );
}
#[tokio::test]
async fn concurrent_account_and_quota_share_one_request_and_discard_key_switch_response() {
    let f = Fixture::new();
    f.write("auth.json", key("old-key"));
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    http.reply(200, profile("old-member", json!(10)));
    let (started, release) = http.block_next();
    let first = tokio::spawn({
        let a = a.clone();
        async move { a.account_snapshot(CancellationToken::new()).await.unwrap() }
    });
    started.notified().await;
    let second = tokio::spawn({
        let a = a.clone();
        async move { a.quota("cstoa", CancellationToken::new()).await.unwrap() }
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(http.count(), 1);
    f.write("auth.json", key("new-key"));
    http.reply(200, profile("new-member", json!(20)));
    release.notify_one();
    let result = first.await.unwrap();
    let quota = second.await.unwrap();
    assert_eq!(result["account"]["profile"]["studentId"], "new-member");
    assert_eq!(quota["balance"], 20);
    assert!(!result.to_string().contains("old-member"));
    assert_eq!(http.count(), 2);
}
#[tokio::test]
async fn logout_during_query_discards_profile_and_cancellation_does_not_clear_other_waiters() {
    let f = Fixture::new();
    f.write("auth.json", key("old-key"));
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    http.reply(200, profile("private-member", json!(10)));
    let (started, release) = http.block_next();
    let first = tokio::spawn({
        let a = a.clone();
        async move { a.account_snapshot(CancellationToken::new()).await.unwrap() }
    });
    started.notified().await;
    a.logout("cstoa").await.unwrap();
    release.notify_one();
    let value = first.await.unwrap();
    assert_eq!(value["account"]["signedIn"], false);
    assert!(!value.to_string().contains("private-member"));
    a.runtime()
        .set_runtime_api_key("cstoa", "new-key")
        .await
        .unwrap();
    http.reply(200, profile("current", json!(2)));
    let (started, release) = http.block_next();
    let cancel = CancellationToken::new();
    let first = tokio::spawn({
        let a = a.clone();
        let cancel = cancel.clone();
        async move { a.account(cancel).await }
    });
    started.notified().await;
    let second = tokio::spawn({
        let a = a.clone();
        async move { a.quota("cstoa", CancellationToken::new()).await.unwrap() }
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    cancel.cancel();
    assert!(first.await.unwrap().is_err());
    release.notify_one();
    assert_eq!(second.await.unwrap()["balance"], 2);
}
#[tokio::test]
async fn account_missing_name_bad_balance_and_oauth_route_are_explicit_and_not_cached() {
    let f = Fixture::new();
    f.write("auth.json",json!({"cstoa":{"type":"oauth","access":"test-oauth","refresh":"","expires":pi_ai::now_ms()+86400000}}));
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    http.reply(200,json!({"result":true,"data":{"memberId":"001","name":" ","balanceAvailable":true,"balance":9007199254740992_u64}}));
    let value = a.account_snapshot(CancellationToken::new()).await.unwrap();
    assert_eq!(value["account"]["profile"]["studentId"], "001");
    assert_eq!(value["account"]["profile"]["name"], Value::Null);
    assert_eq!(value["quota"]["balance"], Value::Null);
    assert_eq!(value["account"]["requiresLogin"], false);
    http.reply(401, json!({"access":"private"}));
    assert_eq!(
        a.account(CancellationToken::new()).await.unwrap()["requiresLogin"],
        true
    );
    assert!(
        http.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.url.ends_with("/api/agent/profile"))
    );
}
#[tokio::test]
async fn known_quota_routes_validate_hosts_currencies_windows_and_credentials() {
    let f = Fixture::new();
    f.write("auth.json",json!({"deepseek":{"type":"api_key","key":"deepseek-test"},"opencode-go":{"type":"api_key","key":"go-test"}}));
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    http.reply(200,json!({"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"2.00"},{"currency":"CNY","total_balance":"0.00"}]}));
    let value = a.quota("deepseek", CancellationToken::new()).await.unwrap();
    assert_eq!(value["balance"], 0.0);
    assert_eq!(value["currency"], "CNY");
    let window = json!({"percent":33.5,"status":"ok","resetsAt":"2026-11-10T10:00:00Z"});
    http.reply(
        200,
        json!({"usage":{"rolling":window,"weekly":window,"monthly":window}}),
    );
    let go = a
        .quota("opencode-go", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(go["windows"]["weekly"]["percent"], 33.5);
    let requests = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.url.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        requests,
        [
            "https://api.deepseek.com/user/balance",
            "https://opencode.ai/zen/go/v1/usage"
        ]
    );
    f.write(
        "models.json",
        json!({"providers":{"deepseek":{"baseUrl":"https://evil.example/v1"}}}),
    );
    a.runtime()
        .refresh(Some(pi_ai::ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await;
    assert_eq!(
        a.quota("deepseek", CancellationToken::new()).await.unwrap()["supported"],
        false
    );
    assert_eq!(http.count(), 2);
    a.invalidate_account();
    http.reply(200, json!({"usage":{"rolling":{"percent":101}}}));
    assert_eq!(
        a.quota("opencode-go", CancellationToken::new())
            .await
            .unwrap()["supported"],
        false
    );
    http.reply(
        200,
        json!({"usage":{"rolling":window,"weekly":window,"monthly":window}}),
    );
    assert_eq!(
        a.quota("opencode-go", CancellationToken::new())
            .await
            .unwrap()["supported"],
        true
    );
}
#[tokio::test]
async fn credential_store_uses_pi_templates_preserves_metadata_and_none_modify_keeps_previous() {
    let f = Fixture::new();
    f.write("auth.json",json!({"deepseek":{"type":"api_key","key":"${TEST_CST_KEY}","env":{"TEST_CST_KEY":"test-template-key"},"note":"keep"},"empty-key":{"type":"api_key"}}));
    let a = auth(&f, Arc::default(), Arc::default()).await;
    assert!(
        a.runtime()
            .credentials()
            .inspect("deepseek")
            .unwrap()
            .configured
    );
    assert_eq!(
        a.runtime()
            .resolve_request_api_key("deepseek")
            .await
            .unwrap()
            .as_deref(),
        Some("test-template-key")
    );
    let original = fs::read(f.file("auth.json")).unwrap();
    let current = a
        .runtime()
        .credentials()
        .modify("deepseek", Box::new(|_| Box::pin(async { Ok(None) })))
        .await
        .unwrap();
    assert!(current.is_some());
    assert_eq!(fs::read(f.file("auth.json")).unwrap(), original);
    a.runtime()
        .credentials()
        .modify(
            "deepseek",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("replaced-key".into()),
                        env: None,
                    })))
                })
            }),
        )
        .await
        .unwrap();
    assert_eq!(f.read("auth.json")["deepseek"]["note"], "keep");
    assert!(f.read("auth.json")["deepseek"].get("env").is_none());
    a.logout("deepseek").await.unwrap();
    assert!(f.read("auth.json").get("deepseek").is_none());
    assert!(f.read("auth.json").get("empty-key").is_some());
}
#[tokio::test]
async fn kit_session_keeps_provider_on_reload_and_views_are_isolated_with_fixed_default() {
    let f = Fixture::new();
    f.resources();
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    let kit = Kit::new(&f.0).unwrap();
    let options = || CreateAgentSessionOptions {
        session_manager: Some(
            SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
        ),
        ..Default::default()
    };
    let first = kit
        .create_session_with_auth(options(), a.clone())
        .await
        .unwrap();
    let second = kit.create_session_with_auth(options(), a).await.unwrap();
    assert_eq!(first.created.session.model().provider, "deepseek");
    assert_eq!(first.created.session.model().id, "deepseek-flash");
    assert!(
        first
            .auth
            .runtime()
            .credentials()
            .read("deepseek")
            .await
            .unwrap()
            .is_none()
    );
    first.created.session.reload().await.unwrap();
    assert!(
        first
            .auth
            .runtime()
            .models()
            .get_provider("cstoa")
            .is_some()
    );
    first.shutdown().await.unwrap();
    assert!(
        second
            .auth
            .runtime()
            .models()
            .get_provider("cstoa")
            .is_some()
    );
    assert_eq!(
        second.created.session.get_active_tool_names().len(),
        cst_pilot::OPEN_TOOL_NAMES.len()
    );
    second.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(http.count(), 0);
}

#[tokio::test]
async fn late_model_directory_response_cannot_publish_or_persist_previous_identity() {
    let f = Fixture::new();
    f.write("auth.json", key("old-key"));
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    http.reply(200, json!({"data":[{"id":"old-only"}]}));
    let (started, release) = http.block_next();
    let pending = tokio::spawn({
        let a = a.clone();
        async move { a.refresh(true).await }
    });
    started.notified().await;
    f.write("auth.json", key("new-key"));
    release.notify_one();
    let result = pending.await.unwrap();
    assert!(result.aborted || result.errors.contains_key("cstoa"));
    assert!(
        a.runtime()
            .get_available_models()
            .iter()
            .all(|m| m.provider != "cstoa")
    );
    if f.file("models-store.json").exists() {
        assert!(!f.read("models-store.json").to_string().contains("old-only"));
    }
    http.reply(200, json!({"data":[{"id":"new-only"}]}));
    assert!(a.refresh(true).await.errors.is_empty());
    assert!(
        a.runtime()
            .get_available_models()
            .iter()
            .any(|m| m.provider == "cstoa" && m.id == "new-only")
    );
}

#[tokio::test]
async fn cancelling_last_account_waiter_drops_request_and_allows_a_new_query() {
    let f = Fixture::new();
    f.write("auth.json", key("test-key"));
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    http.reply(200, profile("cancelled", json!(1)));
    let (started, _release) = http.block_next();
    let cancel = CancellationToken::new();
    let pending = tokio::spawn({
        let a = a.clone();
        let cancel = cancel.clone();
        async move { a.account(cancel).await }
    });
    started.notified().await;
    assert_eq!(http.active.load(Ordering::SeqCst), 1);
    cancel.cancel();
    assert!(pending.await.unwrap().is_err());
    assert_eq!(http.active.load(Ordering::SeqCst), 0);
    http.reply(200, profile("current", json!(2)));
    assert_eq!(
        a.account(CancellationToken::new()).await.unwrap()["profile"]["studentId"],
        "current"
    );
    assert_eq!(http.count(), 2);
}

#[tokio::test]
async fn dropping_kit_wrapper_cancels_background_catalog_request() {
    let f = Fixture::new();
    f.resources();
    f.write("auth.json", key("test-key"));
    let http = Arc::new(MockHttp::default());
    let a = auth(&f, http.clone(), Arc::default()).await;
    http.reply(200, json!({"data":[]}));
    let (started, _release) = http.block_next();
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_auth(
            CreateAgentSessionOptions {
                session_manager: Some(
                    SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
                ),
                ..Default::default()
            },
            a,
        )
        .await
        .unwrap();
    started.notified().await;
    assert_eq!(http.active.load(Ordering::SeqCst), 1);
    let created = session.created.session.clone();
    drop(session);
    tokio::time::timeout(Duration::from_secs(1), async {
        while http.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    created.shutdown().await.unwrap();
}

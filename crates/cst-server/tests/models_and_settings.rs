use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_kit::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_kit::telemetry::TelemetryOptions;
use cst_kit::tools::ssrf::{HttpRequest, HttpResponse, HttpTransport};
use cst_kit::update::service::UpdateOptions;
use cst_kit::{Kit, KitHost, SKILL_NAMES};
use cst_server::discovery::{ServerInstanceLock, discovery_file_path, read_discovery_file};
use cst_server::{RunningServer, ServerConfig};
use pi_agent::BoxFuture;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Default)]
struct CountingOaTransport {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<(u16, Value)>>,
}

impl CountingOaTransport {
    fn push_reply(&self, status: u16, body: Value) {
        self.replies.lock().unwrap().push_back((status, body));
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for CountingOaTransport {
    fn request(
        &self,
        request: HttpRequest,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>> {
        self.requests.lock().unwrap().push(request);
        let (status, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or((200, json!({"data": [{"id": "deepseek-flash"}]})));
        Box::pin(async move {
            Ok(HttpResponse {
                status,
                body: body.to_string(),
            })
        })
    }
}

struct TestKitDir(PathBuf);

impl TestKitDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cst-server-models-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        for name in SKILL_NAMES {
            let p = root.join(format!("app/skills/{name}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(
                p,
                format!("---\nname: {name}\ndescription: {name} 工具指南\n---\n维护指南。"),
            )
            .unwrap();
        }
        fs::create_dir_all(root.join("app/prompts")).unwrap();
        fs::create_dir_all(root.join("data/config")).unwrap();
        fs::write(
            root.join("app/prompts/APPEND_SYSTEM.md"),
            "CST Pilot 固定诊断规则。",
        )
        .unwrap();
        Self(root)
    }
}

impl Drop for TestKitDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn kit_host_shares_single_catalog_maintenance_task_when_individual_sessions_close() {
    let fixture = TestKitDir::new();
    // Write a CSTOA API key so catalog refresh will hit our mock OA transport
    fs::write(
        fixture.0.join("data/config/auth.json"),
        serde_json::to_vec(&json!({
            "cstoa": {
                "type": "api_key",
                "key": "sk-test-cstoa-key"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let transport = Arc::new(CountingOaTransport::default());
    transport.push_reply(
        200,
        json!({"code":0,"data":{"student_id":"20260001","name":"测试同学","quota":100,"used_quota":10}}),
    );
    transport.push_reply(200, json!({"data": [{"id": "deepseek-flash"}]}));

    let oa = OaClient::with_transport(
        "https://oa.example",
        transport.clone(),
        Arc::new(cst_kit::auth::oa::SystemClock),
    )
    .unwrap();
    let kit = Kit::new(&fixture.0).unwrap();
    let auth = AuthRuntime::create(&fixture.0, Channel::Gui, oa)
        .await
        .unwrap();
    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Disabled,
    )
    .await
    .unwrap();

    // Open two hosted sessions and shut down the first one
    let s1 = host
        .open_session(pi_runtime::CreateAgentSessionOptions::default())
        .await
        .unwrap();
    let s2 = host
        .open_session(pi_runtime::CreateAgentSessionOptions::default())
        .await
        .unwrap();
    s1.shutdown().await.unwrap();

    // Wait briefly for initial maintenance loop tick, then force-refresh / wake catalog
    tokio::time::sleep(Duration::from_millis(50)).await;
    let before = transport.count();
    let refresh_res = host.auth().refresh(true).await;
    assert!(refresh_res.errors.is_empty());
    assert!(transport.count() > before);

    s2.shutdown().await.unwrap();
    host.shutdown().await;
}

#[tokio::test]
async fn saving_an_unrelated_model_keeps_the_original_scope_rule() {
    let fixture = TestKitDir::new();
    fs::write(
        fixture.0.join("data/config/auth.json"),
        json!({ "deepseek": { "type": "api_key", "key": "test-offline-key" } }).to_string(),
    )
    .unwrap();
    let kit = Kit::new(&fixture.0).unwrap();
    let oa = OaClient::new("https://oa.example").unwrap();
    let auth = AuthRuntime::create(&fixture.0, Channel::Gui, oa)
        .await
        .unwrap();
    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Disabled,
    )
    .await
    .unwrap();
    let settings_path = host.kit().config_dir().join("settings.json");
    let wildcard = "DeepSeek/*:high";
    let unrelated = "anthropic/claude-fable-5";
    let unmatched = "missing-provider/*";
    fs::write(
        &settings_path,
        json!({ "enabledModels": [wildcard, unrelated, unmatched] }).to_string(),
    )
    .unwrap();

    let mut cfg = ServerConfig::new(&fixture.0);
    cfg.port = Some(0);
    cfg.acquire_instance_lock = false;
    let server = RunningServer::start(host, cfg).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let snapshot: Value = client
        .get(format!("{}/api/models", server.base_url()))
        .header("X-CST-Client", "app")
        .bearer_auth(server.app_token())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let rules = snapshot["scopeRules"].as_array().unwrap();
    assert_eq!(rules.len(), 3, "{rules:?}");
    assert_eq!(rules[0]["pattern"], wildcard);
    assert!(rules[0]["matches"].as_array().unwrap().iter().any(|m| {
        m["id"] == "deepseek/deepseek-flash" && m["pattern"] == "deepseek/deepseek-flash:high"
    }));
    assert_eq!(rules[1]["matches"][0]["id"], unrelated);
    assert_eq!(rules[2], json!({ "pattern": unmatched, "matches": [] }));
    let saved: Vec<_> = rules
        .iter()
        .filter_map(|rule| {
            rule["pattern"]
                .as_str()
                .filter(|pattern| *pattern != unrelated)
        })
        .collect();

    client
        .post(format!("{}/api/models/scoped", server.base_url()))
        .header("X-CST-Client", "app")
        .header("Idempotency-Key", "remove-unrelated-model")
        .bearer_auth(server.app_token())
        .json(&json!({ "patterns": saved }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let settings: Value = serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
    assert_eq!(settings["enabledModels"], json!([wildcard, unmatched]));

    let ready = host_available_ids(&snapshot);
    assert!(!ready.is_empty());
    for (key, preserve_explicit, expected) in [
        ("legacy-default-order", false, Value::Null),
        ("explicit-default-order", true, json!(ready)),
    ] {
        let result: Value = client
            .post(format!("{}/api/models/scoped", server.base_url()))
            .header("X-CST-Client", "app")
            .header("Idempotency-Key", key)
            .bearer_auth(server.app_token())
            .json(&json!({ "patterns": ready, "preserveExplicit": preserve_explicit }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["enabled"], expected);
        let settings: Value = serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(settings["enabledModels"], expected);
    }
    server.shutdown().await;
}

fn host_available_ids(snapshot: &Value) -> Vec<String> {
    let full_id = |model: &Value| {
        format!(
            "{}/{}",
            model["provider"].as_str().unwrap(),
            model["id"].as_str().unwrap()
        )
    };
    let ready: std::collections::HashSet<_> = snapshot["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(full_id)
        .collect();
    snapshot["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .map(full_id)
        .filter(|id| ready.contains(id))
        .collect()
}

#[tokio::test]
async fn three_end_theme_isolation_and_activity_gate_exemptions() {
    let fixture = TestKitDir::new();
    let kit = Kit::new(&fixture.0).unwrap();
    let oa = OaClient::new("https://oa.example").unwrap();
    let auth = AuthRuntime::create(&fixture.0, Channel::Gui, oa)
        .await
        .unwrap();
    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Disabled,
    )
    .await
    .unwrap();

    // Pre-set TUI theme in shared `settings.json` to "solarized-tui"
    fs::create_dir_all(host.kit().config_dir()).unwrap();
    fs::write(
        host.kit().config_dir().join("settings.json"),
        serde_json::to_vec_pretty(&json!({ "theme": "solarized-tui" })).unwrap(),
    )
    .unwrap();

    let mut cfg = ServerConfig::new(&fixture.0);
    cfg.port = Some(0);
    cfg.acquire_instance_lock = false;
    cfg.mode_config.standalone_web = true;
    let server = RunningServer::start(host.clone(), cfg).await.unwrap();
    let port = server.port();
    let host_header = format!("127.0.0.1:{port}");
    let origin_header = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // 1. Update Web theme (`PATCH /api/settings` -> `web-settings.json`) to `dark`
    let web_patch = client
        .patch(format!("{}/api/settings", server.base_url()))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "theme-web-1")
        .json(&json!({ "theme": "dark" }))
        .send()
        .await
        .unwrap();
    assert_eq!(web_patch.status(), 200);
    let set_cookie = web_patch
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(set_cookie.contains("cst-theme=dark"));

    // 2. Update Desktop theme & geometry (`PATCH /api/app/settings` -> `desktop.json`) to `system`
    let app_patch = client
        .patch(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({
            "theme": "system",
            "defaultView": "tui",
            "windowWidth": 1440,
            "windowHeight": 900
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(app_patch.status(), 200);
    let app_json: Value = app_patch.json().await.unwrap();
    assert_eq!(app_json["theme"], "system");
    assert_eq!(app_json["defaultView"], "tui");
    assert_eq!(app_json["windowWidth"], 1440);
    assert!(host.kit().config_dir().join("desktop.json").exists());
    assert!(
        !host
            .kit()
            .config_dir()
            .join("desktop-settings.json")
            .exists()
    );

    // 3. Verify all three themes remain strictly isolated on disk
    let tui_settings_json: Value =
        serde_json::from_slice(&fs::read(host.kit().config_dir().join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(tui_settings_json["theme"], "solarized-tui");

    let web_get: Value = client
        .get(format!("{}/api/settings", server.base_url()))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(web_get["theme"], "dark");

    let app_get: Value = client
        .get(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(app_get["theme"], "system");

    // 4. Close `ActivityGate` (simulating an upgrade handoff in progress):
    // Read-only queries (`GET /api/models`, `GET /api/state`, `GET /api/health`) are exempt and succeed,
    // while business & configuration writes (`POST /api/models/default`, `PATCH /api/app/settings`,
    // `POST /api/app/web/open`) are rejected with `409 update_in_progress`.
    let upgrade_lease = host.kit().activity_gate().close().unwrap();

    let models_get = client
        .get(format!("{}/api/models", server.base_url()))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap();
    assert_eq!(models_get.status(), 200);

    let blocked_write = client
        .post(format!("{}/api/models/default", server.base_url()))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "blocked-by-upgrade")
        .json(&json!({ "provider": "deepseek", "modelId": "deepseek-flash" }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_write.status(), 409);
    let err_body: Value = blocked_write.json().await.unwrap();
    assert_eq!(err_body["error"]["code"], "update_in_progress");

    let blocked_app_settings = client
        .patch(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({ "sidebarCollapsed": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_app_settings.status(), 409);
    let app_err_body: Value = blocked_app_settings.json().await.unwrap();
    assert_eq!(app_err_body["error"]["code"], "update_in_progress");

    let blocked_web_open = client
        .post(format!("{}/api/app/web/open", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_web_open.status(), 409);
    let web_open_err: Value = blocked_web_open.json().await.unwrap();
    assert_eq!(web_open_err["error"]["code"], "update_in_progress");

    drop(upgrade_lease);

    // After upgrade gate reopens, the write succeeds!
    let allowed_write = client
        .post(format!("{}/api/models/default", server.base_url()))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "allowed-after-upgrade")
        .json(&json!({ "provider": "openai", "modelId": "gpt-4o" }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed_write.status(), 200);

    server.shutdown().await;
}

#[tokio::test]
async fn desktop_json_forward_compatibility_corruption_protection_and_health_enrichment() {
    let fixture = TestKitDir::new();
    fs::write(fixture.0.join("VERSION"), "cst-pilot v0.8.2\n").unwrap();

    let oa = OaClient::with_transport(
        "https://oa.example",
        Arc::new(CountingOaTransport::default()),
        Arc::new(cst_kit::auth::oa::SystemClock),
    )
    .unwrap();
    let kit = Kit::new(&fixture.0).unwrap();
    let auth = AuthRuntime::create(&fixture.0, Channel::Gui, oa)
        .await
        .unwrap();
    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Disabled,
    )
    .await
    .unwrap();

    let mut cfg = ServerConfig::new(&fixture.0);
    cfg.port = Some(0);
    cfg.acquire_instance_lock = false;
    cfg.mode_config.standalone_web = true;
    let server = RunningServer::start(host.clone(), cfg).await.unwrap();
    let port = server.port();
    let host_header = format!("127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // 1. Missing `desktop.json`: `GET /api/app/settings` returns defaults without creating file on disk
    let desktop_path = host.kit().config_dir().join("desktop.json");
    assert!(!desktop_path.exists());
    let initial_get: Value = client
        .get(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial_get["theme"], "light");
    assert_eq!(initial_get["defaultView"], "gui");
    assert_eq!(initial_get["sidebarCollapsed"], false);
    assert_eq!(initial_get["windowMaximized"], false);
    assert!(!desktop_path.exists());

    // 2. Write future fields and unrecognized enum values to `desktop.json`, then partially PATCH
    fs::write(
        &desktop_path,
        serde_json::to_vec_pretty(&json!({
            "theme": "future-oled",
            "defaultView": "split-view",
            "sidebarCollapsed": true,
            "displayHint": "a1b2c3d4e5f60718",
            "futureNested": { "alpha": 42 }
        }))
        .unwrap(),
    )
    .unwrap();

    let fallback_get: Value = client
        .get(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fallback_get["theme"], "light");
    assert_eq!(fallback_get["defaultView"], "gui");
    assert_eq!(fallback_get["sidebarCollapsed"], true);
    assert_eq!(fallback_get["displayHint"], "a1b2c3d4e5f60718");

    let partial_patch = client
        .patch(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({ "windowWidth": 1360, "windowMaximized": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(partial_patch.status(), 200);

    let on_disk_after_patch: Value =
        serde_json::from_slice(&fs::read(&desktop_path).unwrap()).unwrap();
    assert_eq!(on_disk_after_patch["theme"], "future-oled");
    assert_eq!(on_disk_after_patch["defaultView"], "split-view");
    assert_eq!(on_disk_after_patch["futureNested"]["alpha"], 42);
    assert_eq!(on_disk_after_patch["windowWidth"], 1360);
    assert_eq!(on_disk_after_patch["windowMaximized"], true);

    // 3. Corrupt `desktop.json` -> 503 configuration_unavailable and corrupt bytes preserved untouched
    let raw_corrupt = b"{not-valid-json";
    fs::write(&desktop_path, raw_corrupt).unwrap();
    let bad_get = client
        .get(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_get.status(), 503);
    let bad_get_json: Value = bad_get.json().await.unwrap();
    assert_eq!(bad_get_json["error"]["code"], "configuration_unavailable");

    let bad_patch = client
        .patch(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({ "theme": "dark" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_patch.status(), 503);
    assert_eq!(fs::read(&desktop_path).unwrap(), raw_corrupt);

    // 4. `GET /api/health` differentiates App caller vs unauthenticated caller; `GET /api/lifecycle` includes counts
    let web_health: Value = client
        .get(format!("{}/api/health", server.base_url()))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(web_health["connected"], true);
    assert!(web_health.get("pid").is_none());
    assert!(web_health.get("kitRoot").is_none());

    let app_health: Value = client
        .get(format!("{}/api/health", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(app_health["pid"], std::process::id());
    assert_eq!(app_health["version"], "v0.8.2");
    assert!(app_health["kitRoot"].as_str().is_some());
    assert!(app_health["elevated"].is_boolean());

    let lifecycle: Value = client
        .get(format!("{}/api/lifecycle", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lifecycle["version"], "v0.8.2");
    assert_eq!(lifecycle["activeExports"], 0);

    server.shutdown().await;
}

#[tokio::test]
async fn shared_missing_key_prompt_claim_atomic_single_winner_and_cooldown() {
    let fixture = TestKitDir::new();
    let oa = OaClient::with_transport(
        "https://oa.example",
        Arc::new(CountingOaTransport::default()),
        Arc::new(cst_kit::auth::oa::SystemClock),
    )
    .unwrap();
    let kit = Kit::new(&fixture.0).unwrap();
    let auth = AuthRuntime::create(&fixture.0, Channel::Gui, oa)
        .await
        .unwrap();
    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Disabled,
    )
    .await
    .unwrap();

    let mut cfg = ServerConfig::new(&fixture.0);
    cfg.port = Some(0);
    cfg.acquire_instance_lock = false;
    cfg.mode_config.standalone_web = true;
    let server = RunningServer::start(host.clone(), cfg).await.unwrap();
    let port = server.port();
    let host_header = format!("127.0.0.1:{port}");
    let origin_header = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Fire 6 concurrent claims across Web and App callers: strictly 1 winner!
    let barrier = Arc::new(tokio::sync::Barrier::new(6));
    let mut handles = Vec::new();
    for idx in 0..6usize {
        let c = client.clone();
        let base = server.base_url();
        let h = host_header.clone();
        let o = origin_header.clone();
        let token = server.app_token().to_owned();
        let b = barrier.clone();
        handles.push(tokio::spawn(async move {
            b.wait().await;
            let req = if idx % 2 == 0 {
                c.post(format!("{base}/api/ui-hints/missing-key-prompt/claim"))
                    .header("Host", &h)
                    .header("Origin", &o)
                    .header("X-CST-Web-Request", "1")
                    .header("Idempotency-Key", format!("claim-web-{idx}"))
                    .json(&json!({}))
            } else {
                c.post(format!("{base}/api/ui-hints/missing-key-prompt/claim"))
                    .header("Host", &h)
                    .header("X-CST-Client", "app")
                    .header("Authorization", format!("Bearer {token}"))
                    .json(&json!({}))
            };
            let resp = req.send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let body: Value = resp.json().await.unwrap();
            body["claimed"].as_bool().unwrap()
        }));
    }

    let mut winners = 0usize;
    for handle in handles {
        if handle.await.unwrap() {
            winners += 1;
        }
    }
    assert_eq!(winners, 1);

    let hints_get: Value = client
        .get(format!("{}/api/ui-hints", server.base_url()))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(hints_get["missingKeyPromptedAtMs"].as_u64().is_some());

    // Simulate expired cooldown (> 24h old) on disk -> next claim succeeds
    let expired_ts = (chrono::Utc::now().timestamp_millis() as u64) - 25 * 60 * 60 * 1000;
    fs::write(
        fixture.0.join(".state/ui-hints.json"),
        serde_json::to_vec(&json!({ "missingKeyPromptedAtMs": expired_ts })).unwrap(),
    )
    .unwrap();
    let after_expiry: Value = client
        .post(format!(
            "{}/api/ui-hints/missing-key-prompt/claim",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_expiry["claimed"], true);

    server.shutdown().await;
}

#[cfg(windows)]
#[tokio::test]
async fn app_update_routes_check_prepare_cancel_and_launch_guards() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use cst_kit::update::{
        UpdateClient,
        handoff::{Handoff, LaunchReceipt},
        layout::REQUIRED_FONT_FILES,
        manifest::{
            Archive, MANIFEST_DOMAIN, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
        },
        network::{Download, Fetcher},
        process::ProcessIdentity,
        service::HandoffLauncher,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::io::{Cursor, Write};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio_util::sync::CancellationToken;
    use zip::{ZipWriter, write::SimpleFileOptions};

    // 1. Default `UpdateOptions::Compiled` without compile-time env vars -> Phase::Unavailable (200)
    {
        let fixture = TestKitDir::new();
        fs::write(fixture.0.join("VERSION"), "cst-pilot v0.8.0\n").unwrap();
        let oa = OaClient::with_transport(
            "https://oa.example",
            Arc::new(CountingOaTransport::default()),
            Arc::new(cst_kit::auth::oa::SystemClock),
        )
        .unwrap();
        let kit = Kit::new(&fixture.0).unwrap();
        let auth = AuthRuntime::create(&fixture.0, Channel::Gui, oa)
            .await
            .unwrap();
        let host = KitHost::new(
            kit,
            auth,
            TelemetryOptions::default(),
            UpdateOptions::Compiled,
        )
        .await
        .unwrap();
        let mut cfg = ServerConfig::new(&fixture.0);
        cfg.port = Some(0);
        cfg.acquire_instance_lock = false;
        cfg.mode_config.standalone_web = true;
        let server = RunningServer::start(host, cfg).await.unwrap();
        let host_header = format!("127.0.0.1:{}", server.port());
        let client = reqwest::Client::new();

        let status_resp: Value = client
            .get(format!("{}/api/app/update/status", server.base_url()))
            .header("Host", &host_header)
            .header("X-CST-Client", "app")
            .header("Authorization", format!("Bearer {}", server.app_token()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status_resp["status"]["phase"], "unavailable");
        assert_eq!(status_resp["status"]["message"], "当前构建未启用在线更新。");

        let check_resp = client
            .post(format!("{}/api/app/update/check", server.base_url()))
            .header("Host", &host_header)
            .header("X-CST-Client", "app")
            .header("Authorization", format!("Bearer {}", server.app_token()))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(check_resp.status(), 200);
        let check_json: Value = check_resp.json().await.unwrap();
        assert_eq!(check_json["phase"], "unavailable");
        server.shutdown().await;
    }

    // 2. Trusted signed release: check -> prepare + cancel -> prepare -> launch validation & handoff
    struct MockUpdateNet {
        manifest_bytes: Vec<u8>,
        archive_bytes: Vec<u8>,
        stall_archive: AtomicBool,
        archive_started: tokio::sync::Notify,
    }
    impl Fetcher for MockUpdateNet {
        fn get(
            &self,
            url: String,
            _limit: u64,
            cancel: CancellationToken,
        ) -> BoxFuture<'_, Result<Download, String>> {
            if url.ends_with("/latest") {
                let bytes = self.manifest_bytes.clone();
                return Box::pin(async move {
                    Ok(Download {
                        body: Box::pin(futures_util::stream::once(async move { Ok(bytes) })),
                    })
                });
            }
            let stall = self.stall_archive.load(Ordering::SeqCst);
            let bytes = self.archive_bytes.clone();
            Box::pin(async move {
                if stall {
                    self.archive_started.notify_waiters();
                    let body = async_stream::stream! {
                        yield Ok(bytes[..64].to_vec());
                        cancel.cancelled().await;
                        yield Err("升级已取消。".to_owned());
                    };
                    Ok(Download {
                        body: Box::pin(body),
                    })
                } else {
                    Ok(Download {
                        body: Box::pin(futures_util::stream::once(async move { Ok(bytes) })),
                    })
                }
            })
        }
    }

    struct RecordingLauncher {
        launched: Mutex<Option<u32>>,
    }
    impl HandoffLauncher for RecordingLauncher {
        fn launch(&self, _handoff: Handoff) -> Result<LaunchReceipt, String> {
            let updater_pid = 42424u32;
            *self.launched.lock().unwrap() = Some(updater_pid);
            Ok(LaunchReceipt {
                updater_pid,
                request_id: "mock-req-1".into(),
            })
        }
    }

    let fixture = TestKitDir::new();
    fs::write(fixture.0.join("VERSION"), "cst-pilot v0.8.0\n").unwrap();

    // Build a valid signed v0.9.0 release zip including 3 binaries + 5 SFNT fonts + skills
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::from([
        ("VERSION".into(), b"cst-pilot v0.9.0\n".to_vec()),
        (
            "app/BUILD-INFO.json".into(),
            br#"{"version":"v0.9.0"}"#.to_vec(),
        ),
        ("LICENSE".into(), b"MIT".to_vec()),
        ("THIRD-PARTY-NOTICES.md".into(), b"notices".to_vec()),
        ("app/licenses/mock.txt".into(), b"mock notice".to_vec()),
        ("cst-pilot.exe".into(), b"new-pilot-bin".to_vec()),
        ("app/cst-server.exe".into(), b"new-server-bin".to_vec()),
        ("app/cst-updater.exe".into(), b"new-updater-bin".to_vec()),
        (
            "app/prompts/APPEND_SYSTEM.md".into(),
            b"CST Pilot Rules".to_vec(),
        ),
        (
            "app/telemetry/timserver_1.crt".into(),
            b"synthetic public certificate".to_vec(),
        ),
        ("app/telemetry/currency.json".into(), b"{}".to_vec()),
        ("app/defaults/settings.json".into(), b"{}".to_vec()),
        ("app/defaults/models-store.json".into(), b"{}".to_vec()),
        ("app/defaults/telemetry.json".into(), b"{}".to_vec()),
        ("app/defaults/web-search.json".into(), b"{}".to_vec()),
    ]);
    for name in SKILL_NAMES {
        files.insert(
            format!("app/skills/{name}/SKILL.md"),
            format!("---\nname: {name}\ndescription: {name}\n---\nGuide").into_bytes(),
        );
    }
    for rel in REQUIRED_FONT_FILES {
        let font_name = rel.strip_prefix("app/fonts/").unwrap();
        files.insert(
            (*rel).to_owned(),
            fs::read(repo_root.join("assets/fonts").join(font_name)).unwrap(),
        );
    }
    let signing = SigningKey::from_bytes(&[9u8; 32]);
    let release_key = ReleaseKey::from_bytes(&signing.verifying_key().to_bytes()).unwrap();
    let sums = files
        .iter()
        .map(|(path, content)| format!("{:x}  {path}\n", Sha256::digest(content)))
        .collect::<String>();
    let sign = |domain: &[u8], bytes: &[u8]| {
        STANDARD.encode(signing.sign(&[domain, bytes].concat()).to_bytes())
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        for (path, content) in &files {
            zip.start_file(format!("cst-pilot/{path}"), SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content).unwrap();
        }
        zip.start_file("cst-pilot/app/SHA256SUMS", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(sums.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    let archive_bytes = cursor.into_inner();
    let archive_size = archive_bytes.len() as u64;
    let unpacked_size = files.values().map(|b| b.len() as u64).sum::<u64>() + sums.len() as u64;
    let manifest = ReleaseManifest {
        schema_version: 1,
        platform: "windows-x86_64".into(),
        version: "v0.9.0".into(),
        min_upgradable_version: "v0.6.0".into(),
        built_at: "2026-11-19T00:00:00Z".into(),
        release_notes: "Desktop Step 1".into(),
        archive: Archive {
            url: "https://updates.example/v0.9.0.zip".into(),
            sha256: format!("{:x}", Sha256::digest(&archive_bytes)),
            size: archive_size,
            unpacked_size,
        },
        sha256_signature: sign(SUMS_DOMAIN, sums.as_bytes()),
        sha256_sums: sums,
    };
    let payload = serde_json::to_string(&manifest).unwrap();
    let manifest_bytes = serde_json::to_vec(&SignedManifest {
        signature: sign(MANIFEST_DOMAIN, payload.as_bytes()),
        payload,
    })
    .unwrap();

    let net = Arc::new(MockUpdateNet {
        manifest_bytes,
        archive_bytes,
        stall_archive: AtomicBool::new(true),
        archive_started: tokio::sync::Notify::new(),
    });
    let launcher = Arc::new(RecordingLauncher {
        launched: Mutex::new(None),
    });
    let update_client =
        UpdateClient::with_fetcher("https://updates.example/latest", release_key, net.clone())
            .unwrap();
    let oa = OaClient::with_transport(
        "https://oa.example",
        Arc::new(CountingOaTransport::default()),
        Arc::new(cst_kit::auth::oa::SystemClock),
    )
    .unwrap();
    let kit = Kit::new(&fixture.0).unwrap();
    let auth = AuthRuntime::create(&fixture.0, Channel::Gui, oa)
        .await
        .unwrap();
    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Trusted {
            client: Arc::new(update_client),
            launcher: launcher.clone(),
        },
    )
    .await
    .unwrap();

    let mut cfg = ServerConfig::new(&fixture.0);
    cfg.port = Some(0);
    cfg.acquire_instance_lock = false;
    let server = RunningServer::start(host.clone(), cfg).await.unwrap();
    let host_header = format!("127.0.0.1:{}", server.port());
    let client = reqwest::Client::new();

    // Connect `/api/app/events` so App presence is Connected and we can observe SSE events
    let sse_resp = client
        .get(format!("{}/api/app/events", server.base_url()))
        .header("Host", &host_header)
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), 200);
    let mut sse_stream = sse_resp.bytes_stream();
    let _hello_chunk = sse_stream.next().await.unwrap().unwrap();

    // Check -> Available with archiveBytes
    let check_val: Value = client
        .post(format!("{}/api/app/update/check", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(check_val["phase"], "available");
    assert_eq!(check_val["version"], "v0.9.0");
    assert_eq!(check_val["archiveBytes"], archive_size);

    // Observe `update_changed` over SSE
    let sse_update_chunk = tokio::time::timeout(Duration::from_secs(2), sse_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let sse_text = String::from_utf8_lossy(&sse_update_chunk);
    assert!(sse_text.contains("\"update_changed\""), "sse: {sse_text}");

    // Start stalling `prepare` and cancel it midway -> staging cleaned, phase returns to `available`
    let started_wait = net.archive_started.notified();
    let prep_client = client.clone();
    let prep_url = format!("{}/api/app/update/prepare", server.base_url());
    let prep_host = host_header.clone();
    let prep_token = server.app_token().to_owned();
    let prep_task = tokio::spawn(async move {
        prep_client
            .post(prep_url)
            .header("Host", prep_host)
            .header("X-CST-Client", "app")
            .header("Authorization", format!("Bearer {prep_token}"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
    });
    started_wait.await;

    let cancel_resp = client
        .post(format!("{}/api/app/update/cancel", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel_resp.status(), 200);
    let cancel_val: Value = cancel_resp.json().await.unwrap();
    assert_eq!(cancel_val["phase"], "available");

    let aborted_prep = prep_task.await.unwrap();
    assert_eq!(aborted_prep.status(), 409);

    let staging_dir = fixture.0.join(".state/update/staging");
    if staging_dir.exists() {
        assert_eq!(fs::read_dir(&staging_dir).unwrap().count(), 0);
    }

    // Allow archive download to complete -> `phase == "ready"`
    net.stall_archive.store(false, Ordering::SeqCst);
    let ready_resp = client
        .post(format!("{}/api/app/update/prepare", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(ready_resp.status(), 200);
    let ready_val: Value = ready_resp.json().await.unwrap();
    assert_eq!(ready_val["phase"], "ready");
    assert!(ready_val["operationId"].is_string());

    let stale_cancel = client
        .post(format!("{}/api/app/update/cancel", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({ "operationId": "previous-download" }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_cancel.status(), 409);
    assert!(host.updates().is_ready());

    // `launch` with wrong executable path (current test runner PID != `<kit_root>/cst-pilot.exe`) -> 400 invalid_parent_process
    let self_id = ProcessIdentity::current().unwrap();
    let bad_proc_resp = client
        .post(format!("{}/api/app/update/launch", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({
            "appPid": self_id.pid,
            "appCreated": self_id.created,
            "ui": "desktop"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_proc_resp.status(), 400);
    let bad_proc_val: Value = bad_proc_resp.json().await.unwrap();
    assert_eq!(bad_proc_val["error"]["code"], "invalid_parent_process");

    // Spawn a real dummy `<kit_root>/cst-pilot.exe` process so `ProcessIdentity` + `QueryFullProcessImageNameW` match!
    let dummy_pilot = fixture.0.join("cst-pilot.exe");
    fs::copy(env!("CARGO_BIN_EXE_cst-server"), &dummy_pilot).unwrap();
    let dummy_agent_kit = TestKitDir::new();
    let mut child = std::process::Command::new(&dummy_pilot)
        .arg("--standalone-web")
        .arg("--port")
        .arg("0")
        .env("CST_PILOT_ROOT", &dummy_agent_kit.0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn dummy cst-pilot.exe");
    let child_identity = ProcessIdentity::capture(child.id())
        .unwrap()
        .expect("child identity alive");

    // Write an unloaded session's unresolved `data/inbox/<id>.json` on disk -> `409 session_busy_for_update`
    let inbox_dir = host.kit().inbox_dir();
    fs::create_dir_all(&inbox_dir).unwrap();
    let inbox_file = inbox_dir.join("unloaded-sess-1.json");
    fs::write(
        &inbox_file,
        serde_json::to_vec(&json!({
            "version": 1,
            "nextSequence": 2,
            "paused": true,
            "items": [{
                "id": "pending-req-1",
                "sequence": 1,
                "text": "待发消息",
                "delivery": "queue",
                "acceptedText": "待发消息",
                "acceptedDelivery": "queue",
                "images": [],
                "status": "pending",
                "acceptedAt": 1700000000000u64
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let busy_resp = client
        .post(format!("{}/api/app/update/launch", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({
            "appPid": child_identity.pid,
            "appCreated": child_identity.created,
            "ui": "desktop"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(busy_resp.status(), 409);
    let busy_val: Value = busy_resp.json().await.unwrap();
    assert_eq!(busy_val["error"]["code"], "session_busy_for_update");

    // Clear the pending inbox item -> `launch` succeeds with 200 receipt and triggers async shutdown!
    fs::remove_file(&inbox_file).unwrap();
    let launch_resp = client
        .post(format!("{}/api/app/update/launch", server.base_url()))
        .header("Host", &host_header)
        .header("X-CST-Client", "app")
        .header("Authorization", format!("Bearer {}", server.app_token()))
        .json(&json!({
            "appPid": child_identity.pid,
            "appCreated": child_identity.created,
            "ui": "desktop"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(launch_resp.status(), 200);
    let launch_val: Value = launch_resp.json().await.unwrap();
    assert_eq!(launch_val["receipt"]["updaterPid"], 42424);
    assert_eq!(*launcher.launched.lock().unwrap(), Some(42424));

    let _ = child.kill();
    let _ = child.wait();
    drop(sse_stream);
    server.shutdown().await;
}

#[tokio::test]
async fn running_server_publishes_and_cleans_up_dynamically_bound_port() {
    let fixture = TestKitDir::new();
    let kit = Kit::new(&fixture.0).unwrap();
    let oa = OaClient::with_transport(
        "https://oa.example.test",
        Arc::new(CountingOaTransport::default()),
        Arc::new(cst_kit::auth::oa::SystemClock),
    )
    .unwrap();
    let auth = AuthRuntime::create(kit.root(), Channel::Gui, oa)
        .await
        .unwrap();
    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Disabled,
    )
    .await
    .unwrap();

    let mut cfg = ServerConfig::new(&fixture.0);
    // Exercise dynamic-port publication without relying on the host's default port or environment.
    cfg.port = Some(0);
    cfg.mode_config.standalone_web = true;
    let server = RunningServer::start(host, cfg).await.unwrap();

    assert!(server.port() > 0);
    assert_eq!(server.state.port(), server.port());
    let lock_conflict = ServerInstanceLock::acquire(&fixture.0)
        .err()
        .expect("running server should hold the instance lock");
    assert!(lock_conflict.starts_with("server_already_running:"));

    let discovery = read_discovery_file(&fixture.0)
        .expect("discovery file should be written with the bound port");
    assert_eq!(discovery.port, server.port());
    assert_eq!(discovery.token, server.app_token());
    let origin = format!("http://127.0.0.1:{}", discovery.port);
    assert_eq!(origin, server.base_url());

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let health = async {
        client
            .get(format!("{origin}/api/health"))
            .header("Origin", &origin)
            .header("X-CST-Client", "app")
            .bearer_auth(&discovery.token)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
    }
    .await;

    server.shutdown().await;
    let health = health.unwrap();
    assert_eq!(health["pid"], discovery.pid);
    assert_eq!(health["connected"], true);
    assert!(!discovery_file_path(&fixture.0).try_exists().unwrap());
    ServerInstanceLock::acquire(&fixture.0).expect("shutdown should release the instance lock");
}

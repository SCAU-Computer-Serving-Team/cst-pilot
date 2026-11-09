use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_pilot::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_pilot::telemetry::TelemetryOptions;
use cst_pilot::tools::ssrf::{HttpRequest, HttpResponse, HttpTransport};
use cst_pilot::update::service::UpdateOptions;
use cst_pilot::{Kit, KitHost, SKILL_NAMES};
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
            let p = root.join(format!("agent/home/skills/{name}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(
                p,
                format!("---\nname: {name}\ndescription: {name} 工具指南\n---\n维护指南。"),
            )
            .unwrap();
        }
        fs::write(
            root.join("agent/home/APPEND_SYSTEM.md"),
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
        fixture.0.join("agent/home/auth.json"),
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
        Arc::new(cst_pilot::auth::oa::SystemClock),
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
    fs::write(
        host.kit().agent_dir().join("settings.json"),
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

    // 2. Update Desktop theme & geometry (`PATCH /api/app/settings` -> `desktop-settings.json`) to `system`
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

    // 3. Verify all three themes remain strictly isolated on disk
    let tui_settings_json: Value =
        serde_json::from_slice(&fs::read(host.kit().agent_dir().join("settings.json")).unwrap())
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
    // while business writes (`POST /api/models/default`, `POST /api/sessions`) are rejected with `409 updating`!
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
    assert_eq!(err_body["error"]["code"], "updating");

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

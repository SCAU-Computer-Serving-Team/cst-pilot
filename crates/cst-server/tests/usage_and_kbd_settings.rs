use std::fs;
use std::path::PathBuf;

use cst_kit::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_kit::telemetry::TelemetryOptions;
use cst_kit::update::service::UpdateOptions;
use cst_kit::{Kit, KitHost, SKILL_NAMES};
use cst_server::{RunningServer, ServerConfig};
use serde_json::{Value, json};
use uuid::Uuid;

struct UsageTestKitDir(PathBuf);

impl UsageTestKitDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cst-server-usage-{}", Uuid::new_v4()));
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

impl Drop for UsageTestKitDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn start_server(fixture: &UsageTestKitDir) -> RunningServer {
    let oa = OaClient::new("https://oa.example").unwrap();
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
    cfg.cwd = Some(fixture.0.clone());
    cfg.acquire_instance_lock = false;
    RunningServer::start(host, cfg).await.unwrap()
}

fn build_branching_session_jsonl(session_id: &str, cwd: &str) -> String {
    let lines = vec![
        json!({
            "type": "session",
            "version": 3,
            "id": session_id,
            "timestamp": "2026-11-24T00:00:00.000Z",
            "cwd": cwd,
        }),
        json!({
            "type": "model_change",
            "id": "m0",
            "parentId": null,
            "timestamp": "2026-11-24T00:00:00.100Z",
            "provider": "deepseek",
            "modelId": "deepseek-flash",
        }),
        json!({
            "type": "message",
            "id": "u1",
            "parentId": "m0",
            "timestamp": "2026-11-24T00:00:01.000Z",
            "message": {
                "role": "user",
                "content": "问题 1",
                "timestamp": 1000
            }
        }),
        json!({
            "type": "message",
            "id": "a1",
            "parentId": "u1",
            "timestamp": "2026-11-24T00:00:02.000Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "分支 A 回复"}],
                "api": "openai-completions",
                "provider": "deepseek",
                "model": "deepseek-flash",
                "usage": {
                    "input": 2000,
                    "output": 200,
                    "cacheRead": 2000,
                    "cacheWrite": 0,
                    "totalTokens": 4200,
                    "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}
                },
                "stopReason": "stop",
                "timestamp": 2000
            }
        }),
        // Side branch off u1 with a different cache hit rate (80%).
        json!({
            "type": "message",
            "id": "a1b",
            "parentId": "u1",
            "timestamp": "2026-11-24T00:00:02.500Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "分支 B 回复"}],
                "api": "openai-completions",
                "provider": "deepseek",
                "model": "deepseek-flash",
                "usage": {
                    "input": 1000,
                    "output": 100,
                    "cacheRead": 4000,
                    "cacheWrite": 0,
                    "totalTokens": 5100,
                    "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}
                },
                "stopReason": "stop",
                "timestamp": 2500
            }
        }),
        // Continue on branch A -> compaction -> post-compaction reply (81.16%).
        json!({
            "type": "compaction",
            "id": "c1",
            "parentId": "a1",
            "timestamp": "2026-11-24T00:00:03.000Z",
            "summary": "压缩摘要",
            "firstKeptEntryId": "a1",
            "tokensBefore": 21071
        }),
        json!({
            "type": "message",
            "id": "u2",
            "parentId": "c1",
            "timestamp": "2026-11-24T00:00:04.000Z",
            "message": {
                "role": "user",
                "content": "压缩后的新问题",
                "timestamp": 4000
            }
        }),
        json!({
            "type": "message",
            "id": "a2",
            "parentId": "u2",
            "timestamp": "2026-11-24T00:00:05.000Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "压缩后的新回复"}],
                "api": "openai-completions",
                "provider": "deepseek",
                "model": "deepseek-flash",
                "usage": {
                    "input": 1307,
                    "output": 136,
                    "cacheRead": 5632,
                    "cacheWrite": 0,
                    "totalTokens": 7075,
                    "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}
                },
                "stopReason": "stop",
                "timestamp": 5000
            }
        }),
    ];
    lines
        .into_iter()
        .map(|v| serde_json::to_string(&v).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[tokio::test]
async fn session_usage_endpoint_tracks_active_branch_and_compaction_boundaries() {
    let fixture = UsageTestKitDir::new();
    let server = start_server(&fixture).await;
    let client = reqwest::Client::new();
    let host_header = format!("127.0.0.1:{}", server.port());
    let auth_header = format!("Bearer {}", server.app_token());

    let session_id = "01a20927-1668-71fb-b50e-f0001f03f3be";
    let clean_cwd_str = fixture
        .0
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .map(str::to_owned)
        .unwrap_or_else(|| fixture.0.to_string_lossy().into_owned());
    let clean_cwd = PathBuf::from(&clean_cwd_str);
    let session_dir =
        pi_runtime::get_default_session_dir_in(&clean_cwd, &clean_cwd.join("data/sessions"));
    fs::create_dir_all(&session_dir).unwrap();
    let session_file = session_dir.join(format!("2026-11-24T00-00-00-000Z_{session_id}.jsonl"));
    // A regular session open repairs this tail; the read-only usage endpoint must leave it alone.
    let mut original = format!(
        "{}\n{{\"type\":",
        build_branching_session_jsonl(session_id, &clean_cwd_str)
    )
    .into_bytes();
    original.push(0xff);
    fs::write(&session_file, &original).unwrap();

    // 1. Default leaf is `a2` (post-compaction reply on branch A): 5632 / (1307 + 5632)
    let resp = client
        .get(format!(
            "{}/api/sessions/{session_id}/usage",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["sessionId"], session_id);
    assert_eq!(body["leafId"], "a2");
    assert_eq!(fs::read(&session_file).unwrap(), original);
    assert_eq!(body["usage"]["scope"], "last_assistant");
    assert_eq!(body["usage"]["sourceEntryId"], "a2");
    let expected_a2 = 5632.0 / (1307.0 + 5632.0);
    assert!((body["usage"]["cacheHitRate"].as_f64().unwrap() - expected_a2).abs() < 1e-9);
    assert_eq!(body["contextUsage"]["tokens"], 7075);

    let head = client
        .head(format!(
            "{}/api/sessions/{session_id}/usage",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert!(head.bytes().await.unwrap().is_empty());
    assert_eq!(fs::read(&session_file).unwrap(), original);
    assert_eq!(server.state.pool.active_slot_count().await, 0);

    // The regular session index requires UTF-8; keep its fixture's tail truncated but valid UTF-8.
    original.pop();
    fs::write(&session_file, original).unwrap();

    // 2. Navigate tree to `c1` (compaction boundary with no post-compaction assistant reply) -> null cacheHitRate & null tokens
    let nav_c1 = client
        .post(format!(
            "{}/api/sessions/{session_id}/tree/navigate",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .header("Idempotency-Key", "nav-c1")
        .json(&json!({ "entryId": "c1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(nav_c1.status(), 200);

    let after_c1: Value = client
        .get(format!(
            "{}/api/sessions/{session_id}/usage",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_c1["leafId"], "c1");
    assert!(after_c1["usage"]["cacheHitRate"].is_null());
    assert!(after_c1["contextUsage"]["tokens"].is_null());

    // 3. Navigate tree to side branch `a1b`: must reflect branch B (80%), never mixing branch A!
    let nav_a1b = client
        .post(format!(
            "{}/api/sessions/{session_id}/tree/navigate",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .header("Idempotency-Key", "nav-a1b")
        .json(&json!({ "entryId": "a1b" }))
        .send()
        .await
        .unwrap();
    assert_eq!(nav_a1b.status(), 200);

    let after_a1b: Value = client
        .get(format!(
            "{}/api/sessions/{session_id}/usage",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_a1b["leafId"], "a1b");
    assert_eq!(after_a1b["usage"]["sourceEntryId"], "a1b");
    assert!((after_a1b["usage"]["cacheHitRate"].as_f64().unwrap() - 0.8).abs() < 1e-9);
    assert_eq!(after_a1b["contextUsage"]["tokens"], 5100);

    // 4. Navigate tree to root user message `u1`: no prior assistant reply -> null cacheHitRate
    let nav_u1 = client
        .post(format!(
            "{}/api/sessions/{session_id}/tree/navigate",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .header("Idempotency-Key", "nav-u1")
        .json(&json!({ "entryId": "u1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(nav_u1.status(), 200);

    let after_u1: Value = client
        .get(format!(
            "{}/api/sessions/{session_id}/usage",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(after_u1["usage"]["cacheHitRate"].is_null());

    server.shutdown().await;
}

#[tokio::test]
async fn cold_usage_ignores_linked_transcripts_and_rejects_duplicate_ids() {
    let fixture = UsageTestKitDir::new();
    let server = start_server(&fixture).await;
    let session_id = "01a20927-1668-71fb-b50e-f0001f03f3bf";
    let session_dir = fixture.0.join("data/sessions/project");
    fs::create_dir_all(&session_dir).unwrap();
    let external = fixture.0.join("outside-sessions.jsonl");
    let transcript = build_branching_session_jsonl(session_id, &fixture.0.to_string_lossy());
    fs::write(&external, &transcript).unwrap();
    let first = session_dir.join("one.jsonl");
    fs::hard_link(&external, &first).unwrap();
    let client = reqwest::Client::new();
    let request = || {
        client
            .get(format!(
                "{}/api/sessions/{session_id}/usage",
                server.base_url()
            ))
            .header("Host", format!("127.0.0.1:{}", server.port()))
            .bearer_auth(server.app_token())
    };
    assert_eq!(request().send().await.unwrap().status(), 404);
    fs::remove_file(&first).unwrap();
    fs::write(&first, &transcript).unwrap();
    assert_eq!(request().send().await.unwrap().status(), 200);
    fs::write(session_dir.join("two.jsonl"), &transcript).unwrap();
    assert_eq!(request().send().await.unwrap().status(), 404);
    assert_eq!(server.state.pool.active_slot_count().await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn settings_persist_keyboard_focus_style_for_desktop_and_web() {
    let fixture = UsageTestKitDir::new();
    let server = start_server(&fixture).await;
    let client = reqwest::Client::new();
    let host_header = format!("127.0.0.1:{}", server.port());
    let auth_header = format!("Bearer {}", server.app_token());

    // Desktop settings default to `brackets`, persist `rail`, and round-trip cleanly.
    let initial_app: Value = client
        .get(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial_app["keyboardFocusStyle"], "brackets");

    let patched_app: Value = client
        .patch(format!("{}/api/app/settings", server.base_url()))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .json(&json!({ "keyboardFocusStyle": "rail" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched_app["keyboardFocusStyle"], "rail");

    // Web settings default to `brackets` and support updating `keyboardFocusStyle` independently of `theme`.
    let initial_web: Value = client
        .get(format!("{}/api/settings", server.base_url()))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial_web["theme"], "light");
    assert_eq!(initial_web["keyboardFocusStyle"], "brackets");

    let patched_web: Value = client
        .patch(format!("{}/api/settings", server.base_url()))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .header("Idempotency-Key", "web-kbd-rail")
        .json(&json!({ "keyboardFocusStyle": "rail" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched_web["theme"], "light");
    assert_eq!(patched_web["keyboardFocusStyle"], "rail");

    let bad_web = client
        .patch(format!("{}/api/settings", server.base_url()))
        .header("Host", &host_header)
        .header("Authorization", &auth_header)
        .header("Idempotency-Key", "web-kbd-invalid")
        .json(&json!({ "keyboardFocusStyle": "glow" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_web.status(), 400);

    server.shutdown().await;
}

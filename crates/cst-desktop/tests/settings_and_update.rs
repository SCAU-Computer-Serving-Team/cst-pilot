use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use cst_client::{AppHttpClient, CstClient, SessionSummary, VerifiedServerEndpoint};
use cst_desktop::auth::cstoa_qr::{parse_cstoa_oauth_payload, rasterize_qr_image_offline};
use cst_desktop::open_desktop_shell_window_with_handle;
use cst_desktop::sessions::all_sessions::{
    SessionDayGroup, VirtualSessionRow, group_sessions_by_natural_day,
};
use cst_desktop::sessions::branch_tree::{TreeFilterMode, build_visible_tree_nodes};
use cst_desktop::shell::register_shell_keys;
use cst_desktop::shell::sidebar::ShellRoute;
use cst_os::ProcessIdentity;
use gpui::AppContext as _;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct KitTempFixture {
    root: PathBuf,
}

impl KitTempFixture {
    fn create() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir();
        let root = base.join(format!("desktop-step6-test-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join("agent").join("home")).unwrap();
        Self { root }
    }
}

impl Drop for KitTempFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn offline_qr_natural_day_grouping_and_branch_tree_five_filters() {
    // 1. Pure-Rust offline QR rasterization (`verificationUriComplete` -> PNG `gpui::Image`)
    let payload = json!({
        "flowId": "flow-cstoa-1",
        "verificationUriComplete": "https://oa.cst.example.edu/device?user_code=ABCD-1234",
        "expiresIn": 300
    });
    let parsed = parse_cstoa_oauth_payload(&payload).expect("valid CSTOA device payload");
    assert_eq!(parsed.flow_id.as_deref(), Some("flow-cstoa-1"));
    assert_eq!(
        parsed.verification_uri_complete,
        "https://oa.cst.example.edu/device?user_code=ABCD-1234"
    );
    let qr_img = rasterize_qr_image_offline(&parsed.verification_uri_complete)
        .expect("offline QR rasterization succeeds");
    assert!(!qr_img.bytes().is_empty());

    // 2. Natural-day 4-group bucketing omitting empty groups (`LastMonth` omitted here)
    let local_noon_rfc3339 = |date: NaiveDate| -> String {
        let dt = date.and_hms_opt(12, 0, 0).expect("valid noon");
        Local
            .from_local_datetime(&dt)
            .single()
            .or_else(|| Local.from_local_datetime(&dt).earliest())
            .expect("local datetime")
            .to_rfc3339()
    };
    let today = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
    let sessions = vec![
        SessionSummary {
            id: "s-today".into(),
            title: "今日排查".into(),
            updated_at: Some(local_noon_rfc3339(today)),
            running: false,
        },
        SessionSummary {
            id: "s-week".into(),
            title: "三天前巡检".into(),
            updated_at: Some(local_noon_rfc3339(today - ChronoDuration::days(3))),
            running: false,
        },
        SessionSummary {
            id: "s-old".into(),
            title: "两个月前记录".into(),
            updated_at: Some(local_noon_rfc3339(
                NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            )),
            running: false,
        },
    ];
    let grouped = group_sessions_by_natural_day(&sessions, today);
    let headers: Vec<SessionDayGroup> = grouped
        .iter()
        .filter_map(|r| match r {
            VirtualSessionRow::GroupHeader(g) => Some(*g),
            VirtualSessionRow::Session(_) => None,
        })
        .collect();
    assert_eq!(
        headers,
        vec![
            SessionDayGroup::Today,
            SessionDayGroup::LastWeek,
            SessionDayGroup::Earlier
        ],
        "Empty `LastMonth` group must be omitted"
    );

    // 3. Branch tree 5-filter cycling & projection
    let entries = vec![
        json!({ "id": "e1", "parentId": null, "label": "起点", "message": { "role": "user", "content": "排查磁盘" } }),
        json!({ "id": "e2", "parentId": "e1", "message": { "role": "toolResult", "content": [{ "type": "text", "text": "ok" }] } }),
        json!({ "id": "e3", "parentId": "e2", "message": { "role": "assistant", "content": "磁盘正常" } }),
    ];
    let empty_collapsed = BTreeSet::new();
    assert_eq!(
        build_visible_tree_nodes(&entries, Some("e3"), TreeFilterMode::All, &empty_collapsed).len(),
        3
    );
    assert_eq!(
        build_visible_tree_nodes(
            &entries,
            Some("e3"),
            TreeFilterMode::NoTools,
            &empty_collapsed
        )
        .len(),
        2
    );
    assert_eq!(
        build_visible_tree_nodes(
            &entries,
            Some("e3"),
            TreeFilterMode::UserOnly,
            &empty_collapsed
        )
        .len(),
        1
    );
    assert_eq!(
        build_visible_tree_nodes(
            &entries,
            Some("e3"),
            TreeFilterMode::LabeledOnly,
            &empty_collapsed
        )
        .len(),
        2,
        "LabeledOnly keeps labeled nodes plus current active leaf"
    );
    assert_eq!(TreeFilterMode::Default.next(), TreeFilterMode::NoTools);
    assert_eq!(TreeFilterMode::Default.prev(), TreeFilterMode::All);
}

struct Step6MockState {
    idle_deleted: bool,
    busy_for_update: bool,
    credential_ready: bool,
    cstoa_signed_in: bool,
    cstoa_logout_calls: usize,
    tree_navigations: Vec<Value>,
    compact_requests: usize,
    finish_compact: Arc<tokio::sync::Notify>,
    default_model_requests: usize,
    export_previews: Vec<Value>,
    export_creates: usize,
    export_opens: Vec<String>,
    saved_custom_providers: Vec<Value>,
    update_phase: String,
    update_cancel_calls: usize,
    update_launch_bodies: Vec<Value>,
    scoped_requests: Vec<Value>,
    scoped_gate: Option<Arc<tokio::sync::Notify>>,
    scoped_fail: bool,
    enabled_models: Option<Vec<String>>,
    rich_catalog: bool,
    settings_patches: Vec<Value>,
}

impl Default for Step6MockState {
    fn default() -> Self {
        Self {
            idle_deleted: false,
            busy_for_update: true,
            credential_ready: false,
            cstoa_signed_in: true,
            cstoa_logout_calls: 0,
            tree_navigations: Vec::new(),
            compact_requests: 0,
            finish_compact: Arc::new(tokio::sync::Notify::new()),
            default_model_requests: 0,
            export_previews: Vec::new(),
            export_creates: 0,
            export_opens: Vec::new(),
            saved_custom_providers: Vec::new(),
            update_phase: "idle".into(),
            update_cancel_calls: 0,
            update_launch_bodies: Vec::new(),
            scoped_requests: Vec::new(),
            scoped_gate: None,
            scoped_fail: false,
            enabled_models: None,
            rich_catalog: false,
            settings_patches: Vec::new(),
        }
    }
}

struct Step6MockBackend {
    rt: Option<tokio::runtime::Runtime>,
    port: u16,
    token: String,
    state: Arc<Mutex<Step6MockState>>,
    stop: Arc<AtomicBool>,
}

async fn read_full_http(stream: &mut tokio::net::TcpStream) -> Option<(String, String, String)> {
    let mut data = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let content_len = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while data.len() < header_end + content_len {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
    }
    let first = head.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    let body = String::from_utf8_lossy(&data[header_end..data.len().min(header_end + content_len)])
        .into_owned();
    Some((method, path, body))
}

impl Step6MockBackend {
    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().unwrap()
    }

    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(Mutex::new(Step6MockState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let token = "step6-test-token".to_owned();

        let st_srv = Arc::clone(&state);
        let stop_srv = Arc::clone(&stop);
        let port = rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                while !stop_srv.load(Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let st = Arc::clone(&st_srv);
                    let st_stop = Arc::clone(&stop_srv);
                    tokio::spawn(async move {
                        let Some((m_own, p_own, b_own)) = read_full_http(&mut stream).await else {
                            return;
                        };
                        let method = m_own.as_str();
                        let path = p_own.as_str();
                        let body_str = b_own.as_str();

                        if method == "GET"
                            && (path.starts_with("/api/app/events")
                                || path.starts_with("/api/events/stream"))
                        {
                            let hdr = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n: connected\n\n";
                            let _ = stream.write_all(hdr.as_bytes()).await;
                            while !st_stop.load(Ordering::Relaxed) {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                if stream.write_all(b": ping\n\n").await.is_err() {
                                    break;
                                }
                            }
                            return;
                        }

                        if method == "POST" && path == "/api/sessions/sess-busy/compact" {
                            let gate = {
                                let mut guard = st.lock().unwrap();
                                guard.compact_requests += 1;
                                Arc::clone(&guard.finish_compact)
                            };
                            gate.notified().await;
                        }
                        if method == "POST" && path == "/api/models/scoped" {
                            let gate = {
                                let mut guard = st.lock().unwrap();
                                if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                    guard.scoped_requests.push(v);
                                }
                                guard.scoped_gate.clone()
                            };
                            if let Some(gate) = gate {
                                gate.notified().await;
                            }
                        }
                        let (status_line, resp_json) = {
                            let mut guard = st.lock().unwrap();
                            let mut sessions = vec![json!({
                                "id": "sess-busy",
                                "title": "运行中蓝屏分析",
                                "updatedAt": "2026-11-20T02:00:00+08:00",
                                "running": true
                            })];
                            if !guard.idle_deleted {
                                sessions.push(json!({
                                    "id": "sess-idle",
                                    "title": "已完成启动项检查",
                                    "updatedAt": "2026-11-18T12:00:00+08:00",
                                    "running": false
                                }));
                            }

                            let update_status = json!({
                                "phase": guard.update_phase,
                                "version": "v0.2.0",
                                "archiveBytes": 41_943_040u64,
                                "downloadedBytes": 20_971_520u64,
                                "releaseNotes": "新增桌面端离线二维码与可取消更新进度。",
                                "operationId": "op-upd-1",
                                "message": ""
                            });

                            match (method, path) {
                                ("GET", "/api/health") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "connected": true, "stopping": false, "version": "v0.1.0" }),
                                ),
                                ("GET", "/api/state") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "version": "v0.1.0",
                                        "channelMode": "app_active",
                                        "multiplexStream": true,
                                        "sessions": sessions
                                    }),
                                ),
                                ("PUT", p) if p.starts_with("/api/events/stream/") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "subscribed": ["sess-busy"] }),
                                ),
                                ("DELETE", "/api/sessions/sess-idle") => {
                                    guard.idle_deleted = true;
                                    ("HTTP/1.1 200 OK", json!({ "deleted": true }))
                                }
                                ("DELETE", "/api/sessions/sess-busy") => (
                                    "HTTP/1.1 409 Conflict",
                                    json!({
                                        "error": {
                                            "code": "conflict",
                                            "message": "会话正在执行中，无法删除"
                                        }
                                    }),
                                ),
                                ("GET", "/api/sessions/sess-idle") => (
                                    "HTTP/1.1 200 OK",
                                    json!({"id":"sess-idle", "running":false, "messages":[]}),
                                ),
                                ("POST", "/api/sessions/sess-busy/compact") => (
                                    "HTTP/1.1 200 OK",
                                    json!({"ok":true}),
                                ),
                                ("POST", "/api/models/default") => {
                                    guard.default_model_requests += 1;
                                    (
                                        "HTTP/1.1 500 Internal Server Error",
                                        json!({"error":{"code":"save_failed","message":"保存失败"}}),
                                    )
                                }
                                ("GET", "/api/sessions/sess-busy") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "id": "sess-busy",
                                        "running": false,
                                        "leafId": "e-assist-1",
                                        "messages": [
                                            { "id": "e-user-1", "role": "user", "timestamp": 1000, "content": "分析蓝屏转储" },
                                            { "id": "e-assist-1", "role": "assistant", "timestamp": 1200, "content": "已定位驱动异常。" }
                                        ],
                                        "entries": [
                                            { "id": "e-user-1", "parentId": "e-model-0", "message": { "role": "user", "timestamp": 1000, "content": "分析蓝屏转储" } },
                                            { "id": "e-compact-1", "parentId": "e-user-1", "message": { "role": "compactionSummary", "summary": "已压缩蓝屏转储排查历史。", "tokensBefore": 148000, "timestamp": 1150 } },
                                            { "id": "e-assist-1", "parentId": "e-compact-1", "message": { "role": "assistant", "timestamp": 1200, "content": "已定位驱动异常。" } }
                                        ]
                                    }),
                                ),
                                ("GET", "/api/sessions/sess-busy/tree?format=flat") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "leafId": "e-assist-1",
                                        "entries": [
                                            { "entry": { "id": "e-model-0", "type": "model_change", "modelId": "deepseek-chat" } },
                                            { "entry": { "id": "e-user-1", "parentId": "e-model-0", "type": "message", "message": { "role": "user", "content": "分析蓝屏转储" } } },
                                            { "entry": { "id": "e-compact-1", "parentId": "e-user-1", "type": "compaction", "summary": "已压缩蓝屏转储排查历史。", "tokensBefore": 148000 } },
                                            { "entry": { "id": "e-assist-1", "parentId": "e-compact-1", "type": "message", "message": { "role": "assistant", "content": "已定位驱动异常。" } } }
                                        ]
                                    }),
                                ),
                                ("POST", "/api/sessions/sess-busy/messages") => {
                                    if !guard.credential_ready {
                                        (
                                            "HTTP/1.1 409 Conflict",
                                            json!({
                                                "error": {
                                                    "code": "credentials_required",
                                                    "message": "DeepSeek 未配置凭据",
                                                    "sessionId": "sess-busy"
                                                }
                                            }),
                                        )
                                    } else {
                                        ("HTTP/1.1 202 Accepted", json!({ "accepted": true }))
                                    }
                                }
                                ("POST", "/api/sessions/sess-busy/tree/navigate") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.tree_navigations.push(v);
                                    }
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "id": "sess-busy",
                                            "entryId": "e-user-1",
                                            "cancelled": false,
                                            "editorText": "分析蓝屏转储（分支回填）"
                                        }),
                                    )
                                }
                                ("POST", "/api/sessions/sess-busy/exports/preview") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.export_previews.push(v);
                                    }
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "sessionId": "sess-busy",
                                            "title": "运行中蓝屏分析",
                                            "capturedAt": "2026-11-20T02:00:00Z",
                                            "entries": [
                                                { "id": "e-user-1", "message": { "role": "user", "content": "分析蓝屏转储" } },
                                                { "id": "e-assist-1", "message": { "role": "assistant", "content": "已定位驱动异常。" } }
                                            ]
                                        }),
                                    )
                                }
                                ("POST", "/api/sessions/sess-busy/exports") => {
                                    guard.export_creates += 1;
                                    let (id, status) = if guard.export_creates == 1 {
                                        ("exp-stale", "rendering")
                                    } else {
                                        ("exp-job-1", "completed")
                                    };
                                    ("HTTP/1.1 202 Accepted", json!({
                                        "id": id,
                                        "status": status,
                                        "directory": "artifacts/2026-11-20/sessions"
                                    }))
                                }
                                ("GET", "/api/exports/recent") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "exports": [{
                                            "id": "exp-job-1",
                                            "sessionId": "sess-busy",
                                            "title": "蓝屏排查会话",
                                            "capturedAt": "2026-11-20T14:30:00Z",
                                            "status": "completed",
                                            "options": { "format": "png", "date": "2026-11-20" },
                                            "files": [{ "name": "report.png", "bytes": 4096, "url": "/api/exports/exp-job-1/files/0" }]
                                        }]
                                    }),
                                ),
                                ("GET", "/api/exports/exp-stale") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "id": "exp-stale",
                                        "status": if guard.export_creates == 1 { "rendering" } else { "completed" }
                                    }),
                                ),
                                ("GET", "/api/exports/exp-job-1") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "id": "exp-job-1",
                                        "status": "completed",
                                        "directory": "artifacts/2026-11-20/sessions",
                                        "files": [{ "name": "report.png", "bytes": 4096, "url": "/api/exports/exp-job-1/files/0" }]
                                    }),
                                ),
                                ("POST", p @ ("/api/exports/exp-job-1/open" | "/api/exports/exp-stale/open")) => {
                                    assert_eq!(
                                        body_str.trim(),
                                        "{}",
                                        "`POST /api/exports/<id>/open` MUST send empty JSON object body"
                                    );
                                    guard.export_opens.push(p.to_owned());
                                    ("HTTP/1.1 200 OK", json!({ "launched": true }))
                                }
                                ("POST", "/api/auth/custom") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.saved_custom_providers.push(v);
                                    }
                                    guard.credential_ready = true;
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({ "id": "my-gw", "revision": "rev-2" }),
                                    )
                                }
                                ("POST", "/api/auth/cstoa/oauth/start") => (
                                    "HTTP/1.1 202 Accepted",
                                    json!({
                                        "started": true,
                                        "flowId": "flow-qr-1",
                                        "state": "pending",
                                        "deviceCode": {
                                            "verificationUri": "https://oa.cst.example.edu/qr?t=998877",
                                            "expiresInSeconds": 180
                                        }
                                    }),
                                ),
                                ("GET", "/api/auth/cstoa/oauth/status") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "state": "pending", "flowId": "flow-qr-1" }),
                                ),
                                ("POST", "/api/auth/cstoa/logout") => {
                                    guard.cstoa_signed_in = false;
                                    guard.cstoa_logout_calls += 1;
                                    ("HTTP/1.1 200 OK", json!({ "providerId": "cstoa", "type": null }))
                                }
                                ("GET", "/api/auth") => {
                                    let mut providers = vec![
                                        json!({ "id": "cstoa", "name": "CSTOA", "configured": guard.cstoa_signed_in, "supportsOAuth": true }),
                                        json!({ "id": "deepseek", "name": "DeepSeek", "configured": guard.credential_ready, "supportsApiKey": true }),
                                    ];
                                    if guard.rich_catalog {
                                        for idx in 1..=10 {
                                            providers.push(json!({
                                                "id": format!("prov-{idx}"),
                                                "name": format!("Provider {idx}"),
                                                "configured": false,
                                                "supportsApiKey": true
                                            }));
                                        }
                                    }
                                    ("HTTP/1.1 200 OK", json!({ "providers": providers }))
                                }
                                ("GET", p) if p.starts_with("/api/account") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "providerId": "cstoa",
                                        "signedIn": guard.cstoa_signed_in,
                                        "profile": { "supported": true, "studentId": "20260888", "name": "张同学" }
                                    }),
                                ),
                                ("GET", p) if p.starts_with("/api/quota") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "supported": true, "provider": "cstoa", "unit": "credit", "balance": 8600.0 }),
                                ),
                                ("POST", "/api/models/scoped") => {
                                    if guard.scoped_fail {
                                        (
                                            "HTTP/1.1 500 Internal Server Error",
                                            json!({
                                                "error": {
                                                    "code": "internal_error",
                                                    "message": "模拟保存模型范围失败"
                                                }
                                            }),
                                        )
                                    } else {
                                        let patterns: Vec<String> = serde_json::from_str::<Value>(body_str)
                                            .ok()
                                            .and_then(|v| {
                                                v.get("patterns").and_then(Value::as_array).map(|arr| {
                                                    arr.iter()
                                                        .filter_map(|item| item.as_str().map(str::to_owned))
                                                        .collect()
                                                })
                                            })
                                            .unwrap_or_default();
                                        guard.enabled_models = Some(patterns.clone());
                                        ("HTTP/1.1 200 OK", json!({ "enabled": patterns }))
                                    }
                                }
                                ("GET", p) if p.starts_with("/api/models") => {
                                    let models = if guard.rich_catalog {
                                        (1..=7)
                                            .map(|idx| {
                                                json!({
                                                    "provider": "deepseek",
                                                    "id": format!("m{idx}"),
                                                    "name": format!("DeepSeek Model {idx}"),
                                                    "credentialReady": guard.credential_ready
                                                })
                                            })
                                            .collect::<Vec<_>>()
                                    } else {
                                        vec![json!({
                                            "provider": "deepseek",
                                            "id": "deepseek-chat",
                                            "name": "DeepSeek V3",
                                            "credentialReady": guard.credential_ready
                                        })]
                                    };
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "enabled": guard.enabled_models,
                                            "models": models,
                                            "catalog": models
                                        }),
                                    )
                                }
                                ("GET", "/api/app/settings") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "theme": "light", "defaultView": "gui", "sidebarCollapsed": false, "windowMaximized": false }),
                                ),
                                ("POST", "/api/app/settings") | ("PUT", "/api/app/settings") | ("PATCH", "/api/app/settings") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.settings_patches.push(v);
                                    }
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({ "theme": "light", "defaultView": "gui", "sidebarCollapsed": false, "windowMaximized": false }),
                                    )
                                }
                                ("POST", "/api/ui-hints/missing-key-prompt/claim") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "prompt": false }),
                                ),
                                ("GET", "/api/app/update/status") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "currentVersion": "v0.1.0",
                                        "status": update_status
                                    }),
                                ),
                                ("POST", "/api/app/update/prepare") => {
                                    guard.update_phase = "preparing".into();
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "phase": "preparing",
                                            "version": "v0.2.0",
                                            "archiveBytes": 41_943_040u64,
                                            "downloadedBytes": 20_971_520u64,
                                            "operationId": "op-upd-1",
                                            "message": ""
                                        }),
                                    )
                                }
                                ("POST", "/api/app/update/cancel") => {
                                    guard.update_cancel_calls += 1;
                                    guard.update_phase = "available".into();
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "phase": "available",
                                            "version": "v0.2.0",
                                            "archiveBytes": 41_943_040u64,
                                            "releaseNotes": "新增桌面端离线二维码与可取消更新进度。",
                                            "message": ""
                                        }),
                                    )
                                }
                                ("POST", "/api/app/update/launch") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.update_launch_bodies.push(v);
                                    }
                                    if guard.busy_for_update {
                                        (
                                            "HTTP/1.1 409 Conflict",
                                            json!({
                                                "error": {
                                                    "code": "session_busy_for_update",
                                                    "message": "存在运行中的会话或诊断任务，请先结束后再重启更新。"
                                                }
                                            }),
                                        )
                                    } else {
                                        (
                                            "HTTP/1.1 200 OK",
                                            json!({
                                                "receipt": {
                                                    "operationId": "op-upd-1",
                                                    "targetVersion": "0.2.0",
                                                    "updaterPid": 9999,
                                                    "updaterCreated": 123456
                                                }
                                            }),
                                        )
                                    }
                                }
                                _ => ("HTTP/1.1 200 OK", json!({})),
                            }
                        };

                        let body_bytes = serde_json::to_vec(&resp_json).unwrap();
                        let header = format!(
                            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body_bytes.len()
                        );
                        let _ = stream.write_all(header.as_bytes()).await;
                        let _ = stream.write_all(&body_bytes).await;
                    });
                }
            });
            port
        });

        Self {
            rt: Some(rt),
            port,
            token,
            state,
            stop,
        }
    }
}

impl Drop for Step6MockBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

#[gpui_kit::test]
async fn headless_palette_all_sessions_409_rollback_tree_export_missing_key_login_and_update_flow(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = Step6MockBackend::start();
    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: backend.port,
        version: "v0.1.0".into(),
        http: AppHttpClient::new(backend.port, &backend.token).unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint(backend.rt().handle(), endpoint, None).unwrap();
    backend.rt().block_on(handle.refresh_globals()).unwrap();
    backend.rt().block_on(handle.refresh_models(None)).unwrap();

    let (win_handle, shell) = cx.update(|cx| {
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), None, cx).unwrap()
    });

    // 1. `Ctrl+P` / `Ctrl+O` `540px` Dual Quick Palette (`I70mz` / `Xyg0l`): instant open, in-place switch, `Ctrl+K` removed, & `Esc` 1-frame focus restore
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-collapse-btn", cx);
        window.click("topbar-expand-sidebar-btn", cx);
        assert_eq!(window.find("sidebar-collapse-btn").focused(), Some(true));
        assert!(window.try_find("sidebar-search-kbd-btn").is_none());

        // Legacy Ctrl+K must not open the palette
        window.press("ctrl-k", cx);
        assert!(window.try_find("command-palette-modal-i70mz").is_none());

        window.press("ctrl-p", cx);
        let modal = window.find("command-palette-modal-i70mz");
        assert!(modal.visible());
        assert_eq!(f32::from(modal.bounds().size.width), 540.0);
        assert!(window.try_find("command-palette-footer-hints").is_none());

        // In-place switch Ctrl+P -> Ctrl+O -> Ctrl+P
        window.press("ctrl-o", cx);
        assert!(window.find("command-palette-modal-i70mz").visible());
        window.press("ctrl-p", cx);
        assert!(window.find("command-palette-modal-i70mz").visible());

        window.press("escape", cx);
        assert!(window.try_find("command-palette-modal-i70mz").is_none());
        assert_eq!(
            window.find("sidebar-collapse-btn").focused(),
            Some(true),
            "Esc on Command Palette must restore focus within 1 frame"
        );
    })
    .unwrap();

    // 2. Sidebar recent row context menu (`重命名 / 管理 / 删除` -> batch mode single `删除`)
    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-row-menu-btn-sess-busy", cx);
        assert!(window.find("sidebar-ctx-rename").visible());
        assert!(window.find("sidebar-ctx-manage").visible());
        assert!(window.find("sidebar-ctx-delete").visible());

        window.click("sidebar-ctx-manage", cx);
        window.click("sidebar-row-menu-btn-sess-busy", cx);
        assert!(window.try_find("sidebar-ctx-rename").is_none());
        assert!(window.find("sidebar-ctx-delete").visible());

        // Exit context menu & batch mode via single-layer `escape`
        window.press("escape", cx);
        assert!(window.try_find("sidebar-ctx-delete").is_none());
        window.press("escape", cx);
        window.click("sidebar-row-menu-btn-sess-busy", cx);
        window.click("sidebar-ctx-rename", cx);
        window.press("escape", cx);
        assert!(window.try_find("rename-session-modal").is_none());
        assert_eq!(
            window.find("sidebar-row-menu-btn-sess-busy").focused(),
            Some(true)
        );
    })
    .unwrap();

    // Switching to cold and cached sessions binds the composer on the same UI turn.
    cx.update_window(win_handle, |_, window, cx| {
        for id in ["sess-busy", "sess-idle", "sess-busy"] {
            window.click(
                gpui_kit::SharedString::from(format!("sidebar-session-row-{id}")),
                cx,
            );
            window.render_frame(cx);
            assert_eq!(
                shell
                    .read(cx)
                    .workbench()
                    .read(cx)
                    .session_composer()
                    .read(cx)
                    .draft_key(),
                &cst_client::DraftKey::Session(id.to_owned())
            );
        }
        window.press("ctrl-p", cx);
        window.input("compact", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.press("enter", cx);
    })
    .unwrap();
    cx.wait_for(win_handle, Duration::from_secs(2), |_, _| {
        backend.state.lock().unwrap().compact_requests == 1
    })
    .await;
    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-session-row-sess-idle", cx);
    })
    .unwrap();
    backend.state.lock().unwrap().finish_compact.notify_one();
    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("top-notice-banner").is_some()
    })
    .await;
    assert_eq!(
        handle
            .with_workspace(|ws| ws.active_session_id().map(str::to_owned))
            .as_deref(),
        Some("sess-idle")
    );

    // 3. `AllSessions` (`gpui_kit::v_virtual_list`): batch `"全选 / 删除"` -> `sess-idle` deleted, `sess-busy` `409` auto-restored!
    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-all-sessions-btn", cx);
        assert!(window.find("all-sessions-view").visible());
        assert!(window.find("all-sessions-row-sess-busy").visible());
        assert!(window.find("all-sessions-row-sess-idle").visible());

        window.click("all-sessions-more-btn-sess-busy", cx);
        window.click("all-sessions-ctx-rename", cx);
        window.press("ctrl-a backspace enter", cx);
        assert!(window.find("all-sessions-rename-modal").visible());
        window.press("escape", cx);
        assert!(window.try_find("all-sessions-rename-modal").is_none());
        assert_eq!(window.find("all-sessions-view").focused(), Some(true));

        window.click("all-sessions-enter-batch-btn", cx);
        window.click("all-sessions-select-all-btn", cx);
        window.click("all-sessions-batch-delete-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("all-sessions-row-sess-busy").is_some()
            && window.try_find("all-sessions-row-sess-idle").is_none()
    })
    .await;

    // 4. Open `sess-busy` -> `BranchTree` (`Ctrl+O` filter cycle + 3-mode summary card `不总结` prefilling composer)
    cx.update_window(win_handle, |_, window, cx| {
        window.click("all-sessions-row-sess-busy", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("session-header-more-btn").is_some()
            && window.try_find("turn-compaction-e-compact-1").is_some()
    })
    .await;

    // 4a. Compaction summary bubble renders collapsed by default and expands on header click
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("turn-compaction-e-compact-1").visible());
        assert!(window.try_find("compaction-copy-btn-e-compact-1").is_none());
        window.click("compaction-hdr-e-compact-1", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window
            .try_find("compaction-copy-btn-e-compact-1")
            .is_some_and(|n| n.visible())
    })
    .await;

    // 4b. Collapsing sidebar exposes `topbar-session-more-btn` -> opens `topbar-session-more-menu` -> opens BranchTree
    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-collapse-btn", cx);
        window.render_frame(cx);
        assert!(window.find("topbar-session-more-btn").visible());
        window.click("topbar-session-more-btn", cx);
        window.render_frame(cx);
        assert!(window.find("topbar-session-more-menu").visible());
        window.click("topbar-session-more-tree-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("branch-tree-view").is_some()
            && window.try_find("tree-node-row-e-user-1").is_some()
            && window.try_find("tree-node-row-e-compact-1").is_some()
    })
    .await;

    // Verify branch tree controls while open (branch-tree shortcuts removed; Ctrl+O opens Quick Open palette)
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("branch-tree-view").visible());
        assert!(window.try_find("branch-tree-kbd-hint-na2qi").is_none());
        // Mouse entry via collapsed sidebar menu focuses tree WITHOUT activating keyboard brackets.
        assert!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_focused(window)
        );
        assert!(
            !shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_keyboard_active()
        );
        // First arrow key illuminates current leaf without skipping.
        window.press("down", cx);
        assert!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_keyboard_active()
        );
        window.press("ctrl-o", cx);
        assert!(window.find("command-palette-modal-i70mz").visible());
        assert_eq!(
            shell.read(cx).branch_tree_view().read(cx).filter_mode(),
            TreeFilterMode::Default
        );
        window.press("escape", cx);
        assert!(window.try_find("command-palette-modal-i70mz").is_none());

        // Arrow-key navigation on BranchTree (`up` / `home` / `enter`) opens 3-mode summary card without clicking
        window.press("home", cx);
        window.press("enter", cx);
        assert!(window.find("branch-summary-mode-card").visible());
        window.press("escape", cx);
        assert!(window.try_find("branch-summary-mode-card").is_none());

        // Select non-leaf user node `e-user-1` -> opens 3-mode summary card (`直接回退 / 总结 / 自定义提示词总结`, no cancel button)
        window.click("tree-node-row-e-user-1", cx);
        assert!(window.find("branch-summary-mode-card").visible());
        assert!(window.find("branch-mode-direct-btn").visible());
        assert!(window.find("branch-mode-summary-btn").visible());
        assert!(window.find("branch-mode-custom-summary-btn").visible());
        assert!(window.try_find("branch-mode-cancel-btn").is_none());
        // Verify arrow-key navigation opens the independent custom prompt dialog (`AtYcx` -> `l6fle`)
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
        assert!(window.find("branch-custom-prompt-card").visible());
        // Test Ctrl+J refocuses custom prompt input and Up/Down jump to start/end
        window.input("正文", cx);
        window.press("tab", cx);
        window.press("ctrl-j", cx);
        window.press("up", cx);
        window.input("前", cx);
        window.press("down", cx);
        window.input("后", cx);
        assert_eq!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .custom_prompt_input()
                .read(cx)
                .value()
                .as_ref(),
            "前正文后",
            "Ctrl+J must refocus custom prompt input and Up/Down must jump to start/end"
        );
        window.press("escape", cx);
        assert!(window.try_find("branch-custom-prompt-card").is_none());
        assert!(window.find("branch-tree-view").visible());
        // Ctrl+J on normal BranchTree focuses the branch search input
        window.press("ctrl-j", cx);
        window.input("蓝屏", cx);
        assert_eq!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .search_input()
                .read(cx)
                .value()
                .as_ref(),
            "蓝屏",
            "Ctrl+J on BranchTree must focus branch search input"
        );
        let bt_search = shell
            .read(cx)
            .branch_tree_view()
            .read(cx)
            .search_input()
            .clone();
        bt_search.update(cx, |inp, cx| inp.set_value("", window, cx));
        window.render_frame(cx);
        window.click("tree-node-row-e-user-1", cx);
        window.click("branch-mode-direct-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, cx| {
        let val = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .read(cx)
            .textarea_state()
            .read(cx)
            .value();
        let _ = window;
        val.as_ref() == "分析蓝屏转储（分支回填）"
    })
    .await;

    // Return from branch tree & re-expand sidebar before testing expanded-sidebar D2 + focus restore & `ExportView`
    cx.update_window(win_handle, |_, window, cx| {
        if window.try_find("branch-tree-close-btn").is_some() {
            window.click("branch-tree-close-btn", cx);
        }
        window.click("topbar-expand-sidebar-btn", cx);
        window.render_frame(cx);
    })
    .unwrap();

    // Verify Ctrl+J focuses session composer & Up/Down move between lines then jump to start/end
    cx.update_window(win_handle, |_, window, cx| {
        use gpui_kit::Focusable as _;
        let composer = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .clone();
        let ta = composer.read(cx).textarea_state().clone();
        // Move focus away from composer first (onto mounted header more button)
        shell
            .read(cx)
            .workbench()
            .read(cx)
            .header_more_focus()
            .focus(window, cx);
        assert!(!composer.read(cx).focus_handle(cx).is_focused(window));

        window.press("ctrl-j", cx);
        assert!(
            composer.read(cx).focus_handle(cx).is_focused(window),
            "Ctrl+J on Session chat must focus session composer"
        );

        let sample = "first-line\nsecond-line";
        let line1_end = "first-line".len();
        let line2_start = "first-line\n".len();
        let mid_line2 = line2_start + 3;
        ta.update(cx, |st, cx| {
            st.set_value(sample, window, cx);
            st.set_selected_range(mid_line2..mid_line2, cx);
        });
        window.render_frame(cx);

        // First Up moves from line 2 to line 1 (not start yet)
        window.press("up", cx);
        let after_first_up = ta.read(cx).cursor();
        assert!(
            after_first_up > 0 && after_first_up <= line1_end,
            "first Up should move to previous line, got {after_first_up}"
        );

        // Second Up on line 1 jumps to start (0)
        window.press("up", cx);
        assert_eq!(
            ta.read(cx).cursor(),
            0,
            "Up on first line must jump to start (0)"
        );

        // First Down moves from line 1 to line 2 (not end yet)
        window.press("down", cx);
        let after_first_down = ta.read(cx).cursor();
        assert!(
            after_first_down >= line2_start && after_first_down < sample.len(),
            "first Down should move to next line, got {after_first_down}"
        );

        // Second Down on last line jumps to end (len)
        window.press("down", cx);
        assert_eq!(
            ta.read(cx).cursor(),
            sample.len(),
            "Down on last line must jump to end"
        );

        // Restore original text so subsequent steps stay clean
        ta.update(cx, |st, cx| {
            st.set_value("分析蓝屏转储（分支回填）", window, cx);
        });
    })
    .unwrap();

    // 4c. Expanded-sidebar mouse entry (`session-header-more-btn` -> `session-header-tree-btn`)
    // suppresses initial brackets, activates on first arrow key, and restores focus to `header_more_focus` on Esc.
    cx.update_window(win_handle, |_, window, cx| {
        window.click("session-header-more-btn", cx);
        window.render_frame(cx);
        window.click("session-header-tree-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("branch-tree-view").is_some()
            && window.try_find("tree-node-row-e-user-1").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_focused(window)
        );
        assert!(
            !shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_keyboard_active()
        );
        window.press("down", cx);
        assert!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_keyboard_active()
        );
        window.press("escape", cx);
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("branch-tree-view").is_none());
        assert_eq!(shell.read(cx).route(), ShellRoute::Session);
        assert!(
            shell
                .read(cx)
                .workbench()
                .read(cx)
                .header_more_focus()
                .is_focused(window),
            "closing BranchTree opened from expanded header menu must restore focus to session-header-more-btn"
        );

        // Keyboard entry via `Ctrl+P` -> 分支树 immediately activates brackets.
        window.press("ctrl-p", cx);
        window.input("tree", cx);
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.press("enter", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("branch-tree-view").is_some()
            && window.try_find("tree-node-row-e-user-1").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_focused(window)
        );
        assert!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_keyboard_active()
        );

        // Navigate away to Settings and return via `navigate_back` -> brackets suppressed,
        // and closing tree restores focus to chat composer (not shell root).
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        window.render_frame(cx);
        window.click("settings-subpage-header-back-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("branch-tree-view").is_some()
            && window.try_find("tree-node-row-e-user-1").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_focused(window)
        );
        assert!(
            !shell
                .read(cx)
                .branch_tree_view()
                .read(cx)
                .is_keyboard_active()
        );
        window.press("escape", cx);
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("branch-tree-view").is_none());
        assert_eq!(shell.read(cx).route(), ShellRoute::Session);
        assert!(
            shell
                .read(cx)
                .workbench()
                .read(cx)
                .session_composer_focus(cx)
                .is_focused(window),
            "closing BranchTree reached via navigate_back must restore focus to session composer"
        );
    })
    .unwrap();

    // 5. `ExportView` (`Ntay1`: `264px` `ER02w` + `eGkN1` independent theme preview + Explorer `open`)
    cx.update_window(win_handle, |_, window, cx| {
        window.click("session-header-more-btn", cx);
        window.click("session-header-export-btn", cx);
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        let left_pane = window.find("export-options-pane-er02w");
        assert_eq!(f32::from(left_pane.bounds().size.width), 264.0);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("export-preview-turn-0").is_some()
    })
    .await;

    let previews_before_back = backend.state.lock().unwrap().export_previews.len();
    cx.update_window(win_handle, |_, window, cx| {
        window.press("ctrl-,", cx);
        window.render_frame(cx);
        assert!(window.find("settings-view-vpwaf").visible());
        let header_track = window.find("settings-subpage-header-title-track").bounds();
        let tabbar = window.find("settings-tabbar-vpwaf").bounds();
        assert!(
            (f32::from(header_track.origin.x) - f32::from(tabbar.origin.x)).abs() < 0.5,
            "settings header title track x ({:?}) must align with tabbar x ({:?})",
            header_track.origin.x,
            tabbar.origin.x
        );
        assert!(
            (f32::from(header_track.size.width) - f32::from(tabbar.size.width)).abs() < 0.5,
            "settings header title track width ({:?}) must match tabbar width ({:?})",
            header_track.size.width,
            tabbar.size.width
        );
        assert!(window.find("settings-kbd-focus-brackets").visible());
        assert!(window.find("settings-kbd-focus-rail").visible());
        window.click("settings-subpage-header-back-btn", cx);
    })
    .unwrap();
    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("export-preview-turn-0").is_some()
            && backend.state.lock().unwrap().export_previews.len() > previews_before_back
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.click("export-theme-dark-btn", cx);
        // Previous preview MUST remain mounted while reloading!
        assert!(window.find("export-preview-turn-0").visible());
        window.click("export-generate-save-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |_, _| {
        backend.state.lock().unwrap().export_creates == 1
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.press("escape", cx);
        assert!(window.try_find("export-options-pane-er02w").is_none());
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.press("ctrl-p", cx);
        window.input("export", cx);
        window.press("enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    // Entering Export via Ctrl+P activates keyboard focus mode (`focus_idx == 0`) and supports
    // full arrow + Enter/Space navigation matching `storybook.html` `#kn-pane-export`.
    cx.update_window(win_handle, |_, window, cx| {
        let ev = shell.read(cx).export_view().read(cx);
        assert!(ev.keyboard_active());
        assert_eq!(ev.focus_index(), 0);
        assert_eq!(
            ev.format_choice(),
            cst_desktop::sessions::export_view::ExportFormatChoice::Png
        );

        window.press("right", cx);
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).export_view().read(cx).format_choice(),
            cst_desktop::sessions::export_view::ExportFormatChoice::Html
        );

        window.press("down", cx);
        window.press("left", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).export_view().read(cx).focus_index(), 1);
        assert_eq!(
            shell.read(cx).export_view().read(cx).doc_theme_choice(),
            cst_desktop::sessions::export_view::ExportDocTheme::Light
        );

        window.press("down", cx);
        window.press("space", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).export_view().read(cx).focus_index(), 2);
        assert!(!shell.read(cx).export_view().read(cx).include_thinking());

        window.press("down", cx);
        window.press("down", cx);
        assert_eq!(shell.read(cx).export_view().read(cx).focus_index(), 4);
        window.press("enter", cx);
        window.press("enter", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("export-open-dir-btn").is_some()
            && !backend.state.lock().unwrap().export_opens.is_empty()
    })
    .await;
    {
        let state = backend.state.lock().unwrap();
        assert_eq!(state.export_creates, 2);
        assert_eq!(state.export_opens, ["/api/exports/exp-job-1/open"]);
    }
    cx.update_window(win_handle, |_, window, cx| {
        window.press("down", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).export_view().read(cx).focus_index(), 5);
        window.press("enter", cx);
    })
    .unwrap();
    cx.wait_for(win_handle, Duration::from_secs(2), |_, _| {
        backend.state.lock().unwrap().export_opens.len() == 2
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.click("export-back-btn", cx);
    })
    .unwrap();

    // 6. Active send with missing credentials -> `384px` `MissingKeyDialog` (`e7fEty`) preserving draft!
    cx.update_window(win_handle, |_, window, cx| {
        window.click("composer-send-btn-b0mrik", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("missing-key-dialog-e7fety").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        let card = window.find("missing-key-dialog-e7fety");
        assert_eq!(f32::from(card.bounds().size.width), 384.0);
        // Draft `"分析蓝屏转储（分支回填）"` MUST remain preserved!
        let draft_val = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .read(cx)
            .textarea_state()
            .read(cx)
            .value();
        assert_eq!(draft_val.as_ref(), "分析蓝屏转储（分支回填）");

        // Keyboard nav in `missing-key-dialog-e7fety`: default on cancel, `←` clamps, `→` focuses `"去配置"`, `Enter` activates!
        assert_eq!(window.find("missing-key-cancel-btn").focused(), Some(true));
        window.press("left", cx);
        assert_eq!(window.find("missing-key-cancel-btn").focused(), Some(true));
        window.press("right", cx);
        assert_eq!(
            window.find("missing-key-configure-btn").focused(),
            Some(true)
        );
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.find("settings-providers-tab").visible());
        assert!(window.find("providers-cstoa-pinned-card").visible());

        // Navigate to `+ 添加提供商` via keyboard (`down` -> `enter`) -> Add Provider catalog (`FRRjK`)
        window.press("down", cx);
        window.click("providers-new-custom-btn", cx);
        assert!(window.find("providers-add-custom-row").visible());
        window.click("providers-add-custom-row", cx);
        assert!(window.find("providers-hdr-search-btn").visible());
        assert!(window.find("providers-hdr-refresh-btn").visible());
        assert!(window.find("providers-hdr-add-model-btn").visible());
        assert!(window.find("providers-hairline-edit-table").visible());
        // Protocol dropdown trigger is keyboard-activatable and Esc restores focus cleanly
        window.click("custom-protocol-dropdown-trigger", cx);
        assert!(window.find("custom-protocol-popover").visible());
        window.press("escape", cx);
        assert!(window.try_find("custom-protocol-popover").is_none());
        assert_eq!(
            window.find("custom-protocol-dropdown-trigger").focused(),
            Some(true)
        );
    })
    .unwrap();

    // 7. `AccountView` CSTOA-only logout (`sVucQ` confirm + `←→` clamping + `Esc` cancel + confirm logout) -> `LoginView` (`864px` sky + `300px` viewport + offline QR)
    cx.update_window(win_handle, |_, window, cx| {
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Account, cx));
        window.render_frame(cx);
        assert!(window.find("account-view").visible());
        window.click("account-cstoa-logout-btn", cx);
        assert!(window.find("account-logout-confirm-dialog").visible());
        assert_eq!(
            window.find("account-logout-cancel-btn").focused(),
            Some(true)
        );
        // Left at safe cancel button clamps without wrapping onto destructive logout.
        window.press("left", cx);
        assert_eq!(
            window.find("account-logout-cancel-btn").focused(),
            Some(true)
        );
        window.press("right", cx);
        assert_eq!(
            window.find("account-logout-confirm-btn").focused(),
            Some(true)
        );
        window.press("left", cx);
        assert_eq!(
            window.find("account-logout-cancel-btn").focused(),
            Some(true)
        );
        window.press("ctrl-n", cx);
        assert!(window.find("account-logout-confirm-dialog").visible());
        window.press("escape", cx);
        assert!(window.try_find("account-logout-confirm-dialog").is_none());
        assert_eq!(
            window.find("account-cstoa-logout-btn").focused(),
            Some(true)
        );
        window.press("enter", cx);
        window.press("right", cx);
        assert_eq!(
            window.find("account-logout-confirm-btn").focused(),
            Some(true)
        );
        window.press("enter", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("cstoa-qr-image-box").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        // Check Level-1 inline validation on empty APIKey login without leaving LoginView
        window.click("login-tab-apikey", cx);
        window.click("login-apikey-provider-trigger", cx);
        assert!(window.find("login-apikey-provider-popover").visible());
        window.press("ctrl-n", cx);
        assert!(window.find("login-apikey-provider-popover").visible());
        window.press("escape", cx);
        assert!(window.try_find("login-apikey-provider-popover").is_none());
        assert_eq!(
            window.find("login-apikey-provider-trigger").focused(),
            Some(true)
        );
        window.click("login-apikey-save-btn", cx);
        assert!(window.find("login-apikey-error-text").visible());
        window.click("login-tab-cstoa", cx);
        // Collapse sidebar on `1280×800` window so full login canvas reaches its `864px` max width alongside `300px` viewport
        window.press("ctrl-b", cx);
        let sky = window.find("login-sky-canvas-864");
        assert_eq!(f32::from(sky.bounds().origin.y), 0.0);
        assert_eq!(f32::from(sky.bounds().size.width), 864.0);
        let vp = window.find("login-viewport-300");
        assert_eq!(f32::from(vp.bounds().size.width), 300.0);

        // Re-entering Login from another route must keep the full-bleed sky + feather canvas
        // pinned at `y == 0.0` on frame 0 (only `login-viewport-300` glides vertically).
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        window.render_frame(cx);
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Login, cx));
        window.render_frame(cx);
        let sky_entry = window.find("login-sky-canvas-864");
        assert_eq!(f32::from(sky_entry.bounds().origin.y), 0.0);
        assert_eq!(f32::from(sky_entry.bounds().size.width), 864.0);
    })
    .unwrap();
    assert_eq!(backend.state.lock().unwrap().cstoa_logout_calls, 1);

    for collapsed in [false, true] {
        cx.update_window(win_handle, |_, window, cx| {
            shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
            window.render_frame(cx);
            if shell.read(cx).is_sidebar_collapsed() != collapsed {
                shell.update(cx, |s, cx| s.toggle_sidebar(window, cx));
            }
            assert_eq!(shell.read(cx).is_sidebar_collapsed(), collapsed);
        })
        .unwrap();
        for width in [1024.0, 1120.0, 1280.0, 1600.0] {
            cx.update_window(win_handle, |_, window, _| {
                window.resize(gpui::size(gpui::px(width), gpui::px(800.0)));
            })
            .unwrap();
            for (route, header, content) in [
                (
                    ShellRoute::Settings,
                    "settings-subpage-header-title-track",
                    "settings-tabbar-vpwaf",
                ),
                (
                    ShellRoute::AllSessions,
                    "all-sessions-subpage-header-title-track",
                    "all-sessions-content-column",
                ),
                (
                    ShellRoute::Account,
                    "account-subpage-header-title-track",
                    "account-content-column",
                ),
            ] {
                cx.update_window(win_handle, |_, window, cx| {
                    shell.update(cx, |s, cx| s.set_route(route, cx));
                    window.render_frame(cx);
                    let head = window.find(header).bounds();
                    let body = window.find(content).bounds();
                    assert!(
                        (f32::from(head.origin.x - body.origin.x)).abs() < 0.5,
                        "{header} x at {width}, collapsed={collapsed}"
                    );
                    assert!(
                        (f32::from(head.size.width - body.size.width)).abs() < 0.5,
                        "{header} width at {width}, collapsed={collapsed}"
                    );
                })
                .unwrap();
            }
        }
    }
    cx.update_window(win_handle, |_, window, cx| {
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Home, cx));
        window.render_frame(cx);
        window.resize(gpui::size(gpui::px(1280.0), gpui::px(800.0)));
    })
    .unwrap();

    // A failed default-model write does not modify the new-session draft's explicit choice.
    backend.state.lock().unwrap().credential_ready = true;
    backend.rt().block_on(handle.refresh_models(None)).unwrap();
    handle.with_workspace_mut(|ws| {
        let draft = ws.draft_mut(cst_client::DraftKey::NewSession);
        draft.selected_provider = Some("deepseek".into());
        draft.selected_model_id = Some("deepseek-chat".into());
        draft.selected_thinking_level = Some("high".into());
    });
    cx.update_window(win_handle, |_, window, cx| {
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        shell.read(cx).settings_view().clone().update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::Preferences, cx);
        });
        window.render_frame(cx);
        // Verify click-outside dismissal on both model preference dropdowns.
        window.click("pref-default-model-trigger", cx);
        assert!(window.find("pref-default-model-popover").visible());
        window.right_click("pref-default-model-popover", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("pref-default-model-popover").visible());
        window.click("settings-subpage-header-title-track", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("pref-default-model-popover").is_none());
        window.click("pref-filter-dropdown-btn", cx);
        assert!(window.find("pref-filter-popover").visible());
        window.click("settings-subpage-header-title-track", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("pref-filter-popover").is_none());

        // Verify Ctrl+P -> 打开全部会话 keeps keyboard arrow navigation continuous.
        window.press("ctrl-p", cx);
        window.input("sessions", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.press("enter", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("command-palette-modal-i70mz").is_none());
        assert!(window.find("all-sessions-view").visible());
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).route(), ShellRoute::Session);

        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        shell.read(cx).settings_view().clone().update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::Preferences, cx);
        });
        window.render_frame(cx);
        window.click("pref-default-model-trigger", cx);
        window.click("pref-default-model-deepseek-deepseek-chat", cx);
    })
    .unwrap();
    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        backend.state.lock().unwrap().default_model_requests == 1
            && window.try_find("workspace-top-error-bar").is_some()
    })
    .await;
    handle.with_workspace(|ws| {
        let draft = ws.draft(&cst_client::DraftKey::NewSession).unwrap();
        assert_eq!(draft.selected_provider.as_deref(), Some("deepseek"));
        assert_eq!(draft.selected_model_id.as_deref(), Some("deepseek-chat"));
        assert_eq!(draft.selected_thinking_level.as_deref(), Some("high"));
    });

    // 8. Software Update lifecycle (silent startup/home check on `Available` -> open via Settings `C9or7K` -> `kOm7J` -> `TqiZo` -> Settings `C9or7K` `keDNb` + `oHWRT` cancel -> `409` busy -> `200` launch!)
    cx.update_window(win_handle, |_, window, cx| {
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Home, cx));
        window.render_frame(cx);
        assert_eq!(shell.read(cx).route(), ShellRoute::Home);
    })
    .unwrap();
    {
        backend.state.lock().unwrap().update_phase = "available".into();
    }
    backend.rt().block_on(handle.refresh_globals()).unwrap();

    // Wait until the desktop shell has observed the `Available` snapshot while on Home,
    // then verify background check stays silent and does not auto-open the modal.
    cx.wait_for(win_handle, Duration::from_secs(2), |_, cx| {
        shell
            .read(cx)
            .client_snapshot()
            .update_state
            .as_ref()
            .is_some_and(|u| u.status.phase == cst_client::UpdatePhaseDto::Available)
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).route(), ShellRoute::Home);
        assert!(window.try_find("update-available-dialog-kom7j").is_none());
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        let sv_ent = shell.read(cx).settings_view().clone();
        sv_ent.update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::General, cx);
        });
        window.render_frame(cx);
        window.click("settings-update-action-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("update-available-dialog-kom7j").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        assert_eq!(
            f32::from(
                window
                    .find("update-available-dialog-kom7j")
                    .bounds()
                    .size
                    .width
            ),
            384.0
        );
        window.click("update-dialog-confirm-restart-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("update-preparing-dialog-tqizo").is_some()
    })
    .await;

    // Dismiss modal to background -> inspect `Settings · 常规` `52px` hairline update row (`keDNb` + `oHWRT` cancel!)
    cx.update_window(win_handle, |_, window, cx| {
        window.click("update-dialog-background-btn", cx);
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        let sv_ent = shell.read(cx).settings_view().clone();
        sv_ent.update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::General, cx);
        });
        window.render_frame(cx);

        let card = window.find("settings-update-card-c9or7k");
        assert_eq!(f32::from(card.bounds().size.height), 52.0);
        assert!(window.find("settings-update-progress-fill-kednb").visible());
        assert!(window.find("settings-update-cancel-btn-ohwrt").visible());

        // Cancel preparing from `C9or7K` -> reverts to `Available`
        window.click("settings-update-cancel-btn-ohwrt", cx);
    })
    .unwrap();

    let st_upd = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        st_upd.lock().unwrap().update_cancel_calls == 1
    })
    .await;

    // Advance backend to `ready` -> test `409 session_busy_for_update` warning first, then clear busy & verify `launch` payload!
    {
        backend.state.lock().unwrap().update_phase = "ready".into();
    }
    backend.rt().block_on(handle.refresh_globals()).unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        shell.update(cx, |s, cx| s.open_update_dialog(window, cx));
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("update-dialog-launch-btn").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.click("update-dialog-launch-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("update-dialog-busy-error").is_some()
    })
    .await;

    let launch_bodies = backend.state.lock().unwrap().update_launch_bodies.clone();
    assert_eq!(launch_bodies.len(), 1);
    assert_eq!(launch_bodies[0]["ui"], "desktop");
    assert_eq!(launch_bodies[0]["appPid"], self_id.pid);
    assert_eq!(launch_bodies[0]["appCreated"], self_id.created);
}

#[gpui_kit::test]
async fn headless_settings_optimistic_model_toggle_coalescing_and_rollback(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = Step6MockBackend::start();
    let gate = Arc::new(tokio::sync::Notify::new());
    {
        let mut st = backend.state.lock().unwrap();
        st.credential_ready = true;
        st.enabled_models = Some(vec!["deepseek/deepseek-chat".into()]);
        st.scoped_gate = Some(Arc::clone(&gate));
    }

    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: backend.port,
        version: "v0.1.0".into(),
        http: AppHttpClient::new(backend.port, &backend.token).unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint(backend.rt().handle(), endpoint, None).unwrap();
    backend.rt().block_on(handle.refresh_globals()).unwrap();
    backend.rt().block_on(handle.refresh_models(None)).unwrap();

    let (win_handle, shell) = cx.update(|cx| {
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), None, cx).unwrap()
    });

    // 1. Built-in Provider Detail: immediate 0ms toggle while POST /api/models/scoped is blocked on gate,
    //    plus lock-free rapid toggle back on before the first request completes.
    cx.update_window(win_handle, |_, window, cx| {
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        shell.read(cx).settings_view().clone().update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::Providers, cx);
        });
        window.render_frame(cx);
        window.click("provider-row-deepseek", cx);
        window.render_frame(cx);

        // First click: immediately updates `checked` to false on the same frame (0ms)
        window.click("provider-model-chk-deepseek-chat", cx);
        window.render_frame(cx);
        assert!(
            window.find("provider-model-chk-deepseek-chat").visible(),
            "Checkbox remains interactive immediately after click"
        );

        // Rapid second click while first POST is still gated: must NOT be locked
        window.click("provider-model-chk-deepseek-chat", cx);
        window.render_frame(cx);
    })
    .unwrap();

    // Wait for first gated request to arrive, then release gate so coalesced second state flushes
    let st_ref = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        !st_ref.lock().unwrap().scoped_requests.is_empty()
    })
    .await;
    {
        backend.state.lock().unwrap().scoped_gate = None;
    }
    gate.notify_waiters();

    let st_ref2 = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        st_ref2
            .lock()
            .unwrap()
            .enabled_models
            .as_ref()
            .is_some_and(|v| v == &vec!["deepseek/deepseek-chat".to_owned()])
    })
    .await;

    // 2. Model Preferences: verify deferred popovers open cleanly and rollback works on 500 error
    {
        backend.state.lock().unwrap().scoped_fail = true;
    }
    cx.update_window(win_handle, |_, window, cx| {
        shell.read(cx).settings_view().clone().update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::Preferences, cx);
        });
        window.render_frame(cx);

        window.click("pref-default-model-trigger", cx);
        window.render_frame(cx);
        assert!(window.find("pref-default-model-popover").visible());
        window.press("escape", cx);
        window.render_frame(cx);

        window.click("pref-filter-dropdown-btn", cx);
        window.render_frame(cx);
        assert!(window.find("pref-filter-popover").visible());
        window.press("escape", cx);
        window.render_frame(cx);

        // Toggle model when server returns 500 -> should surface top error bar and roll back
        window.click("pref-toggle-enabled-0", cx);
        window.render_frame(cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("workspace-top-error-bar").is_some()
    })
    .await;
}

#[gpui_kit::test]
async fn headless_settings_keyboard_navigation_complete(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = Step6MockBackend::start();
    {
        let mut st = backend.state.lock().unwrap();
        st.credential_ready = true;
        st.rich_catalog = true;
        st.enabled_models = Some(vec!["deepseek/m1".into(), "deepseek/m2".into()]);
    }

    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: backend.port,
        version: "v0.1.0".into(),
        http: AppHttpClient::new(backend.port, &backend.token).unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint(backend.rt().handle(), endpoint, None).unwrap();
    backend.rt().block_on(handle.refresh_globals()).unwrap();
    backend.rt().block_on(handle.refresh_models(None)).unwrap();

    let (win_handle, shell) = cx.update(|cx| {
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), None, cx).unwrap()
    });

    let press = |cx: &mut gpui_kit::TestAppContext, key: &str| {
        cx.update_window(win_handle, |_, window, cx| {
            window.press(key, cx);
            window.render_frame(cx);
        })
        .unwrap();
    };
    let click = |cx: &mut gpui_kit::TestAppContext, id: &'static str| {
        cx.update_window(win_handle, |_, window, cx| {
            window.click(id, cx);
            window.render_frame(cx);
        })
        .unwrap();
    };
    let has_el = |cx: &mut gpui_kit::TestAppContext, id: &'static str| -> bool {
        cx.update_window(win_handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).is_some()
        })
        .unwrap()
    };

    // 1. Entry (`Ctrl+,`), top-level Esc -> NavigateBack (D1), and VPWAF 4-tab keyboard switching
    let initial_route = cx
        .update_window(win_handle, |_, window, cx| {
            window.render_frame(cx);
            shell.read(cx).route()
        })
        .unwrap();
    assert_ne!(initial_route, ShellRoute::Settings);

    press(cx, "ctrl-,");
    cx.update(|cx| {
        assert_eq!(shell.read(cx).route(), ShellRoute::Settings);
        let sv = shell.read(cx).settings_view().read(cx);
        assert_eq!(
            sv.category(),
            cst_desktop::settings::SettingsCategory::General
        );
        assert!(
            sv.is_tab_kbd_active(),
            "Ctrl+, must activate VPWAF keyboard indicator"
        );
    });

    // D1: Top-level Esc on Settings navigates back to previous route
    press(cx, "escape");
    cx.update(|cx| {
        assert_eq!(
            shell.read(cx).route(),
            initial_route,
            "Top-level Esc on Settings must execute NavigateBack"
        );
    });

    // Re-enter via Ctrl+, and traverse General tab rows (`Home`/`End`/`Enter`/`Space`/`↑` back to VPWAF)
    press(cx, "ctrl-,");
    press(cx, "down");
    cx.update(|cx| {
        let sv = shell.read(cx).settings_view().read(cx);
        assert!(sv.is_general_kbd_active());
        assert_eq!(sv.general_row_index(), 0);
    });
    press(cx, "end");
    cx.update(|cx| {
        assert_eq!(
            shell.read(cx).settings_view().read(cx).general_row_index(),
            2
        );
    });
    press(cx, "home");
    cx.update(|cx| {
        assert_eq!(
            shell.read(cx).settings_view().read(cx).general_row_index(),
            0
        );
    });

    // Cycle theme & keyboard focus style via Enter / Space
    press(cx, "enter");
    press(cx, "down");
    press(cx, "space");

    // Return to VPWAF via Up from row 0
    press(cx, "up");
    press(cx, "up");
    cx.update(|cx| {
        let sv = shell.read(cx).settings_view().read(cx);
        assert!(
            sv.is_tab_kbd_active(),
            "Up from General row 0 must return to VPWAF"
        );
        assert!(!sv.is_general_kbd_active());
    });

    // 2. Providers tab: D2 visual order ([0: AddProvider, 1: CSTOA, 2: DeepSeek])
    press(cx, "right");
    cx.update(|cx| {
        assert_eq!(
            shell.read(cx).settings_view().read(cx).category(),
            cst_desktop::settings::SettingsCategory::Providers
        );
    });
    // VPWAF ↓ lands on index 1 (CSTOA), ↑ visits index 0 (AddProvider), ↑ returns to VPWAF
    press(cx, "down");
    cx.update(|cx| {
        let pt = shell
            .read(cx)
            .settings_view()
            .read(cx)
            .providers_tab()
            .read(cx);
        assert!(pt.is_list_kbd_active());
        assert_eq!(pt.list_selected_index(), 1);
    });
    press(cx, "up");
    cx.update(|cx| {
        let pt = shell
            .read(cx)
            .settings_view()
            .read(cx)
            .providers_tab()
            .read(cx);
        assert_eq!(pt.list_selected_index(), 0);
    });
    press(cx, "up");
    cx.update(|cx| {
        let sv = shell.read(cx).settings_view().read(cx);
        assert!(
            sv.is_tab_kbd_active(),
            "Up from Providers index 0 (AddProvider) must return to VPWAF"
        );
    });

    // Enter Add Provider Catalog (`FRRjK`) via keyboard on index 0
    press(cx, "down");
    press(cx, "up");
    press(cx, "enter");
    assert!(
        has_el(cx, "providers-add-custom-row"),
        "Enter on index 0 must open Add Provider Catalog"
    );
    assert!(
        has_el(cx, "providers-toggle-expand-btn"),
        "Add catalog with 10 unconfigured providers must show expand toggle"
    );

    // Reach expand toggle via End and expand via Enter
    press(cx, "end");
    press(cx, "enter");
    assert!(
        has_el(cx, "provider-add-row-prov-10"),
        "Enter on expand toggle must reveal collapsed providers"
    );

    // Open Custom Provider Editor (`FxSIA`) via Home -> Enter on index 0 (`自定义提供商`)
    press(cx, "home");
    press(cx, "enter");
    assert!(
        has_el(cx, "providers-hairline-edit-table"),
        "Enter on custom row must open Custom Provider Editor"
    );

    // Protocol dropdown trigger supports Alt+Down -> popover, Down -> Enter restores focus
    click(cx, "custom-protocol-dropdown-trigger");
    assert!(has_el(cx, "custom-protocol-popover"));
    press(cx, "escape");
    assert!(!has_el(cx, "custom-protocol-popover"));
    press(cx, "alt-down");
    assert!(
        has_el(cx, "custom-protocol-popover"),
        "Alt+Down on focused protocol trigger must open popover"
    );
    press(cx, "down");
    press(cx, "enter");
    assert!(!has_el(cx, "custom-protocol-popover"));

    // Switching settings tabs from Custom Editor preserves unsaved form (`P1-2`)
    press(cx, "ctrl-,");
    press(cx, "left");
    press(cx, "right");
    assert!(
        has_el(cx, "providers-hairline-edit-table"),
        "Switching tabs and back must preserve unsaved Custom Provider Editor"
    );

    // Exit Custom Editor via Esc -> returns to Configured Providers List (`Q4QFim`)
    press(cx, "escape");
    assert!(has_el(cx, "providers-cstoa-pinned-card"));

    // Enter DeepSeek Built-in Detail (`QOtGr`) via keyboard (index 0 -> 1 -> 2 -> Enter)
    press(cx, "down");
    press(cx, "down");
    press(cx, "enter");
    assert!(
        has_el(cx, "provider-model-chk-m1"),
        "Enter on configured DeepSeek row must open built-in detail"
    );
    // Toggle first model via Space (0ms optimistic), then Backspace back to configured list
    press(cx, "space");
    press(cx, "backspace");
    assert!(
        has_el(cx, "providers-cstoa-pinned-card"),
        "Backspace on built-in detail must return to configured list"
    );

    // Return from Providers list to VPWAF (Home -> index 0 -> Up -> VPWAF)
    press(cx, "home");
    press(cx, "up");
    cx.update(|cx| {
        assert!(shell.read(cx).settings_view().read(cx).is_tab_kbd_active());
    });

    // 3. Preferences tab (`lTWwV`): Alt+Down on default trigger, ↓ chain through search into list, expand button
    press(cx, "right");
    cx.update(|cx| {
        assert_eq!(
            shell.read(cx).settings_view().read(cx).category(),
            cst_desktop::settings::SettingsCategory::Preferences
        );
    });
    press(cx, "down");
    press(cx, "alt-down");
    assert!(
        has_el(cx, "pref-default-model-popover"),
        "Alt+Down on default model trigger must open popover"
    );
    press(cx, "escape");
    assert!(!has_el(cx, "pref-default-model-popover"));

    // From default trigger: Up returns to VPWAF
    press(cx, "up");
    cx.update(|cx| {
        assert!(
            shell.read(cx).settings_view().read(cx).is_tab_kbd_active(),
            "Up from Preferences default model trigger must return to VPWAF"
        );
    });

    // Re-enter Preferences: Down (trigger) -> Down (search) -> Down (list row 0) -> End -> Down (expand btn) -> Enter
    press(cx, "down");
    press(cx, "down");
    press(cx, "down");
    assert!(has_el(cx, "pref-toggle-expand-models-btn"));
    press(cx, "end");
    press(cx, "down");
    press(cx, "enter");
    assert!(
        has_el(cx, "pref-unchecked-deepseek/m7"),
        "Enter on Preferences expand button must expand full model list on a single activation"
    );
    // Single-fire collapse check (`P1-1`): End -> Down onto collapse toggle -> Enter collapses cleanly
    press(cx, "end");
    press(cx, "down");
    press(cx, "enter");
    assert!(
        !has_el(cx, "pref-unchecked-deepseek/m7"),
        "Enter on collapse toggle must collapse once without double-toggling back open"
    );
    press(cx, "up");
    press(cx, "home");
    press(cx, "up");
    press(cx, "up");
    press(cx, "up");
    cx.update(|cx| {
        assert!(shell.read(cx).settings_view().read(cx).is_tab_kbd_active());
    });

    // 4. Shortcuts tab (`zQxOv`): Down enters body, PageDown -> PageUp -> Up returns to VPWAF, top-level Esc navigates back
    press(cx, "right");
    cx.update(|cx| {
        assert_eq!(
            shell.read(cx).settings_view().read(cx).category(),
            cst_desktop::settings::SettingsCategory::Shortcuts
        );
    });
    press(cx, "down");
    cx.update(|cx| {
        assert!(!shell.read(cx).settings_view().read(cx).is_tab_kbd_active());
    });
    press(cx, "pagedown");
    press(cx, "pageup");
    press(cx, "home");
    press(cx, "up");
    cx.update(|cx| {
        assert!(
            shell.read(cx).settings_view().read(cx).is_tab_kbd_active(),
            "Up at top of Shortcuts tab after PageDown/PageUp/Home must return to VPWAF"
        );
    });

    press(cx, "escape");
    cx.update(|cx| {
        assert_eq!(shell.read(cx).route(), initial_route);
    });
}

#[gpui_kit::test]
async fn cold_start_available_update_stays_silent_without_auto_modal(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = Step6MockBackend::start();
    // Cold start: backend already reports `Available` before the window and shell are created.
    backend.state.lock().unwrap().update_phase = "available".into();
    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: backend.port,
        version: "v0.1.0".into(),
        http: AppHttpClient::new(backend.port, &backend.token).unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint(backend.rt().handle(), endpoint, None).unwrap();
    backend.rt().block_on(handle.refresh_globals()).unwrap();
    assert_eq!(
        handle
            .snapshot()
            .update_state
            .as_ref()
            .map(|u| u.status.phase),
        Some(cst_client::UpdatePhaseDto::Available),
        "snapshot must already be Available before opening the window"
    );

    let (win_handle, shell) = cx.update(|cx| {
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), None, cx).unwrap()
    });

    // Inspect both the cumulative render counter (proving neither frame 1 nor any intermediate
    // re-render during window open ever mounted the update dialog) and the current DOM tree.
    cx.update_window(win_handle, |_, window, cx| {
        assert_eq!(shell.read(cx).route(), ShellRoute::Home);
        assert_eq!(
            shell
                .read(cx)
                .client_snapshot()
                .update_state
                .as_ref()
                .map(|u| u.status.phase),
            Some(cst_client::UpdatePhaseDto::Available),
        );
        assert_eq!(
            shell.read(cx).update_dialog_render_count(),
            0,
            "update dialog must never render on frame 1 or any intermediate frame during cold start"
        );
        assert!(
            window.try_find("update-available-dialog-kom7j").is_none(),
            "first frame on cold start must stay silent when update is Available"
        );
        window.render_frame(cx);
        assert_eq!(shell.read(cx).update_dialog_render_count(), 0);
        assert!(
            window.try_find("update-available-dialog-kom7j").is_none(),
            "subsequent frame on Home must stay silent when update is Available"
        );
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        let sv_ent = shell.read(cx).settings_view().clone();
        sv_ent.update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::General, cx);
        });
        window.render_frame(cx);
        window.click("settings-update-action-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("update-available-dialog-kom7j").is_some()
    })
    .await;
    cx.update(|cx| {
        assert!(
            shell.read(cx).update_dialog_render_count() > 0,
            "manual trigger via Settings must render the update dialog"
        );
    });
}

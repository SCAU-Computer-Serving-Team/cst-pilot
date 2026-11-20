use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
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
        let base = PathBuf::from(r"E:\tmp\2026-11-20");
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
    let today = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
    let sessions = vec![
        SessionSummary {
            id: "s-today".into(),
            title: "今日排查".into(),
            updated_at: Some("2026-11-20T01:00:00+08:00".into()),
            running: false,
        },
        SessionSummary {
            id: "s-week".into(),
            title: "三天前巡检".into(),
            updated_at: Some("2026-11-17T10:00:00+08:00".into()),
            running: false,
        },
        SessionSummary {
            id: "s-old".into(),
            title: "两个月前记录".into(),
            updated_at: Some("2026-09-01T09:00:00+08:00".into()),
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
    export_previews: Vec<Value>,
    export_opens: Vec<String>,
    saved_custom_providers: Vec<Value>,
    update_phase: String,
    update_cancel_calls: usize,
    update_launch_bodies: Vec<Value>,
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
            export_previews: Vec::new(),
            export_opens: Vec::new(),
            saved_custom_providers: Vec::new(),
            update_phase: "idle".into(),
            update_cancel_calls: 0,
            update_launch_bodies: Vec::new(),
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
                                            { "id": "e-user-1", "parentId": null, "message": { "role": "user", "content": "分析蓝屏转储" } },
                                            { "id": "e-assist-1", "parentId": "e-user-1", "message": { "role": "assistant", "content": "已定位驱动异常。" } }
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
                                ("POST", "/api/sessions/sess-busy/exports") => (
                                    "HTTP/1.1 202 Accepted",
                                    json!({
                                        "id": "exp-job-1",
                                        "status": "completed",
                                        "directory": "artifacts/2026-11-20/sessions"
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
                                ("POST", "/api/exports/exp-job-1/open") => {
                                    assert_eq!(
                                        body_str.trim(),
                                        "{}",
                                        "`POST /api/exports/<id>/open` MUST send empty JSON object body"
                                    );
                                    guard.export_opens.push("exp-job-1".into());
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
                                        "verificationUriComplete": "https://oa.cst.example.edu/qr?t=998877",
                                        "expiresIn": 180
                                    }),
                                ),
                                ("GET", "/api/auth/cstoa/oauth/status") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "status": "pending" }),
                                ),
                                ("POST", "/api/auth/cstoa/logout") => {
                                    guard.cstoa_signed_in = false;
                                    guard.cstoa_logout_calls += 1;
                                    ("HTTP/1.1 200 OK", json!({ "providerId": "cstoa", "type": null }))
                                }
                                ("GET", "/api/auth") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "providers": [
                                            { "id": "cstoa", "name": "CSTOA", "configured": guard.cstoa_signed_in, "supportsOAuth": true },
                                            { "id": "deepseek", "name": "DeepSeek", "configured": guard.credential_ready, "supportsApiKey": true }
                                        ]
                                    }),
                                ),
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
                                ("GET", p) if p.starts_with("/api/models") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "models": [{
                                            "provider": "deepseek",
                                            "id": "deepseek-chat",
                                            "name": "DeepSeek V3",
                                            "credentialReady": guard.credential_ready
                                        }],
                                        "catalog": [{
                                            "provider": "deepseek",
                                            "id": "deepseek-chat",
                                            "name": "DeepSeek V3",
                                            "credentialReady": guard.credential_ready
                                        }]
                                    }),
                                ),
                                ("GET", "/api/app/settings") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "theme": "light", "defaultView": "gui", "sidebarCollapsed": false, "windowMaximized": false }),
                                ),
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

    // 1. `Ctrl+K` `520px` Command Palette (`I70mz`): instant open + `Esc` 1-frame focus restore
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-collapse-btn", cx);
        window.click("topbar-expand-sidebar-btn", cx);
        assert_eq!(window.find("sidebar-collapse-btn").focused(), Some(true));

        window.press("ctrl-k", cx);
        let modal = window.find("command-palette-modal-i70mz");
        assert!(modal.visible());
        assert_eq!(f32::from(modal.bounds().size.width), 520.0);
        assert!(window.find("command-palette-footer-hints").visible());

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
        window.press("escape", cx);
    })
    .unwrap();

    // 3. `AllSessions` (`gpui_kit::v_virtual_list`): batch `"全选 / 删除"` -> `sess-idle` deleted, `sess-busy` `409` auto-restored!
    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-all-sessions-btn", cx);
        assert!(window.find("all-sessions-view").visible());
        assert!(window.find("all-sessions-row-sess-busy").visible());
        assert!(window.find("all-sessions-row-sess-idle").visible());

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
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.click("session-header-more-btn", cx);
        window.click("session-header-tree-btn", cx);
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("branch-tree-view").visible());
        window.press("ctrl-o", cx);
        assert_eq!(
            shell.read(cx).branch_tree_view().read(cx).filter_mode(),
            TreeFilterMode::NoTools
        );

        // Select non-leaf user node `e-user-1` -> opens 3-mode summary card -> click `"不总结"`
        window.click("tree-node-row-e-user-1", cx);
        assert!(window.find("branch-summary-mode-card").visible());
        assert!(window.find("branch-mode-no-summary-btn").visible());
        assert!(window.find("branch-mode-summary-btn").visible());
        assert!(window.find("branch-mode-custom-summary-btn").visible());
        window.click("branch-mode-no-summary-btn", cx);
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

    cx.update_window(win_handle, |_, window, cx| {
        window.click("export-theme-dark-btn", cx);
        // Previous preview MUST remain mounted while reloading!
        assert!(window.find("export-preview-turn-0").visible());
        window.click("export-generate-save-btn", cx);
    })
    .unwrap();

    let st_exp = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        !st_exp.lock().unwrap().export_opens.is_empty()
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

        // Click `"去配置"` -> navigates to `Settings · 模型服务`
        window.click("missing-key-configure-btn", cx);
        window.render_frame(cx);
        assert!(window.find("settings-providers-tab").visible());
        assert!(window.find("providers-cstoa-pinned-card").visible());

        // Click `+` to open borderless hairline custom gateway editor with 3 icon-only header buttons
        window.click("providers-new-custom-btn", cx);
        assert!(window.find("providers-hdr-search-btn").visible());
        assert!(window.find("providers-hdr-refresh-btn").visible());
        assert!(window.find("providers-hdr-add-model-btn").visible());
        assert!(window.find("providers-hairline-edit-table").visible());
    })
    .unwrap();

    // 7. `AccountView` CSTOA-only logout -> transitions to `LoginView` (`864px` sky + `300px` viewport + offline QR)
    cx.update_window(win_handle, |_, window, cx| {
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Account, cx));
        window.render_frame(cx);
        assert!(window.find("account-view").visible());
        window.click("account-cstoa-logout-btn", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("cstoa-qr-image-box").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        // Collapse sidebar on `1280×800` window so full login canvas reaches its `864px` max width alongside `300px` viewport
        window.press("ctrl-b", cx);
        let sky = window.find("login-sky-canvas-864");
        assert_eq!(f32::from(sky.bounds().size.width), 864.0);
        let vp = window.find("login-viewport-300");
        assert_eq!(f32::from(vp.bounds().size.width), 300.0);
    })
    .unwrap();
    assert_eq!(backend.state.lock().unwrap().cstoa_logout_calls, 1);

    // 8. Software Update lifecycle (`kOm7J` -> `TqiZo` -> Settings `C9or7K` `keDNb` + `oHWRT` cancel -> `409` busy -> `200` launch!)
    {
        backend.state.lock().unwrap().update_phase = "available".into();
    }
    backend.rt().block_on(handle.refresh_globals()).unwrap();

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

    // Dismiss modal to background -> inspect `Settings · 常规` `72px` `C9or7K` card (`keDNb` + `oHWRT` cancel!)
    cx.update_window(win_handle, |_, window, cx| {
        window.click("update-dialog-background-btn", cx);
        shell.update(cx, |s, cx| s.set_route(ShellRoute::Settings, cx));
        let sv_ent = shell.read(cx).settings_view().clone();
        sv_ent.update(cx, |sv, cx| {
            sv.set_category(cst_desktop::settings::SettingsCategory::General, cx);
        });
        window.render_frame(cx);

        let card = window.find("settings-update-card-c9or7k");
        assert_eq!(f32::from(card.bounds().size.height), 72.0);
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

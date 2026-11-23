#![cfg(windows)]

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cst_client::bootstrap::VerifiedServerEndpoint;
use cst_client::http::AppHttpClient;
use cst_client::runtime::CstClient;
use cst_client::state::ChannelModeDto;
use cst_client::workspace::DraftKey;
use cst_kit::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_kit::telemetry::TelemetryOptions;
use cst_kit::update::service::UpdateOptions;
use cst_kit::{Kit, KitHost, SKILL_NAMES};
use cst_os::ProcessIdentity;
use cst_server::{RunningServer, ServerConfig};
use image::{ImageFormat, RgbaImage};
use pi_ai::faux::{FauxResponseStep, create_faux_provider, faux_text_message};
use serde_json::json;
use uuid::Uuid;

struct ServerFixture {
    root: PathBuf,
}

impl ServerFixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cst-client-sse-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join("app/prompts")).unwrap();
        fs::create_dir_all(dir.join("data/config")).unwrap();
        fs::create_dir_all(dir.join("static")).unwrap();
        fs::write(
            dir.join("static/index.html"),
            "<!doctype html><title>test</title>",
        )
        .unwrap();
        fs::write(dir.join("VERSION"), "cst-pilot v0.9.5\n").unwrap();
        fs::write(dir.join("app/prompts/APPEND_SYSTEM.md"), "测试系统规则。").unwrap();
        for name in SKILL_NAMES {
            let p = dir.join(format!("app/skills/{name}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(
                p,
                format!("---\nname: {name}\ndescription: {name}\n---\n指南。"),
            )
            .unwrap();
        }
        Self {
            root: dir.canonicalize().unwrap(),
        }
    }
}

impl Drop for ServerFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn make_endpoint(fixture: &ServerFixture, server: &RunningServer) -> VerifiedServerEndpoint {
    let self_id = ProcessIdentity::current().unwrap();
    VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port: server.port(),
        version: "v0.9.5".into(),
        http: AppHttpClient::new(server.port(), server.app_token()).unwrap(),
        spawned_by_client: false,
    }
}

fn sample_png() -> Vec<u8> {
    let img = RgbaImage::from_pixel(64, 64, image::Rgba([30, 90, 200, 255]));
    let mut cur = Cursor::new(Vec::new());
    img.write_to(&mut cur, ImageFormat::Png).unwrap();
    cur.into_inner()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_ui_consumer_over_thirty_seconds_with_continuous_streaming_never_disconnects_keepalive()
 {
    let fixture = ServerFixture::new();
    let kit = Kit::new(&fixture.root).unwrap();
    let oa = OaClient::new("https://oa.example").unwrap();
    let auth = AuthRuntime::create(&fixture.root, Channel::Gui, oa)
        .await
        .unwrap();

    // Register a slow faux provider that runs for ~31 seconds emitting periodic updates while UI is stalled!
    let faux = create_faux_provider();
    let model_provider = faux.model.provider.clone();
    let model_id = faux.model.id.clone();
    fs::write(
        fixture.root.join("data/config/auth.json"),
        serde_json::to_vec(&json!({
            model_provider.clone(): {
                "type": "api_key",
                "key": "sk-faux-test"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    faux.set_responses(vec![FauxResponseStep::Dynamic(Arc::new(|_, _, _| {
        // Simulate a 31-second active generation turn while heartbeats and state frames also flow
        std::thread::sleep(Duration::from_secs(31));
        faux_text_message("长达三十一秒的连续诊断分析已完成。")
    }))]);
    auth.runtime().register_provider(faux.provider);

    let host = KitHost::new(
        kit,
        auth,
        TelemetryOptions::default(),
        UpdateOptions::Disabled,
    )
    .await
    .unwrap();

    let mut cfg = ServerConfig::new(&fixture.root);
    cfg.port = Some(0);
    cfg.static_dir = Some(fixture.root.join("static"));
    cfg.acquire_instance_lock = false;
    cfg.mode_config.first_connect_timeout = Duration::from_secs(5);
    cfg.mode_config.grace_period = Duration::from_secs(2);
    let server = RunningServer::start(host, cfg).await.unwrap();

    let handle = CstClient::from_verified_endpoint(
        &tokio::runtime::Handle::current(),
        make_endpoint(&fixture, &server),
        None,
    )
    .unwrap();

    // Wait for initial keepalive connection
    let wait_conn = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < wait_conn && !handle.snapshot().connected {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(handle.snapshot().connected);

    // Hold a UI watch::Receiver and NEVER poll `.changed()` for >31 seconds!
    let _stalled_ui_rx = handle.subscribe_revisions();

    // Submit a home-page message (two-phase create + send) with an image attachment
    let pool = handle.with_workspace(|ws| ws.image_pool().clone());
    let img_handle = pool
        .admit_image(sample_png(), Some("diag.png".into()))
        .unwrap();
    assert!(pool.used_bytes() > 0);
    handle.with_workspace_mut(|ws| {
        let d = ws.draft_mut(DraftKey::NewSession);
        d.text = "开始慢速诊断任务".into();
        d.selected_provider = Some(model_provider);
        d.selected_model_id = Some(model_id);
        d.attach_image(img_handle).unwrap();
    });

    let outcome = handle.submit_draft(DraftKey::NewSession).await.unwrap();
    // Image buffer released from local outbox immediately upon `202 Accepted`!
    assert_eq!(pool.used_bytes(), 0);

    // Subscribe to the active session, then stall the UI consumer completely for 31.5 seconds
    // while also firing periodic global settings updates so SSE frames flow continuously!
    handle
        .activate_session(Some(outcome.session_id.clone()))
        .await
        .unwrap();

    for tick in 0..32u32 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if tick % 4 == 0 {
            let _ = handle
                .patch_desktop_settings(cst_client::DesktopSettingsPatchDto {
                    sidebar_collapsed: Some(tick % 8 == 0),
                    ..Default::default()
                })
                .await;
        }
        assert!(
            handle.snapshot().connected,
            "keepalive disconnected at second {tick}"
        );
        assert!(!handle.snapshot().reconnecting);
    }

    // Wait up to 3s for `agent_end` to settle after the 31s generation finishes
    let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snap = handle.snapshot();
        if let Some(sess) = snap.loaded_sessions.get(&outcome.session_id) {
            let text_joined = serde_json::to_string(sess.messages.as_slice()).unwrap();
            if text_joined.contains("长达三十一秒的连续诊断分析已完成") && !sess.running
            {
                break;
            }
        }
        if tokio::time::Instant::now() >= settle_deadline {
            panic!("expected final assistant message in session snapshot after UI unstall");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    handle.stop();
    server.shutdown().await;
}

#[tokio::test]
async fn credentials_required_preserves_draft_and_web_mode_switch_suspends_and_resumes_multiplex() {
    let fixture = ServerFixture::new();
    let kit = Kit::new(&fixture.root).unwrap();
    let oa = OaClient::new("https://oa.example").unwrap();
    let auth = AuthRuntime::create(&fixture.root, Channel::Gui, oa)
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

    let mut cfg = ServerConfig::new(&fixture.root);
    cfg.port = Some(0);
    cfg.static_dir = Some(fixture.root.join("static"));
    cfg.acquire_instance_lock = false;
    let server = RunningServer::start(host, cfg).await.unwrap();

    let handle = CstClient::from_verified_endpoint(
        &tokio::runtime::Handle::current(),
        make_endpoint(&fixture, &server),
        None,
    )
    .unwrap();

    let wait_conn = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < wait_conn && !handle.snapshot().connected {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    // 1. Pick an unconfigured model from `/api/models` catalog -> `submit_draft` returns
    // `CredentialsRequired` AND preserves the home draft!
    let models = handle.refresh_models(None).await.unwrap();
    let unready = models
        .catalog
        .iter()
        .find(|m| !m.credential_ready)
        .expect("catalog has unconfigured model");
    handle.with_workspace_mut(|ws| {
        let d = ws.draft_mut(DraftKey::NewSession);
        d.text = "未配密钥草稿不能丢失".into();
        d.selected_provider = Some(unready.provider.clone());
        d.selected_model_id = Some(unready.id.clone());
    });
    let err = handle.submit_draft(DraftKey::NewSession).await.unwrap_err();
    assert!(err.is_credentials_required(), "got {err:?}");
    let preserved = handle.with_workspace(|ws| {
        ws.draft(&DraftKey::NewSession)
            .map(|d| d.text.clone())
            .unwrap_or_default()
    });
    assert_eq!(preserved, "未配密钥草稿不能丢失");
    assert!(handle.check_web_switch().requires_confirmation());

    // 2. Switch to WebActive -> snapshot reflects WebActive, sessions list remains readable, close_web restores AppActive
    let web_open = handle.open_web().await.unwrap();
    assert_eq!(web_open.mode, ChannelModeDto::WebActive);
    assert_eq!(handle.snapshot().channel_mode, ChannelModeDto::WebActive);
    assert!(web_open.url.contains("ticket="));

    handle.refresh_globals().await.unwrap();
    assert_eq!(handle.snapshot().channel_mode, ChannelModeDto::WebActive);

    handle.close_web().await.unwrap();
    assert_eq!(handle.snapshot().channel_mode, ChannelModeDto::AppActive);

    handle.stop();
    server.shutdown().await;
}

mod controlled_http {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;

    pub struct RawRequest {
        pub method: String,
        pub target: String,
        pub headers: BTreeMap<String, String>,
        pub body: Vec<u8>,
    }

    pub enum RawReply {
        Json {
            status: u16,
            body: serde_json::Value,
        },
        SseStream(mpsc::UnboundedReceiver<String>),
        DropConnection,
    }

    pub async fn read_http_request(stream: &mut TcpStream) -> Option<RawRequest> {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let header_str = String::from_utf8_lossy(&buf[..header_end - 4]);
        let mut lines = header_str.split("\r\n");
        let req_line = lines.next()?;
        let mut parts = req_line.split_whitespace();
        let method = parts.next()?.to_owned();
        let target = parts.next()?.to_owned();
        let mut headers = BTreeMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        let content_len = headers
            .get("content-length")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        while buf.len() < header_end + content_len {
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let body = buf[header_end..(header_end + content_len).min(buf.len())].to_vec();
        Some(RawRequest {
            method,
            target,
            headers,
            body,
        })
    }

    pub async fn spawn_controlled_server<F, Fut>(handler: F) -> (u16, Arc<Mutex<Vec<RawRequest>>>)
    where
        F: Fn(RawRequest) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = RawReply> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let rec_clone = Arc::clone(&recorded);
        let handler = Arc::new(handler);

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let handler = Arc::clone(&handler);
                let rec = Arc::clone(&rec_clone);
                tokio::spawn(async move {
                    while let Some(req) = read_http_request(&mut socket).await {
                        rec.lock().unwrap().push(RawRequest {
                            method: req.method.clone(),
                            target: req.target.clone(),
                            headers: req.headers.clone(),
                            body: req.body.clone(),
                        });
                        match handler(req).await {
                            RawReply::DropConnection => {
                                let _ = socket.shutdown().await;
                                return;
                            }
                            RawReply::Json { status, body } => {
                                let bytes = serde_json::to_vec(&body).unwrap();
                                let reason = match status {
                                    200 => "OK",
                                    201 => "Created",
                                    202 => "Accepted",
                                    404 => "Not Found",
                                    409 => "Conflict",
                                    500 => "Internal Server Error",
                                    507 => "Insufficient Storage",
                                    _ => "Response",
                                };
                                let head = format!(
                                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                                    bytes.len()
                                );
                                if socket.write_all(head.as_bytes()).await.is_err()
                                    || socket.write_all(&bytes).await.is_err()
                                {
                                    return;
                                }
                            }
                            RawReply::SseStream(mut rx) => {
                                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
                                if socket.write_all(head.as_bytes()).await.is_err() {
                                    return;
                                }
                                while let Some(frame) = rx.recv().await {
                                    let chunk = format!("{:X}\r\n{}\r\n", frame.len(), frame);
                                    if socket.write_all(chunk.as_bytes()).await.is_err() {
                                        return;
                                    }
                                }
                                let _ = socket.write_all(b"0\r\n\r\n").await;
                                return;
                            }
                        }
                    }
                });
            }
        });

        (port, recorded)
    }
}

#[tokio::test]
async fn rejected_direct_payload_stays_recoverable_without_overwriting_the_composer() {
    use controlled_http::{RawReply, spawn_controlled_server};
    use cst_client::{ComposerDraft, OutboxItemState};

    let fixture = ServerFixture::new();
    let (port, _) = spawn_controlled_server(|req| async move {
        if req.method == "POST" && req.target == "/api/sessions/s1/messages" {
            return RawReply::Json {
                status: 409,
                body: json!({"error": {"code": "credentials_required", "message": "缺少密钥"}}),
            };
        }
        RawReply::Json {
            status: 200,
            body: json!({}),
        }
    })
    .await;
    let process = ProcessIdentity::current().unwrap();
    let handle = CstClient::from_verified_endpoint(
        &tokio::runtime::Handle::current(),
        VerifiedServerEndpoint {
            kit_root: fixture.root.clone(),
            pid: process.pid,
            process_created: process.created,
            port,
            version: "v0.9.5".into(),
            http: AppHttpClient::new(port, "test-token").unwrap(),
            spawned_by_client: false,
        },
        None,
    )
    .unwrap();
    let key = DraftKey::Session("s1".into());
    handle.with_workspace_mut(|ws| ws.draft_mut(key.clone()).text = "保留草稿".into());

    let error = handle
        .submit_direct_payload(
            "s1".into(),
            ComposerDraft {
                text: "重新生成这轮".into(),
                ..ComposerDraft::default()
            },
        )
        .await
        .unwrap_err();
    assert!(error.is_credentials_required(), "{error:?}");
    handle.with_workspace(|ws| assert_eq!(ws.draft(&key).unwrap().text, "保留草稿"));
    handle.with_outbox(|outbox| {
        let items = outbox.session_items("s1");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "重新生成这轮");
        assert!(matches!(
            items[0].state,
            OutboxItemState::Failed {
                credentials_required: true,
                ..
            }
        ));
    });
    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn op_revision_prevents_stale_query_overwrite_and_cursor_generation_handles_reset_and_overflow()
 {
    use controlled_http::{RawReply, spawn_controlled_server};
    use cst_client::ClientRuntimeOptions;
    use cst_client::state::ThemeModeDto;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Notify, mpsc};

    let fixture = ServerFixture::new();
    let slow_models_started = Arc::new(Notify::new());
    let release_slow_models = Arc::new(Notify::new());
    let slow_s1_started = Arc::new(Notify::new());
    let release_slow_s1 = Arc::new(Notify::new());
    let subscribed_s1_fetch_started = Arc::new(Notify::new());
    let release_subscribed_s1_fetch = Arc::new(Notify::new());

    let models_call_seq = Arc::new(AtomicUsize::new(0));
    let s1_fetch_seq = Arc::new(AtomicUsize::new(0));
    let mux_tx_slot: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>> = Arc::new(Mutex::new(None));
    let last_sub_bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

    let slow_models_started_h = Arc::clone(&slow_models_started);
    let release_slow_models_h = Arc::clone(&release_slow_models);
    let slow_s1_started_h = Arc::clone(&slow_s1_started);
    let release_slow_s1_h = Arc::clone(&release_slow_s1);
    let subscribed_s1_fetch_started_h = Arc::clone(&subscribed_s1_fetch_started);
    let release_subscribed_s1_fetch_h = Arc::clone(&release_subscribed_s1_fetch);
    let models_seq_h = Arc::clone(&models_call_seq);
    let s1_seq_h = Arc::clone(&s1_fetch_seq);
    let mux_tx_h = Arc::clone(&mux_tx_slot);
    let subs_h = Arc::clone(&last_sub_bodies);

    let (port, _requests) = spawn_controlled_server(move |req| {
        let slow_models_started = Arc::clone(&slow_models_started_h);
        let release_slow_models = Arc::clone(&release_slow_models_h);
        let slow_s1_started = Arc::clone(&slow_s1_started_h);
        let release_slow_s1 = Arc::clone(&release_slow_s1_h);
        let subscribed_s1_fetch_started = Arc::clone(&subscribed_s1_fetch_started_h);
        let release_subscribed_s1_fetch = Arc::clone(&release_subscribed_s1_fetch_h);
        let models_seq = Arc::clone(&models_seq_h);
        let s1_seq = Arc::clone(&s1_seq_h);
        let mux_tx = Arc::clone(&mux_tx_h);
        let subs = Arc::clone(&subs_h);

        async move {
            if req.target == "/api/app/events" {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send("event: heartbeat\ndata: {}\n\n".to_owned());
                std::mem::forget(tx);
                return RawReply::SseStream(rx);
            }
            if req.target.starts_with("/api/events/stream?") {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send(
                    "id: global:g0:1\nevent: state\ndata: {\"type\":\"stream_ready\",\"subscribedSessions\":[]}\n\n"
                        .to_owned(),
                );
                *mux_tx.lock().unwrap() = Some(tx);
                return RawReply::SseStream(rx);
            }
            if req.target.ends_with("/subscriptions") && req.method == "PUT" {
                let body_val: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
                let sessions = body_val
                    .get("sessions")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                subs.lock().unwrap().push(body_val);
                return RawReply::Json {
                    status: 200,
                    body: json!({ "subscribedSessions": sessions }),
                };
            }
            if req.target.starts_with("/api/models") {
                // `?sessionId=...` comes from explicit `refresh_models(Some(...))`
                if req.target.contains("sessionId=slow") {
                    let _ = models_seq.fetch_add(1, Ordering::SeqCst);
                    slow_models_started.notify_one();
                    release_slow_models.notified().await;
                    return RawReply::Json {
                        status: 200,
                        body: json!({
                            "models": [{ "id": "stale-model", "provider": "deepseek", "name": "Stale", "reasoning": false }],
                            "catalog": []
                        }),
                    };
                }
                let tag = if req.target.contains("sessionId=fast") {
                    "fresh-model"
                } else {
                    "boot-model"
                };
                return RawReply::Json {
                    status: 200,
                    body: json!({
                        "models": [{ "id": tag, "provider": "deepseek", "name": tag, "reasoning": false }],
                        "catalog": []
                    }),
                };
            }
            if req.target == "/api/sessions/s1" {
                let n = s1_seq.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // The speculative snapshot starts before any subscription to s1.
                    slow_s1_started.notify_one();
                    release_slow_s1.notified().await;
                    return RawReply::Json {
                        status: 200,
                        body: json!({
                            "id": "s1",
                            "eventCursor": "ep1:2",
                            "running": false,
                            "messages": [{ "id": "m-old", "role": "assistant", "content": [{"type": "text", "text": "旧快照"}] }],
                            "entries": []
                        }),
                    };
                }
                if n == 1 {
                    assert!(subs.lock().unwrap().iter().any(|body| body["sessions"] == json!(["s1"])));
                    subscribed_s1_fetch_started.notify_one();
                    release_subscribed_s1_fetch.notified().await;
                }
                if n <= 2 {
                    return RawReply::Json {
                        status: 200,
                        body: json!({
                            "id": "s1",
                            "eventCursor": "ep1:10",
                            "running": false,
                            "messages": [{ "id": "m-10", "role": "assistant", "content": [{"type": "text", "text": "第十帧快照"}] }],
                            "entries": []
                        }),
                    };
                }
                // Generation >= 2 (after QueueOverflow targeted reload!)
                return RawReply::Json {
                    status: 200,
                    body: json!({
                        "id": "s1",
                        "eventCursor": "ep1:25",
                        "running": false,
                        "messages": [
                            { "id": "m-10", "role": "assistant", "content": [{"type": "text", "text": "第十帧快照"}] },
                            { "id": "m-25", "role": "assistant", "content": [{"type": "text", "text": "队列溢出后重拉快照"}] }
                        ],
                        "entries": []
                    }),
                };
            }
            if req.target == "/api/app/settings" {
                return RawReply::Json {
                    status: 200,
                    body: json!({ "theme": "dark", "defaultView": "gui", "sidebarCollapsed": false, "windowMaximized": false }),
                };
            }
            if req.target == "/api/auth" {
                return RawReply::Json {
                    status: 200,
                    body: json!({ "providers": [] }),
                };
            }
            if req.target.starts_with("/api/account") {
                return RawReply::Json {
                    status: 200,
                    body: json!({ "loggedIn": false }),
                };
            }
            if req.target == "/api/app/update/status" {
                return RawReply::Json {
                    status: 200,
                    body: json!({
                        "currentVersion": "v0.9.5",
                        "status": { "phase": "unavailable", "message": "未启用" }
                    }),
                };
            }
            RawReply::Json {
                status: 200,
                body: json!({
                    "version": "v0.9.5",
                    "channelMode": "app_active",
                    "commands": [],
                    "sessions": [{ "id": "s1", "title": "测试会话", "running": false }]
                }),
            }
        }
    })
    .await;

    let self_id = ProcessIdentity::current().unwrap();
    let endpoint = VerifiedServerEndpoint {
        kit_root: fixture.root.clone(),
        pid: self_id.pid,
        process_created: self_id.created,
        port,
        version: "v0.9.5".into(),
        http: AppHttpClient::new(port, "test-token").unwrap(),
        spawned_by_client: false,
    };
    let handle = CstClient::from_verified_endpoint_with_options(
        &tokio::runtime::Handle::current(),
        endpoint,
        None,
        ClientRuntimeOptions {
            max_multiplex_queue_bytes: 768,
            ..Default::default()
        },
    )
    .unwrap();

    let wait_conn = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < wait_conn
        && (!handle.snapshot().connected || mux_tx_slot.lock().unwrap().is_none())
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(handle.snapshot().desktop_settings.theme, ThemeModeDto::Dark);

    // 1. Out-of-order `refresh_models`: slow request #1 (`op_revision` N) MUST NOT overwrite fast request #2 (`op_revision` N+1)
    let h_slow = handle.clone();
    let slow_task = tokio::spawn(async move { h_slow.refresh_models(Some("slow".into())).await });
    slow_models_started.notified().await;

    let fast_receipt = handle.refresh_models(Some("fast".into())).await.unwrap();
    assert!(fast_receipt.applied_to_snapshot);
    assert_eq!(fast_receipt.models[0].id, "fresh-model");

    release_slow_models.notify_one();
    let slow_receipt = slow_task.await.unwrap().unwrap();
    assert!(
        slow_receipt.op_revision < fast_receipt.op_revision,
        "slow={} fast={}",
        slow_receipt.op_revision,
        fast_receipt.op_revision
    );
    assert!(
        !slow_receipt.applied_to_snapshot,
        "stale slow query must not overwrite newer snapshot"
    );
    assert_eq!(
        handle.snapshot().models.as_ref().unwrap().models[0].id,
        "fresh-model"
    );

    // 2. Activating during preload must subscribe first, then start a fresh GET.
    tokio::time::timeout(Duration::from_secs(3), slow_s1_started.notified())
        .await
        .expect("preload did not start");
    let h_act = handle.clone();
    let act_task = tokio::spawn(async move { h_act.activate_session(Some("s1".into())).await });
    tokio::time::timeout(
        Duration::from_secs(3),
        subscribed_s1_fetch_started.notified(),
    )
    .await
    .expect("activation did not replace the unsubscribed preload");

    let mux_sender = mux_tx_slot.lock().unwrap().clone().unwrap();
    // Reset starts the next generation at ep1:10. The overlapping ep1:9 must be skipped.
    mux_sender
        .send("id: global:g0:1,s1:ep1:10\nevent: reset\ndata: {\"scope\":\"session\",\"sessionId\":\"s1\"}\n\n".to_owned())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Frame at `ep1:9` <= `ep1:10`: skipped by cursor watermark even if message id were distinct!
    mux_sender
        .send(
            "id: global:g0:1,s1:ep1:9\nevent: session\ndata: {\"sessionId\":\"s1\",\"event\":{\"type\":\"message_end\",\"message\":{\"id\":\"m-stale-9\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"已被快照覆盖\"}]}}}\n\n"
                .to_owned(),
        )
        .unwrap();
    // Frame at `ep1:11` > `ep1:10`: applied and advances cursor to `ep1:11`!
    mux_sender
        .send(
            "id: global:g0:1,s1:ep1:11\nevent: session\ndata: {\"sessionId\":\"s1\",\"event\":{\"type\":\"message_end\",\"message\":{\"id\":\"m-11\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"增量第十一帧\"}]}}}\n\n"
                .to_owned(),
        )
        .unwrap();

    // Release both superseded snapshots: neither may overwrite the reset generation.
    release_slow_s1.notify_one();
    release_subscribed_s1_fetch.notify_one();
    let loaded_s1 = act_task.await.unwrap().unwrap().unwrap();
    let _ = loaded_s1;
    tokio::time::sleep(Duration::from_millis(80)).await;

    let snap_s1 = handle
        .snapshot()
        .loaded_sessions
        .get("s1")
        .cloned()
        .expect("s1 loaded");
    let msg_ids: Vec<&str> = snap_s1
        .messages
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(msg_ids, vec!["m-10", "m-11"]);
    assert_eq!(snap_s1.event_cursor, Some(("ep1".into(), 11)));

    // 3. Switch away (`None`) and back (`Some("s1")`) -> `PUT /subscriptions` carries `lastEventIds: { "s1": "ep1:11" }`!
    handle.activate_session(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    handle.activate_session(Some("s1".into())).await.unwrap();
    let sub_with_cursor = last_sub_bodies.lock().unwrap().iter().rev().find_map(|b| {
        b.get("lastEventIds")
            .and_then(|m| m.get("s1"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    });
    assert_eq!(sub_with_cursor.as_deref(), Some("ep1:11"));

    // 4. Overflow the 768-byte bounded multiplex queue -> automatic targeted reload converges to `ep1:25` (`"m-25"`)!
    let big_pad = "x".repeat(900);
    for seq in 12..18u64 {
        let _ = mux_sender.send(format!(
            "id: global:g0:1,s1:ep1:{seq}\nevent: session\ndata: {{\"sessionId\":\"s1\",\"event\":{{\"type\":\"turn_start\",\"pad\":\"{big_pad}\"}}}}\n\n"
        ));
    }
    let overflow_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(s1) = handle.snapshot().loaded_sessions.get("s1")
            && s1.event_cursor == Some(("ep1".into(), 25))
        {
            break;
        }
        if tokio::time::Instant::now() >= overflow_deadline {
            panic!(
                "expected QueueOverflow targeted reload to converge to ep1:25, got {:?}",
                handle
                    .snapshot()
                    .loaded_sessions
                    .get("s1")
                    .map(|s| &s.event_cursor)
            );
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_uncertain_recovery_concurrent_draft_protection_and_outbox_actions() {
    use controlled_http::{RawReply, spawn_controlled_server};
    use cst_client::outbox::OutboxItemState;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Notify, mpsc};

    let fixture = ServerFixture::new();
    let create_attempt = Arc::new(AtomicUsize::new(0));
    let msg_attempt = Arc::new(AtomicUsize::new(0));
    let block_second_create = Arc::new(Notify::new());
    let release_second_create = Arc::new(Notify::new());
    let created_key: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let delivered_msgs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let ca_h = Arc::clone(&create_attempt);
    let ma_h = Arc::clone(&msg_attempt);
    let b2_h = Arc::clone(&block_second_create);
    let r2_h = Arc::clone(&release_second_create);
    let ck_h = Arc::clone(&created_key);
    let dm_h = Arc::clone(&delivered_msgs);

    let (port, _requests) = spawn_controlled_server(move |req| {
        let ca = Arc::clone(&ca_h);
        let ma = Arc::clone(&ma_h);
        let b2 = Arc::clone(&b2_h);
        let r2 = Arc::clone(&r2_h);
        let ck = Arc::clone(&ck_h);
        let dm = Arc::clone(&dm_h);

        async move {
            if req.target == "/api/app/events" {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send("event: heartbeat\ndata: {}\n\n".to_owned());
                std::mem::forget(tx);
                return RawReply::SseStream(rx);
            }
            if req.target.starts_with("/api/events/stream?") {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send(
                    "id: global:g0:1\nevent: state\ndata: {\"type\":\"stream_ready\",\"subscribedSessions\":[]}\n\n"
                        .to_owned(),
                );
                std::mem::forget(tx);
                return RawReply::SseStream(rx);
            }
            if req.target == "/api/sessions" && req.method == "POST" {
                let n = ca.fetch_add(1, Ordering::SeqCst);
                let idem = req.headers.get("idempotency-key").cloned().unwrap_or_default();
                if n == 0 {
                    // Attempt 0: persist `s-recovered`, then drop TCP connection mid-flight!
                    *ck.lock().unwrap() = Some(idem);
                    return RawReply::DropConnection;
                }
                // Attempt 1: block so test can type a concurrent draft into `NewSession`, then fail 500!
                b2.notify_one();
                r2.notified().await;
                return RawReply::Json {
                    status: 500,
                    body: json!({ "error": { "code": "storage_unavailable", "message": "模拟创建失败" } }),
                };
            }
            if let Some(key) = req.target.strip_prefix("/api/sessions/creation/") {
                let persisted = ck.lock().unwrap().as_deref() == Some(key);
                if persisted {
                    return RawReply::Json {
                        status: 200,
                        body: json!({ "status": "done", "sessionId": "s-recovered" }),
                    };
                }
                return RawReply::Json {
                    status: 200,
                    body: json!({ "status": "missing" }),
                };
            }
            if let Some(rest) = req.target.strip_prefix("/api/sessions/s-recovered/messages/")
                && req.method == "GET"
            {
                let exists = dm.lock().unwrap().iter().any(|id| id == rest);
                return RawReply::Json {
                    status: 200,
                    body: if exists {
                        json!({ "item": { "id": rest, "status": "pending" } })
                    } else {
                        json!({ "item": null })
                    },
                };
            }
            if req.target == "/api/sessions/s-recovered/messages" && req.method == "POST" {
                let attempt = ma.fetch_add(1, Ordering::SeqCst);
                let body_val: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let msg_id = body_val["id"].as_str().unwrap().to_owned();
                if attempt == 1 {
                    // Simulate `507 storage_full` on second session send
                    return RawReply::Json {
                        status: 507,
                        body: json!({ "error": { "code": "storage_full", "message": "磁盘空间已满" } }),
                    };
                }
                dm.lock().unwrap().push(msg_id.clone());
                return RawReply::Json {
                    status: 202,
                    body: json!({ "item": { "id": msg_id, "status": "pending" } }),
                };
            }
            RawReply::Json {
                status: 200,
                body: json!({
                    "version": "v0.9.5",
                    "channelMode": "app_active",
                    "commands": [],
                    "sessions": [],
                    "providers": [],
                    "loggedIn": false,
                    "models": [],
                    "catalog": [],
                    "theme": "light",
                    "defaultView": "gui",
                    "sidebarCollapsed": false,
                    "windowMaximized": false,
                    "currentVersion": "v0.9.5",
                    "status": { "phase": "unavailable", "message": "未启用" }
                }),
            }
        }
    })
    .await;

    let self_id = ProcessIdentity::current().unwrap();
    let handle = CstClient::from_verified_endpoint(
        &tokio::runtime::Handle::current(),
        VerifiedServerEndpoint {
            kit_root: fixture.root.clone(),
            pid: self_id.pid,
            process_created: self_id.created,
            port,
            version: "v0.9.5".into(),
            http: AppHttpClient::new(port, "test-token").unwrap(),
            spawned_by_client: false,
        },
        None,
    )
    .unwrap();

    // 1. Home `POST /api/sessions` drops connection AFTER persisting `s-recovered`!
    handle.with_workspace_mut(|ws| {
        ws.draft_mut(DraftKey::NewSession).text = "首条断线消息".into();
    });
    let err = handle.submit_draft(DraftKey::NewSession).await.unwrap_err();
    assert!(err.is_disconnected());
    let home_cmid = handle.with_outbox(|ob| {
        let item = ob.home_item().expect("home item retained as Uncertain");
        assert!(matches!(item.state, OutboxItemState::Uncertain { .. }));
        item.client_message_id.clone()
    });

    // Reconcile discovers `s-recovered` (`done`), binds item to `s-recovered` WITHOUT auto-resending,
    // and `retry_outbox_item` delivers `home_cmid` without calling `POST /api/sessions` again!
    let rec = handle.reconcile_uncertain_outbox_detailed().await.unwrap();
    assert_eq!(rec.resolved_count, 1);
    assert_eq!(
        rec.recovered_home_session_id.as_deref(),
        Some("s-recovered")
    );
    assert_eq!(delivered_msgs.lock().unwrap().len(), 0);

    let retry_res = handle
        .retry_outbox_item(Some("s-recovered".into()), home_cmid.clone())
        .await
        .unwrap();
    assert_eq!(retry_res.session_id, "s-recovered");
    assert_eq!(create_attempt.load(Ordering::SeqCst), 1);
    assert_eq!(
        delivered_msgs.lock().unwrap().as_slice(),
        &[home_cmid.as_str()]
    );
    assert_eq!(handle.with_outbox(|ob| ob.total_unresolved_count()), 0);

    // 2. Session `507 storage_full` pauses session -> test `edit_outbox_item`, `retry_outbox_item`, and `remove_outbox_item`!
    let pool = handle.with_workspace(|ws| ws.image_pool().clone());
    let img = pool
        .admit_image(sample_png(), Some("s.png".into()))
        .unwrap();
    assert!(pool.used_bytes() > 0);
    handle.with_workspace_mut(|ws| {
        let d = ws.draft_mut(DraftKey::Session("s-recovered".into()));
        d.text = "遇到磁盘写满的消息".into();
        d.attach_image(img).unwrap();
    });
    let full_err = handle
        .submit_draft(DraftKey::Session("s-recovered".into()))
        .await
        .unwrap_err();
    assert!(full_err.is_storage_full());
    let paused_cmid = handle.with_outbox(|ob| {
        assert!(ob.is_session_paused("s-recovered"));
        let item = &ob.session_items("s-recovered")[0];
        assert!(matches!(
            item.state,
            OutboxItemState::PausedStorageFull { .. }
        ));
        item.client_message_id.clone()
    });

    // If composer already has text, `edit_outbox_item(..., false)` refuses to clobber (`409 draft_not_empty`)
    handle.with_workspace_mut(|ws| {
        ws.draft_mut(DraftKey::Session("s-recovered".into())).text = "未发新草稿".into();
    });
    let conflict_err = handle
        .edit_outbox_item(Some("s-recovered"), &paused_cmid, false)
        .unwrap_err();
    assert_eq!(conflict_err.code(), "draft_not_empty");

    // Clear composer -> `edit_outbox_item` moves item + image back to composer and unpauses session;
    // then re-submit succeeds (`202`) and drops image budget to 0!
    handle.with_workspace_mut(|ws| {
        let _ = ws.take_draft(&DraftKey::Session("s-recovered".into()));
    });
    let edited = handle
        .edit_outbox_item(Some("s-recovered"), &paused_cmid, false)
        .unwrap();
    assert_eq!(edited.text, "遇到磁盘写满的消息");
    assert_eq!(edited.images.len(), 1);
    drop(edited);
    assert!(!handle.with_outbox(|ob| ob.is_session_paused("s-recovered")));

    handle
        .submit_draft(DraftKey::Session("s-recovered".into()))
        .await
        .unwrap();
    assert_eq!(pool.used_bytes(), 0);

    // 3. Concurrent composer edit during Home send failure: newly typed text MUST NOT be overwritten,
    // and failed item stays in `home_pending` until `remove_outbox_item`!
    handle.with_workspace_mut(|ws| {
        ws.set_active_session(None);
        ws.draft_mut(DraftKey::NewSession).text = "旧主页提交".into();
    });
    let h_home = handle.clone();
    let home_fail_task =
        tokio::spawn(async move { h_home.submit_draft(DraftKey::NewSession).await });
    block_second_create.notified().await;

    // User types new text while `POST /api/sessions` is in flight!
    handle.with_workspace_mut(|ws| {
        ws.draft_mut(DraftKey::NewSession).text = "用户刚输入的新草稿".into();
    });
    release_second_create.notify_one();
    let _ = home_fail_task.await.unwrap().unwrap_err();

    let composer_now = handle.with_workspace(|ws| {
        ws.draft(&DraftKey::NewSession)
            .map(|d| d.text.clone())
            .unwrap_or_default()
    });
    assert_eq!(composer_now, "用户刚输入的新草稿");
    let retained_home_cmid = handle.with_outbox(|ob| {
        let item = ob.home_item().expect("failed home item retained");
        assert_eq!(item.text, "旧主页提交");
        assert!(matches!(item.state, OutboxItemState::Failed { .. }));
        item.client_message_id.clone()
    });
    let removed = handle
        .remove_outbox_item(None, &retained_home_cmid)
        .unwrap();
    assert_eq!(removed.text, "旧主页提交");
    assert_eq!(handle.with_outbox(|ob| ob.total_unresolved_count()), 0);

    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_send_lost_202_reconciles_by_original_message_id_without_duplicate_post() {
    use controlled_http::{RawReply, spawn_controlled_server};
    use cst_client::outbox::OutboxItemState;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let fixture = ServerFixture::new();
    let post_count = Arc::new(AtomicUsize::new(0));
    let persisted_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let queried_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let pc_h = Arc::clone(&post_count);
    let pid_h = Arc::clone(&persisted_ids);
    let qid_h = Arc::clone(&queried_ids);

    let (port, _requests) = spawn_controlled_server(move |req| {
        let pc = Arc::clone(&pc_h);
        let pids = Arc::clone(&pid_h);
        let qids = Arc::clone(&qid_h);
        async move {
            if req.target == "/api/app/events" {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send("event: heartbeat\ndata: {}\n\n".to_owned());
                std::mem::forget(tx);
                return RawReply::SseStream(rx);
            }
            if req.target.starts_with("/api/events/stream?") {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send(
                    "id: global:g0:1\nevent: state\ndata: {\"type\":\"stream_ready\",\"subscribedSessions\":[]}\n\n"
                        .to_owned(),
                );
                std::mem::forget(tx);
                return RawReply::SseStream(rx);
            }
            if req.target == "/api/sessions/s-202/messages" && req.method == "POST" {
                pc.fetch_add(1, Ordering::SeqCst);
                let body_val: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let msg_id = body_val["id"].as_str().unwrap().to_owned();
                // Server persisted and accepted the message, then TCP dropped BEFORE delivering 202!
                pids.lock().unwrap().push(msg_id);
                return RawReply::DropConnection;
            }
            if let Some(id) = req.target.strip_prefix("/api/sessions/s-202/messages/")
                && req.method == "GET"
            {
                qids.lock().unwrap().push(id.to_owned());
                let exists = pids.lock().unwrap().iter().any(|x| x == id);
                return RawReply::Json {
                    status: 200,
                    body: if exists {
                        json!({ "item": { "id": id, "status": "pending" } })
                    } else {
                        json!({ "item": null })
                    },
                };
            }
            RawReply::Json {
                status: 200,
                body: json!({
                    "version": "v0.9.5",
                    "channelMode": "app_active",
                    "commands": [],
                    "sessions": [],
                    "providers": [],
                    "loggedIn": false,
                    "models": [],
                    "catalog": [],
                    "theme": "light",
                    "defaultView": "gui",
                    "sidebarCollapsed": false,
                    "windowMaximized": false,
                    "currentVersion": "v0.9.5",
                    "status": { "phase": "unavailable", "message": "未启用" }
                }),
            }
        }
    })
    .await;

    let self_id = ProcessIdentity::current().unwrap();
    let handle = CstClient::from_verified_endpoint(
        &tokio::runtime::Handle::current(),
        VerifiedServerEndpoint {
            kit_root: fixture.root.clone(),
            pid: self_id.pid,
            process_created: self_id.created,
            port,
            version: "v0.9.5".into(),
            http: AppHttpClient::new(port, "test-token").unwrap(),
            spawned_by_client: false,
        },
        None,
    )
    .unwrap();

    handle.with_workspace_mut(|ws| {
        ws.draft_mut(DraftKey::Session("s-202".into())).text = "丢 202 响应测试消息".into();
    });
    let err = handle
        .submit_draft(DraftKey::Session("s-202".into()))
        .await
        .unwrap_err();
    assert!(err.is_disconnected());

    let orig_cmid = handle.with_outbox(|ob| {
        let items = ob.session_items("s-202");
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].state, OutboxItemState::Uncertain { .. }));
        items[0].client_message_id.clone()
    });
    assert_eq!(post_count.load(Ordering::SeqCst), 1);

    // Reconcile queries `GET /api/sessions/s-202/messages/<orig_cmid>`, finds it accepted,
    // and clears outbox WITHOUT issuing any duplicate POST!
    let rec = handle.reconcile_uncertain_outbox_detailed().await.unwrap();
    assert_eq!(rec.resolved_count, 1);
    assert_eq!(post_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        queried_ids.lock().unwrap().as_slice(),
        &[orig_cmid.as_str()]
    );
    assert_eq!(handle.with_outbox(|ob| ob.total_unresolved_count()), 0);

    handle.stop();
}

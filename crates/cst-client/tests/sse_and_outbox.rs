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
        fs::create_dir_all(dir.join("agent/home")).unwrap();
        fs::write(dir.join("VERSION"), "cst-pilot v0.9.5\n").unwrap();
        fs::write(dir.join("agent/home/APPEND_SYSTEM.md"), "测试系统规则。").unwrap();
        for name in SKILL_NAMES {
            let p = dir.join(format!("agent/home/skills/{name}/SKILL.md"));
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
        fixture.root.join("agent/home/auth.json"),
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

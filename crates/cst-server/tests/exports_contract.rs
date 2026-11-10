use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use chrono::{Local, TimeZone};
use cst_pilot::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_pilot::telemetry::TelemetryOptions;
use cst_pilot::update::service::UpdateOptions;
use cst_pilot::{Kit, KitHost, SKILL_NAMES};
use cst_server::exports::{
    AssetResponse, ExportDocumentSource, ExportRenderer, ExportStyleSheet, OfflineHtml,
    SessionExportStore, artifact_name, build_export_snapshot_from_entries,
    ensure_artifact_sessions_dir, export_pages, parse_export_options, validate_artifact_date,
};
#[cfg(windows)]
use cst_server::exports::{ChromiumoxideRenderer, ExportBrowser, ExportBrowserOptions};
use cst_server::{RunningServer, ServerConfig};
use futures_util::stream;
use pi_agent::AgentMessage;
use pi_ai::faux::{faux_assistant_message, faux_tool_call};
use pi_ai::{StopReason, UserContent, UserMessage};
use serde_json::{Value, json};
#[cfg(windows)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn test_temp_dir(prefix: &str) -> PathBuf {
    let base = if Path::new(r"E:\tmp").exists() {
        PathBuf::from(r"E:\tmp")
    } else if Path::new(r"F:\tmp").exists() {
        PathBuf::from(r"F:\tmp")
    } else {
        std::env::temp_dir()
    };
    let day = Local::now().format("%Y-%m-%d").to_string();
    let dir = base.join(day).join(format!("{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        Self(test_temp_dir(prefix))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct MockRenderer<F> {
    render_fn: F,
    closed: Arc<AtomicUsize>,
}

impl<F> MockRenderer<F>
where
    for<'a> F: Fn(
            &'a Value,
            cst_server::exports::ExportEmitFn<'a>,
            CancellationToken,
        ) -> pi_agent::BoxFuture<'a, Result<(), cst_server::error::ServiceError>>
        + Send
        + Sync,
{
    fn new(render_fn: F) -> (Arc<Self>, Arc<AtomicUsize>) {
        let closed = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                render_fn,
                closed: closed.clone(),
            }),
            closed,
        )
    }
}

impl<F> ExportRenderer for MockRenderer<F>
where
    for<'a> F: Fn(
            &'a Value,
            cst_server::exports::ExportEmitFn<'a>,
            CancellationToken,
        ) -> pi_agent::BoxFuture<'a, Result<(), cst_server::error::ServiceError>>
        + Send
        + Sync,
{
    fn render<'a>(
        &'a self,
        snapshot: &'a Value,
        emit: cst_server::exports::ExportEmitFn<'a>,
        cancel: CancellationToken,
    ) -> pi_agent::BoxFuture<'a, Result<(), cst_server::error::ServiceError>> {
        (self.render_fn)(snapshot, emit, cancel)
    }

    fn close(&self) -> pi_agent::BoxFuture<'_, ()> {
        let closed = self.closed.clone();
        Box::pin(async move {
            closed.fetch_add(1, Ordering::SeqCst);
        })
    }
}

fn fixture_snapshot(format: &str) -> Value {
    let entries = vec![json!({
        "id": "u1",
        "message": {
            "role": "user",
            "content": "fixture",
            "timestamp": 1,
        }
    })];
    let opts = parse_export_options(&json!({ "format": format })).unwrap();
    let dt = Local
        .with_ymd_and_hms(2026, 10, 31, 12, 34, 56)
        .single()
        .unwrap();
    build_export_snapshot_from_entries("fixture", "检查:磁盘/内存", &entries, &opts, dt).unwrap()
}

#[test]
fn snapshot_options_date_filtering_and_pagination_contract() {
    assert_eq!(validate_artifact_date("2024-02-29").unwrap(), "2024-02-29");
    for invalid_day in [
        "2026-02-29",
        "2026-13-01",
        "2026-02-30",
        "../escape",
        "2026-1-1",
        "0000-01-01",
    ] {
        assert!(
            validate_artifact_date(invalid_day).is_err(),
            "{invalid_day}"
        );
    }
    assert_eq!(artifact_name(" title<>:\"/\\|?*. "), "title");

    for invalid_opts in [
        json!({ "format": "pdf" }),
        json!({ "theme": "system" }),
        json!({ "includeTools": "yes" }),
        json!({ "date": "2026-10-29" }),
        json!({ "turnIds": ["u1"] }),
        json!({ "extra": true }),
    ] {
        assert!(parse_export_options(&invalid_opts).is_err());
    }

    let entries = vec![
        json!({ "id": "u1", "message": { "role": "user", "content": "第一轮", "timestamp": 1 } }),
        json!({
            "id": "a1",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "隐藏思考" },
                    { "type": "toolCall", "id": "t1", "name": "read", "arguments": {} }
                ],
                "timestamp": 2
            }
        }),
        json!({
            "id": "t1",
            "message": {
                "role": "toolResult",
                "toolCallId": "t1",
                "toolName": "read",
                "content": "工具输出",
                "timestamp": 3
            }
        }),
        json!({
            "id": "a2",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "第一轮回答" }],
                "timestamp": 4
            }
        }),
    ];

    let before = Local
        .with_ymd_and_hms(2026, 10, 31, 23, 59, 59)
        .single()
        .unwrap();
    let after = Local
        .with_ymd_and_hms(2026, 11, 1, 0, 0, 1)
        .single()
        .unwrap();
    let default_opts = parse_export_options(&json!({ "format": "html", "theme": "dark" })).unwrap();

    let first =
        build_export_snapshot_from_entries("fixture", "跨天", &entries, &default_opts, before)
            .unwrap();
    assert_eq!(first["options"]["date"], json!("2026-10-31"));
    let chosen_ids: Vec<&str> = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(chosen_ids, vec!["u1", "a2"]);
    let first_str = serde_json::to_string(&first).unwrap();
    assert!(!first_str.contains("隐藏思考"));
    assert!(!first_str.contains("toolCall"));

    let second =
        build_export_snapshot_from_entries("fixture", "跨天", &entries, &default_opts, after)
            .unwrap();
    assert_eq!(second["options"]["date"], json!("2026-11-01"));

    let full_opts = parse_export_options(&json!({
        "format": "html",
        "theme": "dark",
        "includeThinking": true,
        "includeTools": true
    }))
    .unwrap();
    let full = build_export_snapshot_from_entries("fixture", "全量", &entries, &full_opts, before)
        .unwrap();
    assert_eq!(full["entries"].as_array().unwrap().len(), 4);

    let secret = ["123456"; 8].join("-");
    let redacted = build_export_snapshot_from_entries(
        "fixture",
        &secret,
        &[json!({ "id": "s", "message": { "role": "user", "content": secret, "timestamp": 1 } })],
        &default_opts,
        before,
    )
    .unwrap();
    assert!(!serde_json::to_string(&redacted).unwrap().contains(&secret));

    for height in [1, 1600, 3200, 3201, 9500, 100_000] {
        let pages = export_pages(
            height,
            &[3000.0, 1700.0, 1700.0, -1.0, f64::NAN, 10_000_000.0, 6000.0],
        )
        .unwrap();
        let mut top = 0u32;
        for page in pages {
            assert_eq!(page.top, top);
            assert!(page.height > 0 && page.height <= 3200);
            top += page.height;
        }
        assert_eq!(i64::from(top), height);
    }
    assert_eq!(
        export_pages(7000, &[3100.0, 2500.0]).unwrap()[0].height,
        3100
    );
    assert_eq!(
        export_pages(100_001, &[]).unwrap_err().code,
        "export_too_tall"
    );
    assert_eq!(
        export_pages(0, &[]).unwrap_err().code,
        "export_layout_invalid"
    );
}

#[tokio::test]
async fn store_queue_monotonic_polling_restart_recovery_and_collision_handling() {
    let dir = TempDirGuard::new("export-store");
    let artifacts = dir.path().join("artifacts");
    let metadata = dir.path().join("metadata");

    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let (renderer, closed) = MockRenderer::new(move |snap, emit, _cancel| {
        let mut rx = gate_rx.clone();
        let fmt = snap["options"]["format"]
            .as_str()
            .unwrap_or("html")
            .to_owned();
        Box::pin(async move {
            while !*rx.borrow_and_update() {
                let _ = rx.changed().await;
            }
            if fmt == "png" {
                emit("png", b"fixture 1".to_vec()).await?;
                emit("png", b"fixture 2".to_vec()).await?;
            } else {
                emit("html", "<!DOCTYPE html>完整文件".as_bytes().to_vec()).await?;
            }
            Ok(())
        })
    });

    let store = SessionExportStore::new(&artifacts, &metadata, renderer);
    let mut jobs = Vec::new();
    for _ in 0..8 {
        jobs.push(store.create(&fixture_snapshot("html")).await.unwrap());
    }
    let ninth_err = store
        .create(&fixture_snapshot("html"))
        .await
        .expect_err("9th queued export must return 429 export_busy");
    assert_eq!(ninth_err.code, "export_busy");

    let _ = gate_tx.send(true);
    for job in &jobs {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut saw_rendering = false;
        loop {
            let cur = store.get(&job.id).await.unwrap();
            assert_ne!(cur.status, "failed", "{:?}", cur.error);
            if saw_rendering {
                assert_ne!(cur.status, "queued");
            }
            saw_rendering |= cur.status == "rendering";
            if cur.status == "completed" {
                let file = store.file(&job.id, 0).await.unwrap();
                assert_eq!(file.data, "<!DOCTYPE html>完整文件".as_bytes());
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "export job timed out"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    store.close().await;
    store.close().await;
    assert_eq!(closed.load(Ordering::SeqCst), 1);

    // Simulate crash during rendering and verify restart marks `export_interrupted`
    let first_id = &jobs[0].id;
    let rec_path = metadata.join(format!("{first_id}.json"));
    let mut rec_val: Value = serde_json::from_slice(&fs::read(&rec_path).unwrap()).unwrap();
    rec_val["result"]["status"] = json!("rendering");
    rec_val["result"]["files"] = json!([]);
    rec_val["stored"] = json!([]);
    fs::write(&rec_path, serde_json::to_vec(&rec_val).unwrap()).unwrap();

    let interrupted_work = metadata.join("work").join(first_id);
    let stale_profile = metadata.join("work/browsers/browser-interrupted");
    fs::create_dir_all(&interrupted_work).unwrap();
    fs::create_dir_all(&stale_profile).unwrap();
    fs::write(interrupted_work.join("01.png"), b"partial image").unwrap();
    fs::write(stale_profile.join("Preferences"), b"stale profile").unwrap();

    let (noop_renderer, _) = MockRenderer::new(|_s, _e, _c| Box::pin(async { Ok(()) }));
    let restored = SessionExportStore::new(&artifacts, &metadata, noop_renderer);
    let recovered = restored.get(first_id).await.unwrap();
    assert_eq!(recovered.status, "failed");
    assert_eq!(
        recovered.error.unwrap()["code"],
        json!("export_interrupted")
    );
    assert!(
        !interrupted_work.exists(),
        "interrupted staging files must be removed"
    );
    assert!(
        !stale_profile.exists(),
        "interrupted browser profiles must be removed"
    );

    // Verify tampered file detection (409 export_file_changed)
    let second_job = restored.get(&jobs[1].id).await.unwrap();
    let second_dir = restored.directory(&jobs[1].id).await.unwrap();
    fs::write(second_dir.join(&second_job.files[0].name), b"tampered").unwrap();
    let tamper_err = restored
        .file(&jobs[1].id, 0)
        .await
        .expect_err("tampered file must be rejected");
    assert_eq!(tamper_err.code, "export_file_changed");
    restored.close().await;

    // Verify 2nd page collision rolls back 1st page and increments group to -2
    let col_dir = dir.path().join("collision");
    let sess_dir = col_dir.join("artifacts/2026-10-31/sessions");
    fs::create_dir_all(&sess_dir).unwrap();
    let occupied = "检查磁盘内存-20261031-123456-02.png";
    fs::write(sess_dir.join(occupied), b"existing page").unwrap();

    let (png_renderer, _) = MockRenderer::new(|_s, emit, _c| {
        Box::pin(async move {
            emit("png", b"page 1".to_vec()).await?;
            emit("png", b"page 2".to_vec()).await?;
            Ok(())
        })
    });
    let col_store = SessionExportStore::new(
        col_dir.join("artifacts"),
        col_dir.join("metadata"),
        png_renderer,
    );
    let created = col_store.create(&fixture_snapshot("png")).await.unwrap();
    let completed_png = loop {
        let cur = col_store.get(&created.id).await.unwrap();
        if cur.status == "completed" {
            break cur;
        }
        assert_ne!(cur.status, "failed", "{:?}", cur.error);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let names: Vec<String> = completed_png.files.into_iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        vec![
            "检查磁盘内存-20261031-123456-2-01.png".to_owned(),
            "检查磁盘内存-20261031-123456-2-02.png".to_owned(),
        ]
    );
    assert_eq!(fs::read(sess_dir.join(occupied)).unwrap(), b"existing page");
    col_store.close().await;
}

#[tokio::test]
async fn store_shutdown_removes_orphaned_work_without_loading_records() {
    let dir = TempDirGuard::new("export-orphan-cleanup");
    let metadata = dir.path().join("metadata");
    let stale_work = metadata.join("work").join(Uuid::new_v4().to_string());
    fs::create_dir_all(&stale_work).unwrap();
    fs::write(stale_work.join("01.png"), b"interrupted image").unwrap();
    let (renderer, _) = MockRenderer::new(|_s, _e, _c| Box::pin(async { Ok(()) }));
    let store = SessionExportStore::new(dir.path().join("artifacts"), &metadata, renderer);
    store.close().await;
    assert!(!metadata.join("work").exists());
}

#[tokio::test]
async fn store_reads_preserve_active_staging_files() {
    let dir = TempDirGuard::new("export-active-staging");
    let metadata = dir.path().join("metadata");
    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let (renderer, _) = MockRenderer::new(move |_snapshot, emit, _cancel| {
        let mut gate = gate_rx.clone();
        Box::pin(async move {
            emit("png", b"active image".to_vec()).await?;
            while !*gate.borrow_and_update() {
                gate.changed().await.unwrap();
            }
            Ok(())
        })
    });
    let store = SessionExportStore::new(dir.path().join("artifacts"), &metadata, renderer);
    let job = store.create(&fixture_snapshot("png")).await.unwrap();
    let staging_file = metadata.join("work").join(&job.id).join("01.png");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !staging_file.exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.get(&job.id).await.unwrap().status, "rendering");
    assert_eq!(store.list("fixture").await.unwrap().len(), 1);
    assert!(staging_file.exists(), "reads must preserve active output");
    gate_tx.send(true).unwrap();
    while store.get(&job.id).await.unwrap().status != "completed" {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    store.close().await;
    assert!(!metadata.join("work").exists());
    assert_eq!(store.file(&job.id, 0).await.unwrap().data, b"active image");
}

#[tokio::test]
async fn offline_html_parallel_assets_cache_cancellation_and_ssrf_guard() {
    let requests: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let fail_retry = Arc::new(AtomicBool::new(true));

    let req_for_fetcher = requests.clone();
    let active_for_fetcher = active.clone();
    let peak_for_fetcher = peak.clone();
    let fail_for_fetcher = fail_retry.clone();

    let origin = "http://127.0.0.1:52831";
    let pack = OfflineHtml::with_fetcher(
        origin,
        Arc::new(move |url, _cancel| {
            let req_map = req_for_fetcher.clone();
            let active = active_for_fetcher.clone();
            let peak = peak_for_fetcher.clone();
            let fail = fail_for_fetcher.clone();
            Box::pin(async move {
                let path = url::Url::parse(&url).unwrap().path().to_owned();
                *req_map.lock().unwrap().entry(path.clone()).or_insert(0) += 1;
                let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(25)).await;
                active.fetch_sub(1, Ordering::SeqCst);

                if path == "/fonts/retry.woff2" && fail.load(Ordering::SeqCst) {
                    return Ok(AssetResponse {
                        status: 503,
                        content_type: None,
                        body: Box::pin(stream::empty()),
                    });
                }
                let data = Bytes::from(format!("font:{path}"));
                Ok(AssetResponse {
                    status: 200,
                    content_type: Some("font/woff2".to_owned()),
                    body: Box::pin(stream::once(async move { Ok(data) })),
                })
            })
        }),
    )
    .unwrap();

    let mut snap = fixture_snapshot("html");
    snap["title"] = json!("<导出 & 标题>");
    let source = ExportDocumentSource {
        body: r#"<main class="export-document"><details><summary>过程</summary>完整内容</details></main>"#.to_owned(),
        styles: vec![ExportStyleSheet {
            base: origin.to_owned(),
            css: r#"@font-face{src:url("/fonts/a.woff2")}@font-face{src:url("/fonts/b.woff2")}p{background:url("/fonts/a.woff2")}</style>"#.to_owned(),
        }],
    };

    let html_bytes = pack
        .render(&snap, &source, CancellationToken::new())
        .await
        .unwrap();
    let html_str = String::from_utf8(html_bytes).unwrap();
    assert!(html_str.starts_with("<!DOCTYPE html>"));
    assert!(html_str.contains("&lt;导出 &amp; 标题&gt;"));
    assert!(html_str.contains(r"<\/style>"));
    assert!(peak.load(Ordering::SeqCst) > 1 && peak.load(Ordering::SeqCst) <= 4);
    assert_eq!(requests.lock().unwrap().get("/fonts/a.woff2"), Some(&1));

    // Cache hit on second render
    pack.render(&snap, &source, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap().get("/fonts/b.woff2"), Some(&1));

    // Failed asset is not cached and succeeds on retry
    let retry_source = ExportDocumentSource {
        body: "<main>fixture</main>".to_owned(),
        styles: vec![ExportStyleSheet {
            base: origin.to_owned(),
            css: r#"p{src:url("/fonts/retry.woff2")}"#.to_owned(),
        }],
    };
    let err = pack
        .render(&snap, &retry_source, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(err.code, "export_asset_unavailable");
    fail_retry.store(false, Ordering::SeqCst);
    pack.render(&snap, &retry_source, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap().get("/fonts/retry.woff2"), Some(&2));

    // External and /api/* URLs are rejected
    for forbidden in [
        "https://external.invalid/font.woff2",
        "http://127.0.0.1:52831/api/auth",
    ] {
        let bad_source = ExportDocumentSource {
            body: "<main>fixture</main>".to_owned(),
            styles: vec![ExportStyleSheet {
                base: origin.to_owned(),
                css: format!(r#"p{{src:url("{forbidden}")}}"#),
            }],
        };
        let err = pack
            .render(&snap, &bad_source, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.code, "export_asset_forbidden");
    }
}

#[tokio::test]
async fn bitlocker_verification_projection_and_export_http_routes() {
    let dir = TempDirGuard::new("export-http-routes");
    let kit_root = dir.path().canonicalize().unwrap();
    for name in SKILL_NAMES {
        let p = kit_root.join(format!("agent/home/skills/{name}/SKILL.md"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(
            p,
            format!("---\nname: {name}\ndescription: {name} 工具指南\n---\n维护指南。"),
        )
        .unwrap();
    }
    fs::write(
        kit_root.join("agent/home/APPEND_SYSTEM.md"),
        "CST Pilot 固定诊断规则。",
    )
    .unwrap();

    let kit = Kit::new(&kit_root).unwrap();
    let oa = OaClient::new("https://oa.example").unwrap();
    let auth = AuthRuntime::create(&kit_root, Channel::Gui, oa)
        .await
        .unwrap();
    auth.runtime()
        .set_runtime_api_key("openai", "sk-test-openai")
        .await
        .unwrap();
    auth.runtime()
        .set_runtime_api_key("deepseek", "sk-test-deepseek")
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

    let opened_dirs: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let opened_for_cfg = opened_dirs.clone();
    let last_rendered_snapshot: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let snap_for_cfg = last_rendered_snapshot.clone();
    let (mock_renderer, _) = MockRenderer::new(move |snap, emit, _cancel| {
        *snap_for_cfg.lock().unwrap() = Some(snap.clone());
        Box::pin(async move {
            emit("html", b"<!DOCTYPE html><p>exported</p>".to_vec()).await?;
            Ok(())
        })
    });

    let mut config = ServerConfig::new(&kit_root);
    config.port = Some(0);
    config.acquire_instance_lock = false;
    config.mode_config.standalone_web = true;
    config.export_renderer = Some(mock_renderer);
    config.open_export_directory = Some(Arc::new(move |p| {
        let opened = opened_for_cfg.clone();
        Box::pin(async move {
            opened.lock().unwrap().push(p);
            Ok(())
        })
    }));

    let server = RunningServer::start(host, config).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let create_sess: Value = client
        .post(format!("{}/api/sessions", server.base_url()))
        .header("Origin", server.base_url())
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "create-exp-sess")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = create_sess["id"].as_str().unwrap().to_owned();

    let slot = server.state.pool.open_saved(&sid).await.unwrap();
    let mut assistant_msg = faux_assistant_message(
        vec![faux_tool_call(
            "bitlocker",
            json!({ "scope": "unlock", "drive": "D:" })
                .as_object()
                .unwrap()
                .clone(),
            Some("bl-call-1".to_owned()),
        )],
        Some(StopReason::ToolUse),
    );
    assistant_msg.timestamp = 200;

    let assistant_for_check = assistant_msg.clone();
    let assistant_entry_id = slot
        .session()
        .with_session_manager_mut(|sm| {
            let _ = sm.append_message(AgentMessage::User(UserMessage {
                content: UserContent::Text("解锁 D 盘".to_owned()),
                timestamp: 100,
            }));
            let entry_id = sm
                .append_message(AgentMessage::Assistant(assistant_msg))
                .unwrap();
            let _ = sm.append_custom_entry(
                "cst.bitlocker-state-verification",
                Some(json!({
                    "bitlocker": {
                        "scope": "status",
                        "operation": {
                            "status": "unknown",
                            "reason": "上次操作异常中断；实时查询失败，请重新查询。"
                        },
                        "degraded": true
                    }
                })),
            );
            Ok(entry_id)
        })
        .unwrap();

    slot.session()
        .with_session_manager_mut(|sm| {
            sm.append_custom_entry(
                "cst.bitlocker-state-verification",
                Some(json!({
                    "bitlocker": {
                        "scope": "status",
                        "operation": { "status": "unknown", "reason": "latest verification" },
                        "degraded": true
                    }
                })),
            )
        })
        .unwrap();

    // Each restart updates unresolved calls with the latest verification.
    let fresh_decorators =
        cst_server::session::decorators::MessageDecorators::attach(slot.session().clone());
    assert!(
        fresh_decorators
            .verified_bitlocker_result(200, "bl-call-1")
            .is_some()
    );
    let decorated_assistant = fresh_decorators
        .decorate_message_for_snapshot(&AgentMessage::Assistant(assistant_for_check));
    assert_eq!(
        decorated_assistant["content"][0]["execution"]["result"]["details"]["bitlocker"]["operation"]
            ["status"],
        json!("unknown")
    );
    assert_eq!(
        decorated_assistant["content"][0]["execution"]["result"]["details"]["bitlocker"]["operation"]
            ["reason"],
        json!("latest verification")
    );
    drop(slot);
    server.state.pool.evict_excess_idle_slots().await;

    let create_resp = client
        .post(format!("{}/api/sessions/{sid}/exports", server.base_url()))
        .header("Origin", server.base_url())
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "create-exp-1")
        .json(&json!({ "format": "html", "theme": "light", "includeTools": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), http::StatusCode::ACCEPTED);
    let created_job: Value = create_resp.json().await.unwrap();
    let exp_id = created_job["id"].as_str().unwrap().to_owned();

    let completed_job = loop {
        let j: Value = client
            .get(format!("{}/api/exports/{exp_id}", server.base_url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if j["status"] == "completed" {
            break j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let _ = assistant_entry_id;
    let file_url = completed_job["files"][0]["url"].as_str().unwrap();
    let dl_resp = client
        .get(format!("{}{file_url}", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(dl_resp.status(), http::StatusCode::OK);
    assert!(
        dl_resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("attachment; filename*=UTF-8''")
    );
    assert!(dl_resp.headers().get("content-security-policy").is_some());
    assert_eq!(
        dl_resp.bytes().await.unwrap().as_ref(),
        b"<!DOCTYPE html><p>exported</p>"
    );

    // Open export directory via POST /api/exports/:id/open (rejects non-empty body, succeeds on {})
    let bad_open = client
        .post(format!("{}/api/exports/{exp_id}/open", server.base_url()))
        .header("Origin", server.base_url())
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "bad-open")
        .json(&json!({ "directory": "C:/Windows" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_open.status(), http::StatusCode::BAD_REQUEST);

    let ok_open = client
        .post(format!("{}/api/exports/{exp_id}/open", server.base_url()))
        .header("Origin", server.base_url())
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "ok-open")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_open.status(), http::StatusCode::OK);
    assert_eq!(opened_dirs.lock().unwrap().len(), 1);

    server.shutdown().await;
}

#[cfg(windows)]
#[tokio::test]
async fn chromiumoxide_browser_reuse_context_isolation_crash_recovery_and_renderer() {
    let dir = TempDirGuard::new("export-chromiumoxide");
    let work_dir = dir.path().join("work");

    // Start a tiny local HTTP test server for browser & renderer verification
    let html_state: Arc<Mutex<String>> = Arc::new(Mutex::new(
        "<!DOCTYPE html><title>fixture</title>".to_owned(),
    ));
    let forbidden_hits = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = format!("http://127.0.0.1:{port}");

    let html_for_srv = html_state.clone();
    let forbidden_for_srv = forbidden_hits.clone();
    let srv_task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let html = html_for_srv.lock().unwrap().clone();
            let forbidden = forbidden_for_srv.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                if req.contains("GET /api/") {
                    forbidden.fetch_add(1, Ordering::SeqCst);
                }
                let body = html.into_bytes();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });

    let browser = ExportBrowser::new(ExportBrowserOptions::new(&work_dir));
    let cancel = CancellationToken::new();

    // 1. First run sets localStorage in its BrowserContext
    let u1 = origin.clone();
    let first_pid = browser
        .run(cancel.clone(), |page, lease| async move {
            page.goto(u1.as_str()).await.unwrap();
            page.evaluate("localStorage.setItem('secret', 'first-job')")
                .await
                .unwrap();
            Ok(lease.pid().expect("browser pid"))
        })
        .await
        .unwrap();

    // 2. Second run reuses the same browser process PID with isolated empty localStorage
    let u2 = origin.clone();
    let second_pid = browser
        .run(cancel.clone(), |page, lease| async move {
            page.goto(u2.as_str()).await.unwrap();
            let secret: String = page
                .evaluate("localStorage.getItem('secret') ?? ''")
                .await
                .unwrap()
                .into_value()
                .unwrap();
            assert_eq!(secret, "", "BrowserContext must isolate localStorage");
            Ok(lease.pid().expect("browser pid"))
        })
        .await
        .unwrap();
    assert_eq!(first_pid, second_pid);

    // 3. Kill the browser process mid-run and verify 4 concurrent callers recover on 1 shared new PID
    let _ = browser
        .run(cancel.clone(), |page, lease| async move {
            lease.kill_process().await;
            let _ = page.evaluate("new Promise(() => {})").await;
            Ok(())
        })
        .await;

    let mut handles = Vec::new();
    for _ in 0..4 {
        let b = browser.clone();
        let u = origin.clone();
        let c = cancel.clone();
        handles.push(tokio::spawn(async move {
            b.run(c, |page, lease| async move {
                page.goto(u.as_str()).await.unwrap();
                Ok(lease.pid().expect("recovered pid"))
            })
            .await
            .unwrap()
        }));
    }
    let mut recovered_pids = Vec::new();
    for h in handles {
        recovered_pids.push(h.await.unwrap());
    }
    assert_ne!(recovered_pids[0], first_pid);
    for pid in &recovered_pids {
        assert_eq!(*pid, recovered_pids[0]);
    }
    browser.close().await;
    assert_eq!(fs::read_dir(&work_dir).unwrap().count(), 0);

    // 4. ChromiumoxideRenderer PNG multi-page slicing & /api/* interception + HTML sanitization
    *html_state.lock().unwrap() = r#"<!DOCTYPE html><html data-export-receiver="ready"><script>addEventListener('message',async()=>{await fetch('/api/auth').catch(()=>{});document.documentElement.dataset.exportState='ready'})</script><style>body{margin:0}.export-message{width:100%}</style><main class="export-document"><section class="export-message" style="height:1800px;background:rgb(190,30,20)"></section><section class="export-message" style="height:2500px;background:rgb(30,180,40)"></section></main></html>"#.to_owned();

    let renderer =
        ChromiumoxideRenderer::new(&origin, ExportBrowserOptions::new(dir.path().join("rwork")))
            .unwrap();
    let png_snap = fixture_snapshot("png");
    let mut pages: Vec<Vec<u8>> = Vec::new();
    {
        let pages_ref = &mut pages;
        let mut emit =
            |ext: &'static str,
             bytes: Vec<u8>|
             -> pi_agent::BoxFuture<'_, Result<(), cst_server::error::ServiceError>> {
                assert_eq!(ext, "png");
                pages_ref.push(bytes);
                Box::pin(async { Ok(()) })
            };
        renderer
            .render(&png_snap, &mut emit, CancellationToken::new())
            .await
            .unwrap();
    }
    assert_eq!(pages.len(), 2);
    // PNG IHDR width at offset 16..20 must be 2480 (1240 CSS px * 2x scale)
    for png in &pages {
        let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        assert_eq!(width, 2480);
    }
    assert_eq!(
        forbidden_hits.load(Ordering::SeqCst),
        0,
        "Fetch interception must block /api/auth"
    );

    *html_state.lock().unwrap() = r#"<!DOCTYPE html><html data-export-receiver="ready"><script>addEventListener('message',()=>document.documentElement.dataset.exportState='ready')</script><main class="export-document"><p onclick="alert(1)">安全正文</p><button>移除按钮</button></main></html>"#.to_owned();
    let html_snap = fixture_snapshot("html");
    let mut html_out = Vec::new();
    {
        let out_ref = &mut html_out;
        let mut emit =
            |ext: &'static str,
             bytes: Vec<u8>|
             -> pi_agent::BoxFuture<'_, Result<(), cst_server::error::ServiceError>> {
                assert_eq!(ext, "html");
                *out_ref = bytes;
                Box::pin(async { Ok(()) })
            };
        renderer
            .render(&html_snap, &mut emit, CancellationToken::new())
            .await
            .unwrap();
    }
    let rendered_html = String::from_utf8(html_out).unwrap();
    assert!(rendered_html.contains("安全正文"));
    assert!(!rendered_html.contains("onclick="));
    assert!(!rendered_html.contains("<button"));
    assert!(!rendered_html.contains("<script"));

    renderer.close().await;
    srv_task.abort();
}

#[test]
fn artifact_sessions_dir_rejects_ntfs_junction_escape() {
    let dir = TempDirGuard::new("export-junction");
    let artifacts = dir.path().join("artifacts");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let day = "2026-10-31";
    let valid = ensure_artifact_sessions_dir(&artifacts, day).unwrap();
    assert!(valid.ends_with(Path::new("artifacts/2026-10-31/sessions")));

    #[cfg(windows)]
    {
        fs::remove_dir_all(&valid).unwrap();
        let status = std::process::Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/J",
                &valid.to_string_lossy(),
                &outside.to_string_lossy(),
            ])
            .output()
            .unwrap();
        assert!(status.status.success());
        let err = ensure_artifact_sessions_dir(&artifacts, day)
            .expect_err("NTFS junction escaping artifacts root must be rejected");
        assert_eq!(err.code, "export_directory_invalid");
    }
}

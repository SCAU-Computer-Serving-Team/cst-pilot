use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_client::{AppHttpClient, CstClient, DraftKey, VerifiedServerEndpoint, WorkspaceView};
use cst_desktop::open_desktop_shell_window_with_handle;
use cst_desktop::shell::register_shell_keys;
use cst_desktop::shell::sidebar::ShellRoute;
use cst_desktop::workbench::markdown_view::MarkdownStreamStore;
use cst_desktop::workbench::model_picker::{format_quota_balance, thinking_level_label};
use cst_desktop::workbench::turn_segments::{
    TurnSegmentKind, build_turn_segments, build_turn_segments_with_results, format_assistant_error,
    format_duration_zh, glyph_light_strength, group_turns_and_results, light_glyphs,
    process_segment_label, split_turn,
};
use cst_os::ProcessIdentity;
use gpui::{AppContext as _, EntityInputHandler};
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
        let root = base.join(format!("desktop-wb-test-{}-{id}", std::process::id()));
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
fn turn_segments_special_isolation_and_running_light_parity() {
    let entries = vec![
        json!({
            "id": "u-1",
            "role": "user",
            "timestamp": 1000,
            "content": "帮我检查整机概览与 C 盘空间"
        }),
        json!({
            "id": "a-1",
            "role": "assistant",
            "timestamp": 1200,
            "durationMs": 2400,
            "content": [
                { "type": "thinking", "thinking": "先看整机概览，再查磁盘空间。" },
                { "type": "toolCall", "id": "tc-disk", "name": "disk", "arguments": { "scope": "volumes" } },
                { "type": "toolCall", "id": "tc-sys", "name": "sys", "arguments": { "scope": "overview" } }
            ]
        }),
        json!({
            "role": "toolResult",
            "toolCallId": "tc-disk",
            "toolName": "disk",
            "content": [{ "type": "text", "text": "{\"volumes\":[]}" }]
        }),
        json!({
            "role": "toolResult",
            "toolCallId": "tc-sys",
            "toolName": "sys",
            "content": [{ "type": "text", "text": "{\"machine\":\"Legion\"}" }]
        }),
        json!({
            "id": "a-2",
            "role": "assistant",
            "timestamp": 3800,
            "durationMs": 1600,
            "content": [
                { "type": "thinking", "thinking": "汇总分析结果。" },
                { "type": "text", "text": "巡检完成，系统盘空间充足。" }
            ]
        }),
    ];

    let (turns, results) = group_turns_and_results(&entries);
    assert_eq!(turns.len(), 1);

    let split = split_turn(&turns[0].assistants);
    assert_eq!(split.total_duration_ms, 4000);
    assert_eq!(split.final_text, "巡检完成，系统盘空间充足。");
    assert_eq!(split.final_entry_id.as_deref(), Some("a-2"));

    let segments = build_turn_segments(split.process);
    // Expected: [0] Process (`thinking` + `disk`), [1] Visible (`sys(overview)`), [2] Process (`thinking`)
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].kind, TurnSegmentKind::Process);
    assert_eq!(segments[1].kind, TurnSegmentKind::Visible);
    assert_eq!(segments[2].kind, TurnSegmentKind::Process);

    // `sys(overview)` MUST NOT be counted in ordinary tool call counts!
    let first_label = process_segment_label(&segments[0].parts, &results, false);
    assert_eq!(first_label.as_deref(), Some("已完成 1 次工具调用"));

    // Post-special thinking-only segment shows `"思考过程"`
    let tail_label = process_segment_label(&segments[2].parts, &results, false);
    assert_eq!(tail_label.as_deref(), Some("思考过程"));

    assert_eq!(format_duration_zh(400), "不足 1 秒");
    assert_eq!(format_duration_zh(4200), "4 秒");
    assert_eq!(format_duration_zh(125_000), "2 分 5 秒");

    // Error prose formatting (`{服务名} {故障现象}，{处理指引}。`)
    let auth_err = format_assistant_error(Some("DeepSeek"), "HTTP 401 invalid api key");
    assert_eq!(
        auth_err.headline,
        "DeepSeek 凭据无效或已过期，请在「设置 · 模型服务」更新后重试。"
    );

    // `RunningLight` grapheme cluster segmentation (CJK + emoji ZWJ stays intact as 1 grapheme!)
    let glyphs = light_glyphs("工作中👨‍👩‍👧3秒");
    assert_eq!(glyphs, vec!["工", "作", "中", "👨‍👩‍👧", "3", "秒"]);
    let peak = (0..100)
        .map(|i| glyph_light_strength(0, glyphs.len(), (i as f32) / 100.0))
        .fold(0.0f32, f32::max);
    assert!(peak > 0.40 && peak <= 0.4801);

    // Quota & thinking labels
    assert_eq!(thinking_level_label("max"), "Max");
    assert_eq!(
        format_quota_balance(&json!({
            "supported": true,
            "provider": "cstoa",
            "unit": "credit",
            "balance": 12500.0
        }))
        .as_deref(),
        Some("12,500 额度点")
    );

    // `read` with returned image splits out of collapsed `Process` into `Visible`
    let read_entries = vec![
        json!({
            "id": "u-img",
            "message": { "role": "user", "timestamp": 10, "content": [{"type": "text", "text": "查截图"}] }
        }),
        json!({
            "id": "a-img",
            "message": {
                "role": "assistant",
                "timestamp": 20,
                "durationMs": 500,
                "content": [
                    {"type": "toolCall", "id": "tc-d", "name": "disk", "arguments": {"scope": "space"}},
                    {"type": "toolCall", "id": "tc-img", "name": "read", "arguments": {"path": "C:\\bsod.png"}},
                    {"type": "toolCall", "id": "tc-e", "name": "eventlog", "arguments": {"scope": "security"}}
                ]
            }
        }),
        json!({
            "id": "r-d",
            "message": { "role": "toolResult", "toolCallId": "tc-d", "toolName": "disk", "isError": false, "content": [{"type": "text", "text": "ok"}] }
        }),
        json!({
            "id": "r-img",
            "message": { "role": "toolResult", "toolCallId": "tc-img", "toolName": "read", "isError": false, "content": [{"type": "text", "text": "C:\\bsod.png"}, {"type": "image", "mimeType": "image/png", "data": "AAAA"}] }
        }),
        json!({
            "id": "r-e",
            "message": { "role": "toolResult", "toolCallId": "tc-e", "toolName": "eventlog", "isError": false, "content": [{"type": "text", "text": "ok"}] }
        }),
    ];
    let (img_turns, img_results) = group_turns_and_results(&read_entries);
    let img_split = split_turn(&img_turns[0].assistants);
    let img_segs = build_turn_segments_with_results(img_split.process, &img_results);
    assert_eq!(img_segs.len(), 3);
    assert_eq!(img_segs[0].kind, TurnSegmentKind::Process);
    assert_eq!(img_segs[1].kind, TurnSegmentKind::Visible);
    assert_eq!(img_segs[2].kind, TurnSegmentKind::Process);
    assert_eq!(
        process_segment_label(&img_segs[0].parts, &img_results, false).as_deref(),
        Some("已完成 1 次工具调用")
    );
    assert_eq!(
        process_segment_label(&img_segs[2].parts, &img_results, false).as_deref(),
        Some("已完成 1 次工具调用")
    );
}

#[test]
fn standalone_reads_preserve_order_and_process_expansion_identity() {
    let entries = [json!({
        "id": "a",
        "role": "assistant",
        "timestamp": 20,
        "content": [
            {"type": "toolCall", "id": "read-1", "name": "read"},
            {"type": "toolCall", "id": "read-2", "name": "read"},
            {"type": "toolCall", "id": "read-3", "name": "read"},
            {"type": "toolCall", "id": "disk", "name": "disk"},
            {"type": "toolCall", "id": "sys", "name": "sys"}
        ]
    })];
    let (turns, _) = group_turns_and_results(&entries);
    let parts = split_turn(&turns[0].assistants).process;
    let image_result = json!({"content": [{"type": "image", "data": "AAAA"}]});
    let mut results = std::collections::BTreeMap::from([
        ("read-2".to_owned(), image_result.clone()),
        (
            "read-3".to_owned(),
            json!({"content": [{"type": "image", "data": " "}]}),
        ),
    ]);

    let before = build_turn_segments_with_results(parts.clone(), &results);
    assert_eq!(before.len(), 4);
    let process_anchor = before[2].expansion_anchor();
    assert_eq!(process_anchor, Some((0, 2)));

    results.insert("read-1".to_owned(), image_result);
    let after = build_turn_segments_with_results(parts, &results);
    assert_eq!(after.len(), 4);
    assert_eq!(after[0].key, "process-start");
    assert!(after[0].parts.is_empty());
    assert_eq!(after[0].expansion_anchor(), None);
    assert_eq!(after[1].kind, TurnSegmentKind::Visible);
    assert_ne!(after[2].key, before[2].key);
    assert_eq!(after[2].expansion_anchor(), process_anchor);
    assert_eq!(
        process_segment_label(&after[2].parts, &results, false).as_deref(),
        Some("已完成 1 / 2 次工具调用")
    );
    let actual: Vec<_> = after[1]
        .parts
        .iter()
        .flat_map(|part| match &part.part {
            cst_desktop::workbench::turn_segments::ConversationPart::Tools(calls) => {
                calls.iter().map(|call| call.id.as_str())
            }
            _ => panic!("expected standalone reads"),
        })
        .collect();
    assert_eq!(actual, ["read-1", "read-2"]);
}

#[gpui_kit::test]
async fn streaming_markdown_incremental_push_str_and_unclosed_code_fence(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.update(gpui_kit::init);

    cx.update(|cx| {
        let mut store = MarkdownStreamStore::default();
        let mut acc = String::new();

        let first_entity = store.sync_entry("msg-stream-1", "# 磁盘与驱动巡检报告\n\n", cx);
        let first_id = first_entity.entity_id();
        acc.push_str("# 磁盘与驱动巡检报告\n\n");

        // Stream 120 incremental chunks including a long single paragraph and an unclosed ```powershell code block
        for i in 0..80 {
            let chunk = format!("第{i}段连续采样说明，核对系统卷簇大小与驱动签名状态。");
            acc.push_str(&chunk);
            let current = store.sync_entry("msg-stream-1", &acc, cx);
            assert_eq!(
                current.entity_id(),
                first_id,
                "streaming delta {i} must mutate existing TextViewState via push_str without recreating Entity"
            );
        }

        acc.push_str("\n\n```powershell\nGet-Volume | Format-Table DriveLetter, SizeRemaining\n");
        for i in 0..40 {
            let line = format!("Write-Output '采样行 {i}'\n");
            acc.push_str(&line);
            let current = store.sync_entry("msg-stream-1", &acc, cx);
            assert_eq!(current.entity_id(), first_id);
        }

        // Finalize closing fence via `message_end` snapshot reconciliation (`set_text` / `push_str`)
        acc.push_str("```\n\n| 盘符 | 剩余空间 |\n|---|---|\n| C: | 128 GB |\n");
        let finalized = store.sync_entry("msg-stream-1", &acc, cx);
        assert_eq!(finalized.entity_id(), first_id);
        assert_eq!(store.len(), 1);
    });
}

#[derive(Default)]
struct WorkbenchMockState {
    created_sessions: usize,
    sent_messages: Vec<Value>,
    reordered_queues: Vec<Value>,
    steered_items: Vec<String>,
    selected_thinking_bodies: Vec<Value>,
}

struct WorkbenchMockBackend {
    rt: Option<tokio::runtime::Runtime>,
    port: u16,
    token: String,
    state: Arc<Mutex<WorkbenchMockState>>,
    stop: Arc<AtomicBool>,
}

async fn read_full_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Option<(String, String, String)> {
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
    let head_str = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let content_len = head_str
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
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
    let first_line = head_str.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    let body = String::from_utf8_lossy(&data[header_end..data.len().min(header_end + content_len)])
        .into_owned();
    Some((method, path, body))
}

impl WorkbenchMockBackend {
    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().unwrap()
    }

    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(Mutex::new(WorkbenchMockState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let token = "wb-test-token".to_owned();

        let state_srv = Arc::clone(&state);
        let stop_srv = Arc::clone(&stop);
        let port = rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                while !stop_srv.load(Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let st = Arc::clone(&state_srv);
                    let st_stop = Arc::clone(&stop_srv);
                    tokio::spawn(async move {
                        let Some((method_owned, path_owned, body_owned)) =
                            read_full_http_request(&mut stream).await
                        else {
                            return;
                        };
                        let method = method_owned.as_str();
                        let path = path_owned.as_str();
                        let body_str = body_owned.as_str();

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
                                        "multiplexStream": true
                                    }),
                                ),
                                ("GET", "/api/sessions") => {
                                    let sessions = if guard.created_sessions > 0 {
                                        vec![json!({
                                            "id": "sess-new",
                                            "title": "磁盘空间与蓝屏巡检",
                                            "running": true
                                        })]
                                    } else {
                                        Vec::new()
                                    };
                                    ("HTTP/1.1 200 OK", json!({ "sessions": sessions }))
                                }
                                ("POST", "/api/sessions") => {
                                    guard.created_sessions += 1;
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "id": "sess-new",
                                            "title": "磁盘空间与蓝屏巡检",
                                            "running": false
                                        }),
                                    )
                                }
                                ("POST", "/api/sessions/sess-new/messages") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.sent_messages.push(v);
                                    }
                                    (
                                        "HTTP/1.1 202 Accepted",
                                        json!({ "accepted": true, "sessionId": "sess-new" }),
                                    )
                                }
                                ("GET", "/api/sessions/sess-new") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "id": "sess-new",
                                        "running": true,
                                        "contextUsage": {
                                            "tokens": 24000,
                                            "contextWindow": 128000,
                                            "percent": 18.75
                                        },
                                        "queue": {
                                            "version": 7,
                                            "paused": false,
                                            "items": [
                                                { "id": "q1", "text": "补充查看系统事件日志", "status": "pending", "delivery": "queue" },
                                                { "id": "q2", "text": "检查显卡驱动版本", "status": "pending", "delivery": "queue" }
                                            ]
                                        },
                                        "messages": [
                                            { "id": "u-1", "role": "user", "timestamp": 1000, "content": "你好，排查蓝屏" },
                                            { "id": "a-1", "role": "assistant", "timestamp": 1200, "durationMs": 1500, "content": "正在收集转储与磁盘状态。" }
                                        ],
                                        "entries": []
                                    }),
                                ),
                                ("PUT", p) if p.starts_with("/api/events/stream/") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "ok": true }),
                                ),
                                ("PUT", "/api/sessions/sess-new/queue/order") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.reordered_queues.push(v);
                                    }
                                    if guard.reordered_queues.len() == 1 {
                                        ("HTTP/1.1 409 Conflict", json!({
                                            "error": "queue_version_conflict",
                                            "message": "队列已更新"
                                        }))
                                    } else {
                                    ("HTTP/1.1 200 OK", json!({
                                        "queue": {
                                            "version": 8,
                                            "items": [
                                                { "id": "q2", "text": "检查显卡驱动版本", "status": "pending", "delivery": "queue" },
                                                { "id": "q1", "text": "补充查看系统事件日志", "status": "pending", "delivery": "queue" },
                                                { "id": "q3", "text": "排查最新事件", "status": "pending", "delivery": "queue" }
                                            ]
                                        }
                                    }))
                                    }
                                }
                                ("POST", "/api/sessions/sess-new/queue/q1/steer") => {
                                    guard.steered_items.push("q1".to_owned());
                                    ("HTTP/1.1 200 OK", json!({ "ok": true }))
                                }
                                ("GET", p) if p.starts_with("/api/models") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "models": [
                                            {
                                                "provider": "cstoa",
                                                "id": "gpt-5.4",
                                                "name": "GPT-5.4",
                                                "reasoning": true,
                                                "credentialReady": true,
                                                "thinkingLevels": ["low", "medium", "high", "max"],
                                                "thinkingLevel": "medium"
                                            }
                                        ],
                                        "catalog": [
                                            {
                                                "provider": "cstoa",
                                                "id": "gpt-5.4",
                                                "name": "GPT-5.4",
                                                "reasoning": true,
                                                "credentialReady": true,
                                                "thinkingLevels": ["low", "medium", "high", "max"],
                                                "thinkingLevel": "medium"
                                            }
                                        ],
                                        "selected": {
                                            "provider": "cstoa",
                                            "id": "gpt-5.4",
                                            "name": "GPT-5.4",
                                            "reasoning": true,
                                            "credentialReady": true,
                                            "thinkingLevels": ["low", "medium", "high", "max"],
                                            "thinkingLevel": "medium"
                                        },
                                        "thinkingLevels": ["low", "medium", "high", "max"],
                                        "contextUsage": {
                                            "tokens": 24000,
                                            "contextWindow": 128000,
                                            "percent": 18.75
                                        },
                                        "usage": {
                                            "cacheHitRate": 0.642
                                        },
                                        "unavailableEnabled": []
                                    }),
                                ),
                                ("POST", "/api/models/thinking") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.selected_thinking_bodies.push(v);
                                    }
                                    ("HTTP/1.1 200 OK", json!({ "ok": true }))
                                }
                                ("GET", p) if p.starts_with("/api/quota") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "supported": true,
                                        "provider": "cstoa",
                                        "unit": "credit",
                                        "balance": 12500.0
                                    }),
                                ),
                                ("GET", p) if p.starts_with("/api/files") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "total": 1,
                                        "files": [
                                            { "path": "logs/minidump.dmp", "dir": "logs" }
                                        ]
                                    }),
                                ),
                                ("GET", "/api/auth") => ("HTTP/1.1 200 OK", json!({ "providers": [] })),
                                ("GET", p) if p.starts_with("/api/account") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "providerId": "cstoa",
                                        "signedIn": true,
                                        "profile": { "supported": true, "studentId": "20260001", "name": "测试队员" }
                                    }),
                                ),
                                ("GET", "/api/app/settings") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "theme": "light",
                                        "defaultView": "gui",
                                        "sidebarCollapsed": false,
                                        "windowMaximized": false
                                    }),
                                ),
                                ("GET", "/api/ui-hints") => ("HTTP/1.1 200 OK", json!({})),
                                ("GET", "/api/app/update/status") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "status": { "phase": "idle" } }),
                                ),
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

impl Drop for WorkbenchMockBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

#[gpui_kit::test]
async fn headless_ime_composition_isolation_slash_tokens_draft_sharing_queue_and_model_picker(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = WorkbenchMockBackend::start();
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

    // 1. Focus Home Composer, open Context Popover, and simulate active Chinese IME preedit (`replace_and_mark_text_in_range`)
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("workspace-home-view").visible());

        window.click("composer-context-ring-btn", cx);
        assert!(window.find("composer-context-popover-h5jg6d").visible());

        let home_comp = shell.read(cx).workbench().read(cx).home_composer().clone();
        let textarea = home_comp.read(cx).textarea_state().clone();
        textarea.update(cx, |st, cx| {
            let fh = st.presentation().focus_handle().clone();
            fh.focus(window, cx);
            EntityInputHandler::replace_and_mark_text_in_range(st, None, "nihao", None, window, cx);
        });
        let composing = home_comp.update(cx, |comp, cx| comp.is_ime_composing(window, cx));
        assert!(
            composing,
            "TextareaState must report active IME marked range during preedit"
        );
    })
    .unwrap();

    // Pressing `enter` and `escape` during IME composition MUST NOT submit OR dismiss the open popover!
    cx.update_window(win_handle, |_, window, cx| {
        window.press("enter", cx);
        window.press("escape", cx);
        assert!(
            window.find("composer-context-popover-h5jg6d").visible(),
            "IME Escape must only end IME composition without closing open popovers"
        );

        // Now that IME composition has ended, pressing `escape` closes the popover cleanly!
        window.press("escape", cx);
        assert!(window.try_find("composer-context-popover-h5jg6d").is_none());
    })
    .unwrap();

    assert_eq!(
        backend.state.lock().unwrap().created_sessions,
        0,
        "IME candidate selection Enter must NEVER trigger session creation or message send!"
    );

    // Tab inserts at the caret, preserves the suffix and follows keyboard selection.
    cx.update_window(win_handle, |_, window, cx| {
        let textarea = shell
            .read(cx)
            .workbench()
            .read(cx)
            .home_composer()
            .read(cx)
            .textarea_state()
            .clone();
        textarea.update(cx, |st, cx| {
            st.focus(window, cx);
            st.set_value("尾部", window, cx);
        });
        window.press("home", cx);
        window.press("tab", cx);
        assert!(window.find("composer-completion-popover-p2bit").visible());
        window.press("down", cx);
        window.press("tab", cx);
        assert_eq!(textarea.read(cx).value().as_ref(), "/skill:ls 尾部");
    })
    .unwrap();

    // 2. Clear cancelled preedit, commit `/disk`, and test `/` completion -> `InlineToken` + GUI/TUI draft sharing!
    cx.update_window(win_handle, |_, window, cx| {
        let home_comp = shell.read(cx).workbench().read(cx).home_composer().clone();
        let textarea = home_comp.read(cx).textarea_state().clone();
        textarea.update(cx, |st, cx| {
            let fh = st.presentation().focus_handle().clone();
            fh.focus(window, cx);
            st.set_value("", window, cx);
            EntityInputHandler::replace_text_in_range(st, None, "/disk", window, cx);
        });
        let composing = home_comp.update(cx, |comp, cx| comp.is_ime_composing(window, cx));
        assert!(!composing);
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        // `/` slash completion popover (`p2biT`) must be visible!
        assert!(window.find("composer-completion-popover-p2bit").visible());
        assert!(window.find("slash-item-disk").visible());

        // Press `enter` to accept `/skill:disk` into an atomic `InlineToken`!
        window.press("enter", cx);
        assert!(
            window
                .try_find("composer-completion-popover-p2bit")
                .is_none()
        );

        // Append user diagnostic question after the inline token
        let home_comp = shell.read(cx).workbench().read(cx).home_composer().clone();
        let textarea = home_comp.read(cx).textarea_state().clone();
        textarea.update(cx, |st, cx| {
            st.insert("你好，排查蓝屏", window, cx);
        });
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    // Verify GUI <-> TUI shared `WorkspaceState` draft holds both `skill: "disk"` and `"你好，排查蓝屏"`
    let (draft_skill, draft_text) = handle.with_workspace_mut(|ws| {
        ws.switch_view(WorkspaceView::Tui, None);
        ws.switch_view(WorkspaceView::Gui, None);
        let d = ws.draft(&DraftKey::NewSession).unwrap();
        (d.skill_name().map(str::to_owned), d.text.clone())
    });
    assert_eq!(draft_skill.as_deref(), Some("disk"));
    assert_eq!(draft_text, "你好，排查蓝屏");

    // 3. Press `enter` (with IME idle) -> optimistic two-phase Home submission transitions to `ShellRoute::Session`!
    cx.update_window(win_handle, |_, window, cx| {
        window.press("enter", cx);
        window.press("enter", cx); // The unacknowledged draft must not be submitted again.
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(3), |window, _| {
        window.try_find("workspace-session-view").is_some()
            && window.try_find("composer-queue-bar-rneha").is_some()
    })
    .await;

    cx.update(|cx| {
        assert_eq!(shell.read(cx).route(), ShellRoute::Session);
    });
    {
        let guard = backend.state.lock().unwrap();
        assert_eq!(guard.created_sessions, 1);
        assert_eq!(guard.sent_messages.len(), 1);
        assert_eq!(
            guard.sent_messages[0].get("skill").and_then(Value::as_str),
            Some("disk")
        );
        assert_eq!(
            guard.sent_messages[0].get("text").and_then(Value::as_str),
            Some("你好，排查蓝屏")
        );
    }

    // 4. Verify `QueueBar` (`rnehA`) reorder (`version = 7`) and `立即` steer!
    cx.update_window(win_handle, |_, window, cx| {
        assert!(window.find("session-header-hukqs").visible());
        window.click("queue-move-down-q1", cx);
        // Projection changes the row order immediately, before the network response.
        assert!(
            window.find("queue-item-row-q2").bounds().origin.y
                < window.find("queue-item-row-q1").bounds().origin.y
        );
    })
    .unwrap();

    // A rejected mutation restores the authoritative order and allows retry.
    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("workspace-top-error-bar").is_some()
            && window.try_find("queue-move-down-q1").is_some()
    })
    .await;
    cx.update_window(win_handle, |_, window, cx| {
        window.click("queue-move-down-q1", cx);
    })
    .unwrap();
    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("queue-item-row-q3").is_some()
    })
    .await;
    cx.update_window(win_handle, |_, window, cx| {
        window.click("queue-steer-btn-q1", cx);
    })
    .unwrap();

    let st_wait_q = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        let g = st_wait_q.lock().unwrap();
        !g.reordered_queues.is_empty() && !g.steered_items.is_empty()
    })
    .await;
    {
        let guard = backend.state.lock().unwrap();
        assert_eq!(guard.reordered_queues.len(), 2);
        for reorder in &guard.reordered_queues {
            assert_eq!(reorder["version"], 7);
            assert_eq!(reorder["ids"], json!(["q2", "q1"]));
        }
        assert_eq!(guard.steered_items, vec!["q1"]);
    }

    // 5. Verify `336px` Context & Quota Popover (`h5jG6d`) and Two-Level Model Picker (`d8zzg` -> `L7GWjV` `Max`)
    cx.update_window(win_handle, |_, window, cx| {
        window.click("composer-context-ring-btn", cx);
        let ctx_pop = window.find("composer-context-popover-h5jg6d");
        assert_eq!(f32::from(ctx_pop.bounds().size.width), 336.0);

        // Press `escape` -> closes context popover and restores focus to ring button
        window.press("escape", cx);
        assert!(window.try_find("composer-context-popover-h5jg6d").is_none());

        // Open two-level Model Picker (`d8zzg`) -> Thinking page (`L7GWjV`) -> select `Max`!
        window.click("composer-model-picker-btn", cx);
        assert!(window.find("composer-model-picker-d8zzg").visible());
        window.press("down", cx);
        window.press("enter", cx);
        assert!(window.find("thinking-option-max").visible());
        window.press("end", cx);
        window.press("enter", cx);
        assert!(window.try_find("composer-model-picker-d8zzg").is_none());
    })
    .unwrap();

    let st_wait_t = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        !st_wait_t
            .lock()
            .unwrap()
            .selected_thinking_bodies
            .is_empty()
    })
    .await;
    {
        let guard = backend.state.lock().unwrap();
        assert_eq!(guard.selected_thinking_bodies.len(), 1);
        assert_eq!(
            guard.selected_thinking_bodies[0],
            json!({ "sessionId": "sess-new", "level": "max" })
        );
    }
}

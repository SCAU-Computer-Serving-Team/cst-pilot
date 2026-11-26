use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_client::{AppHttpClient, CstClient, DraftKey, VerifiedServerEndpoint, WorkspaceView};
use cst_desktop::open_desktop_shell_window_with_handle;
use cst_desktop::shell::register_shell_keys;
use cst_desktop::shell::sidebar::ShellRoute;
use cst_desktop::theme::motion::{DigitDiffPlan, plan_label_digit_diff};
use cst_desktop::workbench::markdown_view::MarkdownStreamStore;
use cst_desktop::workbench::model_picker::{format_quota_balance, thinking_level_label};
use cst_desktop::workbench::turn_segments::{
    TurnSegmentKind, build_turn_segments, build_turn_segments_with_results, format_assistant_error,
    format_duration_zh, glyph_light_strength, group_turns_and_results, light_glyphs,
    process_segment_label, segment_is_running, split_turn,
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

    assert_eq!(format_duration_zh(400), "0.4s");
    assert_eq!(format_duration_zh(4200), "4.2s");
    assert_eq!(format_duration_zh(125_000), "125.0s");

    // Error prose formatting (`{服务名} {故障现象}，{处理指引}。`)
    let auth_err = format_assistant_error(Some("DeepSeek"), "HTTP 401 invalid api key");
    assert_eq!(
        auth_err.headline,
        "DeepSeek 凭据无效或已过期，请在「设置 · 提供商」更新后重试。"
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

    // Intermediate assistant prose before tool calls stays inside the collapsible `Process`
    // segment (web parity), so live working progress can be folded cleanly under `process-start`.
    let prose_before_tool = vec![
        json!({
            "id": "u-c",
            "role": "user",
            "timestamp": 100,
            "content": "看看C盘"
        }),
        json!({
            "id": "a-c1",
            "role": "assistant",
            "timestamp": 200,
            "durationMs": 1200,
            "content": [
                { "type": "text", "text": "我先读取一下 disk 工具说明和 C 盘的空间情况。" },
                { "type": "toolCall", "id": "tc-read", "name": "read", "arguments": { "path": "disk.md" } }
            ]
        }),
        json!({
            "role": "toolResult",
            "toolCallId": "tc-read",
            "toolName": "read",
            "content": [{ "type": "text", "text": "ok" }]
        }),
        json!({
            "id": "a-c2",
            "role": "assistant",
            "timestamp": 1500,
            "durationMs": 900,
            "content": [
                { "type": "text", "text": "C 盘目前剩余 74.2 GB。" }
            ]
        }),
    ];
    let (prose_turns, prose_results) = group_turns_and_results(&prose_before_tool);
    let prose_split = split_turn(&prose_turns[0].assistants);
    assert_eq!(prose_split.total_duration_ms, 2100);
    assert_eq!(prose_split.final_text, "C 盘目前剩余 74.2 GB。");
    let prose_segs = build_turn_segments_with_results(prose_split.process, &prose_results);
    // [0] collapsible `process-start` containing intermediate prose + 1 tool call
    assert_eq!(prose_segs.len(), 1);
    assert_eq!(prose_segs[0].kind, TurnSegmentKind::Process);
    assert_eq!(prose_segs[0].parts.len(), 2);
    assert!(prose_segs[0].expansion_anchor().is_some());
    assert_eq!(
        process_segment_label(&prose_segs[0].parts, &prose_results, false).as_deref(),
        Some("已完成 1 次工具调用")
    );
    assert!(!segment_is_running(
        &prose_segs[0].parts,
        &prose_results,
        true,
        true,
        true
    ));

    // Digit-only status rolling diff algorithm specification checks:
    assert_eq!(
        plan_label_digit_diff("工作中 52s", "工作中 52s"),
        DigitDiffPlan::Unchanged
    );
    assert_eq!(
        plan_label_digit_diff("工作中 52s", "工作中 53s"),
        DigitDiffPlan::DigitsOnly(vec![(0, 0, Some('2'), '3', true)])
    );
    assert_eq!(
        plan_label_digit_diff("工作中 59s", "工作中 60s"),
        DigitDiffPlan::DigitsOnly(vec![
            (0, 1, Some('5'), '6', true),
            (0, 0, Some('9'), '0', true),
        ])
    );
    assert_eq!(
        plan_label_digit_diff("工作中 9s", "工作中 10s"),
        DigitDiffPlan::DigitsOnly(vec![(0, 1, None, '1', true), (0, 0, Some('9'), '0', true),])
    );
    assert_eq!(
        plan_label_digit_diff("工作中 52s", "已工作 52.3s"),
        DigitDiffPlan::FullSwap
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

    // Tab accepts an open completion at the caret (preserving suffix) and lets focus escape when closed.
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
        // With no active completion menu, Tab escapes composer without inserting text or opening completion.
        window.press("tab", cx);
        assert!(
            window
                .try_find("composer-completion-popover-p2bit")
                .is_none()
        );
        assert!(
            !textarea
                .read(cx)
                .presentation()
                .focus_handle()
                .is_focused(window),
            "Tab with no completion menu must move focus out of composer textarea"
        );
        assert_eq!(textarea.read(cx).value().as_ref(), "尾部");
        // Refocus composer at caret 0 and type `/` to trigger slash completion before the suffix.
        textarea.update(cx, |st, cx| {
            st.presentation().focus_handle().focus(window, cx);
            st.set_selected_range(0..0, cx);
            EntityInputHandler::replace_text_in_range(st, None, "/", window, cx);
        });
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        let textarea = shell
            .read(cx)
            .workbench()
            .read(cx)
            .home_composer()
            .read(cx)
            .textarea_state()
            .clone();
        window.render_frame(cx);
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
        let q1_y = window.find("queue-item-row-q1").bounds().origin.y;
        let q2_y = window.find("queue-item-row-q2").bounds().origin.y;
        window.click("queue-move-down-q1", cx);
        // Controls reflect the optimistic order immediately; FLIP starts the
        // rows at their previous visual positions in that same frame.
        assert!(window.try_find("queue-move-up-q1").is_some());
        assert!(window.try_find("queue-move-down-q1").is_none());
        assert_eq!(window.find("queue-item-row-q1").bounds().origin.y, q1_y);
        assert_eq!(window.find("queue-item-row-q2").bounds().origin.y, q2_y);
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
        let main_bounds = window.find("composer-model-picker-d8zzg").bounds();
        assert!(window.find("composer-model-picker-d8zzg").visible());
        assert_eq!(f32::from(main_bounds.size.width), 224.0);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();

    // Height smoothly transitions (`300ms ease_out`) from Main (76px) to Thinking (4 rows);
    // wait for both controls to be visible and the picker to finish expanding past 150px.
    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window
            .try_find("thinking-picker-back-btn")
            .is_some_and(|el| el.visible())
            && window
                .try_find("thinking-option-max")
                .is_some_and(|el| el.visible())
            && window
                .try_find("composer-model-picker-d8zzg")
                .is_some_and(|el| f32::from(el.bounds().size.height) > 150.0)
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        let thinking_bounds = window.find("composer-model-picker-d8zzg").bounds();
        assert_eq!(f32::from(thinking_bounds.size.width), 224.0);
        assert!(f32::from(thinking_bounds.size.height) > 150.0);
        assert!(window.find("thinking-option-max").visible());
        window.press("end", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        // Selecting a thinking level returns to Main (matching Web); Esc closes the picker.
        assert!(window.find("composer-model-picker-d8zzg").visible());
        assert!(window.try_find("thinking-option-max").is_none());
        window.press("escape", cx);
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

#[derive(Default)]
struct ChatActionsMockState {
    navigated_entries: Vec<Value>,
    cleared_count: usize,
    forked_entries: Vec<Value>,
    deleted_queue_items: Vec<(String, Value)>,
    sent_messages: Vec<(String, Value)>,
}

struct ChatActionsMockBackend {
    rt: Option<tokio::runtime::Runtime>,
    port: u16,
    token: String,
    state: Arc<Mutex<ChatActionsMockState>>,
    stop: Arc<AtomicBool>,
}

impl ChatActionsMockBackend {
    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().unwrap()
    }

    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(Mutex::new(ChatActionsMockState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let token = "chat-actions-token".to_owned();

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

                        if method == "GET" && path == "/api/sessions/sess-forked/attachments/queue-attachment" {
                            use image::ImageEncoder as _;
                            assert!(st.lock().unwrap().deleted_queue_items.is_empty());
                            let mut png = Vec::new();
                            image::codecs::png::PngEncoder::new(&mut png)
                                .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
                                .unwrap();
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                png.len()
                            );
                            let _ = stream.write_all(header.as_bytes()).await;
                            let _ = stream.write_all(&png).await;
                            return;
                        }

                        let (status_line, resp_json) = {
                            let mut guard = st.lock().unwrap();
                            match (method, path) {
                                ("GET", "/api/health") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "connected": true, "stopping": false, "version": "v0.1.0" }),
                                ),
                                ("GET", "/api/state") => {
                                    let mut sessions = vec![json!({
                                        "id": "sess-chat",
                                        "title": "磁盘与分区排查",
                                        "createdAt": "2026-11-23T08:00:00Z",
                                        "updatedAt": "2026-11-23T08:05:00Z",
                                        "running": false
                                    })];
                                    if !guard.forked_entries.is_empty() {
                                        sessions.insert(
                                            0,
                                            json!({
                                                "id": "sess-forked",
                                                "title": "磁盘与分区排查 (分支)",
                                                "createdAt": "2026-11-23T08:06:00Z",
                                                "updatedAt": "2026-11-23T08:06:00Z",
                                                "running": true
                                            }),
                                        );
                                    }
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "version": "v0.1.0",
                                            "channelMode": "app_active",
                                            "multiplexStream": true,
                                            "sessions": sessions
                                        }),
                                    )
                                }
                                ("GET", "/api/sessions") => {
                                    let mut sessions = vec![json!({
                                        "id": "sess-chat",
                                        "title": "磁盘与分区排查",
                                        "createdAt": "2026-11-23T08:00:00Z",
                                        "updatedAt": "2026-11-23T08:05:00Z",
                                        "running": false
                                    })];
                                    if !guard.forked_entries.is_empty() {
                                        sessions.insert(
                                            0,
                                            json!({
                                                "id": "sess-forked",
                                                "title": "磁盘与分区排查 (分支)",
                                                "createdAt": "2026-11-23T08:06:00Z",
                                                "updatedAt": "2026-11-23T08:06:00Z",
                                                "running": true
                                            }),
                                        );
                                    }
                                    ("HTTP/1.1 200 OK", json!({ "sessions": sessions }))
                                }
                                ("GET", "/api/sessions/sess-chat") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "id": "sess-chat",
                                        "running": false,
                                        "leafId": "a-1",
                                        "entries": [
                                            {
                                                "id": "u-0",
                                                "parentId": null,
                                                "message": { "role": "user", "timestamp": 1000, "content": "你好" }
                                            },
                                            {
                                                "id": "a-0",
                                                "parentId": "u-0",
                                                "message": {
                                                    "role": "assistant",
                                                    "timestamp": 1200,
                                                    "durationMs": 1200,
                                                    "stopReason": "stop",
                                                    "content": [
                                                        { "type": "toolCall", "id": "tc-disk", "name": "disk", "arguments": { "scope": "space" } },
                                                        { "type": "text", "text": "你好，刚才查过 C 盘空间充足。" }
                                                    ]
                                                }
                                            },
                                            {
                                                "id": "r-0",
                                                "parentId": "a-0",
                                                "message": {
                                                    "role": "toolResult",
                                                    "toolCallId": "tc-disk",
                                                    "toolName": "disk",
                                                    "isError": false,
                                                    "content": [{ "type": "text", "text": "{\"volumes\":[]}" }]
                                                }
                                            },
                                            {
                                                "id": "u-1",
                                                "parentId": "a-0",
                                                "message": { "role": "user", "timestamp": 2000, "content": "排查 D 盘" }
                                            },
                                            {
                                                "id": "a-1",
                                                "parentId": "u-1",
                                                "message": {
                                                    "role": "assistant",
                                                    "timestamp": 2400,
                                                    "durationMs": 800,
                                                    "stopReason": "stop",
                                                    "content": [{ "type": "text", "text": "D 盘未加密，剩余 512 GB。" }]
                                                }
                                            }
                                        ],
                                        "messages": []
                                    }),
                                ),
                                ("GET", "/api/sessions/sess-chat/tree") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "leafId": "a-1",
                                        "tree": [
                                            {
                                                "entry": { "id": "u-0", "parentId": null, "type": "message", "message": { "role": "user", "content": "你好" } },
                                                "children": [
                                                    {
                                                        "entry": { "id": "a-0", "parentId": "u-0", "type": "message", "message": { "role": "assistant", "content": "你好" } },
                                                        "children": [
                                                            {
                                                                "entry": { "id": "u-1", "parentId": "a-0", "type": "message", "message": { "role": "user", "content": "排查 D 盘" } },
                                                                "children": [
                                                                    {
                                                                        "entry": { "id": "a-1", "parentId": "u-1", "type": "message", "message": { "role": "assistant", "content": "D 盘未加密" } },
                                                                        "children": []
                                                                    }
                                                                ]
                                                            }
                                                        ]
                                                    }
                                                ]
                                            }
                                        ]
                                    }),
                                ),
                                ("POST", "/api/sessions/sess-chat/tree/navigate") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.navigated_entries.push(v);
                                    }
                                    ("HTTP/1.1 200 OK", json!({ "cancelled": false }))
                                }
                                ("POST", "/api/sessions/sess-chat/clear") => {
                                    guard.cleared_count += 1;
                                    ("HTTP/1.1 200 OK", json!({ "ok": true }))
                                }
                                ("POST", "/api/sessions/sess-chat/fork") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.forked_entries.push(v);
                                    }
                                    ("HTTP/1.1 200 OK", json!({ "id": "sess-forked" }))
                                }
                                ("GET", "/api/sessions/sess-forked") => {
                                    let q_items = if guard.deleted_queue_items.is_empty() {
                                        vec![json!({
                                            "id": "q1",
                                            "text": "然后是D盘",
                                            "skill": "disk",
                                            "images": [{"id": "queue-attachment", "mimeType": "image/png"}],
                                            "status": "pending",
                                            "delivery": "queue"
                                        })]
                                    } else {
                                        Vec::new()
                                    };
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "id": "sess-forked",
                                            "running": true,
                                            "leafId": "a-0",
                                            "queue": {
                                                "version": if guard.deleted_queue_items.is_empty() { 3 } else { 4 },
                                                "paused": false,
                                                "items": q_items
                                            },
                                            "entries": [
                                                {
                                                    "id": "u-0",
                                                    "parentId": null,
                                                    "message": { "role": "user", "timestamp": 1000, "content": "你好" }
                                                },
                                                {
                                                    "id": "a-0",
                                                    "parentId": "u-0",
                                                    "message": {
                                                        "role": "assistant",
                                                        "timestamp": 1200,
                                                        "durationMs": 1200,
                                                        "stopReason": "stop",
                                                        "content": [{ "type": "text", "text": "你好，刚才查过 C 盘空间充足。" }]
                                                    }
                                                }
                                            ],
                                            "messages": []
                                        }),
                                    )
                                }
                                ("DELETE", "/api/sessions/sess-forked/queue/q1") => {
                                    let parsed = serde_json::from_str::<Value>(body_str)
                                        .unwrap_or_else(|_| json!({}));
                                    guard.deleted_queue_items.push(("q1".to_owned(), parsed));
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "queue": {
                                                "version": 4,
                                                "paused": false,
                                                "items": []
                                            }
                                        }),
                                    )
                                }
                                ("POST", p) if p.ends_with("/messages") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.sent_messages.push((p.to_owned(), v));
                                    }
                                    (
                                        "HTTP/1.1 202 Accepted",
                                        json!({ "accepted": true }),
                                    )
                                }
                                ("PUT", p) if p.starts_with("/api/events/stream/") => {
                                    ("HTTP/1.1 200 OK", json!({ "ok": true }))
                                }
                                ("GET", p) if p.starts_with("/api/models") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "models": [{
                                            "provider": "cstoa",
                                            "id": "gpt-5.4",
                                            "name": "GPT-5.4",
                                            "reasoning": true,
                                            "credentialReady": true,
                                            "thinkingLevels": ["low", "medium", "high"],
                                            "thinkingLevel": "medium"
                                        }],
                                        "catalog": [{
                                            "provider": "cstoa",
                                            "id": "gpt-5.4",
                                            "name": "GPT-5.4",
                                            "reasoning": true,
                                            "credentialReady": true,
                                            "thinkingLevels": ["low", "medium", "high"],
                                            "thinkingLevel": "medium"
                                        }],
                                        "selected": {
                                            "provider": "cstoa",
                                            "id": "gpt-5.4",
                                            "name": "GPT-5.4",
                                            "reasoning": true,
                                            "credentialReady": true,
                                            "thinkingLevels": ["low", "medium", "high"],
                                            "thinkingLevel": "medium"
                                        },
                                        "thinkingLevels": ["low", "medium", "high"],
                                        "unavailableEnabled": []
                                    }),
                                ),
                                ("GET", "/api/auth") => {
                                    ("HTTP/1.1 200 OK", json!({ "providers": [] }))
                                }
                                ("GET", p) if p.starts_with("/api/account") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "providerId": "cstoa", "signedIn": true }),
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

impl Drop for ChatActionsMockBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

#[gpui_kit::test]
async fn message_actions_direct_fork_queue_edit_backfill_scroll_anchor_and_ime_clear(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = ChatActionsMockBackend::start();
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

    cx.wait_for(win_handle, Duration::from_secs(3), |window, _| {
        window.try_find("sidebar-session-row-sess-chat").is_some()
    })
    .await;
    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-session-row-sess-chat", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(3), |window, _| {
        window.try_find("assistant-actions-u-0").is_some()
            && window.try_find("assistant-actions-u-1").is_some()
    })
    .await;

    // 1. Expanding a collapsible process header must disable `auto_follow_bottom` so the row stays stationary.
    cx.update_window(win_handle, |_, window, cx| {
        let ml = shell.read(cx).workbench().read(cx).message_list().clone();
        assert!(ml.read(cx).is_auto_following());
        window.click("process-hdr-u-0:process-start", cx);
        assert!(
            !ml.read(cx).is_auto_following(),
            "manual expand/collapse must stop auto-follow so the header row stays stationary"
        );
    })
    .unwrap();

    // 2. Put an unsent draft in `session_composer`; verify `重新生成` (`assistant-retry-btn-u-1`)
    // navigates to `a-0` and resubmits `"排查 D 盘"` directly WITHOUT wiping the unsent draft!
    cx.update_window(win_handle, |_, window, cx| {
        let sess_comp = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .clone();
        let textarea = sess_comp.read(cx).textarea_state().clone();
        textarea.update(cx, |st, cx| {
            st.focus(window, cx);
            st.set_value("保留中的草稿", window, cx);
        });
        window.render_frame(cx);
        window.click("assistant-retry-btn-u-1", cx);
    })
    .unwrap();

    let st_retry = Arc::clone(&backend.state);
    let shell_retry = shell.clone();
    cx.wait_for(win_handle, Duration::from_secs(3), move |_, cx| {
        let g = st_retry.lock().unwrap();
        let settled = shell_retry
            .read(cx)
            .workbench()
            .read(cx)
            .message_list()
            .read(cx)
            .is_auto_following();
        g.navigated_entries.len() == 1 && g.sent_messages.len() == 1 && settled
    })
    .await;
    {
        let g = backend.state.lock().unwrap();
        assert_eq!(g.navigated_entries[0]["entryId"], "u-1");
        assert_eq!(g.sent_messages[0].0, "/api/sessions/sess-chat/messages");
        assert_eq!(g.sent_messages[0].1["text"], "排查 D 盘");
    }
    cx.update(|cx| {
        let val = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .read(cx)
            .textarea_state()
            .read(cx)
            .value()
            .to_string();
        assert_eq!(
            val, "保留中的草稿",
            "regenerating a history turn must preserve unsent composer draft"
        );
    });

    // 3. Edit Turn 0 (`user-edit-btn-u-0`; assistant side `VS7mw` has no edit button):
    // enters seamless inline editing inside the original user bubble without opening any draft-overwrite modal
    // and without disturbing the unsent bottom composer draft `"保留中的草稿"`.
    cx.update_window(win_handle, |_, window, cx| {
        let ml = shell.read(cx).workbench().read(cx).message_list().clone();
        ml.update(cx, |m, cx| m.scroll_to_top(cx));
        window.render_frame(cx);
        assert!(
            window.try_find("assistant-edit-btn-u-0").is_none(),
            "assistant action bar must not render edit message button"
        );
        window.click("user-edit-btn-u-0", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("overwrite-draft-confirm-card").is_none(),
            "inline bubble edit must not show draft overwrite dialog"
        );
        assert!(window.find("user-inline-edit-bubble-u-0").visible());
        window.click("user-inline-edit-cancel-u-0", cx);
        window.render_frame(cx);
        assert!(window.try_find("user-inline-edit-bubble-u-0").is_none());
    })
    .unwrap();
    assert_eq!(backend.state.lock().unwrap().navigated_entries.len(), 1);

    cx.update_window(win_handle, |_, window, cx| {
        window.click("user-edit-btn-u-0", cx);
        window.render_frame(cx);
        let inline_ta = shell
            .read(cx)
            .workbench()
            .read(cx)
            .message_list()
            .read(cx)
            .inline_edit_textarea()
            .expect("inline bubble textarea must be active");
        inline_ta.update(cx, |st, cx| {
            st.set_value("修改后的你好", window, cx);
        });
        window.render_frame(cx);
        window.click("user-inline-edit-submit-u-0", cx);
    })
    .unwrap();

    let st_edit_done = Arc::clone(&backend.state);
    let shell_edit_done = shell.clone();
    cx.wait_for(win_handle, Duration::from_secs(3), move |_, cx| {
        let g = st_edit_done.lock().unwrap();
        let rewound = g.navigated_entries.len() == 2;
        let sent_edited =
            g.sent_messages.len() == 2 && g.sent_messages[1].1["text"] == "修改后的你好";
        let ml = shell_edit_done
            .read(cx)
            .workbench()
            .read(cx)
            .message_list()
            .read(cx);
        let settled = !ml.is_action_in_flight();
        let draft_preserved = shell_edit_done
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .read(cx)
            .textarea_state()
            .read(cx)
            .value()
            .as_ref()
            == "保留中的草稿";
        rewound && sent_edited && settled && draft_preserved
    })
    .await;
    assert_eq!(
        backend.state.lock().unwrap().navigated_entries[1]["entryId"],
        "u-0"
    );

    // 4. Direct Fork (`assistant-fork-btn-u-0`): posts `entryId: "a-0"` and switches straight to `"sess-forked"`!
    cx.update_window(win_handle, |_, window, cx| {
        let ml = shell.read(cx).workbench().read(cx).message_list().clone();
        ml.update(cx, |m, cx| m.scroll_to_top(cx));
        window.render_frame(cx);
        window.click("assistant-fork-btn-u-0", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(3), |window, _| {
        window.try_find("queue-steer-btn-q1").is_some()
            && window.try_find("queue-edit-btn-q1").is_some()
    })
    .await;
    {
        let g = backend.state.lock().unwrap();
        assert_eq!(g.forked_entries.len(), 1);
        assert_eq!(
            g.forked_entries[0]["entryId"], "a-0",
            "fork must pass the target assistant entry ID directly"
        );
    }

    // 5. Queue chinese message submission with active IME preedit via mouse click on `composer-send-btn-b0mrik`:
    // composer must clear both Chinese text and IME marked range immediately!
    cx.update_window(win_handle, |_, window, cx| {
        let sess_comp = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .clone();
        let textarea = sess_comp.read(cx).textarea_state().clone();
        textarea.update(cx, |st, cx| {
            st.focus(window, cx);
            st.set_value("检查内存", window, cx);
            let end_utf16 = st.value().encode_utf16().count();
            EntityInputHandler::replace_and_mark_text_in_range(
                st,
                Some(end_utf16..end_utf16),
                "占用",
                None,
                window,
                cx,
            );
        });
        assert!(sess_comp.update(cx, |c, cx| c.is_ime_composing(window, cx)));
        window.render_frame(cx);
        window.click("composer-send-btn-b0mrik", cx);

        assert_eq!(
            textarea.read(cx).value().as_ref(),
            "",
            "Chinese text must be cleared from composer immediately upon queuing/sending"
        );
        assert!(
            !sess_comp.update(cx, |c, cx| c.is_ime_composing(window, cx)),
            "IME marked range must be cleared immediately after queuing/sending"
        );
    })
    .unwrap();

    let st_queued = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(3), move |_, _| {
        st_queued.lock().unwrap().sent_messages.len() == 3
    })
    .await;
    assert_eq!(
        backend.state.lock().unwrap().sent_messages[2].1["text"],
        "检查内存占用"
    );

    // 6. Queue Edit (`queue-edit-btn-q1`): non-empty draft prompts confirmation, then `DELETE /queue/q1` (`version: 3`)
    // removes the queued item and backfills `/skill:disk` + `"然后是D盘"` into the main composer!
    cx.update_window(win_handle, |_, window, cx| {
        let textarea = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .read(cx)
            .textarea_state()
            .clone();
        textarea.update(cx, |st, cx| {
            st.focus(window, cx);
            st.set_value("未发草稿", window, cx);
        });
        window.render_frame(cx);
        window.click("queue-edit-btn-q1", cx);
    })
    .unwrap();
    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("overwrite-draft-confirm-card").visible());
        window.click("overwrite-draft-confirm-btn", cx);
    })
    .unwrap();

    let st_qedit = Arc::clone(&backend.state);
    let shell_qedit = shell.clone();
    cx.wait_for(win_handle, Duration::from_secs(3), move |_, cx| {
        let deleted = st_qedit.lock().unwrap().deleted_queue_items.len() == 1;
        let val = shell_qedit
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .read(cx)
            .textarea_state()
            .read(cx)
            .value()
            .to_string();
        deleted && val == "/skill:disk 然后是D盘"
    })
    .await;
    {
        let g = backend.state.lock().unwrap();
        assert_eq!(g.deleted_queue_items[0].0, "q1");
        assert_eq!(g.deleted_queue_items[0].1["version"], 3);
    }
    handle.with_workspace_mut(|ws| {
        let key = DraftKey::Session("sess-forked".into());
        assert_eq!(ws.draft(&key).unwrap().images.len(), 1);
        assert!(ws.image_pool().used_bytes() > 0);
        ws.draft_mut(key).images.clear();
        assert_eq!(ws.image_pool().used_bytes(), 0);
    });
}

#[gpui::test]
async fn markdown_code_block_renders_header_bar_and_triggers_copy_notice(
    cx: &mut gpui::TestAppContext,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use cst_desktop::theme::DesktopPalette;
    use cst_desktop::workbench::markdown_view::{
        MarkdownStreamStore, build_markdown_style, render_assistant_markdown_with_copy_notice,
    };
    use gpui_kit::test::TestQueryExt as _;
    use gpui_kit::{IntoElement, ParentElement as _, Styled as _, div, px};

    struct CodeBlockProbe {
        state: gpui::Entity<gpui_kit::base::text::TextViewState>,
        copied_hits: Arc<AtomicUsize>,
    }

    impl gpui::Render for CodeBlockProbe {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let palette = DesktopPalette::light();
            let style = build_markdown_style(&palette);
            let hits = Arc::clone(&self.copied_hits);
            div()
                .w(px(816.0))
                .p_0()
                .child(render_assistant_markdown_with_copy_notice(
                    &self.state,
                    false,
                    true,
                    &palette,
                    style,
                    Some(Arc::new(move |_| {
                        hits.fetch_add(1, Ordering::SeqCst);
                    })),
                ))
        }
    }

    let copied_hits = Arc::new(AtomicUsize::new(0));
    let copied_hits_view = Arc::clone(&copied_hits);
    cx.update(|cx| {
        gpui_kit::init(cx);
    });
    let win = cx.add_window(|_window, cx| {
        let mut store = MarkdownStreamStore::default();
        let md = "测试说明：\n\n```powershell\npowercfg /h off\n```\n";
        let state = store.sync_entry("code-probe", md, cx);
        CodeBlockProbe {
            state,
            copied_hits: copied_hits_view,
        }
    });

    cx.update_window(*win, |_, window, cx| {
        window.render_frame(cx);
        let block = window
            .elements()
            .into_iter()
            .find(|el| {
                el.path()
                    .last()
                    .is_some_and(|id| id.to_string().starts_with("md-code-block-"))
            })
            .expect("custom header-bar code block must render");
        assert!(
            (f32::from(block.bounds().size.width) - 816.0).abs() < 1.0,
            "code block must fill full 816px column width, got {}",
            f32::from(block.bounds().size.width)
        );
        let header = window
            .elements()
            .into_iter()
            .find(|el| {
                el.path()
                    .last()
                    .is_some_and(|id| id.to_string().starts_with("md-code-header-"))
            })
            .expect("code block header bar must render");
        assert_eq!(f32::from(header.bounds().size.height), 36.0);

        let copy_id = window
            .elements()
            .into_iter()
            .find_map(|el| {
                el.path().last().and_then(|id| {
                    id.to_string()
                        .starts_with("md-code-copy-")
                        .then(|| id.clone())
                })
            })
            .expect("code block copy button must render");
        window.click(copy_id, cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some("powercfg /h off".to_owned())
        );
        assert_eq!(copied_hits.load(Ordering::SeqCst), 1);
    })
    .unwrap();
}

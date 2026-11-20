use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cst_client::{AppHttpClient, CstClient, VerifiedServerEndpoint};
use cst_desktop::open_desktop_shell_window_with_handle;
use cst_desktop::shell::register_shell_keys;
use cst_desktop::tools::bitlocker_card::{MAX_RECOVERY_PASSWORD_BYTES, SecretInput};
use cst_desktop::tools::normal_card::localized_normal_tool_title;
use cst_os::ProcessIdentity;
use gpui::{AppContext as _, ClipboardItem, EntityInputHandler, Role};
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
        let root = base.join(format!("desktop-tools-test-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join("agent").join("home")).unwrap();
        Self { root }
    }
}

impl Drop for KitTempFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[gpui_kit::test]
async fn secret_input_55_byte_zeroize_accessibility_copy_block_and_paste_validation(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.update(gpui_kit::init);

    let win = cx.add_window(|_, cx| SecretInput::new(cx));
    let secret = win.root(cx).unwrap();

    // The standalone field keeps its default ID and never exposes its value.
    cx.update(|cx| {
        let s = secret.read(cx);
        assert_eq!(s.accessibility().role(), Some(Role::PasswordInput));
        assert_eq!(
            s.accessibility().accessibility_id().map(|id| id.as_ref()),
            Some("bitlocker-secret-input"),
        );
        assert!(
            !s.accessibility().value_exposed(),
            "SecretInput must strictly suppress accessibility value exposure"
        );
    });

    // 2. Auto-hyphenation across 48 digits -> 55 bytes & byte-by-byte zeroize on Backspace
    cx.update_window(win.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("bitlocker-secret-input-box", cx);

        // Type 7 digits `"1234567"` -> auto-inserts hyphen at index 6 (`"123456-7"`, len = 8)
        for b in b"1234567" {
            secret.update(cx, |s, cx| {
                assert!(s.push_ascii_byte(*b, cx));
            });
        }
        let (snap_after_7, len_after_7) = secret.read(cx).snapshot_zeroizing_payload();
        assert_eq!(len_after_7, 8);
        assert_eq!(&snap_after_7[..8], b"123456-7");
        assert!(secret.read(cx).is_tail_zeroed());

        // Backspace deletes `'7'` AND trailing auto-hyphen `'-'`, zeroizing BOTH slots (`[6] == 0, [7] == 0`)!
        window.press("backspace", cx);
        let (snap_after_bs, len_after_bs) = secret.read(cx).snapshot_zeroizing_payload();
        assert_eq!(len_after_bs, 6);
        assert_eq!(&snap_after_bs[..6], b"123456");
        assert!(
            secret.read(cx).is_tail_zeroed(),
            "deleted hyphen & digit bytes in internal buffer must be zeroed immediately"
        );

        // 3. Copy & Cut (`Ctrl+C` / `Ctrl+X`) must NEVER export secret bytes to clipboard!
        cx.write_to_clipboard(ClipboardItem::new_string("SAFE_CLIPBOARD_MARKER".into()));
        window.press("ctrl-c", cx);
        window.press("ctrl-x", cx);
        let clip_after = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .unwrap_or_default();
        assert_eq!(clip_after, "SAFE_CLIPBOARD_MARKER");
        assert_eq!(secret.read(cx).len(), 6);

        // 4. Invalid paste rejected with inline red error without overwriting existing buffer
        cx.write_to_clipboard(ClipboardItem::new_string("123456-BAD-CHARS".into()));
        window.press("ctrl-v", cx);
        assert!(window.find("bitlocker-secret-input-error").visible());
        assert_eq!(secret.read(cx).len(), 6);

        // 5. Valid 48-digit paste accepted and expanded to exact 55-byte capacity
        let valid_48 = "111111222222333333444444555555666666777777888888";
        cx.write_to_clipboard(ClipboardItem::new_string(valid_48.into()));
        window.press("ctrl-v", cx);
        assert!(window.try_find("bitlocker-secret-input-error").is_none());
        assert_eq!(secret.read(cx).len(), MAX_RECOVERY_PASSWORD_BYTES);
        let (snap_full, len_full) = secret.read(cx).snapshot_zeroizing_payload();
        assert_eq!(len_full, MAX_RECOVERY_PASSWORD_BYTES);
        assert_eq!(
            &snap_full[..],
            b"111111-222222-333333-444444-555555-666666-777777-888888"
        );

        // 56th digit rejected at capacity
        let accepted_extra = secret.update(cx, |s, cx| s.push_ascii_byte(b'9', cx));
        assert!(!accepted_extra);

        // 6. Clear zeroes all 55 bytes
        secret.update(cx, |s, cx| s.clear(cx));
        assert!(secret.read(cx).is_empty());
        assert!(secret.read(cx).is_tail_zeroed());
    })
    .unwrap();

    // Also verify all 13 normal tool localized titles are non-fallback
    for name in [
        "disk",
        "driver",
        "eventlog",
        "ls",
        "startup",
        "sys",
        "bitlocker",
        "read",
        "fffind",
        "ffgrep",
        "source_check",
        "fetch_content",
        "get_search_content",
    ] {
        assert_ne!(localized_normal_tool_title(name), "诊断工具调用");
    }
}

#[derive(Default)]
struct ToolsMockState {
    bitlocker_submissions: Vec<String>,
    bitlocker_unlocked: bool,
    runbook_opens: Vec<Value>,
}

struct ToolsMockBackend {
    rt: Option<tokio::runtime::Runtime>,
    port: u16,
    token: String,
    state: Arc<Mutex<ToolsMockState>>,
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

impl ToolsMockBackend {
    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().unwrap()
    }

    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let state = Arc::new(Mutex::new(ToolsMockState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let token = "tools-test-token".to_owned();

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

                        let expires_at = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64
                            + 180_000;

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
                                        "multiplexStream": true,
                                        "sessions": [
                                            { "id": "sess-tools", "title": "BitLocker 与工具专项测试", "running": true }
                                        ]
                                    }),
                                ),
                                ("GET", "/api/sessions") => (
                                    "HTTP/1.1 200 OK",
                                    json!({
                                        "sessions": [
                                            { "id": "sess-tools", "title": "BitLocker 与工具专项测试", "running": true }
                                        ]
                                    }),
                                ),
                                ("PUT", p) if p.starts_with("/api/events/stream/") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "subscribed": ["sess-tools"] }),
                                ),
                                ("GET", "/api/sessions/sess-tools") => {
                                    let bl_requests = if guard.bitlocker_unlocked {
                                        Vec::new()
                                    } else {
                                        vec![json!({
                                            "id": "req-bl-1",
                                            "toolCallId": "tc-bl-unlock",
                                            "kind": "password",
                                            "drive": "D:",
                                            "prompt": "请输入 D: 盘 48 位数字恢复密码以继续解锁。",
                                            "expiresAt": expires_at
                                        })]
                                    };
                                    (
                                        "HTTP/1.1 200 OK",
                                        json!({
                                            "id": "sess-tools",
                                            "running": true,
                                            "bitlocker": {
                                                "revision": if guard.bitlocker_unlocked { 2 } else { 1 },
                                                "requests": bl_requests
                                            },
                                            "messages": [
                                                { "id": "u-tools", "role": "user", "timestamp": 1000, "content": "全套巡检并解锁 D 盘" },
                                                {
                                                    "id": "a-tools",
                                                    "role": "assistant",
                                                    "timestamp": 1200,
                                                    "durationMs": 3200,
                                                    "content": [
                                                        { "type": "toolCall", "id": "tc-disk", "name": "disk", "arguments": { "scope": "volumes" } },
                                                        { "type": "toolCall", "id": "tc-sys", "name": "sys", "arguments": { "scope": "overview" } },
                                                        { "type": "toolCall", "id": "tc-ws", "name": "web_search", "arguments": { "queries": ["0x116 nvlddmkm", "BitLocker error"] } },
                                                        { "type": "toolCall", "id": "tc-rb", "name": "runbook_deliver", "arguments": { "name": "fix-d-drive.txt" } },
                                                        { "type": "toolCall", "id": "tc-bl-unlock", "name": "bitlocker", "arguments": { "scope": "unlock", "drive": "D:" } },
                                                        { "type": "toolCall", "id": "tc-bl-unknown", "name": "bitlocker", "arguments": { "scope": "decrypt", "drive": "E:" } }
                                                    ]
                                                },
                                                {
                                                    "role": "toolResult",
                                                    "toolCallId": "tc-disk",
                                                    "toolName": "disk",
                                                    "durationMs": 420,
                                                    "content": [{ "type": "text", "text": "{\"volumes\":[{\"drive\":\"C:\",\"freeBytes\":68719476736,\"totalBytes\":274877906944}]}" }],
                                                    "details": {
                                                        "volumes": {
                                                            "volumes": [
                                                                { "drive": "C:", "freeGB": 64.0, "totalGB": 256.0 }
                                                            ]
                                                        }
                                                    }
                                                },
                                                {
                                                    "role": "toolResult",
                                                    "toolCallId": "tc-sys",
                                                    "toolName": "sys",
                                                    "content": [{ "type": "text", "text": "已采集整机概览" }],
                                                    "details": {
                                                        "overview": {
                                                            "cpuTotalPct": 32.5,
                                                            "logicalCores": 16,
                                                            "mem": { "usedPct": 58.0, "usedMB": 19005, "totalMB": 32768 },
                                                            "machine": { "vendor": "Lenovo", "model": "ThinkPad T14", "cpu": "Core Ultra 7" },
                                                            "uptime": { "text": "2 小时", "bootTime": "2026-11-20 00:10" }
                                                        }
                                                    }
                                                },
                                                {
                                                    "role": "toolResult",
                                                    "toolCallId": "tc-ws",
                                                    "toolName": "web_search",
                                                    "content": [{ "type": "text", "text": "部分检索超时" }],
                                                    "details": {
                                                        "successfulQueries": 1,
                                                        "queryCount": 2,
                                                        "queries": [
                                                            { "query": "0x116 nvlddmkm" },
                                                            { "query": "BitLocker error", "error": "BitLocker error 查询超时" }
                                                        ]
                                                    }
                                                },
                                                {
                                                    "role": "toolResult",
                                                    "toolCallId": "tc-rb",
                                                    "toolName": "runbook_deliver",
                                                    "content": [{ "type": "text", "text": "已生成清单" }],
                                                    "details": {
                                                        "runbook": {
                                                            "levelLabel": "需确认操作",
                                                            "items": 1,
                                                            "file": "artifacts/2026-11-20/fix-d-drive.txt",
                                                            "name": "fix-d-drive.txt",
                                                            "commands": [
                                                                { "shell": "powershell", "admin": true, "summary": "查看卷保护状态", "command": "Get-BitLockerVolume -MountPoint D:" }
                                                            ]
                                                        }
                                                    }
                                                },
                                                {
                                                    "role": "toolResult",
                                                    "toolCallId": "tc-bl-unknown",
                                                    "toolName": "bitlocker",
                                                    "content": [{ "type": "text", "text": "执行中断，结果未确认" }],
                                                    "details": {
                                                        "bitlocker": {
                                                            "scope": "decrypt",
                                                            "operation": {
                                                                "status": "unknown",
                                                                "reason": "manage-bde 子进程异常中断，需复查 E: 当前状态"
                                                            },
                                                            "volumes": []
                                                        }
                                                    }
                                                }
                                            ],
                                            "entries": []
                                        }),
                                    )
                                }
                                ("POST", "/api/sessions/sess-tools/bitlocker/req-bl-1") => {
                                    let parsed = serde_json::from_str::<Value>(body_str).unwrap_or_default();
                                    let pw = parsed
                                        .get("password")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_owned();
                                    guard.bitlocker_submissions.push(pw.clone());
                                    if pw.len() != 55 {
                                        (
                                            "HTTP/1.1 400 Bad Request",
                                            json!({
                                                "error": {
                                                    "code": "invalid_bitlocker_input",
                                                    "message": "恢复密码必须为八组六位数字，用连字符分隔"
                                                }
                                            }),
                                        )
                                    } else {
                                        guard.bitlocker_unlocked = true;
                                        ("HTTP/1.1 200 OK", json!({ "answered": true }))
                                    }
                                }
                                ("POST", "/api/sessions/sess-tools/runbook/open") => {
                                    if let Ok(v) = serde_json::from_str::<Value>(body_str) {
                                        guard.runbook_opens.push(v);
                                    }
                                    ("HTTP/1.1 200 OK", json!({ "opened": true }))
                                }
                                ("GET", p) if p.starts_with("/api/models") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "models": [], "catalog": [], "unavailableEnabled": [] }),
                                ),
                                ("GET", "/api/auth") => ("HTTP/1.1 200 OK", json!({ "providers": [] })),
                                ("GET", p) if p.starts_with("/api/account") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "providerId": "cstoa", "signedIn": false, "profile": { "supported": true } }),
                                ),
                                ("GET", "/api/app/settings") => (
                                    "HTTP/1.1 200 OK",
                                    json!({ "theme": "light", "defaultView": "gui", "sidebarCollapsed": false, "windowMaximized": false }),
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

impl Drop for ToolsMockBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

#[gpui_kit::test]
async fn headless_tools_17_cards_bitlocker_400_retry_ime_restore_unknown_review_and_runbook_open(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.executor().allow_parking();
    let fixture = KitTempFixture::create();
    let backend = ToolsMockBackend::start();
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

    let (win_handle, shell) = cx.update(|cx| {
        gpui_kit::init(cx);
        register_shell_keys(cx);
        open_desktop_shell_window_with_handle(handle.clone(), None, cx).unwrap()
    });

    // 1. Open `sess-tools` from sidebar -> verify all 4 standalone special cards + collapsible normal tools!
    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("sidebar-session-row-sess-tools").is_some()
    })
    .await;

    cx.update_window(win_handle, |_, window, cx| {
        window.click("sidebar-session-row-sess-tools", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(3), |window, _| {
        window.try_find("special-sys-overview-tc-sys").is_some()
            && window.try_find("special-web-search-tc-ws").is_some()
            && window.try_find("special-runbook-card-tc-rb").is_some()
            && window
                .try_find("special-bitlocker-card-tc-bl-unlock")
                .is_some()
            && window
                .try_find("special-bitlocker-card-tc-bl-unknown")
                .is_some()
    })
    .await;

    // 2. Verify Unknown-Status BitLocker Review Card (`operation.status == "unknown"`, empty `volumes`)
    //    NEVER infers success and displays warning + `"结果未确认"` + first error!
    cx.update_window(win_handle, |_, window, cx| {
        assert!(
            window
                .find("bitlocker-status-label-tc-bl-unknown")
                .visible()
        );
        assert!(window.find("bitlocker-first-error-tc-bl-unknown").visible());

        // Scroll up to top of turn to inspect earlier collapsible normal tool & Runbook cards
        let ml = shell.read(cx).workbench().read(cx).message_list().clone();
        ml.update(cx, |m, cx| m.scroll_to_top(cx));
        window.render_frame(cx);

        // Expand process segment -> verify `normal-tool-card-tc-disk` & its `"原始数据"` disclosure
        window.click("process-hdr-u-tools:process-start", cx);
        assert!(window.find("normal-tool-hdr-tc-disk").visible());
        window.click("normal-tool-hdr-tc-disk", cx);
        assert!(window.find("normal-tool-raw-toggle-tc-disk").visible());

        // Scroll back to bottom so Runbook & BitLocker cards are inside viewport
        ml.update(cx, |m, cx| m.scroll_to_bottom_and_follow(cx));
        window.render_frame(cx);

        // Click Runbook filename button -> calls `POST /api/sessions/sess-tools/runbook/open`
        window.click("runbook-open-file-btn-tc-rb", cx);
    })
    .unwrap();

    let st_rb = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        !st_rb.lock().unwrap().runbook_opens.is_empty()
    })
    .await;
    assert_eq!(
        backend.state.lock().unwrap().runbook_opens[0],
        json!({ "entryId": "a-tools", "toolCallId": "tc-rb" })
    );

    // 3. Focus BitLocker `SecretInput` -> verify IME unbind (`accepts_text_input == false`),
    //    submit short password -> `400 invalid_bitlocker_input` inline retry keeping `req-bl-1` alive!
    cx.update_window(win_handle, |_, window, cx| {
        window.click("bitlocker-secret-input-box", cx);
        let ml = shell.read(cx).workbench().read(cx).message_list().clone();
        let si = ml
            .read(cx)
            .secret_input_for_call("tc-bl-unlock")
            .unwrap()
            .clone();
        let accepts_ime = si.update(cx, |s, cx| {
            EntityInputHandler::accepts_text_input(s, window, cx)
        });
        assert!(
            !accepts_ime,
            "SecretInput must unbind IME (`accepts_text_input == false`) while focused"
        );

        si.update(cx, |s, cx| {
            let _ = s.try_paste_str("123456-123456", cx);
        });
        window.render_frame(cx);
        window.click("bitlocker-unlock-submit-btn-tc-bl-unlock", cx);
    })
    .unwrap();

    cx.wait_for(win_handle, Duration::from_secs(2), |window, _| {
        window.try_find("bitlocker-secret-input-error").is_some()
    })
    .await;

    // Verify `SessionOutbox` stays completely empty (BitLocker bypasses outbox!)
    assert_eq!(handle.with_outbox(|ob| ob.total_unresolved_count()), 0);

    // 4. Correct password (`48` digits -> `55` hyphenated bytes) -> resubmit -> `200 OK` + immediate zeroize!
    cx.update_window(win_handle, |_, window, cx| {
        let ml = shell.read(cx).workbench().read(cx).message_list().clone();
        let si = ml
            .read(cx)
            .secret_input_for_call("tc-bl-unlock")
            .unwrap()
            .clone();
        si.update(cx, |s, cx| {
            assert!(s.try_paste_str("123456123456123456123456123456123456123456123456", cx));
        });
        window.render_frame(cx);
        window.click("bitlocker-unlock-submit-btn-tc-bl-unlock", cx);
    })
    .unwrap();

    let st_bl = Arc::clone(&backend.state);
    cx.wait_for(win_handle, Duration::from_secs(2), move |_, _| {
        st_bl.lock().unwrap().bitlocker_unlocked
    })
    .await;

    // Verify `SecretInput` memory was wiped to `[0u8; 55]` immediately upon `200 OK`!
    cx.update(|cx| {
        let ml = shell.read(cx).workbench().read(cx).message_list().clone();
        if let Some(si) = ml.read(cx).secret_input_for_call("tc-bl-unlock") {
            assert!(si.read(cx).is_empty());
            assert!(si.read(cx).is_tail_zeroed());
        }
    });

    // 5. Return focus to `session_composer`'s `TextareaState` -> verify Chinese IME preedit & commit restore cleanly!
    cx.update_window(win_handle, |_, window, cx| {
        let sess_comp = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .clone();
        let textarea = sess_comp.read(cx).textarea_state().clone();
        let accepts_ime = textarea.update(cx, |st, cx| {
            let fh = st.presentation().focus_handle().clone();
            fh.focus(window, cx);
            let accepts = EntityInputHandler::accepts_text_input(st, window, cx);
            EntityInputHandler::replace_and_mark_text_in_range(
                st, None, "zhongwen", None, window, cx,
            );
            accepts
        });
        assert!(
            accepts_ime,
            "Composer Textarea must restore `accepts_text_input == true` after leaving SecretInput"
        );
        let composing = sess_comp.update(cx, |comp, cx| comp.is_ime_composing(window, cx));
        assert!(composing);

        textarea.update(cx, |st, cx| {
            EntityInputHandler::replace_text_in_range(st, None, "中文输入恢复正常", window, cx);
        });
    })
    .unwrap();

    cx.update_window(win_handle, |_, window, cx| {
        window.render_frame(cx);
        let sess_comp = shell
            .read(cx)
            .workbench()
            .read(cx)
            .session_composer()
            .clone();
        assert_eq!(
            sess_comp
                .read(cx)
                .textarea_state()
                .read(cx)
                .value()
                .as_ref(),
            "中文输入恢复正常"
        );
    })
    .unwrap();
}

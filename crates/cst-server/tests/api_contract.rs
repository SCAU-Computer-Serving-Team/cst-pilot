use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_pilot::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_pilot::bitlocker::{
    BitLockerPendingKind,
    core::{BitLockerSource, Executor},
    model::{Protector, Scope, Snapshot, Volume},
};
use cst_pilot::diagnostics::win_native::{Control, NativeError};
use cst_pilot::telemetry::TelemetryOptions;
use cst_pilot::update::service::UpdateOptions;
use cst_pilot::{Kit, KitHost, SKILL_NAMES};
use cst_server::mode::CallerIdentity;
use cst_server::session::events::SessionEvents;
use cst_server::{RunningServer, ServerConfig};
use pi_agent::AgentMessage;
use pi_ai::faux::{faux_assistant_message, faux_tool_call};
use pi_ai::{StopReason, TextContent, ToolResultMessage, UserContentBlock, now_ms};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct MockBitLockerVolumes {
    unlocked_passwords: Mutex<Vec<String>>,
}

impl Default for MockBitLockerVolumes {
    fn default() -> Self {
        Self {
            unlocked_passwords: Mutex::new(Vec::new()),
        }
    }
}

impl BitLockerSource for MockBitLockerVolumes {
    fn snapshot(&self, _control: &Control) -> Result<Snapshot, NativeError> {
        let unlocked = !self.unlocked_passwords.lock().unwrap().is_empty();
        Ok(Snapshot {
            collected_at: "2026-11-09T00:00:00.000Z".to_owned(),
            admin: true,
            volumes: vec![Volume {
                volume_id: "\\\\?\\Volume{11111111-2222-3333-4444-555555555555}\\".to_owned(),
                drive: Some("D:".to_owned()),
                volume_type: Some(1),
                conversion_status: Some(1),
                encryption_percentage: Some(100),
                encryption_method: Some(6),
                protection_status: Some(1),
                lock_status: Some(if unlocked { 0 } else { 1 }),
                auto_unlock_enabled: Some(false),
                auto_unlock_keys_stored: None,
                key_protectors: vec![Protector {
                    id: "{AAAA-BBBB}".to_owned(),
                    r#type: Some(3),
                }],
                collection_errors: Vec::new(),
                operation: None,
            }],
            collection_errors: Vec::new(),
        })
    }

    fn mutate(
        &self,
        _target: &Volume,
        scope: Scope,
        password: Option<&str>,
        _control: &Control,
    ) -> Result<u32, NativeError> {
        if scope == Scope::Unlock
            && let Some(pw) = password
        {
            self.unlocked_passwords.lock().unwrap().push(pw.to_owned());
        }
        Ok(0)
    }
}

struct ContractFixture {
    root: PathBuf,
    opened_runbooks: Arc<Mutex<Vec<PathBuf>>>,
}

impl ContractFixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cst-server-contract-{}", Uuid::new_v4()));
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
        // Create a dummy `index.html` in static dir to test static serving + theme injection + ticket redemption
        let static_dir = root.join("agent/home/extensions/web/static");
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(
            static_dir.join("index.html"),
            "<!doctype html><html><head><title>CST Pilot</title></head><body></body></html>",
        )
        .unwrap();

        Self {
            root,
            opened_runbooks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn start_server(&self, standalone_web: bool) -> RunningServer {
        let kit = Kit::new(&self.root).unwrap();
        let oa = OaClient::new("https://oa.example").unwrap();
        let auth = AuthRuntime::create(&self.root, Channel::Gui, oa)
            .await
            .unwrap();
        // Configure a dummy OpenAI key so session creation & model selection preflight pass
        auth.runtime()
            .set_runtime_api_key("openai", "sk-test-openai")
            .await
            .unwrap();
        auth.runtime()
            .set_runtime_api_key("deepseek", "sk-test-deepseek")
            .await
            .unwrap();

        let mock_bl = Arc::new(MockBitLockerVolumes::default());
        let host = KitHost::new(
            kit,
            auth,
            TelemetryOptions::default(),
            UpdateOptions::Disabled,
        )
        .await
        .unwrap()
        .with_bitlocker_executor(Executor::new(mock_bl));

        let opened = self.opened_runbooks.clone();
        let mut cfg = ServerConfig::new(&self.root);
        cfg.port = Some(0);
        cfg.acquire_instance_lock = false;
        cfg.mode_config.standalone_web = standalone_web;
        cfg.open_runbook = Some(Arc::new(move |path| {
            let opened = opened.clone();
            Box::pin(async move {
                opened.lock().unwrap().push(path);
                Ok(())
            })
        }));

        RunningServer::start(host, cfg).await.unwrap()
    }
}

impl Drop for ContractFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn bitlocker_wakeup_settlement_validates_before_consuming_and_redacts_passwords() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(true).await;
    let port = server.port();
    let host_header = format!("127.0.0.1:{port}");
    let origin_header = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // 1. Create a session via HTTP
    let create_res: Value = client
        .post(format!("{}/api/sessions", server.base_url()))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "create-bl-sess")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = create_res["id"].as_str().unwrap().to_owned();

    // Check `/api/sessions/creation/create-bl-sess` receipt
    let receipt_res: Value = client
        .get(format!(
            "{}/api/sessions/creation/create-bl-sess",
            server.base_url()
        ))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(receipt_res["status"], "done");
    assert_eq!(receipt_res["sessionId"], session_id);

    let slot = server.state.pool.open_saved(&session_id).await.unwrap();

    // Append an assistant toolCall entry so `BitLockerSessionBridge` resolves `entry_id`
    let entry_id = slot
        .session()
        .with_session_manager_mut(|sm| {
            let msg = faux_assistant_message(
                vec![faux_tool_call(
                    "bitlocker",
                    json!({"scope": "unlock", "drive": "D:"})
                        .as_object()
                        .unwrap()
                        .clone(),
                    Some("tc-bitlocker-1".to_owned()),
                )],
                Some(StopReason::ToolUse),
            );
            sm.append_message(AgentMessage::Assistant(msg))
        })
        .unwrap();

    // 2. Spawn the BitLocker password request on `slot.bitlocker` (simulating the `bitlocker` tool waiting for local password)
    let bridge = slot.bitlocker.clone();
    let wait_handle = tokio::spawn(async move {
        use cst_pilot::bitlocker::BitLockerHostChannel;
        bridge
            .request(
                "tc-bitlocker-1",
                BitLockerPendingKind::Password,
                vec![Volume {
                    volume_id: "\\\\?\\Volume{11111111-2222-3333-4444-555555555555}\\".to_owned(),
                    drive: Some("D:".to_owned()),
                    ..Default::default()
                }],
                Duration::from_secs(180),
                CancellationToken::new(),
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(20)).await;

    // 3. Query `GET /api/sessions/:id/bitlocker`: verify pending request is listed with `entryId`
    let bl_state: Value = client
        .get(format!(
            "{}/api/sessions/{session_id}/bitlocker",
            server.base_url()
        ))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let requests = bl_state["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    let request_id = requests[0]["requestId"].as_str().unwrap().to_owned();
    assert_eq!(requests[0]["entryId"].as_str(), Some(entry_id.as_str()));
    assert_eq!(requests[0]["kind"], "password");

    // 4. Submit WRONG action type (`{ confirm: true }` for a `password` prompt) -> 400 `invalid_bitlocker_input`,
    // and verify the `requestId` is PRESERVED!
    let bad_action = client
        .post(format!(
            "{}/api/sessions/{session_id}/bitlocker/{request_id}",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .json(&json!({ "confirm": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_action.status(), 400);
    let bad_action_json: Value = bad_action.json().await.unwrap();
    assert_eq!(bad_action_json["error"]["code"], "invalid_bitlocker_input");

    // 5. Submit MALFORMED recovery password -> 400 `invalid_bitlocker_input`, and `requestId` is STILL preserved!
    let bad_pw = client
        .post(format!(
            "{}/api/sessions/{session_id}/bitlocker/{request_id}",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .json(&json!({ "password": "123456-invalid" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_pw.status(), 400);

    // 6. Now submit a VALID 48-digit recovery password (divisible by 11 per group: 000000-000000-...)
    // concurrently from two tasks: exactly ONE must succeed (200) and the second must get 409 `expired_request`!
    let valid_pw = "000000-111111-222222-333333-444444-555555-666666-777777";
    let url = format!(
        "{}/api/sessions/{session_id}/bitlocker/{request_id}",
        server.base_url()
    );
    let c1 = client.clone();
    let c2 = client.clone();
    let u1 = url.clone();
    let u2 = url.clone();
    let h1 = host_header.clone();
    let h2 = host_header.clone();
    let o1 = origin_header.clone();
    let o2 = origin_header.clone();

    let (r1, r2) = tokio::join!(
        c1.post(u1)
            .header("Host", h1)
            .header("Origin", o1)
            .header("X-CST-Web-Request", "1")
            .json(&json!({ "password": valid_pw }))
            .send(),
        c2.post(u2)
            .header("Host", h2)
            .header("Origin", o2)
            .header("X-CST-Web-Request", "1")
            .json(&json!({ "password": valid_pw }))
            .send()
    );
    let statuses = [r1.unwrap().status().as_u16(), r2.unwrap().status().as_u16()];
    assert!(
        statuses == [200, 409] || statuses == [409, 200],
        "expected one 200 and one 409, got {statuses:?}"
    );

    let answer = wait_handle.await.unwrap();
    assert!(matches!(
        answer,
        Some(cst_pilot::bitlocker::BitLockerAnswer::Password(_))
    ));

    // 7. Verify `GET /api/sessions/:id` redacts any 48-digit recovery password in messages
    let snap: Value = client
        .get(format!("{}/api/sessions/{session_id}", server.base_url()))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let snap_str = snap.to_string();
    assert!(!snap_str.contains(valid_pw));

    server.shutdown().await;
}

#[tokio::test]
async fn runbook_open_validates_delivered_artifact_and_launches_opener() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(true).await;
    let port = server.port();
    let host_header = format!("127.0.0.1:{port}");
    let origin_header = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // Create a delivered runbook file under `artifacts/2026-11-09/runbook/01-fix.txt`
    let rb_dir = fixture.root.join("artifacts/2026-11-09/runbook");
    fs::create_dir_all(&rb_dir).unwrap();
    let rb_file = rb_dir.join("01-fix.txt");
    fs::write(
        &rb_file,
        "\u{feff}Title\r\nSummary\r\npwsh\r\nsfc /scannow\r\n",
    )
    .unwrap();

    let create_res: Value = client
        .post(format!("{}/api/sessions", server.base_url()))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "create-rb-sess")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = create_res["id"].as_str().unwrap().to_owned();
    let slot = server.state.pool.open_saved(&session_id).await.unwrap();

    let entry_id = slot
        .session()
        .with_session_manager_mut(|sm| {
            let msg = faux_assistant_message(
                vec![faux_tool_call(
                    "runbook_deliver",
                    json!({"version": 1}).as_object().unwrap().clone(),
                    Some("tc-rb-1".to_owned()),
                )],
                Some(StopReason::ToolUse),
            );
            let eid = sm.append_message(AgentMessage::Assistant(msg)).unwrap();
            sm.append_message(AgentMessage::ToolResult(ToolResultMessage {
                tool_call_id: "tc-rb-1".to_owned(),
                tool_name: "runbook_deliver".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("{}"))],
                details: Some(json!({
                    "runbook": {
                        "file": rb_file.to_string_lossy(),
                    }
                })),
                is_error: false,
                timestamp: now_ms(),
                usage: None,
                nested_calls: None,
                duration_ms: Some(5),
            }))
            .unwrap();
            Ok(eid)
        })
        .unwrap();

    let open_res = client
        .post(format!(
            "{}/api/sessions/{session_id}/runbook/open",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "open-rb-1")
        .json(&json!({
            "entryId": entry_id,
            "toolCallId": "tc-rb-1"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(open_res.status(), 200);
    assert_eq!(fixture.opened_runbooks.lock().unwrap().len(), 1);

    server.shutdown().await;
}

#[tokio::test]
async fn slow_tcp_sse_client_exceeding_8mib_budget_is_forcibly_disconnected_via_conn_cancel() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(true).await;
    let port = server.port();

    // Open a raw TCP connection to `/api/events` and NEVER read from the socket after sending the HTTP request!
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req_bytes = format!(
        "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
    );
    tcp.write_all(req_bytes.as_bytes()).await.unwrap();

    // Wait until the subscriber is registered on `global_events`
    for _ in 0..50 {
        if server.state.pool.global_events.active_subscriber_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.state.pool.global_events.active_subscriber_count(), 1);

    // Publish large events (~512 KiB each) without reading from `tcp`.
    // Once kernel + HTTP write buffer + 8 MiB application queue fills (~24 frames),
    // `enqueue` synchronously removes the subscriber, clears the queue, and cancels `conn_cancel`!
    let chunk = "x".repeat(512 * 1024);
    for i in 0..32 {
        server.state.pool.global_events.publish(
            "state",
            &json!({
                "type": "sessions_changed",
                "seq": i,
                "pad": chunk,
            }),
        );
        if server.state.pool.global_events.active_subscriber_count() == 0 {
            break;
        }
    }

    assert_eq!(
        server.state.pool.global_events.active_subscriber_count(),
        0,
        "slow subscriber exceeding 8 MiB budget must be immediately evicted and its outer connection cancelled"
    );

    drop(tcp);
    server.shutdown().await;
}

#[tokio::test]
async fn multiplex_sse_vector_cursor_interleaved_replay_and_subscription_recovery() {
    let global_events = SessionEvents::new();
    let hub = cst_server::session::events::MultiplexHub::new(global_events.clone());

    let sess_a = SessionEvents::new();
    let sess_b = SessionEvents::new();
    let sess_a_clone = sess_a.clone();
    let sess_b_clone = sess_b.clone();

    let resolver: cst_server::session::events::SessionEventsResolver =
        Arc::new(move |sid: &str| match sid {
            "sess-a" => Some(sess_a_clone.clone()),
            "sess-b" => Some(sess_b_clone.clone()),
            _ => None,
        });

    let caller = CallerIdentity::Web { cookie_epoch: 1 };
    let client_id = "worker-client-1";

    // 1. Subscribe to `["sess-a", "sess-b"]`
    let sub_list = hub
        .update_subscriptions(
            caller,
            client_id,
            cst_server::session::events::MultiplexSubscriptionUpdate {
                sessions: vec!["sess-a".to_owned(), "sess-b".to_owned()],
                last_event_ids: Default::default(),
            },
            &resolver,
        )
        .unwrap();
    assert_eq!(sub_list, vec!["sess-a".to_owned(), "sess-b".to_owned()]);
    assert_eq!(sess_a.active_subscriber_count(), 1);
    assert_eq!(sess_b.active_subscriber_count(), 1);

    // 2. Connect multiplex stream and emit interleaved events across global, sess-a, and sess-b
    let mut body = hub.connect(caller, client_id, None, CancellationToken::new());
    let g_seq = global_events.publish("state", &json!({"type": "settings_changed"}));
    let a_seq1 = sess_a.publish("session", &json!({"type": "agent_start"}));
    let b_seq1 = sess_b.publish(
        "queue",
        &json!({"version": 1, "paused": false, "items": []}),
    );

    // Read the emitted frames from `body` and capture the last frame's `id:` vector cursor
    use http_body_util::BodyExt;
    let mut frames_text = String::new();
    for _ in 0..4 {
        if let Some(Ok(frame)) = body.frame().await
            && let Ok(bytes) = frame.into_data()
        {
            frames_text.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    assert!(frames_text.contains("\"type\":\"stream_ready\""));

    let last_vector_id = frames_text
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .next_back()
        .expect("vector cursor id line")
        .to_owned();

    // Verify the single `id:` line contains accurate cursors for global, sess-a, and sess-b!
    let parsed_cursors =
        cst_server::session::events::MultiplexHub::parse_vector_cursor(&last_vector_id);
    assert_eq!(
        parsed_cursors.get("global"),
        Some(&(global_events.epoch().to_owned(), g_seq))
    );
    assert_eq!(
        parsed_cursors.get("sess-a"),
        Some(&(sess_a.epoch().to_owned(), a_seq1))
    );
    assert_eq!(
        parsed_cursors.get("sess-b"),
        Some(&(sess_b.epoch().to_owned(), b_seq1))
    );

    // 3. Dynamically change subscriptions from `[sess-a, sess-b]` to `[sess-b]` only,
    // verify `sess-a`'s LRU subscriber count drops to 0 while `sess-b` stays 1.
    let updated = hub
        .update_subscriptions(
            caller,
            client_id,
            cst_server::session::events::MultiplexSubscriptionUpdate {
                sessions: vec!["sess-b".to_owned()],
                last_event_ids: Default::default(),
            },
            &resolver,
        )
        .unwrap();
    assert_eq!(updated, vec!["sess-b".to_owned()]);
    assert_eq!(sess_a.active_subscriber_count(), 0);
    assert_eq!(sess_b.active_subscriber_count(), 1);

    let missing = hub.update_subscriptions(
        caller,
        client_id,
        cst_server::session::events::MultiplexSubscriptionUpdate {
            sessions: vec!["missing-session".to_owned()],
            ..Default::default()
        },
        &resolver,
    );
    assert!(missing.is_err());
    assert_eq!(sess_b.active_subscriber_count(), 1);

    // 4. Disconnect `body`, publish a new event on `sess-b` while disconnected,
    // then reconnect with `Last-Event-ID = last_vector_id`!
    drop(body);
    let b_seq2 = sess_b.publish("session", &json!({"type": "agent_end"}));

    let mut reconnected = hub.connect(
        caller,
        client_id,
        Some(&last_vector_id),
        CancellationToken::new(),
    );
    let mut replay_text = String::new();
    for _ in 0..2 {
        if let Some(Ok(frame)) = reconnected.frame().await
            && let Ok(bytes) = frame.into_data()
        {
            replay_text.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    // Reconnected stream must replay `b_seq2` AND report `subscribedSessions: ["sess-b"]` (not reverting to `[sess-a, sess-b]`)!
    assert!(replay_text.contains("\"sessionId\":\"sess-b\""));
    assert!(replay_text.contains("\"type\":\"agent_end\""));
    assert!(replay_text.contains("\"subscribedSessions\":[\"sess-b\"]"));
    assert_eq!(b_seq2, 2);
    assert_eq!(sess_a.active_subscriber_count(), 0);
    assert_eq!(sess_b.active_subscriber_count(), 1);

    // 5. Simulate server restart (fresh `MultiplexHub` with no prior `clientId` record):
    // Reconnecting returns `subscribedSessions: []` and targeted `reset` so client re-pushes `PUT .../subscriptions`.
    let fresh_hub = cst_server::session::events::MultiplexHub::new(SessionEvents::new());
    let mut after_restart = fresh_hub.connect(
        caller,
        client_id,
        Some(&last_vector_id),
        CancellationToken::new(),
    );
    let mut restart_text = String::new();
    for _ in 0..2 {
        if let Some(Ok(frame)) = after_restart.frame().await
            && let Ok(bytes) = frame.into_data()
        {
            restart_text.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    assert!(restart_text.contains("event: reset"));
    assert!(restart_text.contains("\"subscribedSessions\":[]"));
}

#[tokio::test]
async fn multiplex_subscriptions_pin_all_32_sessions_and_reject_missing_sources_atomically() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(true).await;
    let mut ids = Vec::new();
    for i in 0..32 {
        ids.push(
            server
                .state
                .pool
                .create_with_key(&format!("bulk-{i}"), None, None)
                .await
                .unwrap()
                .id
                .clone(),
        );
    }
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!(
        "{}/api/events/stream/bulk-client/subscriptions",
        server.base_url()
    );
    let response = client
        .put(&url)
        .header("Origin", server.base_url())
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "bulk-subscribe")
        .json(&json!({ "sessions": ids }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    let accepted: Value = response.json().await.unwrap();
    assert_eq!(accepted["subscribedSessions"].as_array().unwrap().len(), 32);
    for id in &ids {
        assert_eq!(
            server
                .state
                .pool
                .get_loaded(id)
                .await
                .unwrap()
                .events
                .active_subscriber_count(),
            1
        );
    }

    let state_resp: Value = client
        .get(format!("{}/api/state", server.base_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state_resp["multiplexStream"], json!(true));

    let response = client
        .put(&url)
        .header("Origin", server.base_url())
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "missing-source")
        .json(&json!({ "sessions": [&ids[0], "missing-session"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
    let err_body: Value = response.json().await.unwrap();
    assert_eq!(err_body["error"]["code"], json!("session_not_found"));
    assert_eq!(err_body["error"]["sessionId"], json!("missing-session"));
    for id in &ids {
        assert_eq!(
            server
                .state
                .pool
                .get_loaded(id)
                .await
                .unwrap()
                .events
                .active_subscriber_count(),
            1
        );
    }
    let response = client
        .put(&url)
        .header("Origin", server.base_url())
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "bulk-unsubscribe")
        .json(&json!({ "sessions": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(server.state.pool.active_slot_count().await, 10);
    server.shutdown().await;
}

#[tokio::test]
async fn web_ticket_redemption_redirects_to_a_refreshable_url() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(false).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let opened: Value = client
        .post(format!("{}/api/app/web/open", server.base_url()))
        .bearer_auth(server.app_token())
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = opened["url"].as_str().unwrap();
    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), http::StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(location, "/");
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for _ in 0..2 {
        let page = client
            .get(format!("{}{location}", server.base_url()))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), http::StatusCode::OK);
    }
    assert_eq!(
        client
            .get(url)
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap()
            .status(),
        http::StatusCode::FORBIDDEN
    );
    server.shutdown().await;
}

async fn read_http_response_head(stream: &mut TcpStream) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
            assert!(head.len() < 16_384);
        }
        String::from_utf8(head).unwrap()
    })
    .await
    .expect("HTTP response headers must arrive")
}

#[tokio::test]
async fn revoked_web_requests_cannot_register_any_sse_stream_after_body_collection() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(false).await;
    let id = server
        .state
        .pool
        .create_with_key("late-sse-source", None, None)
        .await
        .unwrap()
        .id
        .clone();
    let client = reqwest::Client::new();
    for path in [
        "/api/events".to_owned(),
        format!("/api/sessions/{id}/events"),
        "/api/events/stream?clientId=late-client".to_owned(),
    ] {
        for reopen in [false, true] {
            let (_, ticket) = server.state.mode.open_web(server.port()).await.unwrap();
            let cookie = server.state.mode.redeem_ticket(&ticket).await.unwrap();
            let mut stream = TcpStream::connect(server.addr).await.unwrap();
            stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: cst-web-session={cookie}\r\nExpect: 100-continue\r\nContent-Length: 1\r\n\r\n", server.port()).as_bytes()).await.unwrap();
            assert!(
                read_http_response_head(&mut stream)
                    .await
                    .starts_with("HTTP/1.1 100 Continue")
            );
            let response = client
                .post(format!("{}/api/app/web/close", server.base_url()))
                .bearer_auth(server.app_token())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), http::StatusCode::OK);
            if reopen {
                server.state.mode.open_web(server.port()).await.unwrap();
            }
            stream.write_all(b"x").await.unwrap();
            let head = read_http_response_head(&mut stream).await;
            assert!(
                head.starts_with("HTTP/1.1 403 Forbidden"),
                "{path}, reopen={reopen}: {head}"
            );
        }
    }
    server.shutdown().await;
}

#[tokio::test]
async fn accepted_mutations_and_duplicate_waiters_do_not_hold_channel_admission() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(false).await;
    let state = server.state.clone();
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (complete, complete_rx) = tokio::sync::oneshot::channel();
    let task_state = state.clone();
    let owner = tokio::spawn(async move {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "idempotency-key",
            "accepted-long-operation".parse().unwrap(),
        );
        task_state
            .write_once::<String, _, _>(
                CallerIdentity::App,
                "POST",
                "/api/models/refresh",
                &json!({}),
                Some(&headers),
                || async {
                    started.send(()).unwrap();
                    complete_rx.await.unwrap();
                    Ok(json!({"finished":true}))
                },
            )
            .await
    });
    started_rx.await.unwrap();
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "idempotency-key",
        "accepted-long-operation".parse().unwrap(),
    );
    let body = json!({});
    let mut duplicate = Box::pin(state.write_once::<String, _, _>(
        CallerIdentity::App,
        "POST",
        "/api/models/refresh",
        &body,
        Some(&headers),
        || async { panic!("duplicate action must not run") },
    ));
    assert!(futures_util::poll!(duplicate.as_mut()).is_pending());
    tokio::time::timeout(Duration::from_secs(1), state.mode.open_web(server.port()))
        .await
        .expect("durably accepted work must not block a channel switch")
        .unwrap();
    assert!(!owner.is_finished());
    complete.send(()).unwrap();
    assert_eq!(owner.await.unwrap().unwrap(), json!({"finished":true}));
    assert_eq!(duplicate.await.unwrap(), json!({"finished":true}));
    server.shutdown().await;
}

#[tokio::test]
async fn mutation_failures_keep_typed_action_errors_and_settle_duplicate_waiters() {
    use cst_server::error::MutationError;
    use cst_server::session::receipts::ReceiptError;

    let fixture = ContractFixture::new();
    let server = fixture.start_server(false).await;
    let state = server.state.clone();
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (complete, complete_rx) = tokio::sync::oneshot::channel();
    let task_state = state.clone();
    let mut headers = http::HeaderMap::new();
    headers.insert("idempotency-key", "typed-action-error".parse().unwrap());
    let owner_headers = headers.clone();
    let owner = tokio::spawn(async move {
        task_state
            .write_once::<String, _, _>(
                CallerIdentity::App,
                "POST",
                "/api/test",
                &json!({}),
                Some(&owner_headers),
                || async {
                    started.send(()).unwrap();
                    complete_rx.await.unwrap();
                    Err("业务操作失败".to_owned())
                },
            )
            .await
    });
    started_rx.await.unwrap();
    let body = json!({});
    let mut duplicate = Box::pin(state.write_once::<String, _, _>(
        CallerIdentity::App,
        "POST",
        "/api/test",
        &body,
        Some(&headers),
        || async { panic!("failed actions must not run twice") },
    ));
    assert!(futures_util::poll!(duplicate.as_mut()).is_pending());
    complete.send(()).unwrap();
    assert!(
        matches!(owner.await.unwrap(), Err(MutationError::Action(error)) if error == "业务操作失败")
    );
    assert!(
        matches!(duplicate.await, Err(MutationError::Receipt(ReceiptError::Action(message))) if message == "操作执行失败")
    );
    server.shutdown().await;
}

#[tokio::test]
async fn mode_switch_does_not_wait_for_a_mutation_queued_on_the_session_lock() {
    let fixture = ContractFixture::new();
    let server = fixture.start_server(false).await;
    let state = &server.state;
    let held = state.locks.lock("session:queued-session").await;
    let mut headers = http::HeaderMap::new();
    headers.insert("idempotency-key", "queued-mutation".parse().unwrap());
    let body = json!({});
    let mut mutation = Box::pin(state.write_once_for_session::<String, _, _>(
        CallerIdentity::App,
        "PATCH",
        "/api/sessions/queued-session",
        &body,
        &headers,
        "queued-session",
        || async { panic!("mutation from the inactive channel must be rejected") },
    ));
    assert!(futures_util::poll!(mutation.as_mut()).is_pending());

    tokio::time::timeout(Duration::from_secs(1), state.mode.open_web(server.port()))
        .await
        .expect("queued mutation must not retain channel admission")
        .unwrap();
    drop(held);
    let error = tokio::time::timeout(Duration::from_secs(1), mutation)
        .await
        .expect("queued mutation must settle after the channel switch")
        .unwrap_err();
    assert!(matches!(
        error,
        cst_server::error::MutationError::Admission(cst_server::mode::ModeRejection::WebModeActive)
    ));
    assert!(
        state
            .pool
            .receipts
            .lookup::<Value>("queued-mutation")
            .await
            .unwrap()
            .is_none()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn oversized_sse_replays_cancel_without_blocking_single_or_multiplex_streams() {
    use cst_server::session::events::{MultiplexHub, MultiplexSubscriptionUpdate};

    for scenario in ["single", "single-large", "multiplex", "subscriptions"] {
        let runtime = tokio::runtime::Handle::current();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _runtime = runtime.enter();
            let large = scenario == "single-large";
            let budget = if large { 8 * 1024 * 1024 } else { 1024 };
            let events = SessionEvents::with_limits(budget, Duration::from_secs(20));
            let text = "x".repeat(if large { 1024 * 1024 } else { 512 });
            for _ in 0..if large { 10 } else { 4 } {
                events.publish("session", &json!({ "text": text }));
            }
            let cancel = CancellationToken::new();
            let hub = MultiplexHub::with_limits(
                if scenario == "subscriptions" {
                    SessionEvents::new()
                } else {
                    events.clone()
                },
                budget,
                Duration::from_secs(20),
                Duration::from_millis(10),
            );
            let body = match scenario {
                "single" | "single-large" => events.subscribe_single(
                    CallerIdentity::App,
                    Some(&format!("{}:0", events.epoch())),
                    cancel.clone(),
                    None,
                ),
                "multiplex" => hub.connect(
                    CallerIdentity::App,
                    "replay-client",
                    Some(&format!("global:{}:0", events.epoch())),
                    cancel.clone(),
                ),
                _ => {
                    let body =
                        hub.connect(CallerIdentity::App, "replay-client", None, cancel.clone());
                    let source = events.clone();
                    let resolver: cst_server::session::events::SessionEventsResolver =
                        Arc::new(move |_| Some(source.clone()));
                    hub.update_subscriptions(
                        CallerIdentity::App,
                        "replay-client",
                        MultiplexSubscriptionUpdate {
                            sessions: vec!["replay-session".to_owned()],
                            last_event_ids: [(
                                "replay-session".to_owned(),
                                format!("{}:0", events.epoch()),
                            )]
                            .into(),
                        },
                        &resolver,
                    )
                    .unwrap();
                    body
                }
            };
            let result = (
                cancel.is_cancelled(),
                body.subscriber().is_overflowed(),
                body.subscriber().current_queued_bytes(),
            );
            drop(body);
            sender.send((result, events)).unwrap();
        });
        let (result, events) = receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{scenario} replay blocked on overflow"));
        assert_eq!(result, (true, true, 0), "{scenario}");
        worker.join().unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while events.active_subscriber_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("overflowed multiplex slots must release their retained session pins");
    }
}

#[tokio::test]
async fn unconnected_multiplex_slot_expires_and_releases_session_pins() {
    use cst_server::session::events::{MultiplexHub, MultiplexSubscriptionUpdate};

    let global_events = SessionEvents::new();
    let session_events = SessionEvents::new();
    let hub = MultiplexHub::with_limits(
        global_events,
        8 * 1024 * 1024,
        Duration::from_secs(20),
        Duration::from_millis(40),
    );
    let source = session_events.clone();
    let resolver: cst_server::session::events::SessionEventsResolver =
        Arc::new(move |_| Some(source.clone()));

    hub.update_subscriptions(
        CallerIdentity::App,
        "never-connected-client",
        MultiplexSubscriptionUpdate {
            sessions: vec!["s1".to_owned()],
            last_event_ids: Default::default(),
        },
        &resolver,
    )
    .unwrap();
    assert_eq!(session_events.active_subscriber_count(), 1);

    tokio::time::timeout(Duration::from_secs(2), async {
        while session_events.active_subscriber_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("unconnected multiplex slot must expire and release session pins");
}

#[tokio::test]
async fn session_snapshots_exports_and_fork_omit_system_messages_while_preserving_jsonl_and_tree() {
    use pi_ai::{SystemContent, SystemMessage, UserMessage};

    let fixture = ContractFixture::new();
    let server = fixture.start_server(true).await;
    let port = server.port();
    let host_header = format!("127.0.0.1:{port}");
    let origin_header = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let create_res: Value = client
        .post(format!("{}/api/sessions", server.base_url()))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "create-sys-role-sess")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = create_res["id"].as_str().unwrap().to_owned();
    let slot = server.state.pool.open_saved(&session_id).await.unwrap();

    // Populate initial system metadata, user + assistant + toolResult, mid-conversation system metadata,
    // second user + branch summary + orphan assistant.
    let (sys_init_id, u1_id, sys_mid_id, u2_id, session_file) = slot
        .session()
        .with_session_manager_mut(|sm| {
            let sys_init_id = sm.append_message(AgentMessage::System(SystemMessage {
                content: SystemContent::Text(String::new()),
                sections: Some(std::collections::BTreeMap::from([(
                    "append".to_owned(),
                    Some("INTERNAL_SYSTEM_PROMPT_PATCH".to_owned()),
                )])),
                tools_added: None,
                tools_removed: None,
                timestamp: 1_000,
            }))?;
            let mut u1 = UserMessage::new("首轮排障提问");
            u1.timestamp = 1_010;
            let u1_id = sm.append_message(AgentMessage::User(u1))?;

            let mut a1 = faux_assistant_message(
                vec![faux_tool_call(
                    "ls",
                    json!({"path": "."}).as_object().unwrap().clone(),
                    Some("tc-sys-1".to_owned()),
                )],
                Some(StopReason::ToolUse),
            );
            a1.timestamp = 1_020;
            sm.append_message(AgentMessage::Assistant(a1))?;

            sm.append_message(AgentMessage::ToolResult(ToolResultMessage {
                tool_call_id: "tc-sys-1".to_owned(),
                tool_name: "ls".to_owned(),
                content: vec![UserContentBlock::Text(TextContent {
                    text: "ok".to_owned(),
                    text_signature: None,
                })],
                details: None,
                is_error: false,
                timestamp: 1_030,
                usage: None,
                nested_calls: None,
                duration_ms: Some(5),
            }))?;

            let sys_mid_id = sm.append_message(AgentMessage::System(SystemMessage {
                content: SystemContent::Text(String::new()),
                sections: Some(std::collections::BTreeMap::from([(
                    "tools".to_owned(),
                    Some("MID_CONVO_TOOL_LOADOUT".to_owned()),
                )])),
                tools_added: None,
                tools_removed: None,
                timestamp: 1_040,
            }))?;

            let mut u2 = UserMessage::new("次轮排障提问");
            u2.timestamp = 1_050;
            let u2_id = sm.append_message(AgentMessage::User(u2))?;

            let _bs_id = sm.branch_with_summary(Some(&u2_id), "分支总结正文", None, None, None)?;

            let mut orphan_asst = faux_assistant_message(Vec::new(), Some(StopReason::Aborted));
            orphan_asst.timestamp = 1_070;
            sm.append_message(AgentMessage::Assistant(orphan_asst))?;

            let file = sm
                .get_session_file()
                .expect("flushed session file")
                .to_path_buf();
            Ok((sys_init_id, u1_id, sys_mid_id, u2_id, file))
        })
        .unwrap();

    // Close and reload the session from disk so `session.state().messages` is hydrated from the
    // persisted JSONL projection (which contains `AgentMessage::System`), verifying `messages` filtering too.
    drop(slot);
    server.shutdown().await;
    let server = fixture.start_server(true).await;
    let port = server.port();
    let host_header = format!("127.0.0.1:{port}");
    let origin_header = format!("http://127.0.0.1:{port}");
    let raw_jsonl = fs::read_to_string(&session_file).unwrap();
    assert_eq!(
        raw_jsonl.matches("\"role\":\"system\"").count(),
        2,
        "JSONL persistence must retain both initial and mid-conversation system entries"
    );

    let snap_res: Value = client
        .get(format!("{}/api/sessions/{session_id}", server.base_url()))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let snap_messages = snap_res["messages"].as_array().unwrap();
    assert!(
        snap_messages
            .iter()
            .all(|m| m["role"].as_str() != Some("system")),
        "GET /api/sessions/:id messages must never expose role=system"
    );

    let snap_entries = snap_res["entries"].as_array().unwrap();
    let entry_roles: Vec<&str> = snap_entries
        .iter()
        .map(|e| e["message"]["role"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        entry_roles,
        vec![
            "user",
            "assistant",
            "toolResult",
            "user",
            "branchSummary",
            "assistant",
        ],
        "GET /api/sessions/:id entries must exclude system messages while keeping user, assistant, toolResult, branchSummary, and orphan assistant"
    );
    assert_eq!(snap_entries[0]["id"].as_str(), Some(u1_id.as_str()));
    assert_eq!(
        snap_entries[0]["parentId"].as_str(),
        Some(sys_init_id.as_str())
    );
    assert_eq!(snap_entries[3]["id"].as_str(), Some(u2_id.as_str()));
    assert_eq!(
        snap_entries[3]["parentId"].as_str(),
        Some(sys_mid_id.as_str())
    );

    // Export preview snapshot must likewise exclude system entries and internal system prompt strings.
    let preview_res: Value = client
        .post(format!(
            "{}/api/sessions/{session_id}/exports/preview",
            server.base_url()
        ))
        .header("Host", &host_header)
        .header("Origin", &origin_header)
        .header("X-CST-Web-Request", "1")
        .header("Idempotency-Key", "preview-sys-role")
        .json(&json!({
            "format": "html",
            "theme": "light",
            "includeThinking": true,
            "includeTools": true
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let preview_str = serde_json::to_string(&preview_res).unwrap();
    assert!(!preview_str.contains("\"role\":\"system\""));
    assert!(!preview_str.contains("INTERNAL_SYSTEM_PROMPT_PATCH"));
    assert!(!preview_str.contains("MID_CONVO_TOOL_LOADOUT"));

    // Tree endpoint must retain raw SessionEntry nodes so parentId chains and fork targets never break.
    let tree_res: Value = client
        .get(format!(
            "{}/api/sessions/{session_id}/tree",
            server.base_url()
        ))
        .header("Host", &host_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tree_str = serde_json::to_string(&tree_res).unwrap();
    assert!(tree_str.contains(&sys_init_id));
    assert!(tree_str.contains(&sys_mid_id));

    // Forking at u2's parentId (`sys_mid_id`) and u1's parentId (`sys_init_id`) must both succeed cleanly!
    for (idx, fork_target) in [(&sys_mid_id, 1usize), (&sys_init_id, 0usize)]
        .into_iter()
        .enumerate()
    {
        let fork_resp = client
            .post(format!(
                "{}/api/sessions/{session_id}/fork",
                server.base_url()
            ))
            .header("Host", &host_header)
            .header("Origin", &origin_header)
            .header("X-CST-Web-Request", "1")
            .header("Idempotency-Key", format!("fork-sys-{idx}"))
            .json(&json!({ "entryId": fork_target.0 }))
            .send()
            .await
            .unwrap();
        assert_eq!(fork_resp.status(), 201);
        let fork_json: Value = fork_resp.json().await.unwrap();
        let forked_id = fork_json["id"].as_str().unwrap();
        let forked_snap: Value = client
            .get(format!("{}/api/sessions/{forked_id}", server.base_url()))
            .header("Host", &host_header)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let user_count = forked_snap["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["message"]["role"].as_str() == Some("user"))
            .count();
        assert_eq!(user_count, fork_target.1);
        assert!(
            forked_snap["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["message"]["role"].as_str() != Some("system"))
        );
    }

    server.shutdown().await;
}

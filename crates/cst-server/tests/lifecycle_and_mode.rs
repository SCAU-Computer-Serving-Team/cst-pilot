use std::fs;
use std::sync::Arc;
use std::time::Duration;

use cst_server::auth_guard::{
    WEB_SESSION_COOKIE_NAME, authenticate_api_request, validate_host_and_origin,
};
use cst_server::discovery::{
    ServerInstanceLock, SessionOwnerLock, cleanup_discovery_file, read_discovery_file,
    write_discovery_file,
};
use cst_server::mode::{
    AppPresenceState, CallerIdentity, ChannelMode, ModeController, ModeControllerConfig,
    ModeRejection,
};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use tokio::sync::Barrier;
use uuid::Uuid;

#[test]
fn single_instance_lock_and_discovery_file_lifecycle() {
    let dir = std::env::temp_dir().join(format!("cst-server-disc-{}", Uuid::new_v4()));
    fs::create_dir_all(dir.join("agent/home")).unwrap();

    let lock1 = ServerInstanceLock::acquire(&dir).expect("first instance lock");
    let conflict = ServerInstanceLock::acquire(&dir).err().unwrap();
    assert!(conflict.contains("server_already_running"));

    let record = write_discovery_file(&dir, 52831, "token-123").unwrap();
    let loaded = read_discovery_file(&dir).unwrap();
    assert_eq!(loaded, record);

    cleanup_discovery_file(&dir, record.pid, 9999);
    assert!(read_discovery_file(&dir).is_some());

    cleanup_discovery_file(&dir, record.pid, 52831);
    assert!(read_discovery_file(&dir).is_none());

    drop(lock1);
    let _lock2 = ServerInstanceLock::acquire(&dir).expect("reacquire after drop");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn session_owner_lock_rejects_second_holder_until_dropped() {
    let dir = std::env::temp_dir().join(format!("cst-server-sesslock-{}", Uuid::new_v4()));
    let agent_dir = dir.join("agent/home");
    fs::create_dir_all(&agent_dir).unwrap();

    let lock1 = SessionOwnerLock::acquire(&agent_dir, "sess-1").expect("first session lock");
    let err = SessionOwnerLock::acquire(&agent_dir, "sess-1")
        .err()
        .unwrap();
    assert!(err.starts_with("session_owned:"));

    let _other = SessionOwnerLock::acquire(&agent_dir, "sess-2").expect("distinct session id");
    drop(lock1);
    let _reacquired =
        SessionOwnerLock::acquire(&agent_dir, "sess-1").expect("reacquire after release");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn distinct_sessions_with_colliding_legacy_lock_ports_can_be_owned_together() {
    use cst_server::discovery::normalize_canonical_path_for_hash;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;

    let dir = std::env::temp_dir().join(format!("cst-session-collision-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let normalized = normalize_canonical_path_for_hash(&dir).unwrap();
    let mut ports = HashMap::new();
    let (first, second) = (0..=20000)
        .find_map(|index| {
            let id = format!("session-{index}");
            let hash = Sha256::digest(format!("{normalized}\0{id}").as_bytes());
            let port = u32::from_be_bytes(hash[..4].try_into().unwrap()) % 20000;
            ports
                .insert(port, id.clone())
                .map(|previous| (previous, id))
        })
        .expect("20001 identities must collide in the legacy 20000-port range");

    let first_lock = SessionOwnerLock::acquire(&dir, &first).expect("first identity");
    let second_lock = SessionOwnerLock::acquire(&dir, &second).expect("independent identity");
    drop((first_lock, second_lock));
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(not(windows))]
#[test]
fn server_instance_lock_does_not_depend_on_tcp_port_availability() {
    use cst_server::discovery::normalize_canonical_path_for_hash;
    use sha2::{Digest, Sha256};

    let dir = std::env::temp_dir().join(format!("cst-server-port-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let normalized = normalize_canonical_path_for_hash(&dir).unwrap();
    let hash = Sha256::digest(normalized.as_bytes());
    let port = 42000 + (u32::from_be_bytes(hash[..4].try_into().unwrap()) % 10000) as u16;
    // Either this listener or another process occupies the legacy lock port.
    let _occupied = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port));
    let lock = ServerInstanceLock::acquire(&dir).expect("unrelated TCP listener is not an owner");
    drop(lock);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn app_reconnect_during_web_active_preserves_web_active_and_multi_connection_grace_period() {
    let controller = ModeController::new(ModeControllerConfig {
        standalone_web: false,
        first_connect_timeout: Duration::from_millis(500),
        grace_period: Duration::from_millis(80),
        ticket_ttl: Duration::from_secs(30),
    });

    assert_eq!(
        controller.presence_state(),
        AppPresenceState::WaitingFirstConnect
    );
    assert_eq!(
        controller.mode_snapshot().await.mode,
        ChannelMode::AppActive
    );

    let conn1 = controller.register_app_connection().unwrap();
    let conn2 = controller.register_app_connection().unwrap();
    assert_eq!(controller.active_app_connections(), 2);
    assert_eq!(controller.presence_state(), AppPresenceState::Connected);

    // Switch to WebActive
    let (open_res, _ticket) = controller.open_web(52831).await.unwrap();
    assert_eq!(open_res.mode, ChannelMode::WebActive);

    // Dropping one of two connections does not enter GracePeriod
    drop(conn1);
    assert_eq!(controller.active_app_connections(), 1);
    assert_eq!(controller.presence_state(), AppPresenceState::Connected);

    // Dropping the last connection enters GracePeriod while keeping WebActive
    drop(conn2);
    assert_eq!(controller.active_app_connections(), 0);
    assert!(matches!(
        controller.presence_state(),
        AppPresenceState::GracePeriod { .. }
    ));
    assert_eq!(
        controller.mode_snapshot().await.mode,
        ChannelMode::WebActive
    );

    // Reconnecting within the grace period cancels shutdown and preserves WebActive!
    tokio::time::sleep(Duration::from_millis(25)).await;
    let conn3 = controller.register_app_connection().unwrap();
    assert_eq!(controller.presence_state(), AppPresenceState::Connected);
    assert_eq!(
        controller.mode_snapshot().await.mode,
        ChannelMode::WebActive
    );

    // Wait past the old grace timer to confirm it was cancelled
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!controller.is_stopping());
    assert_eq!(
        controller.mode_snapshot().await.mode,
        ChannelMode::WebActive
    );

    // Now drop conn3 and let grace period expire
    drop(conn3);
    tokio::time::timeout(
        Duration::from_millis(300),
        controller.shutdown_notified().notified(),
    )
    .await
    .expect("grace period should trigger shutdown");
    assert!(controller.is_stopping());
    assert_eq!(controller.presence_state(), AppPresenceState::ShuttingDown);
}

#[tokio::test]
async fn first_connect_timeout_triggers_shutdown_when_app_never_connects() {
    let controller = ModeController::new(ModeControllerConfig {
        standalone_web: false,
        first_connect_timeout: Duration::from_millis(50),
        grace_period: Duration::from_millis(100),
        ticket_ttl: Duration::from_secs(30),
    });

    tokio::time::timeout(
        Duration::from_millis(300),
        controller.shutdown_notified().notified(),
    )
    .await
    .expect("first connect timeout should trigger shutdown");
    assert!(controller.is_stopping());
}

#[tokio::test]
async fn mode_rwlock_and_cookie_epoch_reject_in_flight_stale_web_mutation_after_web_app_web_cycle()
{
    let controller = ModeController::new(ModeControllerConfig::default());
    let _app_conn = controller.register_app_connection().unwrap();

    // 1. Open Web (epoch = 1) and redeem ticket into cookie
    let (_open1, ticket1) = controller.open_web(52831).await.unwrap();
    let cookie1 = controller.redeem_ticket(&ticket1).await.unwrap();
    // Ticket is single-use
    assert!(controller.redeem_ticket(&ticket1).await.is_none());

    let epoch1 = controller.verify_cookie(&cookie1).unwrap();
    let stale_caller = CallerIdentity::Web {
        cookie_epoch: epoch1,
    };

    // Entry-level check passes while still in epoch 1
    controller
        .verify_caller_access(stale_caller, true, false)
        .await
        .unwrap();

    // 2. Hang the in-flight request at a barrier before acquiring `admit_write`,
    // then switch Web -> App -> Web so mode is WebActive again but epoch is now 2.
    let barrier_paused = Arc::new(Barrier::new(2));
    let barrier_resumed = Arc::new(Barrier::new(2));
    let c_clone = controller.clone();
    let bp = barrier_paused.clone();
    let br = barrier_resumed.clone();

    let inflight = tokio::spawn(async move {
        bp.wait().await;
        br.wait().await;
        c_clone.admit_write(stale_caller, None).await
    });

    barrier_paused.wait().await;
    let new_epoch = controller.close_web().await.unwrap();
    assert_eq!(new_epoch, 2);
    let (_open2, ticket2) = controller.open_web(52831).await.unwrap();
    let cookie2 = controller.redeem_ticket(&ticket2).await.unwrap();
    let epoch2 = controller.verify_cookie(&cookie2).unwrap();
    assert_eq!(epoch2, 2);

    barrier_resumed.wait().await;
    let stale_result = inflight.await.unwrap();
    assert_eq!(stale_result.err(), Some(ModeRejection::WebModeInactive));

    // Meanwhile a fresh request carrying epoch 2 succeeds
    let fresh_caller = CallerIdentity::Web {
        cookie_epoch: epoch2,
    };
    let _guard = controller.admit_write(fresh_caller, None).await.unwrap();
}

#[tokio::test]
async fn auth_guard_enforces_host_origin_and_dual_channel_permissions() {
    let controller = ModeController::new(ModeControllerConfig::default());
    let _app_conn = controller.register_app_connection().unwrap();
    let app_token = "a".repeat(64);
    let port = 52831;

    // Invalid Host -> 403 invalid_host
    let mut bad_host = HeaderMap::new();
    bad_host.insert(
        http::header::HOST,
        HeaderValue::from_static("evil.example:52831"),
    );
    let err = validate_host_and_origin(&Method::GET, &bad_host, port).unwrap_err();
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.code, "invalid_host");

    // Invalid Origin -> 403 invalid_origin
    let mut bad_origin = HeaderMap::new();
    bad_origin.insert(
        http::header::HOST,
        HeaderValue::from_static("127.0.0.1:52831"),
    );
    bad_origin.insert(
        http::header::ORIGIN,
        HeaderValue::from_static("http://evil.example"),
    );
    let err = validate_host_and_origin(&Method::POST, &bad_origin, port).unwrap_err();
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.code, "invalid_origin");

    // In AppActive: browser can access /api/state, but /api/sessions returns 403 web_mode_inactive
    let mut browser_headers = HeaderMap::new();
    browser_headers.insert(
        http::header::HOST,
        HeaderValue::from_static("127.0.0.1:52831"),
    );
    authenticate_api_request(
        &Method::GET,
        "/api/state",
        &browser_headers,
        port,
        &app_token,
        &controller,
    )
    .await
    .unwrap();

    let err = authenticate_api_request(
        &Method::GET,
        "/api/sessions",
        &browser_headers,
        port,
        &app_token,
        &controller,
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.code, "web_mode_inactive");

    // Switch to WebActive: App client can read /api/models, but POST /api/sessions returns 409 web_mode_active
    let (_open, ticket) = controller.open_web(port).await.unwrap();
    let cookie = controller.redeem_ticket(&ticket).await.unwrap();

    let mut app_headers = HeaderMap::new();
    app_headers.insert(
        http::header::HOST,
        HeaderValue::from_static("127.0.0.1:52831"),
    );
    app_headers.insert("x-cst-client", HeaderValue::from_static("app"));
    app_headers.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {app_token}")).unwrap(),
    );

    authenticate_api_request(
        &Method::GET,
        "/api/models",
        &app_headers,
        port,
        &app_token,
        &controller,
    )
    .await
    .unwrap();

    let err = authenticate_api_request(
        &Method::POST,
        "/api/sessions",
        &app_headers,
        port,
        &app_token,
        &controller,
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(err.code, "web_mode_active");

    // Browser with valid cookie + Origin + X-CST-Web-Request: 1 succeeds in WebActive
    browser_headers.insert(
        http::header::COOKIE,
        HeaderValue::from_str(&format!("{WEB_SESSION_COOKIE_NAME}={cookie}")).unwrap(),
    );
    browser_headers.insert(
        http::header::ORIGIN,
        HeaderValue::from_static("http://127.0.0.1:52831"),
    );
    browser_headers.insert("x-cst-web-request", HeaderValue::from_static("1"));

    let caller = authenticate_api_request(
        &Method::POST,
        "/api/sessions",
        &browser_headers,
        port,
        &app_token,
        &controller,
    )
    .await
    .unwrap();
    assert_eq!(caller, CallerIdentity::Web { cookie_epoch: 1 });
}

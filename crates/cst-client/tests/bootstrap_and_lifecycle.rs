#![cfg(windows)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use cst_client::bootstrap::{
    BootstrapOptions, DesktopInstanceOutcome, acquire_or_activate_desktop_instance,
    ensure_server_running, recover_pending_update_transaction,
};
use cst_client::error::ClientError;
use cst_kit::SKILL_NAMES;
use cst_os::{ProcessTokenInfo, write_discovery_file_with_timestamp};
use uuid::Uuid;

struct TempKit(PathBuf);

impl TempKit {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cst-client-boot-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join("agent/home")).unwrap();
        fs::write(dir.join("VERSION"), "cst-pilot v0.9.5\n").unwrap();
        fs::write(dir.join("agent/home/APPEND_SYSTEM.md"), "测试提示词。").unwrap();
        for name in SKILL_NAMES {
            let p = dir.join(format!("agent/home/skills/{name}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(
                p,
                format!("---\nname: {name}\ndescription: {name}\n---\n指南。"),
            )
            .unwrap();
        }
        Self(dir.canonicalize().unwrap())
    }
}

impl Drop for TempKit {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn test_bootstrap_options() -> BootstrapOptions {
    let self_exe = std::env::current_exe().unwrap();
    BootstrapOptions {
        require_elevation: false,
        expected_desktop_exe: Some(self_exe),
        ..BootstrapOptions::default()
    }
}

#[test]
fn rapid_multi_click_single_instance_acquires_once_and_wakes_owner() {
    let kit = TempKit::new();
    let opts = test_bootstrap_options();

    let first = match acquire_or_activate_desktop_instance(&kit.0, &opts).unwrap() {
        DesktopInstanceOutcome::Acquired(guard) => guard,
        DesktopInstanceOutcome::ActivatedExisting { .. } => {
            panic!("first caller must acquire lock")
        }
    };
    let wakeup_rx = first.subscribe_wakeup();

    // Simulate 5 rapid concurrent clicks racing to activate the existing desktop instance
    let barrier = Arc::new(Barrier::new(5));
    let mut handles = Vec::new();
    for _ in 0..5 {
        let root = kit.0.clone();
        let o = opts.clone();
        let b = barrier.clone();
        handles.push(std::thread::spawn(move || {
            b.wait();
            acquire_or_activate_desktop_instance(&root, &o).unwrap()
        }));
    }

    for h in handles {
        match h.join().unwrap() {
            DesktopInstanceOutcome::ActivatedExisting { owner_pid } => {
                assert_eq!(owner_pid, std::process::id());
            }
            DesktopInstanceOutcome::Acquired(_) => {
                panic!("subsequent click must not acquire second lock")
            }
        }
    }

    assert_eq!(*wakeup_rx.borrow(), 5);

    // Dropping the guard releases the kernel pipe so a fresh launch acquires cleanly
    drop(first);
    let reacquired = acquire_or_activate_desktop_instance(&kit.0, &opts).unwrap();
    assert!(matches!(reacquired, DesktopInstanceOutcome::Acquired(_)));
}

#[tokio::test]
async fn privilege_mismatch_cross_session_and_path_spoofing_are_intercepted_without_killing() {
    let kit = TempKit::new();

    // 1. Desktop requires elevation when `require_elevation = true` and client token is un-elevated
    let unpriv_opts = BootstrapOptions {
        require_elevation: true,
        current_token_fn: Arc::new(|| {
            Ok(ProcessTokenInfo {
                session_id: 1,
                user_sid: vec![1, 2, 3, 4],
                elevated: false,
            })
        }),
        ..BootstrapOptions::default()
    };
    let not_elev = ensure_server_running(&kit.0, &unpriv_opts).await.unwrap_err();
    assert!(matches!(not_elev, ClientError::NotElevated(_)));

    // 2. Residual un-elevated `cst-server.exe` holding `ServerInstanceLock` -> `PrivilegeMismatch`!
    let _held_server_lock = cst_os::ServerInstanceLock::acquire(&kit.0).unwrap();
    write_discovery_file_with_timestamp(
        &kit.0,
        52999,
        &"a".repeat(64),
        "2026-11-19T00:00:00Z".into(),
    )
    .unwrap();

    // Wrong image path (points to test runner while expected is `<kit>/cst-server.exe`) -> `InvalidServerIdentity`
    let path_check_opts = BootstrapOptions {
        require_elevation: true,
        current_token_fn: Arc::new(|| {
            Ok(ProcessTokenInfo {
                session_id: 1,
                user_sid: vec![1, 2, 3, 4],
                elevated: true,
            })
        }),
        ..BootstrapOptions::default()
    };
    let bad_img = ensure_server_running(&kit.0, &path_check_opts)
        .await
        .unwrap_err();
    assert!(matches!(bad_img, ClientError::InvalidServerIdentity(_)));

    // Matching image path, same session, elevated desktop vs UN-ELEVATED residual server -> `PrivilegeMismatch`
    let self_exe = std::env::current_exe().unwrap();
    let priv_mismatch_opts = BootstrapOptions {
        require_elevation: true,
        expected_server_exe: Some(self_exe.clone()),
        current_token_fn: Arc::new(|| {
            Ok(ProcessTokenInfo {
                session_id: 1,
                user_sid: vec![1, 2, 3, 4],
                elevated: true,
            })
        }),
        query_token_fn: Arc::new(|_| {
            Ok(Some(ProcessTokenInfo {
                session_id: 1,
                user_sid: vec![1, 2, 3, 4],
                elevated: false,
            }))
        }),
        ..BootstrapOptions::default()
    };
    let mismatch_err = ensure_server_running(&kit.0, &priv_mismatch_opts)
        .await
        .unwrap_err();
    assert!(matches!(mismatch_err, ClientError::PrivilegeMismatch(_)));

    // Matching image path, DIFFERENT logon session_id -> `CrossSessionInstance`
    let cross_sess_opts = BootstrapOptions {
        require_elevation: true,
        expected_server_exe: Some(self_exe),
        current_token_fn: Arc::new(|| {
            Ok(ProcessTokenInfo {
                session_id: 1,
                user_sid: vec![1, 2, 3, 4],
                elevated: true,
            })
        }),
        query_token_fn: Arc::new(|_| {
            Ok(Some(ProcessTokenInfo {
                session_id: 2,
                user_sid: vec![1, 2, 3, 4],
                elevated: true,
            }))
        }),
        ..BootstrapOptions::default()
    };
    let cross_err = ensure_server_running(&kit.0, &cross_sess_opts)
        .await
        .unwrap_err();
    assert!(matches!(cross_err, ClientError::CrossSessionInstance(_)));

    // Pending `transaction.json` with missing `cst-updater.exe` -> `UpgradeRecoveryFailed`
    fs::create_dir_all(kit.0.join(".state/update")).unwrap();
    fs::write(kit.0.join(".state/update/transaction.json"), b"{}").unwrap();
    let rec_err = recover_pending_update_transaction(&kit.0, &test_bootstrap_options()).unwrap_err();
    assert!(matches!(rec_err, ClientError::UpgradeRecoveryFailed(_)));
}

#[test]
fn bootstrap_spawns_real_server_verifies_health_and_executes_orderly_exit() {
    use cst_client::{ClientBootstrapResult, CstClient};

    let kit = TempKit::new();
    // Locate compiled `cst-server.exe` in cargo target/debug directory next to test deps
    let target_debug = std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    let server_bin = target_debug.join("cst-server.exe");
    assert!(
        server_bin.exists(),
        "expected compiled cst-server.exe at {}",
        server_bin.display()
    );
    let kit_server_exe = kit.0.join("cst-server.exe");
    fs::copy(&server_bin, &kit_server_exe).unwrap();

    let mut opts = test_bootstrap_options();
    opts.expected_server_exe = Some(kit_server_exe);
    opts.server_extra_args = vec!["--port".into(), "0".into()];

    let started = match CstClient::bootstrap(&kit.0, opts).unwrap() {
        ClientBootstrapResult::Started(client) => client,
        ClientBootstrapResult::ActivatedExisting { .. } => panic!("expected fresh start"),
    };
    let handle = started.handle().clone();

    // Wait briefly on a temporary runtime for initial keepalive connection + state sync
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let snap = handle.snapshot();
            if snap.connected && snap.version == "v0.9.5" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let snap = handle.snapshot();
        assert!(snap.connected);
        assert_eq!(snap.version, "v0.9.5");
        assert!(kit.0.join(".state/tmp").is_dir());

        let exit_check = handle.check_exit().await.unwrap();
        assert!(!exit_check.requires_confirmation());

        handle.shutdown_and_exit().await.unwrap();

        // `.state/server.json` is cleaned up by `cst-server` upon orderly `/api/app/exit`
        let wait_clean = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < wait_clean
            && cst_os::read_discovery_file(&kit.0).is_some()
        {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert!(cst_os::read_discovery_file(&kit.0).is_none());
    });
}

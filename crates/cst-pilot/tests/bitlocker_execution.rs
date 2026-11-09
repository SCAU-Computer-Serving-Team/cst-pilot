use cst_pilot::{
    bitlocker::{
        core::{BitLockerSource, Executor},
        credentials::{PrivacyPolicy, Vault},
        lock::VolumeLock,
        model::*,
    },
    diagnostics::win_native::{Control, NativeError},
};
use pi_session::{ChannelUiContext, ExtensionUiContext, NoOpUiContext, UiPromptRequest};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const PASSWORD: &str = "123456-234567-345678-456789-567890-678901-789012-890123";
fn volume(drive: &str, locked: bool) -> Volume {
    Volume {
        volume_id: format!("mock-volume-{}-{}", drive, uuid::Uuid::new_v4()),
        drive: Some(drive.into()),
        volume_type: Some(1),
        lock_status: Some(u32::from(locked)),
        conversion_status: Some(1),
        encryption_percentage: Some(100),
        ..Default::default()
    }
}
fn snapshot(volumes: Vec<Volume>) -> Snapshot {
    Snapshot {
        collected_at: "2026-11-09T00:00:00.000Z".into(),
        admin: true,
        volumes,
        ..Default::default()
    }
}
struct Source {
    live: Mutex<Snapshot>,
    reads: Mutex<VecDeque<Result<Snapshot, NativeError>>>,
    queries: AtomicUsize,
    calls: Mutex<Vec<(String, Scope, bool)>>,
    code: Mutex<Result<u32, NativeError>>,
    entered: AtomicBool,
    delay: AtomicUsize,
    check_post_lock: AtomicBool,
}
impl Source {
    fn new(volumes: Vec<Volume>) -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(snapshot(volumes)),
            reads: Mutex::new(VecDeque::new()),
            queries: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            code: Mutex::new(Ok(0)),
            entered: AtomicBool::new(false),
            delay: AtomicUsize::new(0),
            check_post_lock: AtomicBool::new(false),
        })
    }
}
impl BitLockerSource for Source {
    fn snapshot(&self, control: &Control) -> Result<Snapshot, NativeError> {
        control.check().map_err(|_| failure("cancel", None))?;
        self.queries.fetch_add(1, Ordering::SeqCst);
        if self.check_post_lock.load(Ordering::SeqCst) && self.entered.load(Ordering::SeqCst) {
            let live = self.live.lock().unwrap();
            assert!(VolumeLock::acquire(&live.volumes[0].volume_id).is_err());
        }
        self.reads
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(self.live.lock().unwrap().clone()))
    }
    fn mutate(
        &self,
        target: &Volume,
        scope: Scope,
        password: Option<&str>,
        _: &Control,
    ) -> Result<u32, NativeError> {
        assert!(VolumeLock::acquire(&target.volume_id).is_err());
        assert!(scope != Scope::Status);
        self.calls.lock().unwrap().push((
            target.volume_id.clone(),
            scope,
            password == Some(PASSWORD),
        ));
        if scope == Scope::Unlock {
            assert_eq!(password, Some(PASSWORD));
        } else {
            assert!(password.is_none());
        }
        self.entered.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(
            self.delay.load(Ordering::SeqCst) as u64
        ));
        let code = self.code.lock().unwrap().clone();
        if code.as_ref().is_ok_and(|n| *n == 0) {
            let mut live = self.live.lock().unwrap();
            if let Some(v) = live
                .volumes
                .iter_mut()
                .find(|v| v.volume_id == target.volume_id)
            {
                if scope == Scope::Unlock {
                    v.lock_status = Some(0);
                } else {
                    v.conversion_status = Some(3);
                    v.encryption_percentage = Some(0);
                }
            }
        }
        code
    }
}
fn failure(source: &str, code: Option<u32>) -> NativeError {
    NativeError {
        source: source.into(),
        code,
        message: format!("synthetic error {PASSWORD}"),
    }
}
fn input(scope: Scope, drive: Option<&str>, password: bool) -> Input {
    Input {
        scope,
        drive: drive.map(str::to_owned),
        recovery_password: password.then(|| PASSWORD.into()),
    }
}
fn dialogs(
    yes: bool,
) -> (
    Arc<dyn ExtensionUiContext>,
    tokio::task::JoinHandle<Vec<String>>,
) {
    let (ui, mut rx) = ChannelUiContext::pair();
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        while let Some(request) = rx.recv().await {
            match request {
                UiPromptRequest::Confirm {
                    title,
                    message,
                    opts,
                    respond,
                } => {
                    assert_eq!(opts.unwrap().timeout_ms, Some(180_000));
                    assert!(!message.contains(PASSWORD));
                    requests.push(format!("{title} {message}"));
                    let _ = respond.send(yes);
                }
                UiPromptRequest::Input {
                    title,
                    opts,
                    respond,
                    ..
                } => {
                    let opts = opts.unwrap();
                    assert!(opts.secret);
                    assert_eq!(opts.timeout_ms, Some(180_000));
                    requests.push(title);
                    let _ = respond.send(yes.then(|| PASSWORD.into()));
                }
                _ => panic!("unexpected dialog"),
            }
        }
        requests
    });
    (Arc::new(ui), handle)
}
async fn run(source: Arc<Source>, input: Input, yes: bool) -> Value {
    let (ui, handle) = dialogs(yes);
    let result = Executor::new(source)
        .execute(
            "call",
            input,
            &Vault::default(),
            ui,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    handle.await.unwrap();
    result
}
#[tokio::test]
async fn status_is_readonly_with_partial_errors_and_null_fields() {
    let source = Source::new(vec![volume("D:", true)]);
    source.live.lock().unwrap().volumes[0]
        .collection_errors
        .push(failure("GetEncryptionMethod", Some(0x80310000)));
    let r = Executor::new(source.clone())
        .execute(
            "s",
            input(Scope::Status, None, false),
            &Vault::default(),
            Arc::new(NoOpUiContext::new()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r["bitlocker"]["degraded"], true);
    assert_eq!(
        r["bitlocker"]["volumes"][0]["encryptionMethod"],
        Value::Null
    );
    assert_eq!(source.queries.load(Ordering::SeqCst), 1);
    assert!(source.calls.lock().unwrap().is_empty());
    assert!(!r.to_string().contains(PASSWORD));
}
#[tokio::test]
async fn local_secret_input_and_ai_confirmation_both_reach_only_the_mock_native_call() {
    for parameter in [false, true] {
        let source = Source::new(vec![volume("D:", true)]);
        let r = run(
            source.clone(),
            input(Scope::Unlock, Some("d"), parameter),
            true,
        )
        .await;
        let v = &r["bitlocker"]["volumes"][0];
        assert_eq!(v["lockStatus"], 0);
        assert_eq!(v["operation"]["status"], "accepted");
        assert_eq!(
            v["operation"]["credentialSource"],
            if parameter {
                "toolParameter"
            } else {
                "localInput"
            }
        );
        assert_eq!(source.calls.lock().unwrap().len(), 1);
        assert_eq!(source.queries.load(Ordering::SeqCst), 3);
        assert!(!r.to_string().contains(PASSWORD));
    }
}
#[tokio::test]
async fn invalid_input_and_no_admin_or_ui_never_submit_a_method() {
    let source = Source::new(vec![volume("D:", true)]);
    for invalid in [
        input(Scope::Unlock, None, true),
        input(Scope::Status, Some("D:"), true),
        input(Scope::Decrypt, Some("D:"), true),
        input(Scope::Unlock, Some("D:/"), true),
        Input {
            scope: Scope::Unlock,
            drive: Some("D:".into()),
            recovery_password: Some("bad-secret".into()),
        },
    ] {
        let result = Executor::new(source.clone())
            .execute(
                "i",
                invalid,
                &Vault::default(),
                Arc::new(NoOpUiContext::new()),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }
    let r = Executor::new(source.clone())
        .execute(
            "n",
            input(Scope::Unlock, Some("D:"), false),
            &Vault::default(),
            Arc::new(NoOpUiContext::new()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        r["bitlocker"]["volumes"][0]["operation"]["reason"],
        "执行操作需要本地交互"
    );
    source.live.lock().unwrap().admin = false;
    let r = run(source.clone(), input(Scope::Unlock, Some("D:"), true), true).await;
    assert_eq!(
        r["bitlocker"]["volumes"][0]["operation"]["reason"],
        "需要管理员权限"
    );
    assert!(source.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn rejection_and_cancelled_dialogs_preserve_state_without_submission() {
    for scope in [Scope::Unlock, Scope::Decrypt] {
        let source = Source::new(vec![volume("D:", scope == Scope::Unlock)]);
        let r = run(
            source.clone(),
            input(scope, Some("D:"), scope == Scope::Unlock),
            false,
        )
        .await;
        assert_eq!(
            r["bitlocker"]["volumes"][0]["operation"]["status"],
            "cancelled"
        );
        assert!(source.calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn decrypt_confirms_only_fixed_eligible_targets_and_zero_percent_is_not_completion() {
    let mut ready = volume("D:", false);
    ready.volume_type = Some(0);
    let mut clear = volume("E:", false);
    clear.conversion_status = Some(0);
    let mut pending = volume("F:", false);
    pending.conversion_status = Some(3);
    let locked = volume("G:", true);
    let source = Source::new(vec![ready.clone(), clear, pending, locked]);
    let (ui, handle) = dialogs(true);
    let r = Executor::new(source.clone())
        .execute(
            "d",
            input(Scope::Decrypt, None, false),
            &Vault::default(),
            ui,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let prompts = handle.await.unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains(&ready.volume_id));
    assert!(!prompts[0].contains("E:"));
    let v = &r["bitlocker"]["volumes"];
    assert_eq!(v[0]["operation"]["status"], "accepted");
    assert_eq!(v[0]["conversionStatus"], 3);
    assert_eq!(v[0]["encryptionPercentage"], 0);
    assert_eq!(v[1]["operation"]["status"], "skipped");
    assert_eq!(v[2]["operation"]["status"], "skipped");
    assert_eq!(v[3]["operation"]["status"], "rejected");
    assert_eq!(source.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn return_codes_and_com_errors_are_preserved_without_automatic_repairs_or_retries() {
    for code in [0x80310000, 0x80310029] {
        let source = Source::new(vec![volume("D:", false)]);
        *source.code.lock().unwrap() = Ok(code);
        let r = run(
            source.clone(),
            input(Scope::Decrypt, Some("D:"), false),
            true,
        )
        .await;
        assert_eq!(
            r["bitlocker"]["volumes"][0]["operation"]["returnCode"],
            code
        );
        assert_eq!(
            r["bitlocker"]["volumes"][0]["operation"]["status"],
            "rejected"
        );
        assert_eq!(source.calls.lock().unwrap().len(), 1);
    }
    let source = Source::new(vec![volume("D:", true)]);
    *source.code.lock().unwrap() = Err(failure("UnlockWithNumericalPassword", Some(0x800706ba)));
    let r = run(source.clone(), input(Scope::Unlock, Some("D:"), true), true).await;
    let op = &r["bitlocker"]["volumes"][0]["operation"];
    assert_eq!(op["status"], "unknown");
    assert_eq!(op["error"]["code"], 0x800706bau32);
    assert!(!r.to_string().contains(PASSWORD));
    assert_eq!(source.calls.lock().unwrap().len(), 1);
    assert_eq!(source.queries.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn drive_identity_and_eligibility_are_rechecked_after_dialog_before_submission() {
    for changed in ["drive", "identity", "state", "admin"] {
        let v = volume("D:", true);
        let source = Source::new(vec![v.clone()]);
        let mut second = snapshot(vec![v.clone()]);
        match changed {
            "drive" => second.volumes[0].drive = Some("E:".into()),
            "identity" => second.volumes[0].volume_id = "different".into(),
            "state" => second.volumes[0].lock_status = None,
            _ => second.admin = false,
        }
        source
            .reads
            .lock()
            .unwrap()
            .extend([Ok(snapshot(vec![v])), Ok(second)]);
        let r = run(source.clone(), input(Scope::Unlock, Some("D:"), true), true).await;
        assert_eq!(
            r["bitlocker"]["volumes"][0]["operation"]["status"],
            "rejected"
        );
        assert!(source.calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn failed_postquery_discards_old_lock_and_conversion_values() {
    let v = volume("D:", true);
    let source = Source::new(vec![v.clone()]);
    source.reads.lock().unwrap().extend([
        Ok(snapshot(vec![v.clone()])),
        Ok(snapshot(vec![v])),
        Err(failure("postOperation", None)),
    ]);
    let r = run(source, input(Scope::Unlock, Some("D:"), false), true).await;
    let v = &r["bitlocker"]["volumes"][0];
    assert!(v["lockStatus"].is_null());
    assert!(v["conversionStatus"].is_null());
    assert_eq!(v["operation"]["status"], "accepted");
    assert_eq!(r["bitlocker"]["degraded"], true);
}
#[tokio::test]
async fn timeout_retains_worker_lock_and_independent_postquery_does_not_reuse_old_state() {
    let v = volume("D:", true);
    let source = Source::new(vec![v.clone()]);
    source.delay.store(300, Ordering::SeqCst);
    let (ui, handle) = dialogs(true);
    let executor = Executor::with_timeout(source.clone(), Duration::from_millis(60));
    let r = executor
        .execute(
            "t",
            input(Scope::Unlock, Some("D:"), true),
            &Vault::default(),
            ui,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    handle.await.unwrap();
    assert_eq!(
        r["bitlocker"]["volumes"][0]["operation"]["status"],
        "unknown"
    );
    assert!(VolumeLock::acquire(&v.volume_id).is_err());
    assert!(source.queries.load(Ordering::SeqCst) >= 3);
    tokio::time::sleep(Duration::from_millis(330)).await;
    assert!(VolumeLock::acquire(&v.volume_id).is_ok());
    assert_eq!(source.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn cancellation_after_submit_rechecks_live_state_and_cancels_remaining_targets() {
    let v = volume("D:", false);
    let source = Source::new(vec![v.clone(), volume("E:", false)]);
    source.delay.store(140, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    let s = source.clone();
    let cancel_task = tokio::spawn(async move {
        while !s.entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        c.cancel();
    });
    let (ui, handle) = dialogs(true);
    let r = Executor::new(source.clone())
        .execute(
            "c",
            input(Scope::Decrypt, None, false),
            &Vault::default(),
            ui,
            cancel.clone(),
        )
        .await
        .unwrap();
    handle.await.unwrap();
    cancel_task.await.unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(
        r["bitlocker"]["volumes"][0]["operation"]["status"],
        "unknown"
    );
    assert_eq!(
        r["bitlocker"]["volumes"][1]["operation"]["status"],
        "cancelled"
    );
    assert!(source.queries.load(Ordering::SeqCst) >= 3);
    assert_eq!(source.calls.lock().unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(170)).await;
}
#[tokio::test]
async fn volume_lock_covers_local_input_and_drop_cancels_without_late_submission() {
    let v = volume("D:", true);
    let source = Source::new(vec![v.clone()]);
    let (ui, mut rx) = ChannelUiContext::pair();
    let cancel = CancellationToken::new();
    let source2 = source.clone();
    let c = cancel.clone();
    let task = tokio::spawn(async move {
        Executor::new(source2)
            .execute(
                "drop",
                input(Scope::Unlock, Some("D:"), false),
                &Vault::default(),
                Arc::new(ui),
                c,
            )
            .await
    });
    let request = rx.recv().await.unwrap();
    assert!(matches!(request, UiPromptRequest::Input { .. }));
    assert!(VolumeLock::acquire(&v.volume_id).is_err());
    task.abort();
    let _ = task.await;
    drop(request);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(VolumeLock::acquire(&v.volume_id).is_ok());
    assert!(!cancel.is_cancelled());
    assert!(source.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn startup_report_only_verifies_unknown_operations_never_replays_them() {
    use pi_agent::ToolReplayPolicy;
    use pi_session::execution_log::{StartupReconciliationReport, UnresolvedToolCallReport};
    let source = Source::new(vec![volume("D:", false)]);
    let executor = Executor::new(source.clone());
    let mut report = StartupReconciliationReport::default();
    assert!(
        executor
            .reconcile(&report, CancellationToken::new())
            .await
            .is_none()
    );
    report.unresolved_tool_calls.push(UnresolvedToolCallReport {
        tool_call_id: "old".into(),
        parent_tool_call_id: None,
        tool_name: "bitlocker".into(),
        replay_policy: ToolReplayPolicy::Never,
        requires_state_verification: true,
        can_auto_replay: false,
        reason: "synthetic crash".into(),
    });
    let r = executor
        .reconcile(&report, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        r["bitlocker"]["volumes"][0]["operation"]["status"],
        "unknown"
    );
    assert!(source.calls.lock().unwrap().is_empty());
    assert_eq!(source.queries.load(Ordering::SeqCst), 1);
}
#[test]
fn tool_policy_is_sequential_never_and_lock_identity_matches_node_hash() {
    use pi_agent::{ToolExecutionMode, ToolReplayPolicy};
    use sha2::{Digest, Sha256};
    let tool = cst_pilot::bitlocker::definition(Vault::default());
    assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));
    assert_eq!(tool.replay_policy, ToolReplayPolicy::Never);
    assert_eq!(
        VolumeLock::address(" mock-volume\\/ ").unwrap(),
        format!(
            r"\\.\pipe\cst-pilot-bitlocker-{:x}",
            Sha256::digest(b"MOCK-VOLUME")
        )
    );
    let v = volume("D:", true);
    let lock = VolumeLock::acquire(&v.volume_id).unwrap();
    assert!(VolumeLock::acquire(&format!(" {}\\/ ", v.volume_id.to_uppercase())).is_err());
    drop(lock);
    assert!(VolumeLock::acquire(&v.volume_id).is_ok());
}

#[tokio::test]
async fn unknown_operation_uses_new_state_and_success_keeps_lock_until_postquery_finishes() {
    for fails in [false, true] {
        let v = volume("D:", true);
        let source = Source::new(vec![v.clone()]);
        source.check_post_lock.store(true, Ordering::SeqCst);
        let mut fresh = v.clone();
        fresh.lock_status = Some(0);
        fresh.encryption_percentage = Some(41);
        source.reads.lock().unwrap().extend([
            Ok(snapshot(vec![v.clone()])),
            Ok(snapshot(vec![v])),
            Ok(snapshot(vec![fresh])),
        ]);
        if fails {
            *source.code.lock().unwrap() = Err(failure("COM", Some(0x800706bau32)));
        }
        let r = run(
            source.clone(),
            input(Scope::Unlock, Some("D:"), false),
            true,
        )
        .await;
        assert_eq!(
            r["bitlocker"]["volumes"][0]["operation"]["status"],
            if fails { "unknown" } else { "accepted" }
        );
        assert_eq!(r["bitlocker"]["volumes"][0]["lockStatus"], 0);
        assert_eq!(r["bitlocker"]["volumes"][0]["encryptionPercentage"], 41);
    }
}
#[tokio::test]
async fn busy_volume_rejects_before_prompt_and_aborted_mutation_stays_locked_until_worker_exits() {
    let v = volume("D:", true);
    let source = Source::new(vec![v.clone()]);
    let lock = VolumeLock::acquire(&v.volume_id).unwrap();
    let (ui, handle) = dialogs(true);
    let r = Executor::new(source.clone())
        .execute(
            "busy",
            input(Scope::Unlock, Some("D:"), true),
            &Vault::default(),
            ui,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(handle.await.unwrap().is_empty());
    assert_eq!(
        r["bitlocker"]["volumes"][0]["operation"]["reason"],
        "目标卷已有操作进行中"
    );
    drop(lock);
    source.delay.store(250, Ordering::SeqCst);
    let s = source.clone();
    let (ui, handle) = dialogs(true);
    let task = tokio::spawn(async move {
        Executor::new(s)
            .execute(
                "abort",
                input(Scope::Unlock, Some("D:"), false),
                &Vault::default(),
                ui,
                CancellationToken::new(),
            )
            .await
    });
    while !source.entered.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    task.abort();
    let _ = task.await;
    handle.await.unwrap();
    assert!(VolumeLock::acquire(&v.volume_id).is_err());
    tokio::time::sleep(Duration::from_millis(280)).await;
    assert!(VolumeLock::acquire(&v.volume_id).is_ok());
    assert_eq!(source.calls.lock().unwrap().len(), 1);
}
#[test]
#[ignore = "helper invoked by process-lock and TypeScript interoperability tests"]
fn bitlocker_lock_child() {
    let Ok(id) = std::env::var("CST_BITLOCKER_LOCK_ID") else {
        return;
    };
    let _lock = VolumeLock::acquire(&id).unwrap();
    std::fs::write(std::env::var("CST_BITLOCKER_LOCK_READY").unwrap(), b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}
#[test]
fn independent_process_lock_is_released_by_process_termination() {
    use std::{
        fs,
        process::{Command, Stdio},
        time::Instant,
    };
    let id = format!("mock-independent-{}", uuid::Uuid::new_v4());
    let ready = std::env::temp_dir().join(format!("bitlocker-lock-ready-{}", uuid::Uuid::new_v4()));
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "bitlocker_lock_child"])
            .env("CST_BITLOCKER_LOCK_ID", &id)
            .env("CST_BITLOCKER_LOCK_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(VolumeLock::acquire(&id).is_err());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let _lock = VolumeLock::acquire(&id).unwrap();
    fs::remove_file(ready).unwrap();
}

#[derive(Clone)]
struct ActualTool {
    source: Arc<Source>,
    vault: Vault,
}
impl pi_session::Extension for ActualTool {
    fn path(&self) -> &str {
        "actual-bitlocker-mocked-native"
    }
    fn register(
        &self,
        api: &mut pi_session::ExtensionRegistrar,
    ) -> Result<(), pi_session::ExtensionInitError> {
        self.vault.register(api);
        api.register_tool(cst_pilot::bitlocker::definition_with(
            self.vault.clone(),
            Executor::new(self.source.clone()),
        ));
        Ok(())
    }
}
#[tokio::test]
async fn real_sdk_actual_tool_uses_secret_channel_and_ai_vault_without_durable_credentials() {
    use pi_ai::{
        StopReason,
        faux::{
            FauxResponseStep, create_faux_provider, faux_assistant_message, faux_text_message,
            faux_tool_call,
        },
    };
    use pi_runtime::{
        CreateAgentSessionOptions, CreateModelRuntimeOptions, ModelRuntime, NoToolsMode,
        create_agent_session,
    };
    use pi_session::{PromptOptions, SessionManager};
    for parameter in [false, true] {
        let root = std::env::temp_dir().join(format!("bitlocker-sdk-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let faux = create_faux_provider();
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            include_builtins: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await;
        runtime.register_provider(faux.provider.clone());
        let source = Source::new(vec![volume("D:", true)]);
        let ext = ActualTool {
            source: source.clone(),
            vault: Vault::default(),
        };
        let factory: pi_session::ExtensionFactory = Arc::new(move || Ok(Box::new(ext.clone())));
        let (ui, handle) = dialogs(true);
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(root.clone()),
            agent_dir: Some(root.join("agent")),
            model_runtime: Some(runtime),
            model: Some(faux.model.clone()),
            session_manager: Some(
                SessionManager::create(root.to_string_lossy(), root.join("sessions"), None)
                    .unwrap(),
            ),
            no_tools: Some(NoToolsMode::Builtin),
            ui_context: Some(ui),
            data_policy: Some(Arc::new(PrivacyPolicy)),
            extension_factories: vec![factory],
            ..Default::default()
        })
        .await
        .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let e = events.clone();
        let _unsubscribe = created
            .session
            .subscribe(Arc::new(move |event| e.lock().unwrap().push(event)));
        let mut args = json!({"scope":"unlock","drive":"d"});
        if parameter {
            args["recoveryPassword"] = json!(PASSWORD);
        }
        faux.set_responses(vec![
            FauxResponseStep::Static(faux_assistant_message(
                vec![faux_tool_call(
                    "bitlocker",
                    args.as_object().unwrap().clone(),
                    Some("real-call".into()),
                )],
                Some(StopReason::ToolUse),
            )),
            FauxResponseStep::Static(faux_text_message("完成")),
        ]);
        created
            .session
            .prompt(
                if parameter {
                    format!("我选择 AI 参数解锁 D: {PASSWORD}")
                } else {
                    "解锁 D:，本地输入".into()
                },
                Some(PromptOptions {
                    client_message_id: Some("credential-input".into()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        assert_eq!(source.calls.lock().unwrap().len(), 1);
        assert!(source.calls.lock().unwrap()[0].2);
        let session_file = created.session.session_file().unwrap();
        for text in [
            std::fs::read_to_string(&session_file).unwrap(),
            std::fs::read_to_string(format!("{}.state.json", session_file.display())).unwrap(),
            serde_json::to_string(&*events.lock().unwrap()).unwrap(),
            serde_json::to_string(&created.session.state().messages).unwrap(),
        ] {
            assert!(!text.contains(PASSWORD));
            assert!(text.contains("accepted"));
        }
        created.session.shutdown().await.unwrap();
        drop(created);
        handle.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

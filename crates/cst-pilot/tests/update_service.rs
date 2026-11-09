#![cfg(windows)]

use base64::{Engine, engine::general_purpose::STANDARD};
use cst_pilot::{
    Kit, KitSession, SKILL_NAMES,
    auth::{AuthRuntime, Channel, oa::OaClient},
    telemetry::TelemetryOptions,
    update::{
        UpdateClient,
        activity::ActivityGate,
        handoff::{Handoff, LaunchReceipt},
        manifest::{
            Archive, MANIFEST_DOMAIN, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
        },
        network::{Download, Fetcher},
        process::{ProcessIdentity, RestartUi},
        service::{HandoffLauncher, Phase, UpdateOptions, UpdateService, startup_check},
    },
};
use ed25519_dalek::{Signer, SigningKey};
use pi_agent::BoxFuture;
use pi_ai::faux::{FauxResponseStep, create_faux_provider, faux_text_message};
use pi_runtime::CreateAgentSessionOptions;
use pi_session::{ChannelUiContext, SessionManager, UiPromptRequest};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zip::{ZipWriter, write::SimpleFileOptions};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("cst-update-service-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("agent/home")).unwrap();
        fs::write(path.join("VERSION"), "cst-pilot v0.6.0\n").unwrap();
        fs::write(path.join("cst-pilot.exe"), "old main").unwrap();
        fs::write(
            path.join("agent/home/telemetry.json"),
            r#"{"enabled":false}"#,
        )
        .unwrap();
        fs::write(
            path.join("agent/home/APPEND_SYSTEM.md"),
            "Synthetic instructions",
        )
        .unwrap();
        for name in SKILL_NAMES {
            let dir = path.join(format!("agent/home/skills/{name}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: synthetic test\n---\nTest"),
            )
            .unwrap();
        }
        Self(path.canonicalize().unwrap())
    }
    fn client(&self, net: Arc<Network>) -> UpdateClient {
        UpdateClient::with_fetcher("https://updates.example/latest", key(), net).unwrap()
    }
    fn service(&self, net: Arc<Network>, launcher: Arc<Launcher>) -> Arc<UpdateService> {
        UpdateService::load(
            &self.0,
            "https://www.cstoa.top",
            UpdateOptions::Trusted {
                client: Arc::new(self.client(net)),
                launcher,
            },
        )
        .unwrap()
    }
    async fn sdk(
        &self,
        net: Arc<Network>,
        launcher: Arc<Launcher>,
        ui: Option<Arc<ChannelUiContext>>,
    ) -> KitSession {
        let auth = AuthRuntime::create(
            &self.0,
            Channel::Gui,
            OaClient::new("http://127.0.0.1:9").unwrap(),
        )
        .await
        .unwrap();
        let faux = create_faux_provider();
        faux.set_responses(vec![
            FauxResponseStep::Static(faux_text_message("reply"));
            8
        ]);
        auth.runtime().register_provider(faux.provider);
        Kit::new(&self.0)
            .unwrap()
            .create_session_with_update(
                CreateAgentSessionOptions {
                    model: Some(faux.model),
                    session_manager: Some(
                        SessionManager::in_memory(self.0.to_string_lossy(), None, None).unwrap(),
                    ),
                    ui_context: ui.map(|u| u as Arc<dyn pi_session::ExtensionUiContext>),
                    ..Default::default()
                },
                auth,
                TelemetryOptions::default(),
                UpdateOptions::Trusted {
                    client: Arc::new(self.client(net)),
                    launcher,
                },
            )
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn key() -> ReleaseKey {
    ReleaseKey::from_bytes(SigningKey::from_bytes(&[42; 32]).verifying_key().as_bytes()).unwrap()
}
struct Network {
    envelope: Vec<u8>,
    archive: Vec<u8>,
    checks: AtomicUsize,
    downloads: AtomicUsize,
    block: AtomicBool,
    fail: AtomicBool,
}
impl Network {
    fn new() -> Arc<Self> {
        let mut files = BTreeMap::from([
            ("cst-pilot.exe".to_owned(), b"new synthetic main".to_vec()),
            ("cst-updater.exe".into(), b"not a real executable".to_vec()),
            ("VERSION".into(), b"cst-pilot v0.7.0\n".to_vec()),
            (
                "BUILD-INFO.json".into(),
                br#"{"version":"v0.7.0"}"#.to_vec(),
            ),
            ("LICENSE".into(), b"MIT".to_vec()),
            ("THIRD-PARTY-NOTICES.md".into(), b"notices".to_vec()),
            ("licenses/mock.txt".into(), b"mock notice".to_vec()),
            (
                "agent/home/APPEND_SYSTEM.md".into(),
                b"Synthetic instructions".to_vec(),
            ),
            (
                "agent/home/extensions/telemetry/timserver_1.crt".into(),
                b"synthetic public certificate".to_vec(),
            ),
        ]);
        for name in SKILL_NAMES {
            files.insert(
                format!("agent/home/skills/{name}/SKILL.md"),
                format!("---\nname: {name}\ndescription: test\n---\nTest").into_bytes(),
            );
        }
        let sums = files
            .iter()
            .map(|(n, b)| format!("{:x}  {n}\n", Sha256::digest(b)))
            .collect::<String>();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in &files {
            zip.start_file(format!("cst-pilot/{name}"), SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.start_file("cst-pilot/SHA256SUMS", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(sums.as_bytes()).unwrap();
        let archive = zip.finish().unwrap().into_inner();
        let sign = |domain: &[u8], bytes: &[u8]| {
            STANDARD.encode(
                SigningKey::from_bytes(&[42; 32])
                    .sign(&[domain, bytes].concat())
                    .to_bytes(),
            )
        };
        let info = ReleaseManifest {
            schema_version: 1,
            platform: "windows-x86_64".into(),
            version: "v0.7.0".into(),
            min_upgradable_version: "v0.6.0".into(),
            built_at: "2026-11-09T00:00:00Z".into(),
            release_notes: "Synthetic release".into(),
            archive: Archive {
                url: "https://updates.example/update.zip".into(),
                size: archive.len() as u64,
                unpacked_size: files.values().map(|b| b.len() as u64).sum::<u64>()
                    + sums.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&archive)),
            },
            sha256_signature: sign(SUMS_DOMAIN, sums.as_bytes()),
            sha256_sums: sums,
        };
        let payload = serde_json::to_string(&info).unwrap();
        let envelope = serde_json::to_vec(&SignedManifest {
            signature: sign(MANIFEST_DOMAIN, payload.as_bytes()),
            payload,
        })
        .unwrap();
        Arc::new(Self {
            envelope,
            archive,
            checks: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
            block: AtomicBool::new(false),
            fail: AtomicBool::new(false),
        })
    }
}
impl Fetcher for Network {
    fn get(
        &self,
        url: String,
        _: u64,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, String>> {
        Box::pin(async move {
            let manifest = url.ends_with("/latest");
            if manifest {
                self.checks.fetch_add(1, Ordering::SeqCst);
            } else {
                self.downloads.fetch_add(1, Ordering::SeqCst);
            }
            if self.block.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err("cancelled".into());
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err("synthetic-secret-not-for-UI".into());
            }
            let bytes = if manifest {
                self.envelope.clone()
            } else {
                self.archive.clone()
            };
            Ok(Download {
                body: Box::pin(futures_util::stream::iter(vec![Ok(bytes)])),
            })
        })
    }
}
#[derive(Default)]
struct Launcher {
    calls: AtomicUsize,
    fail: AtomicBool,
    delay: Mutex<Duration>,
}
impl HandoffLauncher for Launcher {
    fn launch(&self, handoff: Handoff) -> Result<LaunchReceipt, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(*self.delay.lock().unwrap());
        if self.fail.load(Ordering::SeqCst) {
            return Err("synthetic-launch-secret".into());
        }
        // A mock receipt exercises host coordination only. No synthetic binary is started.
        Ok(LaunchReceipt {
            updater_pid: 4242,
            request_id: handoff.request_id().into(),
        })
    }
}
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn startup_check_is_shared_nonblocking_and_status_only_without_dialog() {
    let f = Fixture::new();
    let net = Network::new();
    net.block.store(true, Ordering::SeqCst);
    let (ui, mut dialogs) = ChannelUiContext::pair();
    let ui = Arc::new(ui);
    let before = tokio::time::Instant::now();
    let a = f
        .sdk(net.clone(), Arc::new(Launcher::default()), Some(ui.clone()))
        .await;
    let b = f
        .sdk(net.clone(), Arc::new(Launcher::default()), None)
        .await;
    assert!(before.elapsed() < Duration::from_secs(2));
    assert!(Arc::ptr_eq(&a.updates, &b.updates));
    until(|| net.checks.load(Ordering::SeqCst) == 1).await;
    assert_eq!(a.updates.status().phase, Phase::Checking);
    assert!(dialogs.try_recv().is_err());
    assert!(ui.snapshot().notifications.is_empty());
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}
#[tokio::test]
async fn signed_available_prepare_and_ui_status_survive_reload_without_extra_startup_check() {
    let f = Fixture::new();
    let net = Network::new();
    let (ui, _) = ChannelUiContext::pair();
    let ui = Arc::new(ui);
    let sdk = f
        .sdk(net.clone(), Arc::new(Launcher::default()), Some(ui.clone()))
        .await;
    until(|| sdk.updates.status().phase == Phase::Available).await;
    assert_eq!(sdk.updates.status().version.as_deref(), Some("v0.7.0"));
    assert!(ui.snapshot().statuses.contains_key("cst-update"));
    assert!(ui.snapshot().notifications.is_empty());
    sdk.created.session.reload().await.unwrap();
    assert_eq!(net.checks.load(Ordering::SeqCst), 1);
    sdk.created
        .session
        .prompt("/update check", None)
        .await
        .unwrap();
    assert_eq!(net.checks.load(Ordering::SeqCst), 2);
    assert_eq!(
        sdk.updates
            .prepare(CancellationToken::new())
            .await
            .unwrap()
            .phase,
        Phase::Ready
    );
    assert_eq!(net.downloads.load(Ordering::SeqCst), 1);
    sdk.updates.prepare(CancellationToken::new()).await.unwrap();
    assert_eq!(net.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(f.0.join("cst-pilot.exe")).unwrap(), b"old main");
    sdk.shutdown().await.unwrap();
}
#[tokio::test]
async fn activity_and_volume_locks_refuse_handoff_then_allow_retry() {
    let f = Fixture::new();
    let service = f.service(Network::new(), Arc::new(Launcher::default()));
    service.check(CancellationToken::new()).await.unwrap();
    service.prepare(CancellationToken::new()).await.unwrap();
    let activity = service.gate.enter().unwrap();
    assert!(
        service
            .launch(
                vec![ProcessIdentity::current().unwrap()],
                RestartUi::Web,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    drop(activity);
    let volume =
        cst_pilot::bitlocker::lock::VolumeLock::acquire("SYNTHETIC-UPDATE-VOLUME").unwrap();
    assert!(service.gate.close().is_err());
    drop(volume);
    let receipt = service
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Web,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.updater_pid, 4242);
    assert_eq!(service.status().phase, Phase::ExitRequired);
    assert!(service.gate.enter().is_err());
}
#[tokio::test]
async fn launch_failure_reopens_gate_cleans_stage_and_does_not_close_session() {
    let f = Fixture::new();
    let launcher = Arc::new(Launcher::default());
    launcher.fail.store(true, Ordering::SeqCst);
    let sdk = f.sdk(Network::new(), launcher.clone(), None).await;
    sdk.updates.check(CancellationToken::new()).await.unwrap();
    sdk.updates.prepare(CancellationToken::new()).await.unwrap();
    let error = sdk
        .updates
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Tui,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(!error.contains("synthetic-launch-secret"));
    assert!(sdk.updates.gate.enter().is_ok());
    assert!(sdk.updates.status().launch.is_none());
    sdk.created
        .session
        .prompt("still usable", None)
        .await
        .unwrap();
    launcher.fail.store(false, Ordering::SeqCst);
    sdk.updates.prepare(CancellationToken::new()).await.unwrap();
    sdk.shutdown().await.unwrap();
}
#[tokio::test]
async fn launched_handoff_rejects_future_prompts_tools_configuration_and_new_sessions() {
    let f = Fixture::new();
    let sdk = f
        .sdk(Network::new(), Arc::new(Launcher::default()), None)
        .await;
    sdk.updates.check(CancellationToken::new()).await.unwrap();
    sdk.updates.prepare(CancellationToken::new()).await.unwrap();
    sdk.updates
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Tui,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        sdk.created
            .session
            .prompt("must not become a model request", None)
            .await
            .is_err()
    );
    assert!(sdk.created.session.fatal_runtime_error().is_none());
    assert!(sdk.created.session.prompt(" /update", None).await.is_err());
    assert!(
        sdk.created
            .session
            .prompt(
                "/update",
                Some(pi_session::PromptOptions {
                    expand_prompt_templates: Some(false),
                    ..Default::default()
                })
            )
            .await
            .is_err()
    );
    assert!(
        Kit::new(&f.0)
            .unwrap()
            .session_options(Default::default())
            .is_err()
    );
    let store = cst_pilot::runbook::RunbookStore::new(&f.0).unwrap();
    let input=serde_json::from_value(json!({"title":"test","level":"safe","items":[{"shell":"cmd","summary":"test","command":"echo test"}]})).unwrap();
    assert!(store.plan(input, CancellationToken::new()).await.is_err());
    assert!(sdk.auth.logout("deepseek").await.is_err());
    assert_eq!(fs::read(f.0.join("cst-pilot.exe")).unwrap(), b"old main");
}
#[tokio::test]
async fn missing_compiled_key_keeps_sdk_usable_and_has_no_update_storage() {
    if option_env!("CST_UPDATE_PUBLIC_KEY").is_some() {
        return;
    }
    let f = Fixture::new();
    let service =
        UpdateService::load(&f.0, "https://www.cstoa.top", UpdateOptions::Compiled).unwrap();
    assert_eq!(service.status().phase, Phase::Unavailable);
    assert!(service.check(CancellationToken::new()).await.is_err());
    assert!(!f.0.join(".state/update").exists());
    let sdk = f
        .sdk(Network::new(), Arc::new(Launcher::default()), None)
        .await;
    sdk.created
        .session
        .prompt("ordinary prompt", None)
        .await
        .unwrap();
    sdk.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancel_check_and_download_leave_root_unchanged_and_mask_transport_errors() {
    let f = Fixture::new();
    let net = Network::new();
    net.fail.store(true, Ordering::SeqCst);
    let service = f.service(net.clone(), Arc::new(Launcher::default()));
    assert!(
        !service
            .check(CancellationToken::new())
            .await
            .unwrap_err()
            .contains("synthetic-secret")
    );
    net.fail.store(false, Ordering::SeqCst);
    service.check(CancellationToken::new()).await.unwrap();
    net.block.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let operation = {
        let service = service.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { service.prepare(cancel).await })
    };
    until(|| net.downloads.load(Ordering::SeqCst) > 0).await;
    cancel.cancel();
    assert!(operation.await.unwrap().is_err());
    assert_eq!(
        fs::read(f.0.join("VERSION")).unwrap(),
        b"cst-pilot v0.6.0\n"
    );
    assert!(service.gate.enter().is_ok());
}
#[tokio::test]
async fn startup_marker_blocks_resource_and_session_start_without_recovering_live_program() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join(".state/update")).unwrap();
    fs::write(
        f.0.join(".state/update/transaction.json"),
        b"synthetic incomplete transaction",
    )
    .unwrap();
    assert!(startup_check(&f.0).is_err());
    assert!(
        Kit::new(&f.0)
            .unwrap()
            .session_options(Default::default())
            .is_err()
    );
    assert_eq!(
        fs::read(f.0.join(".state/update/transaction.json")).unwrap(),
        b"synthetic incomplete transaction"
    );
    assert_eq!(fs::read(f.0.join("cst-pilot.exe")).unwrap(), b"old main");
}
#[tokio::test]
async fn concurrent_handoff_and_dropped_waiter_launch_only_once_and_retain_exit_receipt() {
    let f = Fixture::new();
    let launcher = Arc::new(Launcher::default());
    *launcher.delay.lock().unwrap() = Duration::from_millis(180);
    let service = f.service(Network::new(), launcher.clone());
    service.check(CancellationToken::new()).await.unwrap();
    service.prepare(CancellationToken::new()).await.unwrap();
    let task = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .launch(
                    vec![ProcessIdentity::current().unwrap()],
                    RestartUi::Tui,
                    CancellationToken::new(),
                )
                .await
        })
    };
    until(|| launcher.calls.load(Ordering::SeqCst) > 0).await;
    task.abort();
    assert!(
        service
            .launch(
                vec![ProcessIdentity::current().unwrap()],
                RestartUi::Tui,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    until(|| service.status().phase == Phase::ExitRequired).await;
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 1);
    assert!(service.status().launch.is_some());
    assert!(service.gate.check().is_err());
}
#[tokio::test]
async fn invalid_parent_never_closes_telemetry_or_launches() {
    let f = Fixture::new();
    let launcher = Arc::new(Launcher::default());
    let service = f.service(Network::new(), launcher.clone());
    service.check(CancellationToken::new()).await.unwrap();
    service.prepare(CancellationToken::new()).await.unwrap();
    assert!(
        service
            .launch(
                vec![ProcessIdentity {
                    pid: std::process::id(),
                    created: 1
                }],
                RestartUi::Tui,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
    assert!(service.gate.enter().is_ok());
}
#[tokio::test]
async fn per_home_gate_keeps_independent_kits_usable_and_releases_on_drop() {
    let a = Fixture::new();
    let b = Fixture::new();
    let gate = ActivityGate::for_root(&a.0).unwrap();
    let same = ActivityGate::for_root(&a.0).unwrap();
    assert!(Arc::ptr_eq(&gate, &same));
    let close = gate.close().unwrap();
    assert!(same.enter().is_err());
    assert!(ActivityGate::for_root(&b.0).unwrap().enter().is_ok());
    drop(close);
    assert!(gate.enter().is_ok());
}
#[tokio::test]
async fn update_install_requires_ui_confirmation_and_cancellation_preserves_tree() {
    let f = Fixture::new();
    let launcher = Arc::new(Launcher::default());
    let (ui, mut requests) = ChannelUiContext::pair();
    let sdk = f
        .sdk(Network::new(), launcher.clone(), Some(Arc::new(ui)))
        .await;
    let session = sdk.created.session.clone();
    let task = tokio::spawn(async move { session.prompt("/update install", None).await });
    match tokio::time::timeout(Duration::from_secs(4), requests.recv())
        .await
        .unwrap()
        .unwrap()
    {
        UiPromptRequest::Confirm { respond, .. } => {
            respond.send(false).unwrap();
        }
        _ => panic!("expected confirmation"),
    }
    task.await.unwrap().unwrap();
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
    assert!(!f.0.join(".state/update/staging").exists());
    sdk.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_sdk_run_refuses_update_without_consuming_prepared_package() {
    let f = Fixture::new();
    let launcher = Arc::new(Launcher::default());
    let sdk = f.sdk(Network::new(), launcher.clone(), None).await;
    sdk.updates.check(CancellationToken::new()).await.unwrap();
    sdk.updates.prepare(CancellationToken::new()).await.unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let running = started.clone();
    let faux = create_faux_provider();
    faux.set_responses(vec![FauxResponseStep::Dynamic(Arc::new(move |_, _, _| {
        running.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(400));
        faux_text_message("finished")
    }))]);
    sdk.auth.runtime().register_provider(faux.provider);
    sdk.created.session.set_model(faux.model).await.unwrap();
    let session = sdk.created.session.clone();
    let run = tokio::spawn(async move { session.prompt("do work", None).await });
    until(|| started.load(Ordering::SeqCst)).await;
    let error = sdk
        .updates
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Tui,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.contains("会话"));
    assert_eq!(sdk.updates.status().phase, Phase::Ready);
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
    run.await.unwrap().unwrap();
    sdk.updates
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Tui,
            CancellationToken::new(),
        )
        .await
        .unwrap();
}

struct NoProbe;
impl cst_pilot::telemetry::environment::EnvironmentSource for NoProbe {
    fn probe(&self) -> cst_pilot::telemetry::environment::SystemProbe {
        Default::default()
    }
}
struct BlockUpload;
impl cst_pilot::telemetry::transport::Transport for BlockUpload {
    fn request(
        &self,
        _: cst_pilot::telemetry::transport::Request,
        cancel: CancellationToken,
    ) -> BoxFuture<
        '_,
        Result<
            cst_pilot::telemetry::transport::Response,
            cst_pilot::telemetry::transport::FailureKind,
        >,
    > {
        Box::pin(async move {
            cancel.cancelled().await;
            Err(cst_pilot::telemetry::transport::FailureKind::Timeout)
        })
    }
}
struct SlowBilling;
impl cst_pilot::telemetry::billing::BillingLookup for SlowBilling {
    fn lookup<'a>(
        &'a self,
        _: &'a [String],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
        Box::pin(async move {
            cancel.cancelled().await;
            Err("cancelled".into())
        })
    }
}
#[tokio::test]
async fn update_finalizes_all_home_sessions_with_one_budget_and_does_not_wait_for_upload() {
    use cst_pilot::telemetry::TelemetryService;
    let f = Fixture::new();
    fs::write(
        f.0.join("agent/home/telemetry.json"),
        r#"{"enabled":true,"endpoints":["http://127.0.0.1:9411/telemetry"]}"#,
    )
    .unwrap();
    let telemetry = TelemetryService::load(
        &f.0,
        Arc::new(pi_ai::InMemoryCredentialStore::default()),
        Arc::new(SlowBilling),
        TelemetryOptions {
            environment: Arc::new(NoProbe),
            transport: Arc::new(BlockUpload),
            clock: Arc::new(cst_pilot::auth::oa::SystemClock),
            kit_version: "synthetic".into(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let launcher = Arc::new(Launcher::default());
    let a = f.sdk(Network::new(), launcher.clone(), None).await;
    let b = f.sdk(Network::new(), launcher.clone(), None).await;
    assert!(Arc::ptr_eq(&a.telemetry().unwrap(), &telemetry));
    assert!(Arc::ptr_eq(&a.updates, &b.updates));
    for id in ["one", "two"] {
        let record = telemetry.instance(id, "new", "web");
        record.update(|c| {
            c.input("rpc", pi_ai::now_ms());
            c.before_request("cstoa", "", pi_ai::now_ms());
            c.after_response(
                200,
                &BTreeMap::from([("x-oneapi-request-id".into(), id.into())]),
                pi_ai::now_ms(),
            );
        });
    }
    a.updates.check(CancellationToken::new()).await.unwrap();
    a.updates.prepare(CancellationToken::new()).await.unwrap();
    launcher.fail.store(true, Ordering::SeqCst);
    assert!(
        a.updates
            .launch(
                vec![ProcessIdentity::current().unwrap()],
                RestartUi::Web,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(telemetry.active_instances() >= 2);
    launcher.fail.store(false, Ordering::SeqCst);
    a.updates.prepare(CancellationToken::new()).await.unwrap();
    let start = tokio::time::Instant::now();
    a.updates
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Web,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_millis(5500));
    telemetry.drain().await;
    assert_eq!(telemetry.active_instances(), 0);
    let outbox = cst_pilot::telemetry::outbox::Outbox::new(
        &f.0,
        cst_pilot::telemetry::owner::Owner::global().unwrap(),
    )
    .unwrap();
    assert_eq!(
        outbox
            .read_all(Some("http://127.0.0.1:9411/telemetry"))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(a.updates.status().phase, Phase::ExitRequired);
}

#[tokio::test]
async fn confirmed_update_command_uses_verified_handoff_and_requests_orderly_shutdown() {
    let f = Fixture::new();
    let launcher = Arc::new(Launcher::default());
    let (ui, mut requests) = ChannelUiContext::pair();
    let sdk = f
        .sdk(Network::new(), launcher.clone(), Some(Arc::new(ui)))
        .await;
    let session = sdk.created.session.clone();
    let command = tokio::spawn(async move { session.prompt("/update install", None).await });
    match tokio::time::timeout(Duration::from_secs(4), requests.recv())
        .await
        .unwrap()
        .unwrap()
    {
        UiPromptRequest::Confirm { respond, .. } => {
            respond.send(true).unwrap();
        }
        _ => panic!("expected confirmation"),
    }
    command.await.unwrap().unwrap();
    until(|| sdk.updates.status().phase == Phase::ExitRequired).await;
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 1);
    assert!(sdk.updates.gate.check().is_err());
    assert_eq!(fs::read(f.0.join("cst-pilot.exe")).unwrap(), b"old main");
}

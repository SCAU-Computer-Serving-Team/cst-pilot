#![cfg(windows)]

use base64::{Engine, engine::general_purpose::STANDARD};
use cst_kit::{
    Kit, KitSession, SKILL_NAMES,
    auth::{AuthRuntime, Channel, oa::OaClient},
    telemetry::TelemetryOptions,
    update::{
        UpdateClient,
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
        Arc, Condvar, Mutex,
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
        fs::create_dir_all(path.join("app/prompts")).unwrap();
        fs::create_dir_all(path.join("data/config")).unwrap();
        fs::write(path.join("VERSION"), "cst-pilot v0.6.0\n").unwrap();
        fs::write(path.join("cst-pilot.exe"), "old main").unwrap();
        fs::write(
            path.join("data/config/telemetry.json"),
            r#"{"enabled":false}"#,
        )
        .unwrap();
        fs::write(
            path.join("app/prompts/APPEND_SYSTEM.md"),
            "Synthetic instructions",
        )
        .unwrap();
        for name in SKILL_NAMES {
            let dir = path.join(format!("app/skills/{name}"));
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
        let kit = Kit::new(&self.0).unwrap();
        UpdateService::new(
            &self.0,
            "https://www.cstoa.top",
            kit.activity_gate().clone(),
            UpdateOptions::Trusted {
                client: Arc::new(self.client(net)),
                launcher,
            },
        )
        .unwrap()
    }
    async fn host(&self, net: Arc<Network>, launcher: Arc<Launcher>) -> cst_kit::KitHost {
        let kit = Kit::new(&self.0).unwrap();
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
        auth.runtime().register_provider(faux.provider.clone());
        cst_kit::KitHost::new(
            kit,
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
fn mock_sfnt(magic: &[u8; 4]) -> Vec<u8> {
    let tables = [*b"cmap", *b"name"];
    let header_len = 12 + tables.len() * 16;
    let mut buf = vec![0u8; header_len + 8];
    buf[..4].copy_from_slice(magic);
    buf[4..6].copy_from_slice(&(tables.len() as u16).to_be_bytes());
    for (idx, tag) in tables.iter().enumerate() {
        let entry = 12 + idx * 16;
        buf[entry..entry + 4].copy_from_slice(tag);
        let offset = (header_len + idx * 4) as u32;
        buf[entry + 8..entry + 12].copy_from_slice(&offset.to_be_bytes());
        buf[entry + 12..entry + 16].copy_from_slice(&4u32.to_be_bytes());
    }
    buf
}

impl Network {
    fn new() -> Arc<Self> {
        let mut files = BTreeMap::from([
            ("cst-pilot.exe".to_owned(), b"new synthetic main".to_vec()),
            (
                "app/cst-server.exe".into(),
                b"new synthetic server".to_vec(),
            ),
            (
                "app/cst-updater.exe".into(),
                b"not a real executable".to_vec(),
            ),
            ("VERSION".into(), b"cst-pilot v0.7.0\n".to_vec()),
            (
                "app/BUILD-INFO.json".into(),
                br#"{"version":"v0.7.0"}"#.to_vec(),
            ),
            ("LICENSE".into(), b"MIT".to_vec()),
            ("THIRD-PARTY-NOTICES.md".into(), b"notices".to_vec()),
            ("app/licenses/mock.txt".into(), b"mock notice".to_vec()),
            (
                "app/fonts/SourceHanSansCN-VF.otf".into(),
                mock_sfnt(b"OTTO"),
            ),
            ("app/fonts/LICENSE.txt".into(), b"SIL OFL 1.1".to_vec()),
            (
                "app/fonts/JetBrainsMono-Regular.ttf".into(),
                mock_sfnt(&[0, 1, 0, 0]),
            ),
            (
                "app/fonts/JetBrainsMono-Medium.ttf".into(),
                mock_sfnt(&[0, 1, 0, 0]),
            ),
            (
                "app/fonts/JetBrainsMono-OFL.txt".into(),
                b"SIL OFL 1.1".to_vec(),
            ),
            (
                "app/prompts/APPEND_SYSTEM.md".into(),
                b"Synthetic instructions".to_vec(),
            ),
            (
                "app/telemetry/timserver_1.crt".into(),
                b"synthetic public certificate".to_vec(),
            ),
            ("app/telemetry/currency.json".into(), b"{}".to_vec()),
            ("app/defaults/settings.json".into(), b"{}".to_vec()),
            ("app/defaults/models-store.json".into(), b"{}".to_vec()),
            ("app/defaults/telemetry.json".into(), b"{}".to_vec()),
            ("app/defaults/web-search.json".into(), b"{}".to_vec()),
        ]);
        for name in SKILL_NAMES {
            files.insert(
                format!("app/skills/{name}/SKILL.md"),
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
        zip.start_file("cst-pilot/app/SHA256SUMS", SimpleFileOptions::default())
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
    launched_at: Mutex<Option<std::time::Instant>>,
}
impl HandoffLauncher for Launcher {
    fn launch(&self, handoff: Handoff) -> Result<LaunchReceipt, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(*self.delay.lock().unwrap());
        if self.fail.load(Ordering::SeqCst) {
            return Err("synthetic-launch-secret".into());
        }
        *self.launched_at.lock().unwrap() = Some(std::time::Instant::now());
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
    // The mocked fetch never resolves without cancellation. Creating sessions must not await it.
    let (a, b) = tokio::time::timeout(Duration::from_secs(15), async {
        let host = f.host(net.clone(), Arc::new(Launcher::default())).await;
        let a = host
            .open_session(CreateAgentSessionOptions {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        let b = host
            .open_session(CreateAgentSessionOptions::default())
            .await
            .unwrap();
        (a, b)
    })
    .await
    .expect("session initialization must not wait for background update check");
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
    let volume = cst_kit::bitlocker::lock::VolumeLock::acquire("SYNTHETIC-UPDATE-VOLUME").unwrap();
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
    let store = cst_kit::runbook::RunbookStore::new(&f.0).unwrap();
    let input=serde_json::from_value(json!({"title":"test","level":"safe","items":[{"shell":"cmd","summary":"test","command":"echo test"}]})).unwrap();
    assert!(store.plan(input, CancellationToken::new()).await.is_err());
    assert!(sdk.auth.logout("deepseek").await.is_err());
    assert_eq!(fs::read(f.0.join("cst-pilot.exe")).unwrap(), b"old main");
}
#[tokio::test]
async fn missing_compiled_key_keeps_sdk_usable_and_has_no_update_storage() {
    if option_env!("CST_UPDATE_PUBLIC_KEY").is_some()
        && option_env!("CST_UPDATE_MANIFEST_URL").is_some()
    {
        return;
    }
    let f = Fixture::new();
    let kit = Kit::new(&f.0).unwrap();
    let service = UpdateService::new(
        &f.0,
        "https://www.cstoa.top",
        kit.activity_gate().clone(),
        UpdateOptions::Compiled,
    )
    .unwrap();
    assert_eq!(service.status().phase, Phase::Unavailable);
    assert_eq!(service.status().message, "当前构建未启用在线更新。");
    assert_eq!(
        service
            .check_for_app(CancellationToken::new())
            .await
            .unwrap()
            .phase,
        Phase::Unavailable
    );
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
    let gate = Kit::new(&a.0).unwrap().activity_gate().clone();
    let same = Kit::new(&a.0).unwrap().activity_gate().clone();
    assert!(Arc::ptr_eq(&gate, &same));
    let close = gate.close().unwrap();
    assert!(same.enter().is_err());
    assert!(Kit::new(&b.0).unwrap().activity_gate().enter().is_ok());
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
    let finish = Arc::new((Mutex::new(false), Condvar::new()));
    let provider_finish = finish.clone();
    let faux = create_faux_provider();
    faux.set_responses(vec![FauxResponseStep::Dynamic(Arc::new(move |_, _, _| {
        let (released, cv) = &*provider_finish;
        running.store(true, Ordering::SeqCst);
        let (_guard, wait) = cv
            .wait_timeout_while(
                released.lock().unwrap(),
                Duration::from_secs(15),
                |released| !*released,
            )
            .unwrap();
        assert!(
            !wait.timed_out(),
            "test did not release the mocked provider"
        );
        faux_text_message("finished")
    }))]);
    sdk.auth.runtime().register_provider(faux.provider);
    sdk.created.session.set_model(faux.model).await.unwrap();
    let session = sdk.created.session.clone();
    let run = tokio::spawn(async move { session.prompt("do work", None).await });
    until(|| started.load(Ordering::SeqCst)).await;
    let result = sdk
        .updates
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Tui,
            CancellationToken::new(),
        )
        .await;
    *finish.0.lock().unwrap() = true;
    finish.1.notify_one();
    assert!(result.unwrap_err().contains("会话"));
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
impl cst_kit::telemetry::environment::EnvironmentSource for NoProbe {
    fn probe(&self) -> cst_kit::telemetry::environment::SystemProbe {
        Default::default()
    }
}
struct BlockUpload;
impl cst_kit::telemetry::transport::Transport for BlockUpload {
    fn request(
        &self,
        _: cst_kit::telemetry::transport::Request,
        cancel: CancellationToken,
    ) -> BoxFuture<
        '_,
        Result<cst_kit::telemetry::transport::Response, cst_kit::telemetry::transport::FailureKind>,
    > {
        Box::pin(async move {
            cancel.cancelled().await;
            Err(cst_kit::telemetry::transport::FailureKind::Timeout)
        })
    }
}
struct SlowBilling;
impl cst_kit::telemetry::billing::BillingLookup for SlowBilling {
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
    use cst_kit::telemetry::TelemetryService;
    let f = Fixture::new();
    fs::write(
        f.0.join("data/config/telemetry.json"),
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
            clock: Arc::new(cst_kit::auth::oa::SystemClock),
            kit_version: "synthetic".into(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let launcher = Arc::new(Launcher::default());
    let host = f
        .host(Network::new(), launcher.clone())
        .await
        .with_telemetry(Some(telemetry.clone()));
    let a = host
        .open_session(CreateAgentSessionOptions::default())
        .await
        .unwrap();
    let b = host
        .open_session(CreateAgentSessionOptions::default())
        .await
        .unwrap();
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
    a.updates
        .launch(
            vec![ProcessIdentity::current().unwrap()],
            RestartUi::Web,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let launched_at = launcher.launched_at.lock().unwrap().unwrap();
    assert!(launched_at.elapsed() < Duration::from_millis(5500));
    telemetry.drain().await;
    assert_eq!(telemetry.active_instances(), 0);
    let outbox = cst_kit::telemetry::outbox::Outbox::new(
        &f.0,
        cst_kit::telemetry::owner::Owner::global().unwrap(),
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

#[tokio::test]
async fn same_root_activity_gate_is_shared_isolated_across_roots_and_blocks_writes_during_handoff()
{
    let a = Fixture::new();
    let b = Fixture::new();

    let kit_a1 = Kit::new(&a.0).unwrap();
    let kit_a2 = Kit::new(a.0.join(".")).unwrap();
    let kit_b = Kit::new(&b.0).unwrap();

    // Same normalized root shares one ActivityGate instance; different roots are isolated.
    assert!(Arc::ptr_eq(kit_a1.activity_gate(), kit_a2.activity_gate()));
    assert!(!Arc::ptr_eq(kit_a1.activity_gate(), kit_b.activity_gate()));

    // An active business write lease on kit_a2 prevents kit_a1 from closing for upgrade handoff.
    let active_write = kit_a2.activity_gate().enter().unwrap();
    assert!(kit_a1.activity_gate().close().is_err());
    drop(active_write);

    // Closing the gate via kit_a1 immediately blocks kit_a2 session startup and auth/runbook writes.
    let upgrade_lease = kit_a1.activity_gate().close().unwrap();
    assert!(kit_a2.activity_gate().check().is_err());
    assert!(kit_b.activity_gate().check().is_ok());
    assert!(
        kit_a2
            .session_options(CreateAgentSessionOptions::default())
            .is_err()
    );
    let auth_a2 = AuthRuntime::create(
        &a.0,
        Channel::Gui,
        OaClient::new("http://127.0.0.1:9").unwrap(),
    )
    .await
    .unwrap();
    assert!(auth_a2.logout("deepseek").await.is_err());
    let runbook_a2 = cst_kit::runbook::RunbookStore::new(&a.0).unwrap();
    assert!(
        runbook_a2
            .plan(
                cst_kit::runbook::model::PlanInput {
                    title: "blocked-during-handoff".into(),
                    level: cst_kit::runbook::model::Level::Safe,
                    items: vec![cst_kit::runbook::model::RunbookItem {
                        summary: "step".into(),
                        command: "echo hi".into(),
                        shell: cst_kit::runbook::model::Shell::Cmd,
                        admin: false,
                    }],
                    date: None,
                    runbook_id: None,
                    base_version: None,
                    export_txt: true,
                },
                CancellationToken::new(),
            )
            .await
            .is_err()
    );

    // Distinct root B continues working unaffected.
    assert!(
        kit_b
            .session_options(CreateAgentSessionOptions::default())
            .is_ok()
    );

    // Dropping the upgrade lease restores normal writes on root A.
    drop(upgrade_lease);
    assert!(
        kit_a2
            .session_options(CreateAgentSessionOptions::default())
            .is_ok()
    );

    // Two distinct `KitHost` instances targeting root A share the same update participant registry,
    // and `with_provider_endpoint_checked` holds an unbroken `ActivityLease` across write + login + rollback.
    let host_a1 = a.host(Network::new(), Arc::new(Launcher::default())).await;
    let host_a2 = a.host(Network::new(), Arc::new(Launcher::default())).await;
    host_a2
        .updates()
        .check(CancellationToken::new())
        .await
        .unwrap();
    host_a2
        .updates()
        .prepare(CancellationToken::new())
        .await
        .unwrap();

    // Verify `with_provider_endpoint_checked` keeps `ActivityGate` leased while `login` closure runs,
    // and rolls back cleanly even when `login` fails.
    auth_a2
        .save_custom_provider_checked(cst_kit::auth::custom_provider::CustomProviderInput {
            id: "ep-test".into(),
            name: "EP Test".into(),
            base_url: "https://old.example/v1".into(),
            api: cst_kit::auth::custom_provider::CustomApi::OpenaiCompletions,
            api_key: Some("sk-initial".into()),
            models: vec![cst_kit::auth::custom_provider::CustomModel {
                id: "m1".into(),
                name: "M1".into(),
                context_window: 32000,
                max_tokens: 4096,
                reasoning: false,
                image: false,
            }],
            revision: None,
        })
        .await
        .unwrap();
    let gate_check = host_a2.kit().activity_gate().clone();
    let ep_err = auth_a2
        .with_provider_endpoint_checked("ep-test", "https://new.example/v1", || async move {
            assert!(
                gate_check.close().is_err(),
                "ActivityGate must stay leased across with_provider_endpoint_checked login/rollback window"
            );
            Err::<(), String>("simulated login failure".into())
        })
        .await
        .unwrap_err();
    assert!(ep_err.message.contains("simulated login failure"));
    let rolled_back = auth_a2.custom_provider_checked("ep-test").unwrap().unwrap();
    assert_eq!(rolled_back["baseUrl"], "https://old.example/v1");

    // Queue a pending follow-up message on a session opened via `host_a1`; `host_a2.updates().launch(...)`
    // must observe `host_a1`'s session participant and refuse handoff.
    let sess_a1 = host_a1
        .open_session(CreateAgentSessionOptions::default())
        .await
        .unwrap();
    sess_a1
        .session()
        .follow_up("queued work", None)
        .await
        .unwrap();
    assert!(
        host_a2
            .updates()
            .launch(
                vec![ProcessIdentity::current().unwrap()],
                RestartUi::Web,
                CancellationToken::new(),
            )
            .await
            .is_err(),
        "second KitHost on same root must see queued session participant on first KitHost"
    );
    sess_a1.shutdown().await.unwrap();
}

#![cfg(windows)]

use base64::{Engine, engine::general_purpose::STANDARD};
use cst_pilot::update::{
    UpdateClient,
    applier::{Applier, ApplyObserver, ApplyOutcome, ApplyStep, Recovery, UpdateHost},
    handoff::{self, Handoff},
    lock::UpdateLock,
    manifest::{
        Archive, MANIFEST_DOMAIN, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
    },
    network::{Download, Fetcher},
    process::{self, ProcessIdentity, RestartUi},
    verify::VerifiedPackage,
};
use ed25519_dalek::{Signer, SigningKey};
use pi_agent::BoxFuture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zip::{ZipWriter, write::SimpleFileOptions};

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("cst-update-apply-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let f = Self { root };
        for (name, bytes) in [
            ("VERSION", b"cst-pilot v0.6.0\n".as_slice()),
            ("BUILD-INFO.json", br#"{"version":"v0.6.0"}"#),
            ("cst-pilot.exe", b"old main"),
            ("cst-updater.exe", b"old updater"),
            ("assets/old-only.txt", b"obsolete resource"),
            (
                "agent/home/settings.json",
                br#"{"existing":false,"theme":"dark"}"#,
            ),
            (
                "agent/home/auth.json",
                br#"{"cstoa":{"type":"api_key","key":"synthetic-local-secret"}}"#,
            ),
            (
                "agent/home/sessions/session.jsonl",
                b"synthetic session data\n",
            ),
            ("artifacts/2026-11-09/runbook/01.txt", b"user command text"),
            ("wiztree/tmp/old.csv", b"scan cache"),
            ("wiztree/WizTree3.ini", b"user settings"),
            ("user-note.txt", b"unrelated user file"),
        ] {
            f.write(name, bytes)
        }
        f
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.root.join(name)).unwrap()
    }
    fn users(&self) -> BTreeMap<String, Vec<u8>> {
        [
            "agent/home/auth.json",
            "agent/home/sessions/session.jsonl",
            "artifacts/2026-11-09/runbook/01.txt",
            "wiztree/tmp/old.csv",
            "wiztree/WizTree3.ini",
            "user-note.txt",
        ]
        .into_iter()
        .map(|n| (n.into(), self.read(n)))
        .collect()
    }
    fn old_program(&self) -> BTreeMap<String, Vec<u8>> {
        [
            "VERSION",
            "BUILD-INFO.json",
            "cst-pilot.exe",
            "cst-updater.exe",
            "assets/old-only.txt",
            "agent/home/settings.json",
        ]
        .into_iter()
        .map(|n| (n.into(), self.read(n)))
        .collect()
    }
    async fn package(&self) -> VerifiedPackage {
        self.package_with(files()).await
    }
    async fn package_with(&self, files: BTreeMap<String, Vec<u8>>) -> VerifiedPackage {
        let (envelope, zip) = bundle(files);
        let fetch = Arc::new(Mock { envelope, zip });
        let client =
            UpdateClient::with_fetcher("https://updates.example/latest", key(), fetch).unwrap();
        let release = client
            .check("v0.6.0", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        client
            .prepare(&self.root, &release, CancellationToken::new())
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn key() -> ReleaseKey {
    ReleaseKey::from_bytes(SigningKey::from_bytes(&[42; 32]).verifying_key().as_bytes()).unwrap()
}
fn files() -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::from([
        ("cst-pilot.exe".into(), b"new main".to_vec()),
        ("cst-updater.exe".into(), b"new updater".to_vec()),
        ("VERSION".into(), b"cst-pilot v0.7.0\n".to_vec()),
        (
            "BUILD-INFO.json".into(),
            br#"{"version":"v0.7.0"}"#.to_vec(),
        ),
        ("LICENSE".into(), b"MIT".to_vec()),
        ("THIRD-PARTY-NOTICES.md".into(), b"license notices".to_vec()),
        ("licenses/mock.txt".into(), b"mock third party".to_vec()),
        ("assets/new-only.txt".into(), b"new resource".to_vec()),
        (
            "agent/home/APPEND_SYSTEM.md".into(),
            b"synthetic prompt".to_vec(),
        ),
        (
            "agent/home/extensions/telemetry/timserver_1.crt".into(),
            b"synthetic public cert".to_vec(),
        ),
        (
            "agent/home/settings.json".into(),
            br#"{"existing":true,"newOption":1}"#.to_vec(),
        ),
    ]);
    for skill in cst_pilot::SKILL_NAMES {
        files.insert(
            format!("agent/home/skills/{skill}/SKILL.md"),
            format!("---\nname: {skill}\ndescription: synthetic fixture\n---\nInstructions")
                .into_bytes(),
        );
    }
    files
}
fn bundle(files: BTreeMap<String, Vec<u8>>) -> (Vec<u8>, Vec<u8>) {
    let key = SigningKey::from_bytes(&[42; 32]);
    let sign = |domain: &[u8], bytes: &[u8]| {
        STANDARD.encode(key.sign(&[domain, bytes].concat()).to_bytes())
    };
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
    let zip = zip.finish().unwrap().into_inner();
    let manifest = ReleaseManifest {
        schema_version: 1,
        platform: "windows-x86_64".into(),
        version: "v0.7.0".into(),
        built_at: "2026-11-09T00:00:00Z".into(),
        min_upgradable_version: "v0.6.0".into(),
        release_notes: "synthetic update".into(),
        archive: Archive {
            url: "https://updates.example/update.zip".into(),
            size: zip.len() as u64,
            unpacked_size: files.values().map(|b| b.len() as u64).sum::<u64>() + sums.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&zip)),
        },
        sha256_signature: sign(SUMS_DOMAIN, sums.as_bytes()),
        sha256_sums: sums,
    };
    let payload = serde_json::to_string(&manifest).unwrap();
    let envelope = serde_json::to_vec(&SignedManifest {
        signature: sign(MANIFEST_DOMAIN, payload.as_bytes()),
        payload,
    })
    .unwrap();
    (envelope, zip)
}
struct Mock {
    envelope: Vec<u8>,
    zip: Vec<u8>,
}
impl Fetcher for Mock {
    fn get(
        &self,
        url: String,
        _: u64,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, String>> {
        let bytes = if url.ends_with("/latest") {
            self.envelope.clone()
        } else {
            self.zip.clone()
        };
        Box::pin(async move {
            Ok(Download {
                body: Box::pin(futures_util::stream::iter(vec![Ok(bytes)])),
            })
        })
    }
}
#[derive(Default)]
struct Host {
    checks: AtomicUsize,
    restarts: AtomicUsize,
    fail: bool,
    restart_fail: bool,
}
impl UpdateHost for Host {
    fn health_check(&self, root: &Path, cancel: &CancellationToken) -> Result<(), String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        assert!(!cancel.is_cancelled());
        assert_eq!(
            fs::read(root.join("VERSION")).unwrap(),
            b"cst-pilot v0.7.0\n"
        );
        if self.fail {
            Err("synthetic health failure".into())
        } else {
            Ok(())
        }
    }
    fn restart(&self, _: &Path) -> Result<(), String> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        if self.restart_fail {
            Err("synthetic launch failure".into())
        } else {
            Ok(())
        }
    }
}
struct Crash(ApplyStep);
impl ApplyObserver for Crash {
    fn reached(&self, step: ApplyStep) -> Result<(), String> {
        if step == self.0 {
            panic!("synthetic power loss")
        };
        Ok(())
    }
}
struct Fail(ApplyStep);
impl ApplyObserver for Fail {
    fn reached(&self, step: ApplyStep) -> Result<(), String> {
        if step == self.0 {
            Err("synthetic IO failure".into())
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn replaces_program_merges_defaults_preserves_users_and_removes_obsolete_resources() {
    let f = Fixture::new();
    let users = f.users();
    let package = f.package().await;
    let stage = package.directory().parent().unwrap().to_owned();
    let host = Host::default();
    let applier = Applier::new(&f.root, key()).unwrap();
    assert_eq!(
        applier
            .apply(package, &host, &CancellationToken::new())
            .unwrap(),
        ApplyOutcome::Updated { restarted: true }
    );
    assert_eq!(host.checks.load(Ordering::SeqCst), 1);
    assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
    assert_eq!(f.read("cst-pilot.exe"), b"new main");
    assert!(!f.root.join("assets/old-only.txt").exists());
    assert_eq!(users, f.users());
    let settings: Value = serde_json::from_slice(&f.read("agent/home/settings.json")).unwrap();
    assert_eq!(
        settings,
        json!({"existing":false,"theme":"dark","newOption":1})
    );
    assert!(!f.root.join(".state/update/transaction.json").exists());
    assert!(!stage.exists());
    assert_eq!(applier.recover().unwrap(), Recovery::None);
}
#[tokio::test]
async fn health_failure_rolls_back_every_program_and_config_without_touching_users() {
    let f = Fixture::new();
    let old = f.old_program();
    let users = f.users();
    let package = f.package().await;
    let host = Host {
        fail: true,
        ..Default::default()
    };
    assert_eq!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply(package, &host, &CancellationToken::new())
            .unwrap(),
        ApplyOutcome::RolledBack
    );
    assert_eq!(old, f.old_program());
    assert_eq!(users, f.users());
    assert!(!f.root.join("assets/new-only.txt").exists());
    assert_eq!(host.restarts.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn every_rename_boundary_recovers_after_a_simulated_process_crash() {
    // Count actual changed entries rather than relying on their lexical order.
    let fixture = Fixture::new();
    let mut package = fixture.package().await;
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Applier::new(&fixture.root, key()).unwrap().apply_observed(
            &mut package,
            &Host::default(),
            &Crash(ApplyStep::JournalSaved),
            &CancellationToken::new(),
        )
    }));
    assert!(crash.is_err());
    let value: Value =
        serde_json::from_slice(&fixture.read(".state/update/transaction.json")).unwrap();
    let count = value["entries"].as_array().unwrap().len();
    for step in std::iter::once(ApplyStep::JournalSaved)
        .chain((0..count).flat_map(|i| [ApplyStep::BackedUp(i), ApplyStep::Installed(i)]))
        .chain([ApplyStep::Healthy])
    {
        let f = Fixture::new();
        let old = f.old_program();
        let users = f.users();
        let mut package = f.package().await;
        let applier = Applier::new(&f.root, key()).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            applier.apply_observed(
                &mut package,
                &Host::default(),
                &Crash(step),
                &CancellationToken::new(),
            )
        }));
        assert!(result.is_err(), "{step:?}");
        drop(applier);
        assert_eq!(
            Applier::new(&f.root, key()).unwrap().recover().unwrap(),
            Recovery::RolledBack,
            "{step:?}"
        );
        assert_eq!(old, f.old_program(), "{step:?}");
        assert_eq!(users, f.users());
        assert!(!f.root.join("assets/new-only.txt").exists());
    }
}
#[tokio::test]
async fn committed_transaction_cleanup_never_rolls_back_a_healthy_version() {
    let f = Fixture::new();
    let users = f.users();
    let mut package = f.package().await;
    let host = Host::default();
    let applier = Applier::new(&f.root, key()).unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| applier.apply_observed(
            &mut package,
            &host,
            &Crash(ApplyStep::Committed),
            &CancellationToken::new()
        )))
        .is_err()
    );
    drop(applier);
    assert_eq!(
        Applier::new(&f.root, key()).unwrap().recover().unwrap(),
        Recovery::CompletedCleanup
    );
    assert_eq!(f.read("cst-pilot.exe"), b"new main");
    assert_eq!(f.users(), users);
    assert_eq!(host.restarts.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn caller_cancellation_still_restores_old_files_and_does_not_cancel_the_parent() {
    struct Cancel(CancellationToken);
    impl ApplyObserver for Cancel {
        fn reached(&self, step: ApplyStep) -> Result<(), String> {
            if step == ApplyStep::Installed(0) {
                self.0.cancel()
            };
            Ok(())
        }
    }
    let f = Fixture::new();
    let old = f.old_program();
    let mut package = f.package().await;
    let parent = CancellationToken::new();
    let child = parent.child_token();
    assert_eq!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply_observed(
                &mut package,
                &Host::default(),
                &Cancel(child.clone()),
                &child
            )
            .unwrap(),
        ApplyOutcome::RolledBack
    );
    assert!(!parent.is_cancelled());
    assert_eq!(old, f.old_program());
}
#[tokio::test]
async fn observer_io_failure_rolls_back_and_restart_failure_leaves_a_healthy_installation() {
    let f = Fixture::new();
    let old = f.old_program();
    let mut package = f.package().await;
    let applier = Applier::new(&f.root, key()).unwrap();
    assert_eq!(
        applier
            .apply_observed(
                &mut package,
                &Host::default(),
                &Fail(ApplyStep::BackedUp(1)),
                &CancellationToken::new()
            )
            .unwrap(),
        ApplyOutcome::RolledBack
    );
    assert_eq!(old, f.old_program());
    let package = f.package().await;
    assert_eq!(
        applier
            .apply(
                package,
                &Host {
                    restart_fail: true,
                    ..Default::default()
                },
                &CancellationToken::new()
            )
            .unwrap(),
        ApplyOutcome::Updated { restarted: false }
    );
    assert_eq!(f.read("cst-pilot.exe"), b"new main");
}
#[tokio::test]
async fn changed_live_file_or_backup_pauses_recovery_without_overwriting_either() {
    for corrupt_backup in [false, true] {
        let f = Fixture::new();
        let mut package = f.package().await;
        let applier = Applier::new(&f.root, key()).unwrap();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| applier.apply_observed(
                &mut package,
                &Host::default(),
                &Crash(ApplyStep::Installed(0)),
                &CancellationToken::new()
            )))
            .is_err()
        );
        drop(applier);
        let journal: Value =
            serde_json::from_slice(&f.read(".state/update/transaction.json")).unwrap();
        let first = journal["entries"][0]["path"].as_str().unwrap();
        let path = if corrupt_backup {
            format!(
                ".state/update/backup/{}/files/{first}",
                journal["id"].as_str().unwrap()
            )
        } else {
            first.into()
        };
        f.write(&path, b"external modification");
        assert!(Applier::new(&f.root, key()).unwrap().recover().is_err());
        assert_eq!(f.read(&path), b"external modification");
        assert!(f.root.join(".state/update/transaction.json").exists());
    }
}
#[tokio::test]
async fn invalid_journal_or_changed_stage_is_rejected_without_program_mutation() {
    let f = Fixture::new();
    let old = f.old_program();
    let package = f.package().await;
    fs::write(package.directory().join("cst-pilot.exe"), b"tampered").unwrap();
    let applier = Applier::new(&f.root, key()).unwrap();
    assert!(
        applier
            .apply(package, &Host::default(), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(old, f.old_program());
    f.write(
        ".state/update/transaction.json",
        br#"{"v":1,"entries":[{"path":"../../victim"}]}"#,
    );
    assert!(applier.recover().is_err());
    assert_eq!(old, f.old_program());
}
#[tokio::test]
async fn corrupted_merge_config_and_hard_links_stop_before_journal_publication() {
    let f = Fixture::new();
    let package = f.package().await;
    f.write("agent/home/settings.json", b"{invalid");
    assert!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply(package, &Host::default(), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(f.read("agent/home/settings.json"), b"{invalid");
    assert!(!f.root.join(".state/update/transaction.json").exists());
    let f = Fixture::new();
    let package = f.package().await;
    fs::hard_link(f.root.join("cst-pilot.exe"), f.root.join("alias.exe")).unwrap();
    assert!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply(package, &Host::default(), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(f.read("cst-pilot.exe"), b"old main");
}
#[test]
fn update_lock_is_kit_scoped_and_os_released() {
    let a = Fixture::new();
    let b = Fixture::new();
    let lock = UpdateLock::acquire(&a.root).unwrap();
    assert!(UpdateLock::acquire(&a.root).is_err());
    assert!(UpdateLock::acquire(&b.root).is_ok());
    drop(lock);
    assert!(UpdateLock::acquire(&a.root).is_ok());
    assert!(
        UpdateLock::address(&a.root)
            .unwrap()
            .starts_with(r"\\.\pipe\cst-pilot-update-")
    );
}
#[tokio::test]
async fn handoff_validates_parent_identity_and_cleans_unlaunched_requests() {
    let f = Fixture::new();
    let package = f.package().await;
    let parents = vec![ProcessIdentity::current().unwrap()];
    let stage = package.directory().parent().unwrap().to_owned();
    let handoff = Handoff::prepare(
        &f.root,
        package,
        &key(),
        parents,
        RestartUi::Web,
        &CancellationToken::new(),
    )
    .unwrap();
    let id = handoff.request_id().to_owned();
    let updater = handoff.updater_path().to_owned();
    assert_eq!(fs::read(&updater).unwrap(), b"new updater");
    assert!(
        f.root
            .join(format!(".state/update/requests/{id}.json"))
            .exists()
    );
    drop(handoff);
    assert!(!stage.exists());
    assert!(!updater.exists());
    assert!(
        !f.root
            .join(format!(".state/update/requests/{id}.json"))
            .exists()
    );
    let package = f.package().await;
    let parent = ProcessIdentity::current().unwrap();
    assert!(
        Handoff::prepare(
            &f.root,
            package,
            &key(),
            vec![ProcessIdentity {
                created: parent.created + 1,
                ..parent
            }],
            RestartUi::Tui,
            &CancellationToken::new()
        )
        .is_err()
    );
}
#[tokio::test]
async fn failed_process_launch_does_not_retain_the_package_or_replace_programs() {
    let f = Fixture::new();
    let old = f.old_program();
    let package = f.package().await;
    let stage = package.directory().parent().unwrap().to_owned();
    let handoff = Handoff::prepare(
        &f.root,
        package,
        &key(),
        vec![ProcessIdentity::current().unwrap()],
        RestartUi::Tui,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(handoff.launch().is_err());
    assert_eq!(old, f.old_program());
    assert!(!stage.exists());
}
#[tokio::test]
async fn local_health_check_has_no_telemetry_credentials_or_network_side_effects() {
    let f = Fixture::new();
    for (name, bytes) in files() {
        f.write(&name, &bytes)
    }
    let users = f.users();
    handoff::health_check(&f.root).unwrap();
    assert_eq!(users, f.users());
    assert!(!f.root.join("agent/home/telemetry/identity.json").exists());
    assert!(!f.root.join("agent/home/device.json").exists());
    f.write("agent/home/auth.json", b"invalid config");
    assert!(handoff::health_check(&f.root).is_err());
    assert_eq!(f.read("agent/home/auth.json"), b"invalid config");
}
#[cfg(windows)]
#[test]
fn parent_wait_uses_creation_identity_and_has_bounded_cancellation() {
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "update_process_child",
            "--nocapture",
        ])
        .env("CST_UPDATE_CHILD", "wait")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let identity = ProcessIdentity::capture(child.id()).unwrap().unwrap();
    let token = CancellationToken::new();
    assert!(
        process::wait_for_parents(
            &[ProcessIdentity {
                created: identity.created + 1,
                ..identity.clone()
            }],
            Duration::from_secs(1),
            &token
        )
        .is_err()
    );
    assert!(
        process::wait_for_parents(
            std::slice::from_ref(&identity),
            Duration::from_millis(40),
            &token
        )
        .is_err()
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        process::wait_for_parents(
            std::slice::from_ref(&identity),
            Duration::from_secs(1),
            &cancel
        )
        .is_err()
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(process::wait_for_parents(&[identity], Duration::from_secs(1), &token).is_ok());
}
#[cfg(windows)]
fn native_test_host() -> PathBuf {
    static HOST: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("cst-update-native-host-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("main.rs");
        fs::write(&source, include_str!("fixtures/update-host.rs")).unwrap();
        let exe = dir.join("cst-pilot.exe");
        let result = std::process::Command::new("rustc")
            .args(["--crate-name", "cst_update_test_host", "--edition=2024"])
            .arg(&source)
            .arg("-o")
            .arg(&exe)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        exe
    })
    .clone()
}
#[cfg(windows)]
#[tokio::test]
async fn real_native_health_process_and_ui_restart_succeed_inside_the_isolated_kit() {
    let f = Fixture::new();
    let mut next = files();
    next.insert(
        "cst-pilot.exe".into(),
        fs::read(native_test_host()).unwrap(),
    );
    let package = f.package_with(next).await;
    let host = process::NativeHost { ui: RestartUi::Web };
    assert_eq!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply(package, &host, &CancellationToken::new())
            .unwrap(),
        ApplyOutcome::Updated { restarted: true }
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !f.root.join(".state/restarted-ui").exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(f.read(".state/health-checked"), b"checked");
    assert_eq!(f.read(".state/restarted-ui"), b"web");
}
#[cfg(windows)]
#[tokio::test]
async fn real_health_failure_and_fifteen_second_timeout_both_roll_back() {
    for mode in ["fail", "hang"] {
        let f = Fixture::new();
        let old = f.old_program();
        let users = f.users();
        f.write(".state/health-mode", mode.as_bytes());
        let mut next = files();
        next.insert(
            "cst-pilot.exe".into(),
            fs::read(native_test_host()).unwrap(),
        );
        let package = f.package_with(next).await;
        let start = std::time::Instant::now();
        assert_eq!(
            Applier::new(&f.root, key())
                .unwrap()
                .apply(
                    package,
                    &process::NativeHost { ui: RestartUi::Tui },
                    &CancellationToken::new()
                )
                .unwrap(),
            ApplyOutcome::RolledBack
        );
        if mode == "hang" {
            assert!(start.elapsed() >= Duration::from_secs(15));
            assert!(start.elapsed() < Duration::from_secs(22));
        }
        assert_eq!(old, f.old_program());
        assert_eq!(users, f.users());
        assert!(!f.root.join(".state/restarted-ui").exists());
    }
}
#[cfg(windows)]
#[test]
fn independent_process_update_lock_is_released_after_termination() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let f = Fixture::new();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "update_process_child",
            "--nocapture",
        ])
        .env("CST_UPDATE_CHILD", "lock")
        .env("CST_UPDATE_CHILD_ROOT", &f.root)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line.contains("update-lock-ready") {
            break;
        }
    }
    assert!(UpdateLock::acquire(&f.root).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(UpdateLock::acquire(&f.root).is_ok());
}
#[cfg(windows)]
#[tokio::test]
async fn transient_windows_file_occupancy_is_retried_without_losing_the_old_program() {
    struct Hold(PathBuf);
    impl ApplyObserver for Hold {
        fn reached(&self, step: ApplyStep) -> Result<(), String> {
            if step == ApplyStep::JournalSaved {
                use std::os::windows::fs::OpenOptionsExt;
                let file = std::fs::OpenOptions::new()
                    .access_mode(0)
                    .share_mode(3)
                    .open(&self.0)
                    .unwrap();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(150));
                    drop(file);
                });
            }
            Ok(())
        }
    }
    let f = Fixture::new();
    let mut package = f.package().await;
    assert_eq!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply_observed(
                &mut package,
                &Host::default(),
                &Hold(f.root.join("BUILD-INFO.json")),
                &CancellationToken::new()
            )
            .unwrap(),
        ApplyOutcome::Updated { restarted: true }
    );
}
#[cfg(windows)]
#[tokio::test]
async fn recovery_rejects_junction_backups_and_signed_journal_path_tampering() {
    let f = Fixture::new();
    let outside = Fixture::new();
    outside.write("sentinel.txt", b"do not touch");
    let old = f.old_program();
    let mut package = f.package().await;
    let applier = Applier::new(&f.root, key()).unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| applier.apply_observed(
            &mut package,
            &Host::default(),
            &Crash(ApplyStep::JournalSaved),
            &CancellationToken::new()
        )))
        .is_err()
    );
    drop(applier);
    let original = f.read(".state/update/transaction.json");
    let mut journal: Value = serde_json::from_slice(&original).unwrap();
    let link = f.root.join(format!(
        ".state/update/backup/{}/files",
        journal["id"].as_str().unwrap()
    ));
    let result = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(link.to_string_lossy().replace('/', "\\"))
        .arg(outside.root.to_string_lossy().replace('/', "\\"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {} exists={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        link.exists()
    );
    assert!(Applier::new(&f.root, key()).unwrap().recover().is_err());
    assert_eq!(outside.read("sentinel.txt"), b"do not touch");
    fs::remove_dir(&link).unwrap();
    journal["entries"][0]["path"] = json!("agent/home/auth.json");
    f.write(
        ".state/update/transaction.json",
        &serde_json::to_vec(&journal).unwrap(),
    );
    assert!(Applier::new(&f.root, key()).unwrap().recover().is_err());
    assert_eq!(old, f.old_program());
    f.write(".state/update/transaction.json", &original);
    let applier = Applier::new(&f.root, key()).unwrap();
    assert_eq!(applier.recover().unwrap(), Recovery::RolledBack);
    assert_eq!(
        applier.report().unwrap().unwrap().reason,
        Some(cst_pilot::update::applier::RollbackReason::Interrupted)
    );
}
#[test]
fn independent_updater_help_and_missing_compiled_key_do_not_touch_a_kit() {
    let f = Fixture::new();
    let old = f.old_program();
    let exe = env!("CARGO_BIN_EXE_cst-updater");
    assert!(
        std::process::Command::new(exe)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
    if ReleaseKey::compiled().is_err() {
        let result = std::process::Command::new(exe)
            .arg("--root")
            .arg(&f.root)
            .arg("--recover")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("未配置官方升级公钥"));
    }
    assert_eq!(old, f.old_program());
    assert!(!f.root.join(".state/update").exists());
}
#[test]
#[ignore = "invoked explicitly by parent process tests"]
fn update_process_child() {
    match std::env::var("CST_UPDATE_CHILD").as_deref() {
        Ok("wait") => std::thread::sleep(Duration::from_secs(60)),
        Ok("lock") => {
            use std::io::Write;
            let _lock = UpdateLock::acquire(Path::new(
                &std::env::var_os("CST_UPDATE_CHILD_ROOT").unwrap(),
            ))
            .unwrap();
            println!("update-lock-ready");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        Ok("fail") => std::process::exit(7),
        _ => (),
    }
}

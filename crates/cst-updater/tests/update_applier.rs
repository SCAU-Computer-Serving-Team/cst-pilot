#![cfg(windows)]

use base64::{Engine, engine::general_purpose::STANDARD};
use cst_os::{ProcessIdentity, RestartUi, RollbackReason};
use cst_updater::{
    applier::{Applier, ApplyObserver, ApplyOutcome, ApplyStep, Recovery, UpdateHost},
    handoff::Handoff,
    layout::REQUIRED_SKILL_NAMES,
    lock::UpdateLock,
    manifest::{
        Archive, MANIFEST_DOMAIN, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
    },
    process,
    verify::VerifiedPackage,
};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
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
            ("app/BUILD-INFO.json", br#"{"version":"v0.6.0"}"#),
            ("cst-pilot.exe", b"old main"),
            ("app/cst-server.exe", b"old server"),
            ("app/cst-updater.exe", b"old updater"),
            ("app/old-only.txt", b"obsolete resource"),
            (
                "data/config/settings.json",
                br#"{"existing":false,"theme":"dark"}"#,
            ),
            (
                "data/config/auth.json",
                br#"{"cstoa":{"type":"api_key","key":"synthetic-local-secret"}}"#,
            ),
            ("data/sessions/session.jsonl", b"synthetic session data\n"),
            ("artifacts/2026-11-09/runbook/01.txt", b"user command text"),
            (".state/tmp/cache/old.tmp", b"scan cache"),
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
            "data/config/auth.json",
            "data/sessions/session.jsonl",
            "artifacts/2026-11-09/runbook/01.txt",
            ".state/tmp/cache/old.tmp",
            "user-note.txt",
        ]
        .into_iter()
        .map(|n| (n.into(), self.read(n)))
        .collect()
    }

    fn old_program(&self) -> BTreeMap<String, Vec<u8>> {
        [
            "VERSION",
            "app/BUILD-INFO.json",
            "cst-pilot.exe",
            "app/cst-server.exe",
            "app/cst-updater.exe",
            "app/old-only.txt",
            "data/config/settings.json",
        ]
        .into_iter()
        .map(|n| (n.into(), self.read(n)))
        .collect()
    }

    fn package(&self) -> VerifiedPackage {
        self.package_with(files())
    }

    fn package_with(&self, files: BTreeMap<String, Vec<u8>>) -> VerifiedPackage {
        let (envelope, zip) = bundle(files);
        let release = key().verify(&envelope).unwrap();
        VerifiedPackage::stage_from_archive_bytes(
            &self.root,
            release,
            &zip,
            &CancellationToken::new(),
        )
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

fn files() -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::from([
        ("cst-pilot.exe".into(), b"new main".to_vec()),
        ("app/cst-server.exe".into(), b"new server".to_vec()),
        ("app/cst-updater.exe".into(), b"new updater".to_vec()),
        ("VERSION".into(), b"cst-pilot v0.7.0\n".to_vec()),
        (
            "app/BUILD-INFO.json".into(),
            br#"{"version":"v0.7.0"}"#.to_vec(),
        ),
        ("LICENSE".into(), b"MIT".to_vec()),
        ("THIRD-PARTY-NOTICES.md".into(), b"license notices".to_vec()),
        ("app/licenses/mock.txt".into(), b"mock third party".to_vec()),
        ("app/new-only.txt".into(), b"new resource".to_vec()),
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
            b"synthetic prompt".to_vec(),
        ),
        (
            "app/telemetry/timserver_1.crt".into(),
            b"synthetic public cert".to_vec(),
        ),
        ("app/telemetry/currency.json".into(), b"{}".to_vec()),
        (
            "app/defaults/settings.json".into(),
            br#"{"existing":true,"newOption":1}"#.to_vec(),
        ),
        ("app/defaults/models-store.json".into(), b"{}".to_vec()),
        ("app/defaults/telemetry.json".into(), b"{}".to_vec()),
        ("app/defaults/web-search.json".into(), b"{}".to_vec()),
    ]);
    for skill in REQUIRED_SKILL_NAMES {
        files.insert(
            format!("app/skills/{skill}/SKILL.md"),
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
    zip.start_file("cst-pilot/app/SHA256SUMS", SimpleFileOptions::default())
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
        assert!(root.join(".state/update/transaction.json").exists());
        if self.fail {
            Err("synthetic health failure".into())
        } else {
            Ok(())
        }
    }

    fn restart(&self, root: &Path) -> Result<(), String> {
        assert!(!root.join(".state/update/transaction.json").exists());
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

#[test]
fn replaces_program_merges_defaults_preserves_users_and_removes_obsolete_resources() {
    let f = Fixture::new();
    let users = f.users();
    let package = f.package();
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
    assert!(!f.root.join("app/old-only.txt").exists());
    assert_eq!(users, f.users());
    let settings: Value = serde_json::from_slice(&f.read("data/config/settings.json")).unwrap();
    assert_eq!(
        settings,
        json!({"existing":false,"theme":"dark","newOption":1})
    );
    assert!(!f.root.join(".state/update/transaction.json").exists());
    assert!(!stage.exists());
    assert_eq!(applier.recover().unwrap(), Recovery::None);
}

#[test]
fn health_failure_rolls_back_every_program_and_config_without_touching_users() {
    let f = Fixture::new();
    let old = f.old_program();
    let users = f.users();
    let package = f.package();
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

#[test]
fn every_rename_boundary_recovers_after_a_simulated_process_crash() {
    let fixture = Fixture::new();
    let mut package = fixture.package();
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
        let mut package = f.package();
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

#[test]
fn committed_transaction_cleanup_never_rolls_back_a_healthy_version() {
    let f = Fixture::new();
    let users = f.users();
    let mut package = f.package();
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

#[test]
fn caller_cancellation_still_restores_old_files_and_does_not_cancel_the_parent() {
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
    let mut package = f.package();
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

#[test]
fn observer_io_failure_rolls_back_and_restart_failure_leaves_a_healthy_installation() {
    let f = Fixture::new();
    let old = f.old_program();
    let mut package = f.package();
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
    let package = f.package();
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

#[test]
fn changed_live_file_or_backup_pauses_recovery_without_overwriting_either() {
    for corrupt_backup in [false, true] {
        let f = Fixture::new();
        let mut package = f.package();
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

#[test]
fn invalid_journal_or_changed_stage_is_rejected_without_program_mutation() {
    let f = Fixture::new();
    let old = f.old_program();
    let package = f.package();
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

#[test]
fn corrupted_merge_config_and_hard_links_stop_before_journal_publication() {
    let f = Fixture::new();
    let package = f.package();
    f.write("data/config/settings.json", b"{invalid");
    assert!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply(package, &Host::default(), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(f.read("data/config/settings.json"), b"{invalid");
    assert!(!f.root.join(".state/update/transaction.json").exists());
    let f = Fixture::new();
    let package = f.package();
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

#[test]
fn handoff_validates_parent_identity_and_cleans_unlaunched_requests() {
    let f = Fixture::new();
    let package = f.package();
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
    let package = f.package();
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

#[test]
fn failed_process_launch_does_not_retain_the_package_or_replace_programs() {
    let f = Fixture::new();
    let old = f.old_program();
    let package = f.package();
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

#[test]
fn real_native_health_process_and_ui_restart_succeed_inside_the_isolated_kit() {
    let f = Fixture::new();
    let mut next = files();
    let host_bytes = fs::read(native_test_host()).unwrap();
    next.insert("app/cst-server.exe".into(), host_bytes.clone());
    next.insert("cst-pilot.exe".into(), host_bytes);
    let package = f.package_with(next);
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
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(f.read(".state/health-checked"), b"checked");
    assert_eq!(
        String::from_utf8(f.read(".state/health-order")).unwrap(),
        "cst-server.exe\ncst-pilot.exe\n"
    );
    assert_eq!(f.read(".state/restarted-ui"), b"web");
}

#[test]
fn real_health_failure_version_mismatch_and_shared_fifteen_second_budget_all_roll_back() {
    for mode in ["fail", "mismatch", "slow_then_hang"] {
        let f = Fixture::new();
        let old = f.old_program();
        let users = f.users();
        f.write(".state/health-mode", mode.as_bytes());
        let mut next = files();
        let host_bytes = fs::read(native_test_host()).unwrap();
        next.insert("app/cst-server.exe".into(), host_bytes.clone());
        next.insert("cst-pilot.exe".into(), host_bytes);
        let package = f.package_with(next);
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
        if mode == "slow_then_hang" {
            // cst-server took 6s, then cst-pilot hung; shared 15s total budget must cap total wait ~15s (not 21s/30s).
            assert!(start.elapsed() >= Duration::from_secs(15));
            assert!(start.elapsed() < Duration::from_secs(20));
        }
        assert_eq!(old, f.old_program());
        assert_eq!(users, f.users());
        assert!(!f.root.join(".state/restarted-ui").exists());
    }
}

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

#[test]
fn transient_windows_file_occupancy_is_retried_without_losing_the_old_program() {
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
    let mut package = f.package();
    assert_eq!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply_observed(
                &mut package,
                &Host::default(),
                &Hold(f.root.join("app/BUILD-INFO.json")),
                &CancellationToken::new()
            )
            .unwrap(),
        ApplyOutcome::Updated { restarted: true }
    );
}

#[test]
fn recovery_rejects_junction_backups_and_signed_journal_path_tampering() {
    let f = Fixture::new();
    let outside = Fixture::new();
    outside.write("sentinel.txt", b"do not touch");
    let old = f.old_program();
    let mut package = f.package();
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
    journal["entries"][0]["path"] = json!("data/config/auth.json");
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
        Some(RollbackReason::Interrupted)
    );
}

#[test]
fn independent_updater_help_and_missing_compiled_key_do_not_touch_a_kit() {
    let f = Fixture::new();
    let old = f.old_program();
    let exe = env!("CARGO_BIN_EXE_cst-updater");
    #[cfg(windows)]
    {
        let pe = fs::read(exe).expect("read cst-updater.exe PE header");
        assert_eq!(&pe[0..2], b"MZ");
        let pe_offset = u32::from_le_bytes(pe[0x3c..0x40].try_into().unwrap()) as usize;
        assert_eq!(&pe[pe_offset..pe_offset + 4], b"PE\0\0");
        let subsystem_offset = pe_offset + 4 + 20 + 68;
        let subsystem = u16::from_le_bytes(
            pe[subsystem_offset..subsystem_offset + 2]
                .try_into()
                .unwrap(),
        );
        // IMAGE_SUBSYSTEM_WINDOWS_GUI (2) prevents console flash when spawned during upgrade/recovery.
        assert_eq!(
            subsystem, 2,
            "cst-updater.exe must use the Windows GUI subsystem"
        );
    }
    assert!(
        std::process::Command::new(exe)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
    match ReleaseKey::compiled() {
        Ok(compiled) => {
            let probe = std::process::Command::new(exe)
                .arg("--key-fingerprint")
                .output()
                .unwrap();
            assert!(probe.status.success());
            assert_eq!(
                String::from_utf8_lossy(&probe.stdout).trim(),
                compiled.fingerprint()
            );
        }
        Err(_) => {
            let result = std::process::Command::new(exe)
                .arg("--root")
                .arg(&f.root)
                .arg("--recover")
                .output()
                .unwrap();
            assert!(!result.status.success());
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(stderr.contains("未配置官方升级公钥") || stderr.contains("未启用在线更新"));
        }
    }
    assert_eq!(old, f.old_program());
    assert!(!f.root.join(".state/update").exists());
}

#[test]
fn independent_updater_binary_recovers_interrupted_transaction_when_test_key_compiled() {
    if ReleaseKey::compiled().ok().as_ref() != Some(&key()) {
        return;
    }
    let f = Fixture::new();
    let old = f.old_program();
    let users = f.users();
    let mut package = f.package();
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
    assert!(f.root.join(".state/update/transaction.json").exists());

    let exe = env!("CARGO_BIN_EXE_cst-updater");
    let result = std::process::Command::new(exe)
        .arg("--root")
        .arg(&f.root)
        .arg("--recover")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(old, f.old_program());
    assert_eq!(users, f.users());
    assert!(!f.root.join(".state/update/transaction.json").exists());
    assert_eq!(
        Applier::new(&f.root, key())
            .unwrap()
            .report()
            .unwrap()
            .unwrap()
            .reason,
        Some(RollbackReason::Interrupted)
    );
}

#[test]
fn upgrade_removes_legacy_wiztree_files_and_restores_them_on_rollback() {
    // 1. Successful in-place upgrade removes legacy installed `app/tools/wiztree/WizTree64.exe`
    let f = Fixture::new();
    f.write("app/tools/wiztree/WizTree64.exe", b"legacy wiztree binary");
    f.write("app/tools/wiztree/license.txt", b"legacy wiztree license");
    let users = f.users();
    let package = f.package();
    let applier = Applier::new(&f.root, key()).unwrap();
    let outcome = applier
        .apply(package, &Host::default(), &CancellationToken::new())
        .unwrap();
    assert_eq!(outcome, ApplyOutcome::Updated { restarted: true });
    assert!(!f.root.join("app/tools/wiztree/WizTree64.exe").exists());
    assert!(!f.root.join("app/tools/wiztree/license.txt").exists());
    assert_eq!(users, f.users());

    // 2. Rollback after WizTree files have actually been moved out of the install tree
    //    (at health check time, after all entries are installed/deleted) restores them byte-for-byte.
    let f2 = Fixture::new();
    f2.write("app/tools/wiztree/WizTree64.exe", b"legacy wiztree binary");
    f2.write("app/tools/wiztree/license.txt", b"legacy wiztree license");
    let mut package2 = f2.package();
    let applier2 = Applier::new(&f2.root, key()).unwrap();
    struct CrashAfterWizTreeRemoved {
        root: PathBuf,
    }
    impl ApplyObserver for CrashAfterWizTreeRemoved {
        fn reached(&self, step: ApplyStep) -> Result<(), String> {
            if matches!(step, ApplyStep::Installed(_))
                && !self.root.join("app/tools/wiztree/WizTree64.exe").exists()
                && !self.root.join("app/tools/wiztree/license.txt").exists()
            {
                panic!("crash after legacy wiztree files were removed");
            }
            Ok(())
        }
    }
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| applier2.apply_observed(
            &mut package2,
            &Host::default(),
            &CrashAfterWizTreeRemoved {
                root: f2.root.clone(),
            },
            &CancellationToken::new()
        )))
        .is_err()
    );
    // Explicitly prove both legacy WizTree files were removed before rollback runs.
    assert!(!f2.root.join("app/tools/wiztree/WizTree64.exe").exists());
    assert!(!f2.root.join("app/tools/wiztree/license.txt").exists());
    assert_eq!(applier2.recover().unwrap(), Recovery::RolledBack);
    assert_eq!(
        f2.read("app/tools/wiztree/WizTree64.exe"),
        b"legacy wiztree binary"
    );
    assert_eq!(
        f2.read("app/tools/wiztree/license.txt"),
        b"legacy wiztree license"
    );

    // 3. New signed release package containing `app/tools/wiztree/*` is rejected at verify time.
    let mut bad_files = files();
    bad_files.insert("app/tools/wiztree/WizTree64.exe".into(), b"bad".to_vec());
    let (envelope, _zip) = bundle(bad_files);
    assert!(key().verify(&envelope).is_err());

    // 4. Empty mixed-case `app/tools/WizTree/` directory in an assembled dist tree is rejected.
    let dist_dir = std::env::temp_dir().join(format!("cst-dist-check-{}", uuid::Uuid::new_v4()));
    for (rel, bytes) in files() {
        let p = dist_dir.join(&rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    fs::create_dir_all(dist_dir.join("app/tools/WizTree")).unwrap();
    assert!(cst_updater::verify::scan_clean_dist_tree(&dist_dir, "v0.7.0").is_err());
    let _ = fs::remove_dir_all(&dist_dir);
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

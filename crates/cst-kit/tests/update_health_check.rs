#![cfg(windows)]

use base64::{Engine, engine::general_purpose::STANDARD};
use cst_kit::update::{
    UpdateClient,
    applier::{Applier, ApplyOutcome, UpdateHost},
    handoff,
    manifest::{
        Archive, MANIFEST_DOMAIN, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
    },
    network::{Download, Fetcher},
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
};
use tokio_util::sync::CancellationToken;
use zip::{ZipWriter, write::SimpleFileOptions};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("cst-update-health-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let f = Self { root };
        for (name, bytes) in [
            ("VERSION", b"cst-pilot v0.6.0\n".as_slice()),
            ("BUILD-INFO.json", br#"{"version":"v0.6.0"}"#),
            ("cst-pilot.exe", b"old main"),
            ("cst-server.exe", b"old server"),
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
            "cst-server.exe",
            "cst-updater.exe",
            "assets/old-only.txt",
            "agent/home/settings.json",
        ]
        .into_iter()
        .map(|n| (n.into(), self.read(n)))
        .collect()
    }

    async fn package(&self) -> VerifiedPackage {
        let (envelope, zip) = bundle(files());
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
        ("cst-server.exe".into(), b"new server".to_vec()),
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
            "assets/fonts/SourceHanSansCN-VF.otf".into(),
            mock_sfnt(b"OTTO"),
        ),
        ("assets/fonts/LICENSE.txt".into(), b"SIL OFL 1.1".to_vec()),
        (
            "assets/fonts/JetBrainsMono-Regular.ttf".into(),
            mock_sfnt(&[0, 1, 0, 0]),
        ),
        (
            "assets/fonts/JetBrainsMono-Medium.ttf".into(),
            mock_sfnt(&[0, 1, 0, 0]),
        ),
        (
            "assets/fonts/JetBrainsMono-OFL.txt".into(),
            b"SIL OFL 1.1".to_vec(),
        ),
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
    for skill in cst_kit::SKILL_NAMES {
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
struct KitHostCheck {
    checks: AtomicUsize,
    restarts: AtomicUsize,
}

impl UpdateHost for KitHostCheck {
    fn health_check(&self, root: &Path, cancel: &CancellationToken) -> Result<(), String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        assert!(!cancel.is_cancelled());
        assert!(root.join(".state/update/transaction.json").exists());
        assert!(cst_kit::update::service::startup_check(root).is_err());
        assert!(
            cst_kit::Kit::new(root)
                .unwrap()
                .session_options(pi_runtime::CreateAgentSessionOptions::default())
                .is_err()
        );
        handoff::health_check(root)?;
        assert!(cst_kit::update::service::startup_check(root).is_err());
        Ok(())
    }

    fn restart(&self, root: &Path) -> Result<(), String> {
        assert!(cst_kit::update::service::startup_check(root).is_ok());
        self.restarts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn commented_local_settings_and_models_pass_merge_and_public_health_check() {
    let f = Fixture::new();
    let models = br#"{
        // Local model config supports comments.
        "providers": {}
    }"#;
    f.write("agent/home/models.json", models);
    f.write(
        "agent/home/settings.json",
        br#"{
        /* Existing settings remain effective. */
        "theme":"dark", "existing":false
    }"#,
    );
    let package = f.package().await;
    assert_eq!(
        Applier::new(&f.root, key())
            .unwrap()
            .apply(package, &KitHostCheck::default(), &CancellationToken::new())
            .unwrap(),
        ApplyOutcome::Updated { restarted: true }
    );
    let settings: Value = serde_json::from_slice(&f.read("agent/home/settings.json")).unwrap();
    assert_eq!(
        settings,
        json!({"theme":"dark","existing":false,"newOption":1})
    );
    assert_eq!(f.read("agent/home/models.json"), models);
    handoff::health_check(&f.root).unwrap();
}

#[tokio::test]
async fn health_check_accepts_comments_without_writing_local_settings() {
    let f = Fixture::new();
    for (name, bytes) in files() {
        f.write(&name, &bytes);
    }
    let settings = b"{ // local comment\n \"theme\":\"dark\" }";
    f.write("agent/home/settings.json", settings);
    handoff::health_check(&f.root).unwrap();
    assert_eq!(f.read("agent/home/settings.json"), settings);
    f.write(
        "agent/home/settings.json",
        b"{ // duplicate\n \"a\":1,\"a\":2 }",
    );
    assert!(handoff::health_check(&f.root).is_err());
}

#[tokio::test]
async fn public_health_check_failure_rolls_back_without_overwriting_invalid_user_config() {
    let f = Fixture::new();
    f.write("agent/home/auth.json", b"invalid user config");
    let old = f.old_program();
    let users = f.users();
    let package = f.package().await;
    let host = KitHostCheck::default();
    let applier = Applier::new(&f.root, key()).unwrap();
    assert_eq!(
        applier
            .apply(package, &host, &CancellationToken::new())
            .unwrap(),
        ApplyOutcome::RolledBack
    );
    assert_eq!(host.checks.load(Ordering::SeqCst), 1);
    assert_eq!(host.restarts.load(Ordering::SeqCst), 0);
    assert_eq!(old, f.old_program());
    assert_eq!(users, f.users());
    assert!(!f.root.join(".state/update/transaction.json").exists());
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

#[test]
fn cli_health_check_rejects_mixed_binary_build_version() {
    let f = Fixture::new();
    for (name, bytes) in files() {
        f.write(&name, &bytes);
    }
    let disk_ver = cst_kit::update::layout::read_version(&f.root).unwrap();
    assert_eq!(
        handoff::run_cli_health_check_with_version(&f.root, Some(&disk_ver)).unwrap(),
        disk_ver
    );
    assert_eq!(
        handoff::run_cli_health_check_with_version(&f.root, Some(disk_ver.trim_start_matches('v')))
            .unwrap(),
        disk_ver
    );
    let err = handoff::run_cli_health_check_with_version(&f.root, Some("v99.99.99"))
        .expect_err("mixed binary build version must fail health check");
    assert!(err.contains("VERSION 不一致"), "got: {err}");
}

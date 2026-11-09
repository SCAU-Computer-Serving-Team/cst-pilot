use base64::{Engine, engine::general_purpose::STANDARD};
use cst_pilot::update::{
    UpdateClient,
    layout::{self, Area},
    manifest::{
        Archive, MANIFEST_DOMAIN, ReleaseKey, ReleaseManifest, SUMS_DOMAIN, SignedManifest,
    },
    network::{Download, Fetcher},
};
use ed25519_dalek::{Signer, SigningKey};
use pi_agent::BoxFuture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

// Independent deterministic test key; never part of ReleaseKey::compiled().
fn signer() -> SigningKey {
    SigningKey::from_bytes(&[42; 32])
}
fn key() -> ReleaseKey {
    ReleaseKey::from_bytes(signer().verifying_key().as_bytes()).unwrap()
}
fn sign(domain: &[u8], body: &[u8]) -> String {
    STANDARD.encode(signer().sign(&[domain, body].concat()).to_bytes())
}
fn envelope(info: &ReleaseManifest) -> Vec<u8> {
    let payload = serde_json::to_string(info).unwrap();
    serde_json::to_vec(&SignedManifest {
        signature: sign(MANIFEST_DOMAIN, payload.as_bytes()),
        payload,
    })
    .unwrap()
}
fn defaults() -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::from([
        ("cst-pilot.exe".into(), b"mock main binary".to_vec()),
        ("cst-updater.exe".into(), b"mock updater binary".to_vec()),
        ("VERSION".into(), b"cst-pilot v0.7.0\npi rust\n".to_vec()),
        (
            "BUILD-INFO.json".into(),
            br#"{"version":"v0.7.0"}"#.to_vec(),
        ),
        ("LICENSE".into(), b"mock MIT license".to_vec()),
        ("THIRD-PARTY-NOTICES.md".into(), b"mock notices".to_vec()),
        ("licenses/example.txt".into(), b"mock license".to_vec()),
        (
            "agent/home/APPEND_SYSTEM.md".into(),
            b"mock system instructions".to_vec(),
        ),
        (
            "agent/home/settings.json".into(),
            br#"{"newOption":true}"#.to_vec(),
        ),
    ]);
    for skill in cst_pilot::SKILL_NAMES {
        files.insert(
            format!("agent/home/skills/{skill}/SKILL.md"),
            b"mock maintenance skill".to_vec(),
        );
    }
    files.insert(
        "agent/home/extensions/telemetry/timserver_1.crt".into(),
        b"mock public certificate".to_vec(),
    );
    files
}
fn bundle(
    files: &BTreeMap<String, Vec<u8>>,
    extra: &[(&str, &[u8], bool)],
) -> (ReleaseManifest, Vec<u8>) {
    let sums = files
        .iter()
        .map(|(name, bytes)| format!("{:x}  {name}\n", Sha256::digest(bytes)))
        .collect::<String>();
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (name, bytes) in files {
        zip.start_file(format!("cst-pilot/{name}"), options)
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.start_file("cst-pilot/SHA256SUMS", options).unwrap();
    zip.write_all(sums.as_bytes()).unwrap();
    for (name, bytes, symlink) in extra {
        if *symlink {
            zip.add_symlink(name, String::from_utf8_lossy(bytes), options)
                .unwrap();
        } else {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
    }
    let data = zip.finish().unwrap().into_inner();
    let info = ReleaseManifest {
        schema_version: 1,
        platform: "windows-x86_64".into(),
        version: "v0.7.0".into(),
        built_at: "2026-11-09T00:00:00.000Z".into(),
        min_upgradable_version: "v0.6.0".into(),
        release_notes: "版本说明：更新诊断组件。".into(),
        archive: Archive {
            url: "https://updates.example/cst-pilot.zip".into(),
            size: data.len() as u64,
            unpacked_size: files.values().map(|b| b.len() as u64).sum::<u64>() + sums.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&data)),
        },
        sha256_signature: sign(SUMS_DOMAIN, sums.as_bytes()),
        sha256_sums: sums,
    };
    (info, data)
}
#[derive(Default)]
struct Mock {
    bodies: Mutex<BTreeMap<String, Vec<u8>>>,
    calls: Mutex<Vec<(String, u64)>>,
}
impl Mock {
    fn new(info: &ReleaseManifest, zip: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            bodies: Mutex::new(BTreeMap::from([
                ("https://updates.example/latest".into(), envelope(info)),
                (info.archive.url.clone(), zip),
            ])),
            ..Default::default()
        })
    }
    fn client(self: &Arc<Self>) -> UpdateClient {
        UpdateClient::with_fetcher("https://updates.example/latest", key(), self.clone()).unwrap()
    }
}
impl Fetcher for Mock {
    fn get(
        &self,
        url: String,
        limit: u64,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<Download, String>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((url.clone(), limit));
            let body = self
                .bodies
                .lock()
                .unwrap()
                .get(&url)
                .cloned()
                .ok_or("mock not found")?;
            let chunks: Vec<_> = body.chunks(37).map(|bytes| Ok(bytes.to_vec())).collect();
            Ok(Download {
                body: Box::pin(futures_util::stream::iter(chunks)),
            })
        })
    }
}
struct UnsafeDns;
impl cst_pilot::tools::ssrf::DnsResolver for UnsafeDns {
    fn resolve(
        &self,
        _: String,
        _: u16,
    ) -> BoxFuture<'_, Result<Vec<std::net::SocketAddr>, String>> {
        Box::pin(async {
            Ok(vec![
                "8.8.8.8:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ])
        })
    }
}
#[tokio::test]
async fn native_update_fetcher_rejects_mixed_dns_before_any_connection() {
    let fetcher = cst_pilot::update::network::NativeFetcher::with_resolver(
        cst_pilot::tools::ssrf::NetworkPolicy::default(),
        Arc::new(UnsafeDns),
    );
    let result = fetcher
        .get(
            "https://updates.example/latest".into(),
            1024,
            CancellationToken::new(),
        )
        .await;
    assert!(result.err().unwrap().contains("内网"));
}
#[test]
fn both_signatures_cover_exact_bytes_and_key_is_not_downloaded() {
    let (mut info, _) = bundle(&defaults(), &[]);
    let encoded = envelope(&info);
    let verified = key().verify(&encoded).unwrap();
    assert_eq!(verified.info().release_notes, info.release_notes);
    let mut forged: SignedManifest = serde_json::from_slice(&encoded).unwrap();
    forged.payload.push(' ');
    assert!(key().verify(&serde_json::to_vec(&forged).unwrap()).is_err());
    assert!(
        ReleaseKey::from_bytes(SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes())
            .unwrap()
            .verify(&encoded)
            .is_err()
    );
    info.sha256_sums.push('\n');
    assert!(key().verify(&envelope(&info)).is_err());
    info.sha256_signature = "unsigned".into();
    assert!(key().verify(&envelope(&info)).is_err());
    // A downloaded public key cannot replace trusted host configuration.
    let mut raw: Value = serde_json::from_slice(&encoded).unwrap();
    raw["publicKey"] = json!(STANDARD.encode(signer().verifying_key().as_bytes()));
    assert!(key().verify(&serde_json::to_vec(&raw).unwrap()).is_err());
}
#[tokio::test]
async fn version_floor_no_downgrade_and_check_never_downloads_archive() {
    let (info, zip) = bundle(&defaults(), &[]);
    let mock = Mock::new(&info, zip);
    let client = mock.client();
    assert!(
        client
            .check("v0.6.2", CancellationToken::new())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        client
            .check("v0.7.0", CancellationToken::new())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        client
            .check("v0.8.0", CancellationToken::new())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        client
            .check("v0.5.9", CancellationToken::new())
            .await
            .is_err()
    );
    assert!(client.check("dev", CancellationToken::new()).await.is_err());
    assert!(
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .all(|(url, _)| url.ends_with("latest"))
    );
}
#[test]
fn signed_manifest_limits_duplicate_fields_and_windows_path_policy() {
    let (mut info, _) = bundle(&defaults(), &[]);
    for bad in [
        "../outside",
        "assets/../outside",
        "assets\\x",
        "/assets/x",
        "C:/x",
        "assets/x:stream",
        "assets/NUL.txt",
        "assets/COM¹.txt",
        "assets/x.",
        "assets/ x",
        "assets//x",
        "agent/home/auth.json",
        "agent/home/device.json",
        ".state/x",
        "wiztree/tmp/a.csv",
        "lhm/test/x",
        "doc/AGENTS.md",
        "assets/.env.local",
    ] {
        assert!(layout::classify(bad).is_err(), "{bad}");
    }
    assert_eq!(
        layout::classify("agent/home/telemetry.json").unwrap(),
        Area::Merge
    );
    assert_eq!(layout::classify("doc/诊断说明.md").unwrap(), Area::Program);
    for bad in [
        "http://updates.example/a",
        "https://127.0.0.1/a",
        "https://updates.example/#a",
        "https://user@updates.example/a",
    ] {
        info.archive.url = bad.into();
        assert!(key().verify(&envelope(&info)).is_err());
    }
    info.archive.url = "https://updates.example/a".into();
    let original = info.sha256_sums.clone();
    info.sha256_sums
        .push_str(&format!("{}  cst-pilot.exe\n", "0".repeat(64)));
    info.sha256_signature = sign(SUMS_DOMAIN, info.sha256_sums.as_bytes());
    assert!(key().verify(&envelope(&info)).is_err());
    info.sha256_sums = original;
    info.sha256_signature = sign(SUMS_DOMAIN, info.sha256_sums.as_bytes());
    info.archive.unpacked_size = u64::MAX;
    assert!(key().verify(&envelope(&info)).is_err());
    let duplicate = br#"{"payload":"{}","payload":"{}","signature":""}"#;
    assert!(key().verify(duplicate).is_err());
}
#[test]
fn defaults_preserve_user_values_arrays_unknowns_and_dynamic_models() {
    let current =
        br#"{"theme":"dark","enabled":false,"nested":{"mine":1},"keys":["user"],"unknown":4}"#;
    let next = br#"{"theme":"light","enabled":true,"nested":{"new":2},"keys":["new"],"added":5}"#;
    let merged: Value = serde_json::from_slice(
        &layout::merge_defaults("agent/home/settings.json", Some(current), next).unwrap(),
    )
    .unwrap();
    assert_eq!(
        merged,
        json!({"theme":"dark","enabled":false,"nested":{"mine":1,"new":2},"keys":["user"],"unknown":4,"added":5})
    );
    let current = br#"{"cstoa":{"etag":"current-account","lastModified":123,"models":[{"id":"private","maxTokens":9}]},"deepseek":{"models":[{"id":"deepseek-flash","maxTokens":8}]}}"#;
    let next = br#"{"cstoa":{"etag":"preset","lastModified":999,"models":[{"id":"offline","maxTokens":7}]},"deepseek":{"models":[{"id":"deepseek-flash","maxTokens":999},{"id":"added","maxTokens":3}]}}"#;
    let merged: Value = serde_json::from_slice(
        &layout::merge_defaults("agent/home/models-store.json", Some(current), next).unwrap(),
    )
    .unwrap();
    assert_eq!(merged["cstoa"]["etag"], "current-account");
    assert_eq!(merged["cstoa"]["lastModified"], 123);
    assert_eq!(merged["cstoa"]["models"][0]["id"], "private");
    assert_eq!(
        merged["deepseek"]["models"],
        json!([{"id":"deepseek-flash","maxTokens":8},{"id":"added","maxTokens":3}])
    );
    assert!(layout::merge_defaults("agent/home/auth.json", None, next).is_err());
    assert!(
        layout::merge_defaults(
            "agent/home/settings.json",
            Some(b"invalid private contents"),
            b"{}"
        )
        .is_err()
    );
    assert!(
        layout::merge_defaults("agent/home/settings.json", Some(br#"{"a":1,"a":2}"#), b"{}")
            .is_err()
    );
}

#[cfg(windows)]
mod windows_tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("cst-update-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("VERSION"), b"cst-pilot v0.6.2\n").unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn put(&self, name: &str, bytes: &[u8]) {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        fn stages(&self) -> usize {
            fs::read_dir(self.0.join(".state/update/staging"))
                .map(|items| items.count())
                .unwrap_or(0)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    async fn prepare(
        f: &Fixture,
        info: &ReleaseManifest,
        zip: Vec<u8>,
    ) -> Result<cst_pilot::update::verify::VerifiedPackage, String> {
        let mock = Mock::new(info, zip);
        let client = mock.client();
        let release = client
            .check("v0.6.2", CancellationToken::new())
            .await?
            .unwrap();
        client
            .prepare(&f.0, &release, CancellationToken::new())
            .await
    }
    #[tokio::test]
    async fn signed_package_streams_verifies_and_never_touches_user_data() {
        let f = Fixture::new();
        for name in [
            "agent/home/auth.json",
            "agent/home/models.json",
            "agent/home/telemetry/pending.jsonl",
            "artifacts/local.txt",
            "cst-pilot.exe",
        ] {
            f.put(name, b"unchanged private local bytes");
        }
        let (info, zip) = bundle(&defaults(), &[]);
        let package = prepare(&f, &info, zip).await.unwrap();
        assert_eq!(f.stages(), 1);
        assert_eq!(
            layout::read_version(&package.directory()).unwrap(),
            "v0.7.0"
        );
        package
            .revalidate(&key(), &CancellationToken::new())
            .unwrap();
        for name in [
            "agent/home/auth.json",
            "agent/home/models.json",
            "agent/home/telemetry/pending.jsonl",
            "artifacts/local.txt",
            "cst-pilot.exe",
        ] {
            assert_eq!(
                fs::read(f.0.join(name)).unwrap(),
                b"unchanged private local bytes"
            );
        }
        drop(package);
        assert_eq!(f.stages(), 0);
    }
    #[tokio::test]
    async fn prepare_rechecks_installed_version_before_creating_any_staging_files() {
        let f = Fixture::new();
        f.put("VERSION", b"cst-pilot v0.7.0\n");
        let (info, zip) = bundle(&defaults(), &[]);
        assert!(prepare(&f, &info, zip).await.is_err());
        assert!(!f.0.join(".state").exists());
    }
    #[tokio::test]
    async fn every_failed_prepare_cleans_only_its_unique_stage() {
        let f = Fixture::new();
        f.put(".state/update/staging/retained/operator.txt", b"keep");
        let (mut info, mut zip) = bundle(&defaults(), &[]);
        zip.push(0);
        assert!(prepare(&f, &info, zip).await.is_err());
        assert_eq!(f.stages(), 1);
        let (_, zip) = bundle(&defaults(), &[]);
        info.archive.unpacked_size -= 1;
        assert!(prepare(&f, &info, zip).await.is_err());
        assert_eq!(f.stages(), 1);
        assert_eq!(
            fs::read(f.0.join(".state/update/staging/retained/operator.txt")).unwrap(),
            b"keep"
        );
    }
    #[tokio::test]
    async fn unsigned_archive_entries_paths_and_symlinks_are_rejected() {
        let f = Fixture::new();
        for (path, symlink) in [
            ("cst-pilot/../escape.txt", false),
            ("/absolute", false),
            ("cst-pilot/agent/home/auth.json", false),
            ("cst-pilot/assets/missing", false),
            ("cst-pilot/assets/link", true),
            ("cst-pilot/assets/foo:ads", false),
            ("other/cst-pilot.exe", false),
            ("cst-pilot/assets/nul.txt", false),
        ] {
            let (info, zip) = bundle(&defaults(), &[(path, b"target", symlink)]);
            assert!(prepare(&f, &info, zip).await.is_err(), "{path}");
            assert_eq!(f.stages(), 0, "{path}");
        }
        assert!(!f.0.parent().unwrap().join("escape.txt").exists());
    }
    #[tokio::test]
    async fn signed_tree_still_rejects_private_json_tokens_and_hidden_duplicate_fields() {
        let f = Fixture::new();
        for bytes in [
            br#"{"apiKey":"private","apiKey":""}"#.as_slice(),
            br#"{"nested":{"authorization":"private"}}"#,
            b"{\"private\":\"-----BEGIN PRIVATE KEY-----\"}",
        ] {
            let mut files = defaults();
            files.insert("assets/config.json".into(), bytes.to_vec());
            let (info, zip) = bundle(&files, &[]);
            assert!(prepare(&f, &info, zip).await.is_err());
            assert_eq!(f.stages(), 0);
        }
        let mut files = defaults();
        let mut secret = vec![b'a'; 65520];
        secret.extend_from_slice(b" sk-abcdefghijklmnopqrstuvwxyz0123456789 ");
        files.insert("assets/file.bin".into(), secret);
        let (info, zip) = bundle(&files, &[]);
        assert!(prepare(&f, &info, zip).await.is_err());
    }
    #[tokio::test]
    async fn metadata_hash_missing_file_and_case_ancestor_conflicts_are_rejected() {
        let f = Fixture::new();
        for mode in 0..4 {
            let mut files = defaults();
            match mode {
                0 => {
                    files.insert("VERSION".into(), b"cst-pilot v0.6.2\n".to_vec());
                }
                1 => {
                    files.remove("cst-updater.exe");
                }
                2 => {
                    files.insert("assets/A/x".into(), b"x".to_vec());
                    files.insert("assets/a/y".into(), b"y".to_vec());
                }
                _ => {
                    files.insert(
                        "BUILD-INFO.json".into(),
                        br#"{"version":"v0.6.2"}"#.to_vec(),
                    );
                }
            }
            let (info, zip) = bundle(&files, &[]);
            assert!(prepare(&f, &info, zip).await.is_err(), "{mode}");
            assert_eq!(f.stages(), 0);
        }
        let (mut info, zip) = bundle(&defaults(), &[]);
        info.sha256_sums = info.sha256_sums.replace(
            &format!("{:x}", Sha256::digest(b"mock main binary")),
            &"0".repeat(64),
        );
        info.sha256_signature = sign(SUMS_DOMAIN, info.sha256_sums.as_bytes());
        assert!(prepare(&f, &info, zip).await.is_err());
    }
    #[tokio::test]
    async fn revalidation_detects_replacement_hardlinks_and_extra_files() {
        let f = Fixture::new();
        let (info, zip) = bundle(&defaults(), &[]);
        let package = prepare(&f, &info, zip).await.unwrap();
        let main = package.directory().join("cst-pilot.exe");
        let original = fs::read(&main).unwrap();
        fs::write(&main, b"tampered").unwrap();
        assert!(
            package
                .revalidate(&key(), &CancellationToken::new())
                .is_err()
        );
        fs::write(&main, original).unwrap();
        fs::hard_link(&main, f.0.join("alias.exe")).unwrap();
        assert!(
            package
                .revalidate(&key(), &CancellationToken::new())
                .is_err()
        );
        fs::remove_file(f.0.join("alias.exe")).unwrap();
        fs::write(package.directory().join("extra.dll"), b"unsigned").unwrap();
        assert!(
            package
                .revalidate(&key(), &CancellationToken::new())
                .is_err()
        );
    }
    #[tokio::test]
    async fn staging_links_fail_without_writing_outside_the_kit() {
        let f = Fixture::new();
        let outside = Fixture::new();
        f.put(".state/update/placeholder", b"keep");
        junction(&outside.0, &f.0.join(".state/update/staging"));
        let (info, zip) = bundle(&defaults(), &[]);
        assert!(prepare(&f, &info, zip).await.is_err());
        assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 1);
        fs::remove_dir(f.0.join(".state/update/staging")).unwrap();
    }
    #[tokio::test]
    async fn dropping_a_package_with_replaced_root_never_cleans_the_junction_target() {
        let f = Fixture::new();
        let outside = Fixture::new();
        outside.put("private.txt", b"keep");
        let (info, zip) = bundle(&defaults(), &[]);
        let package = prepare(&f, &info, zip).await.unwrap();
        let stage = package.directory().parent().unwrap().to_owned();
        let original = stage.with_extension("saved");
        fs::rename(&stage, &original).unwrap();
        junction(&outside.0, &stage);
        assert!(
            package
                .revalidate(&key(), &CancellationToken::new())
                .is_err()
        );
        drop(package);
        assert_eq!(fs::read(outside.0.join("private.txt")).unwrap(), b"keep");
        fs::remove_dir(&stage).unwrap();
    }
    fn junction(target: &Path, link: &Path) {
        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(status.status.success());
    }
    #[tokio::test]
    async fn malformed_zip_directory_is_bounded_before_archive_index_allocation() {
        let f = Fixture::new();
        for mode in 0..2 {
            let (mut info, mut zip) = bundle(&defaults(), &[]);
            let eocd = zip.len() - 22;
            if mode == 0 {
                zip[eocd + 8..eocd + 12].fill(0xff);
            } else {
                zip[eocd + 12..eocd + 16].copy_from_slice(&(64 * 1024 * 1024u32 + 1).to_le_bytes());
            }
            info.archive.sha256 = format!("{:x}", Sha256::digest(&zip));
            assert!(prepare(&f, &info, zip).await.is_err());
            assert_eq!(f.stages(), 0);
        }
    }
    #[tokio::test]
    async fn bounded_zip64_directory_is_accepted_and_invalid_record_lengths_are_rejected() {
        let f = Fixture::new();
        let (base_info, zip) = bundle(&defaults(), &[]);
        let position = zip.len() - 22;
        let mut end = zip[position..].to_vec();
        let count = u16::from_le_bytes(end[10..12].try_into().unwrap()) as u64;
        let size = u32::from_le_bytes(end[12..16].try_into().unwrap()) as u64;
        let offset = u32::from_le_bytes(end[16..20].try_into().unwrap()) as u64;
        end[8..20].fill(0xff);
        for record_size in [44u64, u64::MAX] {
            let mut data = zip[..position].to_vec();
            data.extend_from_slice(b"PK\x06\x06");
            data.extend_from_slice(&record_size.to_le_bytes());
            data.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            data.extend_from_slice(&count.to_le_bytes());
            data.extend_from_slice(&count.to_le_bytes());
            data.extend_from_slice(&size.to_le_bytes());
            data.extend_from_slice(&offset.to_le_bytes());
            data.extend_from_slice(b"PK\x06\x07\0\0\0\0");
            data.extend_from_slice(&(position as u64).to_le_bytes());
            data.extend_from_slice(&[1, 0, 0, 0]);
            data.extend_from_slice(&end);
            let mut info = base_info.clone();
            info.archive.size = data.len() as u64;
            info.archive.sha256 = format!("{:x}", Sha256::digest(&data));
            let result = prepare(&f, &info, data).await;
            assert_eq!(result.is_ok(), record_size == 44);
            drop(result);
            assert_eq!(f.stages(), 0);
        }
    }
    struct SlowArchive(Arc<Mock>);
    impl Fetcher for SlowArchive {
        fn get(
            &self,
            url: String,
            limit: u64,
            cancel: CancellationToken,
        ) -> BoxFuture<'_, Result<Download, String>> {
            if url.ends_with("latest") {
                return self.0.get(url, limit, cancel);
            }
            Box::pin(async {
                Ok(Download {
                    body: Box::pin(futures_util::stream::once(async {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        Ok(vec![0])
                    })),
                })
            })
        }
    }
    #[tokio::test]
    async fn cancelling_a_pending_archive_body_removes_its_stage() {
        let f = Fixture::new();
        let (info, zip) = bundle(&defaults(), &[]);
        let mock = Mock::new(&info, zip);
        let client = UpdateClient::with_fetcher(
            "https://updates.example/latest",
            key(),
            Arc::new(SlowArchive(mock)),
        )
        .unwrap();
        let release = client
            .check("v0.6.2", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let cancel = CancellationToken::new();
        let operation = client.prepare(&f.0, &release, cancel.clone());
        let stop = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (result, _) = tokio::join!(operation, stop);
        assert!(result.is_err());
        assert_eq!(f.stages(), 0);
    }
    struct Slow;
    impl Fetcher for Slow {
        fn get(
            &self,
            _: String,
            _: u64,
            _: CancellationToken,
        ) -> BoxFuture<'_, Result<Download, String>> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Err("mock timeout".into())
            })
        }
    }
    #[tokio::test]
    async fn cancellation_and_whole_request_timeout_are_bounded() {
        let client =
            UpdateClient::with_fetcher("https://updates.example/latest", key(), Arc::new(Slow))
                .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            client
                .check("v0.6.2", cancel)
                .await
                .err()
                .unwrap()
                .contains("取消")
        );
        let at = std::time::Instant::now();
        let error = client
            .check("v0.6.2", CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(error.contains("10 秒"));
        assert!(at.elapsed() < Duration::from_secs(13));
    }
}

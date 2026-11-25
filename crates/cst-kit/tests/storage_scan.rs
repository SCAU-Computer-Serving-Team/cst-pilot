#[cfg(windows)]
use cst_kit::diagnostics::native_job;
use cst_kit::diagnostics::volume_index::{
    ScanSource, VolumeIdentity, VolumeIndex, display_path, path_key,
};
use cst_kit::diagnostics::win_native::Control;
use cst_kit::diagnostics::{ls, mft, scan, storage_tools, walk};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn control() -> Control {
    Control::new(CancellationToken::new(), Duration::from_secs(20))
}
fn id(serial: u32) -> VolumeIdentity {
    VolumeIdentity {
        serial,
        filesystem: "NTFS".into(),
        root_created: 100 + serial as u64,
        volume_guid: Some(format!(
            r"\\?\Volume{{00000000-0000-0000-0000-{serial:012}}}\"
        )),
    }
}

struct MockScan {
    identity: Mutex<Option<VolumeIdentity>>,
    snapshot: scan::ScanSnapshot,
    calls: AtomicUsize,
    delay: Duration,
    switch: bool,
    fail: bool,
}
impl MockScan {
    fn new(snapshot: scan::ScanSnapshot) -> Self {
        Self {
            identity: Mutex::new(Some(id(1))),
            snapshot,
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            switch: false,
            fail: false,
        }
    }
}
impl ScanSource for MockScan {
    fn identity(&self, _: &str) -> Result<VolumeIdentity, String> {
        self.identity
            .lock()
            .unwrap()
            .clone()
            .ok_or("已拔盘。".into())
    }
    fn scan_volume(&self, _: &str, control: &Control) -> Result<scan::ScanSnapshot, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        control.sleep(self.delay)?;
        if self.switch {
            *self.identity.lock().unwrap() = Some(id(2));
        }
        if self.fail {
            Err("模拟原生 MFT 扫描失败。".into())
        } else {
            Ok(self.snapshot.clone())
        }
    }
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cst-scan-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mock_snapshot(root: &str, dirs: &[(&str, u64)], files: &[(&str, u64)]) -> scan::ScanSnapshot {
    let mut dir_map = HashMap::new();
    for (p, b) in dirs {
        dir_map.insert(path_key(Path::new(p)), *b);
    }
    let mut file_map = HashMap::new();
    for (p, b) in files {
        file_map.insert(path_key(Path::new(p)), *b);
    }
    let total = dir_map
        .get(&path_key(Path::new(root)))
        .copied()
        .unwrap_or(0);
    scan::ScanSnapshot {
        root: root.into(),
        identity: None,
        captured_at: Instant::now(),
        method: scan::ScanMethod::NativeMft,
        mft_index: None,
        dirs: dir_map,
        files: file_map,
        total,
        rows: dirs.len() + files.len(),
        file_count: files.len(),
        top_dirs: dirs
            .iter()
            .filter(|(p, _)| path_key(Path::new(p)) != path_key(Path::new(root)))
            .map(|(p, b)| scan::Ranked {
                bytes: *b,
                path: (*p).into(),
                modified: None,
            })
            .collect(),
        top_files: files
            .iter()
            .map(|(p, b)| scan::Ranked {
                bytes: *b,
                path: (*p).into(),
                modified: Some("2020-01-01T00:00:00Z".into()),
            })
            .collect(),
        stale: Vec::new(),
        extensions: HashMap::new(),
        ext_overflow: false,
    }
}

#[test]
fn snapshot_usage_outputs_strict_api_contract_keys_and_zero_root() {
    let mut snap = mock_snapshot(
        "C:\\",
        &[
            ("C:\\", 200 * 1024 * 1024),
            ("C:\\a,b\\", 100 * 1024 * 1024),
        ],
        &[("C:\\a,b\\large.BIN", 100 * 1024 * 1024)],
    );
    snap.extensions.insert("bin".into(), (100 * 1024 * 1024, 1));
    let usage = snap.usage(Path::new("C:\\"), 1, Some("NTFS"));
    assert_eq!(usage["method"], "native-mft");
    let file_keys: BTreeSet<&str> = usage["topFiles"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(file_keys, BTreeSet::from(["modified", "path", "sizeGB"]));
    assert_eq!(usage["topFiles"][0]["modified"], "2020-01-01T00:00:00Z");
    assert_eq!(usage["extAgg"][0]["ext"], "bin");

    let zero =
        mock_snapshot("C:\\", &[("C:\\", 0)], &[]).usage(Path::new("C:\\"), 20, Some("NTFS"));
    assert_eq!(zero["totalGB"], 0.0);
    assert!(scan::gib(1) > 0.0);
}

#[test]
fn topn_is_descending_bounded_and_deterministic() {
    let mut top = scan::TopN::new(3);
    for n in 0..1000 {
        top.add(scan::Ranked {
            bytes: n,
            path: n.to_string(),
            modified: None,
        });
    }
    assert_eq!(
        top.sorted().iter().map(|r| r.bytes).collect::<Vec<_>>(),
        vec![999, 998, 997]
    );
}

#[test]
fn default_production_gate_never_opens_raw_mft_without_opt_in() {
    // Unless CST_ENABLE_MFT=1 is explicitly set in the process environment, production
    // VolumeIndex must keep raw $MFT scanning off and route directly to `native-walk`.
    if std::env::var("CST_ENABLE_MFT").is_ok() {
        return;
    }
    let fixture = Fixture::new();
    fs::write(fixture.0.join("sample.bin"), [1u8; 4096]).unwrap();
    let source = Arc::new(MockScan::new(mock_snapshot("C:\\", &[("C:\\", 12)], &[])));
    let prod = VolumeIndex::new_production(source.clone());
    assert!(!prod.allows_mft());
    assert!(
        prod.scan(Path::new("C:\\"), true, &control())
            .unwrap()
            .is_none()
    );
    assert!(prod.cached(Path::new("C:\\")).is_none());
    let usage = storage_tools::run_usage(&prod, &fixture.0, 10, &control()).unwrap();
    assert_eq!(usage["method"], "native-walk");
    assert!(usage.get("degradedFrom").is_none());
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn volume_change_and_unidentified_volume_invalidate_cache_and_failures() {
    let source = Arc::new(MockScan::new(mock_snapshot("C:\\", &[("C:\\", 12)], &[])));
    let index = VolumeIndex::new(source.clone());
    let old = index
        .scan(Path::new("C:\\"), true, &control())
        .unwrap()
        .unwrap();
    assert!(index.cached(Path::new("C:\\")).is_some());
    *source.identity.lock().unwrap() = Some(id(2));
    assert!(!index.is_current(&old, Path::new("C:\\")));
    assert!(index.cached(Path::new("C:\\")).is_none());
    index.scan(Path::new("C:\\"), false, &control()).unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    *source.identity.lock().unwrap() = None;
    assert!(index.cached(Path::new("C:\\")).is_none());
}

#[test]
fn identity_change_during_scan_never_publishes() {
    let mut mock = MockScan::new(mock_snapshot("C:\\", &[("C:\\", 12)], &[]));
    mock.switch = true;
    let index = VolumeIndex::new(Arc::new(mock));
    assert!(index.scan(Path::new("C:\\"), true, &control()).is_err());
    assert!(index.cached(Path::new("C:\\")).is_none());
}

#[test]
fn concurrent_same_volume_requests_share_one_scan() {
    let mut mock = MockScan::new(mock_snapshot("C:\\", &[("C:\\", 12)], &[]));
    mock.delay = Duration::from_millis(100);
    let source = Arc::new(mock);
    let index = Arc::new(VolumeIndex::new(source.clone()));
    let gate = Arc::new(Barrier::new(6));
    let children = (0..6)
        .map(|_| {
            let index = index.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                index
                    .scan(Path::new("C:\\"), true, &control())
                    .unwrap()
                    .unwrap()
                    .total
            })
        })
        .collect::<Vec<_>>();
    for child in children {
        assert_eq!(child.join().unwrap(), 12);
    }
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_scan_respects_short_retry_ttl_and_cancel_does_not_poison() {
    let mut mock = MockScan::new(mock_snapshot("C:\\", &[("C:\\", 12)], &[]));
    mock.fail = true;
    let source = Arc::new(mock);
    let index = VolumeIndex::with_limits(
        source.clone(),
        false,
        16 * 1024 * 1024,
        Duration::from_millis(60),
    );
    assert!(index.scan(Path::new("C:\\"), false, &control()).is_err());
    assert!(
        index
            .scan(Path::new("C:\\"), false, &control())
            .unwrap()
            .is_none()
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    // After short failure TTL expires, transient failure can be retried automatically.
    std::thread::sleep(Duration::from_millis(80));
    assert!(index.scan(Path::new("C:\\"), false, &control()).is_err());
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    let token = CancellationToken::new();
    token.cancel();
    assert!(
        index
            .scan(
                Path::new("C:\\"),
                true,
                &Control::new(token, Duration::from_secs(1))
            )
            .is_err()
    );
}

#[test]
fn volume_index_prevents_out_of_order_identity_clobber_and_evicts_by_byte_budget() {
    struct ReorderSource {
        first_entered: Barrier,
        release_first: Barrier,
        calls: AtomicUsize,
    }
    impl ScanSource for ReorderSource {
        fn identity(&self, _: &str) -> Result<VolumeIdentity, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First probe starts earlier, observes id(1), but stalls before locking.
                self.first_entered.wait();
                self.release_first.wait();
                Ok(id(1))
            } else {
                Ok(id(2))
            }
        }
        fn scan_volume(&self, _: &str, _: &Control) -> Result<scan::ScanSnapshot, String> {
            Ok(mock_snapshot("C:\\", &[("C:\\", 12)], &[]))
        }
    }

    let source = Arc::new(ReorderSource {
        first_entered: Barrier::new(2),
        release_first: Barrier::new(2),
        calls: AtomicUsize::new(0),
    });
    let index = Arc::new(VolumeIndex::new(source.clone()));
    let idx_t = index.clone();
    let stale_probe = std::thread::spawn(move || idx_t.cached(Path::new("C:\\")));
    source.first_entered.wait();
    // Newer probe completes and caches id(2) while probe #0 is still stalled outside the lock.
    let fresh = index
        .scan(Path::new("C:\\"), true, &control())
        .unwrap()
        .unwrap();
    assert_eq!(fresh.identity.as_ref(), Some(&id(2)));
    source.release_first.wait();
    let _ = stale_probe.join().unwrap();
    // Stale probe #0 finishing late must NOT evict id(2)'s snapshot.
    assert!(index.cached(Path::new("C:\\")).is_some());

    // Byte-budget eviction: set tiny 8 KiB budget across two drives -> oldest must be evicted.
    let multi = Arc::new(MockScan::new(mock_snapshot("C:\\", &[("C:\\", 10)], &[])));
    let bounded = VolumeIndex::with_limits(multi.clone(), false, 8192, Duration::from_secs(30));
    bounded.scan(Path::new("C:\\"), true, &control()).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let mut mock_d = mock_snapshot("D:\\", &[("D:\\", 20)], &[]);
    mock_d.dirs.insert("D:\\BIG".into(), 20);
    let multi_d = Arc::new(MockScan::new(mock_d));
    let bounded2 = VolumeIndex::with_limits(multi_d, false, 1024, Duration::from_secs(30));
    // Snapshot larger than max_bytes gets evicted immediately instead of staying resident.
    bounded2.scan(Path::new("D:\\"), true, &control()).unwrap();
    assert!(bounded2.cached(Path::new("D:\\")).is_none());
}

#[test]
fn delayed_volume_probe_cannot_revive_identity_after_newer_probe_fails() {
    struct RemovedVolume {
        entered: Barrier,
        release: Barrier,
        calls: AtomicUsize,
    }
    impl ScanSource for RemovedVolume {
        fn identity(&self, _: &str) -> Result<VolumeIdentity, String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.wait();
                self.release.wait();
                Ok(id(1))
            } else {
                Err("volume removed".into())
            }
        }
        fn scan_volume(&self, _: &str, _: &Control) -> Result<scan::ScanSnapshot, String> {
            unreachable!("identity-only test")
        }
    }

    let source = Arc::new(RemovedVolume {
        entered: Barrier::new(2),
        release: Barrier::new(2),
        calls: AtomicUsize::new(0),
    });
    let index = Arc::new(VolumeIndex::new(source.clone()));
    let mut snapshot = mock_snapshot("C:\\", &[], &[]);
    snapshot.identity = Some(id(1));
    let pending = {
        let index = index.clone();
        std::thread::spawn(move || index.is_current(&snapshot, Path::new("C:\\")))
    };
    source.entered.wait();
    let removed = index.filesystem(Path::new("C:\\"));
    source.release.wait();
    assert!(removed.is_none());
    assert!(!pending.join().unwrap());
}

#[test]
fn subtree_refresh_removes_stale_descendants_and_invalidates_ancestor_totals() {
    let mut first = mock_snapshot(
        "C:\\",
        &[("C:\\", 100), ("C:\\data\\", 100)],
        &[("C:\\data\\old.bin", 2 * 1024 * 1024)],
    );
    first.identity = Some(id(1));
    let next = mock_snapshot(
        "C:\\data",
        &[("C:\\data\\", 20)],
        &[("C:\\data\\new.bin", 3 * 1024 * 1024)],
    );
    let result = first.merge(&next);
    assert!(!result.files.contains_key("C:\\DATA\\OLD.BIN"));
    assert!(result.files.contains_key("C:\\DATA\\NEW.BIN"));
    assert!(!result.dirs.contains_key("C:"));
}

#[test]
fn native_walk_topdirs_shared_budget_and_incomplete_percentages() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("a")).unwrap();
    fs::create_dir(fixture.0.join("b")).unwrap();
    fs::write(fixture.0.join("a/x"), [0u8; 100]).unwrap();
    fs::write(fixture.0.join("b/y"), [0u8; 20]).unwrap();
    let value = walk::usage(&fixture.0, 5, &control(), walk::WalkLimits::default()).unwrap();
    assert_eq!(value["method"], "native-walk");
    assert_eq!(value["topDirs"].as_array().unwrap().len(), 2);
    assert_eq!(value["topDirs"][0]["pct"], 83.3);
    let files = value["topFiles"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    let file_keys: BTreeSet<&str> = files[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(file_keys, BTreeSet::from(["modified", "path", "sizeGB"]));
    assert!(files[0]["modified"].is_string());
    assert!(!value["extAgg"].as_array().unwrap().is_empty());
    assert_eq!(value["truncated"], false);
    let value = walk::usage(
        &fixture.0,
        5,
        &control(),
        walk::WalkLimits {
            entries: 1,
            timeout: Duration::from_secs(2),
        },
    )
    .unwrap();
    assert_eq!(value["truncated"], true);
    assert!(
        value["topDirs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["pct"].is_null())
    );
}

#[test]
fn ls_direct_files_respect_the_enumeration_deadline() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file"), [0u8; 10]).unwrap();
    let mut mock = MockScan::new(scan::ScanSnapshot::default());
    mock.fail = true;
    let index = VolumeIndex::new(Arc::new(mock));
    let partial = ls::run(
        &index,
        &fixture.0,
        20,
        &control(),
        walk::WalkLimits {
            entries: 100,
            timeout: Duration::ZERO,
        },
    )
    .unwrap();
    assert_eq!(partial["childrenTruncated"], true);
    assert_eq!(partial["entries"], serde_json::json!([]));
}

#[test]
fn ls_preserves_omitted_counts_and_never_publishes_walk() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("dir")).unwrap();
    fs::write(fixture.0.join("dir/x"), [0u8; 30]).unwrap();
    fs::write(fixture.0.join("file"), [0u8; 10]).unwrap();
    let mut mock = MockScan::new(scan::ScanSnapshot::default());
    mock.fail = true;
    let index = VolumeIndex::new(Arc::new(mock));
    let value = ls::run(
        &index,
        &fixture.0,
        1,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(value["entries"][0]["name"], "dir");
    assert_eq!(value["entries"][0]["bytes"], 30);
    assert_eq!(value["entries"][0]["pct"], 75.0);
    assert_eq!(value["omitted"]["count"], 1);
    assert!(index.cached(&fixture.0).is_none());
    let partial = ls::run(
        &index,
        &fixture.0,
        20,
        &control(),
        walk::WalkLimits {
            entries: 1,
            timeout: Duration::from_secs(2),
        },
    )
    .unwrap();
    assert!(partial["notice"].is_string());
    assert!(
        partial["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["pct"].is_null())
    );
}

#[cfg(windows)]
#[test]
fn ls_live_empty_file_overrides_stale_cached_bytes_and_avoids_mixed_ownership() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("dir")).unwrap();
    fs::write(fixture.0.join("pagefile.sys"), []).unwrap();
    let root_s = display_path(&fixture.0);
    let dir_s = display_path(&fixture.0.join("dir"));
    let pf_s = display_path(&fixture.0.join("pagefile.sys"));
    let snap = mock_snapshot(
        &root_s,
        &[(&root_s, 3 * 1024 * 1024), (&dir_s, 1024 * 1024)],
        &[(&pf_s, 2 * 1024 * 1024)],
    );
    let source = Arc::new(MockScan::new(snap));
    let index = VolumeIndex::new(source.clone());
    index.scan(&fixture.0, true, &control()).unwrap();
    let value = ls::run(
        &index,
        &fixture.0,
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(value["method"], "native-index");
    assert_eq!(value["entries"][1]["name"], "pagefile.sys");
    assert_eq!(value["entries"][1]["bytes"], 0);
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);

    // Adding an uncached subdirectory triggers unified query-scoped walk (no mixing whole-volume
    // cached dir totals with local walk totals).
    fs::create_dir(fixture.0.join("new")).unwrap();
    fs::write(fixture.0.join("new/child.txt"), b"hello").unwrap();
    let unified = ls::run(
        &index,
        &fixture.0,
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(unified["method"], "walk");
    let entries = unified["entries"].as_array().unwrap();
    let new_row = entries.iter().find(|e| e["name"] == "new").unwrap();
    let dir_row = entries.iter().find(|e| e["name"] == "dir").unwrap();
    assert_eq!(new_row["bytes"], 5);
    assert_eq!(dir_row["bytes"], 0);
}

#[cfg(windows)]
#[test]
fn ls_dedups_hardlinks_across_direct_files_and_sibling_subdirs() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("dir_a")).unwrap();
    fs::create_dir_all(fixture.0.join("dir_b")).unwrap();
    // 1. Direct file hardlinked into dir_a -> direct file keeps 10_000, dir_a alias contributes 0.
    let direct = fixture.0.join("top.bin");
    fs::write(&direct, vec![1u8; 10_000]).unwrap();
    fs::hard_link(&direct, fixture.0.join("dir_a/top_alias.bin")).unwrap();
    // 2. Sibling hardlink between dir_a and dir_b -> dir_a owns 40_000, dir_b alias contributes 0.
    let shared = fixture.0.join("dir_a/shared.bin");
    fs::write(&shared, vec![2u8; 40_000]).unwrap();
    fs::hard_link(&shared, fixture.0.join("dir_b/shared_alias.bin")).unwrap();
    fs::write(fixture.0.join("dir_b/own.bin"), vec![3u8; 50_000]).unwrap();

    let index = VolumeIndex::new_production(Arc::new(MockScan::new(scan::ScanSnapshot::default())));
    let res = ls::run(
        &index,
        &fixture.0,
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(res["method"], "walk");
    let entries = res["entries"].as_array().unwrap();
    let bytes_of = |name: &str| {
        entries.iter().find(|e| e["name"] == name).unwrap()["bytes"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(bytes_of("top.bin"), 10_000);
    assert_eq!(bytes_of("dir_a"), 40_000);
    assert_eq!(bytes_of("dir_b"), 50_000);
    // Sum of all entries = 100_000 (not 150_000!) and pcts sum to 100.0
    let pct_sum: f64 = entries.iter().map(|e| e["pct"].as_f64().unwrap()).sum();
    assert!((pct_sum - 100.0).abs() < 0.2, "pct_sum={pct_sum}");
}

#[test]
fn unc_never_builds_a_volume_index() {
    let source = Arc::new(MockScan::new(scan::ScanSnapshot::default()));
    let index = VolumeIndex::new(source.clone());
    assert!(
        index
            .scan(Path::new("\\\\host\\share"), true, &control())
            .unwrap()
            .is_none()
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}

#[cfg(windows)]
#[test]
fn junction_is_skipped_without_traversing_the_target() {
    let fixture = Fixture::new();
    let external = Fixture::new();
    fs::write(external.0.join("secret"), [0u8; 500]).unwrap();
    let link = fixture.0.join("junction");
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(&external.0)
        .output()
        .unwrap();
    assert!(status.status.success());
    let result = walk::usage(&fixture.0, 20, &control(), walk::WalkLimits::default()).unwrap();
    assert_eq!(result["totalGB"], 0.0);
    assert_eq!(result["truncated"], true);
    fs::remove_dir(&link).unwrap();
}

#[test]
#[ignore = "helper used by the isolated native Job Object test"]
fn job_child() {
    std::thread::sleep(Duration::from_secs(30));
}

#[cfg(windows)]
#[test]
fn job_object_enforces_output_timeout_and_cancellation() {
    let exe = std::env::current_exe().unwrap();
    let args = [
        "--ignored".into(),
        "--exact".into(),
        "job_child".into(),
        "--nocapture".into(),
    ];
    let out = native_job::run(&exe, &["--list".into()], &control(), 1024 * 1024).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("snapshot_usage"));
    assert!(native_job::run(&exe, &["--list".into()], &control(), 1).is_err());
    assert!(
        native_job::run(
            &exe,
            &args,
            &Control::new(CancellationToken::new(), Duration::from_millis(100)),
            100
        )
        .unwrap_err()
        .contains("超时")
    );
    let token = CancellationToken::new();
    let cancel = token.clone();
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    assert!(
        native_job::run(
            &exe,
            &args,
            &Control::new(token, Duration::from_secs(10)),
            100
        )
        .unwrap_err()
        .contains("取消")
    );
    trigger.join().unwrap();
}

#[test]
fn cancelled_owner_keeps_shared_scan_alive_for_remaining_waiter_without_rescan() {
    let mut mock = MockScan::new(mock_snapshot("C:\\", &[("C:\\", 12)], &[]));
    mock.delay = Duration::from_millis(150);
    let source = Arc::new(mock);
    let index = Arc::new(VolumeIndex::new(source.clone()));
    let cancel = CancellationToken::new();
    let owner_cancel = cancel.clone();
    let owner_index = index.clone();
    let owner = std::thread::spawn(move || {
        owner_index.scan(
            Path::new("C:\\"),
            true,
            &Control::new(owner_cancel, Duration::from_secs(5)),
        )
    });
    for _ in 0..1000 {
        if source.calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        cancel.cancel();
    });
    let result = index
        .scan(Path::new("C:\\"), true, &control())
        .unwrap()
        .unwrap();
    trigger.join().unwrap();
    assert!(owner.join().unwrap().is_err());
    assert_eq!(result.total, 12);
    // Remaining waiter finishes using the exact same single physical scan (no duplicate scan!).
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn follow_up_scan_immediately_after_all_waiters_cancel_never_joins_dying_flight() {
    let mut mock = MockScan::new(mock_snapshot("D:\\", &[("D:\\", 42)], &[]));
    mock.delay = Duration::from_millis(80);
    let source = Arc::new(mock);
    let index = VolumeIndex::new(source.clone());

    let tok = CancellationToken::new();
    let trig = tok.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(15));
        trig.cancel();
    });
    assert!(
        index
            .scan(
                Path::new("D:\\"),
                true,
                &Control::new(tok, Duration::from_secs(5))
            )
            .is_err()
    );
    h.join().unwrap();

    // Immediately re-scan while the cancelled worker may still be unwinding:
    // must start a fresh flight and succeed instead of joining the dying cancelled flight.
    let fresh = index
        .scan(Path::new("D:\\"), true, &control())
        .expect("follow-up scan must not inherit prior cancellation")
        .expect("fresh snapshot");
    assert_eq!(fresh.total, 42);
    assert!(index.cached(Path::new("D:\\")).is_some());
}

#[test]
fn walk_deadline_returns_lower_bound_and_cancellation_is_an_error() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("x"), [0u8; 10]).unwrap();
    let value = walk::usage(
        &fixture.0,
        5,
        &control(),
        walk::WalkLimits {
            entries: 500_000,
            timeout: Duration::ZERO,
        },
    )
    .unwrap();
    assert_eq!(value["truncated"], true);
    assert_eq!(value["totalGB"], 0.0);
    let token = CancellationToken::new();
    token.cancel();
    assert!(
        walk::usage(
            &fixture.0,
            5,
            &Control::new(token, Duration::from_secs(2)),
            walk::WalkLimits::default()
        )
        .is_err()
    );
}

#[test]
fn canonical_path_keys_ignore_case_separators_and_extended_prefix() {
    assert_eq!(path_key(Path::new(r"\\?\C:\Data/")), "C:\\DATA");
    assert_eq!(
        path_key(Path::new(r"E:\root\\sub\child\")),
        "E:\\ROOT\\SUB\\CHILD"
    );
    assert_eq!(display_path(Path::new(r"\\?\C:\")), "C:\\");
    assert_eq!(
        display_path(Path::new(r"\\?\UNC\server\share\dir")),
        r"\\server\share\dir"
    );
}

#[cfg(windows)]
#[test]
fn ls_continues_measuring_sibling_dirs_after_junction() {
    let fixture = Fixture::new();
    let external = Fixture::new();
    fs::write(external.0.join("secret"), [0u8; 500]).unwrap();
    let link = fixture.0.join("a_junction");
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(&external.0)
        .output()
        .unwrap();
    assert!(status.status.success());

    fs::create_dir(fixture.0.join("z_normal")).unwrap();
    fs::write(fixture.0.join("z_normal/data.bin"), [0u8; 1024]).unwrap();

    let mut mock = MockScan::new(scan::ScanSnapshot::default());
    mock.fail = true;
    let index = VolumeIndex::new(Arc::new(mock));
    let value = ls::run(
        &index,
        &fixture.0,
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert!(!value["path"].as_str().unwrap().starts_with(r"\\?\"));
    let entries = value["entries"].as_array().unwrap();
    let normal = entries
        .iter()
        .find(|e| e["name"] == "z_normal")
        .expect("z_normal entry present");
    assert_eq!(normal["bytes"], 1024);
    assert_eq!(normal["size"], "1.0 KB");
    let junc = entries
        .iter()
        .find(|e| e["name"] == "a_junction")
        .expect("a_junction entry present");
    assert!(junc["bytes"].is_null());
    fs::remove_dir(&link).unwrap();
}

#[cfg(windows)]
#[test]
fn native_walk_dedups_cross_dir_hardlinks_deterministically_across_all_rankings() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("dir_a")).unwrap();
    fs::create_dir_all(fixture.0.join("dir_b")).unwrap();
    let original = fixture.0.join("dir_a/shared.bin");
    fs::write(&original, vec![7u8; 200_000]).unwrap();
    fs::hard_link(&original, fixture.0.join("dir_b/alias.dat")).unwrap();
    fs::write(fixture.0.join("dir_b/own.txt"), vec![1u8; 50_000]).unwrap();

    let value = walk::usage(&fixture.0, 10, &control(), walk::WalkLimits::default()).unwrap();
    assert_eq!(value["method"], "native-walk");
    assert_eq!(value["truncated"], false);
    let dirs = value["topDirs"].as_array().unwrap();
    assert_eq!(dirs.len(), 2);
    assert!(dirs[0]["path"].as_str().unwrap().ends_with("dir_a"));
    assert_eq!(dirs[0]["pct"], 80.0);
    assert!(dirs[1]["path"].as_str().unwrap().ends_with("dir_b"));
    assert_eq!(dirs[1]["pct"], 20.0);
    let files = value["topFiles"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert!(files[0]["path"].as_str().unwrap().ends_with("shared.bin"));
    assert!(files[0]["modified"].as_str().unwrap().ends_with('Z'));
    let exts = value["extAgg"].as_array().unwrap();
    assert_eq!(exts.len(), 2);
    assert!(exts.iter().any(|e| e["ext"] == "bin" && e["files"] == 1));
    assert!(exts.iter().any(|e| e["ext"] == "txt" && e["files"] == 1));
}

// --- Synthetic NTFS $MFT binary builder for deterministic offline MFT tests ---
fn align8(n: usize) -> usize {
    (n + 7) & !7
}

fn push_resident_attr(rec: &mut [u8], pos: &mut usize, type_code: u32, payload: &[u8]) {
    let total = align8(24 + payload.len());
    rec[*pos..*pos + 4].copy_from_slice(&type_code.to_le_bytes());
    rec[*pos + 4..*pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
    rec[*pos + 8] = 0; // resident
    rec[*pos + 10..*pos + 12].copy_from_slice(&24u16.to_le_bytes());
    rec[*pos + 16..*pos + 20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    rec[*pos + 20..*pos + 22].copy_from_slice(&24u16.to_le_bytes());
    rec[*pos + 24..*pos + 24 + payload.len()].copy_from_slice(payload);
    *pos += total;
}

fn push_non_resident_data(rec: &mut [u8], pos: &mut usize, data_size: u64, alloc_size: u64) {
    let total = 72usize;
    rec[*pos..*pos + 4].copy_from_slice(&0x80u32.to_le_bytes());
    rec[*pos + 4..*pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
    rec[*pos + 8] = 1; // non-resident
    rec[*pos + 10..*pos + 12].copy_from_slice(&64u16.to_le_bytes());
    rec[*pos + 16..*pos + 24].copy_from_slice(&0u64.to_le_bytes()); // first VCN
    let last_vcn = alloc_size.saturating_sub(1) / 4096;
    rec[*pos + 24..*pos + 32].copy_from_slice(&last_vcn.to_le_bytes());
    rec[*pos + 32..*pos + 34].copy_from_slice(&64u16.to_le_bytes()); // mapping pairs offset
    rec[*pos + 40..*pos + 48].copy_from_slice(&alloc_size.to_le_bytes());
    rec[*pos + 48..*pos + 56].copy_from_slice(&data_size.to_le_bytes());
    rec[*pos + 56..*pos + 64].copy_from_slice(&data_size.to_le_bytes());
    rec[*pos + 64] = 0; // empty run list terminator
    *pos += total;
}

fn encode_std_info(modified_unix: u64) -> [u8; 48] {
    let mut b = [0u8; 48];
    let ticks = modified_unix * 10_000_000 + 116_444_736_000_000_000;
    b[0..8].copy_from_slice(&ticks.to_le_bytes());
    b[8..16].copy_from_slice(&ticks.to_le_bytes());
    b
}

fn encode_file_name(parent_rec: u64, parent_seq: u16, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut b = vec![0u8; 66 + units.len() * 2];
    let parent_ref = ((parent_seq as u64) << 48) | (parent_rec & 0x0000_FFFF_FFFF_FFFF);
    b[0..8].copy_from_slice(&parent_ref.to_le_bytes());
    b[64] = units.len() as u8;
    b[65] = 1; // Win32 namespace
    for (i, u) in units.iter().enumerate() {
        b[66 + i * 2..66 + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
    }
    b
}

fn build_mft_record(
    rec_num: u32,
    seq: u16,
    is_dir: bool,
    attrs_fn: impl FnOnce(&mut [u8], &mut usize),
) -> [u8; 1024] {
    let mut rec = [0u8; 1024];
    rec[0..4].copy_from_slice(b"FILE");
    rec[4..6].copy_from_slice(&48u16.to_le_bytes()); // USA offset
    rec[6..8].copy_from_slice(&3u16.to_le_bytes()); // USA count (1 USN + 2 sectors)
    rec[16..18].copy_from_slice(&seq.to_le_bytes());
    rec[18..20].copy_from_slice(&1u16.to_le_bytes());
    rec[20..22].copy_from_slice(&56u16.to_le_bytes()); // first attr offset
    let flags: u16 = if is_dir { 0x0003 } else { 0x0001 };
    rec[22..24].copy_from_slice(&flags.to_le_bytes());
    rec[28..32].copy_from_slice(&1024u32.to_le_bytes());
    rec[44..48].copy_from_slice(&rec_num.to_le_bytes());
    let mut pos = 56usize;
    attrs_fn(&mut rec, &mut pos);
    rec[pos..pos + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    pos += 4;
    rec[24..28].copy_from_slice(&(pos as u32).to_le_bytes());
    // Apply Update Sequence Array (USN = 1) at offsets 510 and 1022
    let usn = 1u16.to_le_bytes();
    let orig_s0 = [rec[510], rec[511]];
    let orig_s1 = [rec[1022], rec[1023]];
    rec[48..50].copy_from_slice(&usn);
    rec[50..52].copy_from_slice(&orig_s0);
    rec[52..54].copy_from_slice(&orig_s1);
    rec[510..512].copy_from_slice(&usn);
    rec[1022..1024].copy_from_slice(&usn);
    rec
}

#[test]
fn mft_synthetic_dump_verifies_fileref_prunes_reparse_rejects_damaged_and_dedups_stale_hardlink() {
    let mut dump = vec![0u8; 32 * 1024];
    let put = |buf: &mut [u8], rec_num: usize, rec: [u8; 1024]| {
        buf[rec_num * 1024..(rec_num + 1) * 1024].copy_from_slice(&rec);
    };

    // Record 5 (root, seq=5)
    put(
        &mut dump,
        5,
        build_mft_record(5, 5, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "."));
        }),
    );
    // Record 20: C:\dir_a (seq=2)
    put(
        &mut dump,
        20,
        build_mft_record(20, 2, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "dir_a"));
        }),
    );
    // Record 21: C:\dir_b (seq=3)
    put(
        &mut dump,
        21,
        build_mft_record(21, 3, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "dir_b"));
        }),
    );
    // Record 22: C:\junc_dir (seq=4, reparse point mount/junction -> must be pruned!)
    put(
        &mut dump,
        22,
        build_mft_record(22, 4, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "junc_dir"));
            let mut rp = [0u8; 8];
            rp[0..4].copy_from_slice(&0xA000_0003u32.to_le_bytes());
            push_resident_attr(r, p, 0xC0, &rp);
        }),
    );
    // Record 23: 60 MiB stale file hardlinked in BOTH dir_a (old.bin) AND dir_b (alias.dat)
    let stale_bytes = 60 * 1024 * 1024u64;
    put(
        &mut dump,
        23,
        build_mft_record(23, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800)); // 2020-01-01
            push_resident_attr(r, p, 0x30, &encode_file_name(20, 2, "old.bin"));
            push_resident_attr(r, p, 0x30, &encode_file_name(21, 3, "alias.dat"));
            push_non_resident_data(r, p, stale_bytes, stale_bytes);
        }),
    );
    // Record 24: 10 MiB file inside pruned junction dir -> MUST be excluded from totals!
    put(
        &mut dump,
        24,
        build_mft_record(24, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_700_000_000));
            push_resident_attr(r, p, 0x30, &encode_file_name(22, 4, "hidden.iso"));
            push_non_resident_data(r, p, 10 * 1024 * 1024, 10 * 1024 * 1024);
        }),
    );
    // Record 25: 5 MiB cloud-only file (IO_REPARSE_TAG_CLOUD, alloc=0) in dir_b -> contributes 0!
    put(
        &mut dump,
        25,
        build_mft_record(25, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_700_000_000));
            push_resident_attr(r, p, 0x30, &encode_file_name(21, 3, "cloud.docx"));
            let mut rp = [0u8; 8];
            rp[0..4].copy_from_slice(&0x9000_101Au32.to_le_bytes());
            push_resident_attr(r, p, 0xC0, &rp);
            push_non_resident_data(r, p, 5 * 1024 * 1024, 0);
        }),
    );

    let mft_ok = ntfs_ng::mft::Mft::from_file_with_record_size(dump.clone(), 1024).unwrap();
    let snap = mft::scan_mft_source("C:", &mft_ok, &control()).unwrap();
    // Total must equal 60 MiB (hardlink counted once in dir_a, junction descendant pruned, cloud-only=0)
    assert_eq!(snap.total, stale_bytes);
    assert_eq!(snap.stale.len(), 1);
    assert_eq!(snap.stale[0].path, r"C:\dir_a\old.bin");
    assert_eq!(snap.top_files.len(), 2);
    assert_eq!(snap.top_files[0].path, r"C:\dir_a\old.bin");

    // Verify FileReference (48-bit record + 16-bit sequence) + PathPolicy::LIVE cross-check:
    let file_id_dir_a = (2u64 << 48) | 20u64;
    let file_id_wrong_seq = (99u64 << 48) | 20u64;
    let file_id_junc = (4u64 << 48) | 22u64;
    assert_eq!(
        snap.lookup_dir(Some(file_id_dir_a), Path::new(r"C:\dir_a")),
        Some(stale_bytes)
    );
    assert_eq!(
        snap.lookup_dir(Some(file_id_wrong_seq), Path::new(r"C:\dir_a")),
        None,
        "sequence mismatch must reject cache hit"
    );
    assert_eq!(
        snap.lookup_dir(Some(file_id_dir_a), Path::new(r"C:\renamed_dir")),
        None,
        "path mismatch must reject cache hit"
    );
    assert_eq!(
        snap.lookup_dir(Some(file_id_junc), Path::new(r"C:\junc_dir")),
        None,
        "pruned reparse directory must never return cached bytes"
    );

    // Corrupt record 26 with invalid signature -> Summary.damaged > 0 must refuse publishing snapshot!
    dump[26 * 1024..26 * 1024 + 4].copy_from_slice(b"BAAD");
    let mft_bad = ntfs_ng::mft::Mft::from_file_with_record_size(dump, 1024).unwrap();
    let err = mft::scan_mft_source("C:", &mft_bad, &control())
        .err()
        .expect("damaged MFT record must fail integrity gate");
    assert!(err.contains("损坏"));
}

#[test]
fn sensitive_kit_path_predicates_match_cross_platform_and_avoid_false_positives() {
    assert!(scan::is_sensitive_dir_str(r"C:\kit\.state"));
    assert!(scan::is_sensitive_dir_str(r"C:\kit\.STATE\nested"));
    assert!(scan::is_sensitive_dir_str("/tmp/kit/.state/sub"));
    assert!(!scan::is_sensitive_dir_str(r"C:\kit\.state_backup"));

    for name in [
        "auth.json",
        "models.json",
        "mcp-auth.json",
        "web-search.json",
    ] {
        assert!(scan::is_sensitive_entry_str(r"C:\kit\data\config", name));
        assert!(scan::is_sensitive_entry_str(r"data\config", name));
        assert!(scan::is_sensitive_entry_str("./data/config/", name));
        assert!(scan::is_sensitive_entry_str(
            r"\\?\C:\kit\DATA\CONFIG",
            &name.to_uppercase()
        ));
        assert!(scan::is_sensitive_entry_str(r"C:\kit\agent\home", name));
        assert!(!scan::is_sensitive_entry_str(
            r"C:\kit\my-data\config",
            name
        ));
        assert!(!scan::is_sensitive_entry_str(
            r"C:\kit\data\config\sub",
            name
        ));
    }
    assert!(scan::is_sensitive_entry_str(
        r"C:\kit\data\telemetry",
        "identity.json"
    ));
    assert!(scan::is_sensitive_entry_str(
        r"C:\kit\agent\home\telemetry",
        "IDENTITY.JSON"
    ));
    assert!(!scan::is_sensitive_entry_str(
        r"C:\kit\data\config",
        "settings.json"
    ));
    assert!(!scan::is_sensitive_entry_str(
        r"C:\kit\data\telemetry",
        "events.json"
    ));
    assert!(scan::is_sensitive_entry_str(r"C:\kit", ".state"));
    assert!(scan::is_sensitive_entry_str(
        r"C:\kit\.state",
        "anything.bin"
    ));
    assert!(scan::is_sensitive_full_path(Path::new(
        "/kit/.state/secret.db"
    )));
    assert!(scan::is_sensitive_full_path(Path::new(
        "/kit/data/config/auth.json"
    )));
    assert!(!scan::is_sensitive_full_path(Path::new(
        "/kit/data/config/settings.json"
    )));
}

#[test]
fn walk_usage_and_ls_hide_sensitive_kit_paths_while_preserving_totals() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    fs::create_dir_all(root.join(".state/nested")).unwrap();
    fs::create_dir_all(root.join("data/config")).unwrap();
    fs::create_dir_all(root.join("data/telemetry")).unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();

    fs::write(root.join(".state/nested/secret.bin"), vec![1u8; 40_000]).unwrap();
    fs::write(root.join("data/config/auth.json"), vec![2u8; 20_000]).unwrap();
    fs::write(root.join("data/config/models.json"), vec![3u8; 5_000]).unwrap();
    fs::write(root.join("data/config/mcp-auth.json"), vec![4u8; 5_000]).unwrap();
    fs::write(root.join("data/config/web-search.json"), vec![5u8; 5_000]).unwrap();
    fs::write(root.join("data/config/settings.json"), vec![6u8; 2_000]).unwrap();
    fs::write(root.join("data/telemetry/identity.json"), vec![7u8; 10_000]).unwrap();
    fs::write(root.join("data/telemetry/events.json"), vec![8u8; 3_000]).unwrap();
    fs::write(root.join("notes/readme.txt"), vec![9u8; 10_000]).unwrap();
    // Total = 40_000 + 37_000 + 13_000 + 10_000 = 100_000 bytes.
    let expected_total = 100_000u64;

    // 1. walk::usage with small top=1 must still return non-sensitive items without eviction.
    let usage_top1 = walk::usage(root, 1, &control(), walk::WalkLimits::default()).unwrap();
    assert_eq!(usage_top1["totalGB"], scan::gib(expected_total));
    assert_eq!(usage_top1["topDirs"].as_array().unwrap().len(), 1);
    assert!(
        usage_top1["topDirs"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("data")
    );
    assert_eq!(usage_top1["topDirs"][0]["pct"], 50.0);
    assert_eq!(usage_top1["topFiles"].as_array().unwrap().len(), 1);
    assert!(
        usage_top1["topFiles"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("readme.txt")
    );

    // Full walk::usage must never expose any sensitive dir or file name.
    let usage = walk::usage(root, 20, &control(), walk::WalkLimits::default()).unwrap();
    let dir_paths: Vec<&str> = usage["topDirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    assert!(
        dir_paths
            .iter()
            .all(|p| !p.to_ascii_lowercase().contains(".state"))
    );
    let file_paths: Vec<&str> = usage["topFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(file_paths.len(), 3);
    assert!(file_paths.iter().any(|p| p.ends_with("readme.txt")));
    assert!(file_paths.iter().any(|p| p.ends_with("events.json")));
    assert!(file_paths.iter().any(|p| p.ends_with("settings.json")));

    // Direct walk::usage on `.state` hides all child rankings while preserving totalGB.
    let state_usage = walk::usage(
        &root.join(".state"),
        10,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(state_usage["totalGB"], scan::gib(40_000));
    assert!(state_usage["topDirs"].as_array().unwrap().is_empty());
    assert!(state_usage["topFiles"].as_array().unwrap().is_empty());

    // 2. ls::run on kit root hides `.state` while keeping its 40_000 bytes in totalSize.
    let mut mock = MockScan::new(scan::ScanSnapshot::default());
    mock.fail = true;
    let index = VolumeIndex::new(Arc::new(mock));
    let root_ls = ls::run(&index, root, 20, &control(), walk::WalkLimits::default()).unwrap();
    assert_eq!(root_ls["totalChildren"], 2);
    assert_eq!(root_ls["totalSize"], scan::format_bytes(expected_total));
    let root_entries = root_ls["entries"].as_array().unwrap();
    assert_eq!(root_entries.len(), 2);
    assert!(root_entries.iter().all(|e| e["name"] != ".state"));
    assert_eq!(root_entries[0]["name"], "data");
    assert_eq!(root_entries[0]["bytes"], 50_000);
    assert_eq!(root_entries[0]["pct"], 50.0);
    assert_eq!(root_entries[1]["name"], "notes");
    assert_eq!(root_entries[1]["bytes"], 10_000);
    assert_eq!(root_entries[1]["pct"], 10.0);
    assert!(root_ls["notice"].as_str().unwrap().contains("敏感路径名"));

    // 3. ls::run on data/config hides the 4 secret configs while showing settings.json.
    let cfg_ls = ls::run(
        &index,
        &root.join("data/config"),
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(cfg_ls["totalChildren"], 1);
    assert_eq!(cfg_ls["totalSize"], scan::format_bytes(37_000));
    assert_eq!(cfg_ls["entries"].as_array().unwrap().len(), 1);
    assert_eq!(cfg_ls["entries"][0]["name"], "settings.json");
    assert_eq!(cfg_ls["entries"][0]["bytes"], 2_000);
    assert!(cfg_ls["notice"].as_str().unwrap().contains("敏感路径名"));

    // 4. ls::run on data/telemetry hides identity.json while showing events.json.
    let tel_ls = ls::run(
        &index,
        &root.join("data/telemetry"),
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(tel_ls["totalChildren"], 1);
    assert_eq!(tel_ls["totalSize"], scan::format_bytes(13_000));
    assert_eq!(tel_ls["entries"][0]["name"], "events.json");

    // 5. ls::run directly on `.state` returns 0 entries while keeping totalSize.
    let state_ls = ls::run(
        &index,
        &root.join(".state"),
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(state_ls["totalChildren"], 0);
    assert_eq!(state_ls["totalSize"], scan::format_bytes(40_000));
    assert!(state_ls["entries"].as_array().unwrap().is_empty());
    assert!(state_ls["notice"].as_str().unwrap().contains("敏感路径名"));

    // 6. ls::run on normal directory emits no sensitive-path notice.
    let notes_ls = ls::run(
        &index,
        &root.join("notes"),
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert!(notes_ls.get("notice").is_none());

    // 7. ls::run via `native-index` cached snapshot also hides `.state` while keeping its bytes.
    #[cfg(windows)]
    {
        let root_s = display_path(root);
        let state_s = display_path(&root.join(".state"));
        let data_s = display_path(&root.join("data"));
        let notes_s = display_path(&root.join("notes"));
        let snap = mock_snapshot(
            &root_s,
            &[
                (&root_s, expected_total),
                (&state_s, 40_000),
                (&data_s, 50_000),
                (&notes_s, 10_000),
            ],
            &[],
        );
        let cached_index = VolumeIndex::new(Arc::new(MockScan::new(snap)));
        cached_index.scan(root, true, &control()).unwrap();
        let cached_ls = ls::run(
            &cached_index,
            root,
            20,
            &control(),
            walk::WalkLimits::default(),
        )
        .unwrap();
        assert_eq!(cached_ls["method"], "native-index");
        assert_eq!(cached_ls["totalChildren"], 2);
        assert_eq!(cached_ls["totalSize"], scan::format_bytes(expected_total));
        assert!(
            cached_ls["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["name"] != ".state")
        );
        assert!(cached_ls["notice"].as_str().unwrap().contains("敏感路径名"));
    }
}

#[test]
fn mft_synthetic_dump_hides_sensitive_kit_paths_while_preserving_totals() {
    let mut dump = vec![0u8; 40 * 1024];
    let put = |buf: &mut [u8], rec_num: usize, rec: [u8; 1024]| {
        buf[rec_num * 1024..(rec_num + 1) * 1024].copy_from_slice(&rec);
    };

    put(
        &mut dump,
        5,
        build_mft_record(5, 5, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "."));
        }),
    );
    put(
        &mut dump,
        20,
        build_mft_record(20, 1, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, ".state"));
        }),
    );
    put(
        &mut dump,
        21,
        build_mft_record(21, 1, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "data"));
        }),
    );
    put(
        &mut dump,
        22,
        build_mft_record(22, 1, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(21, 1, "config"));
        }),
    );
    put(
        &mut dump,
        23,
        build_mft_record(23, 1, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(21, 1, "telemetry"));
        }),
    );
    let mib = 1024 * 1024u64;
    // Sensitive stale files: 80 MiB in .state, 70 MiB auth.json, 65 MiB identity.json.
    put(
        &mut dump,
        24,
        build_mft_record(24, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800));
            push_resident_attr(r, p, 0x30, &encode_file_name(20, 1, "runtime.db"));
            push_non_resident_data(r, p, 80 * mib, 80 * mib);
        }),
    );
    put(
        &mut dump,
        25,
        build_mft_record(25, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800));
            push_resident_attr(r, p, 0x30, &encode_file_name(22, 1, "auth.json"));
            push_non_resident_data(r, p, 70 * mib, 70 * mib);
        }),
    );
    put(
        &mut dump,
        26,
        build_mft_record(26, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800));
            push_resident_attr(r, p, 0x30, &encode_file_name(23, 1, "identity.json"));
            push_non_resident_data(r, p, 65 * mib, 65 * mib);
        }),
    );
    // Non-sensitive stale file: 60 MiB settings.json.
    put(
        &mut dump,
        27,
        build_mft_record(27, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800));
            push_resident_attr(r, p, 0x30, &encode_file_name(22, 1, "settings.json"));
            push_non_resident_data(r, p, 60 * mib, 60 * mib);
        }),
    );

    let mft_ok = ntfs_ng::mft::Mft::from_file_with_record_size(dump, 1024).unwrap();
    let snap = mft::scan_mft_source("C:", &mft_ok, &control()).unwrap();
    assert_eq!(snap.total, (80 + 70 + 65 + 60) * mib);
    assert!(
        snap.top_dirs
            .iter()
            .all(|d| !d.path.to_ascii_lowercase().contains(".state"))
    );
    assert_eq!(snap.top_files.len(), 1);
    assert_eq!(snap.top_files[0].path, r"C:\data\config\settings.json");
    assert_eq!(snap.stale.len(), 1);
    assert_eq!(snap.stale[0].path, r"C:\data\config\settings.json");
    // Real MftIndex FileReference (48-bit record 20 + 16-bit seq 1) still resolves `.state`
    // bytes so `ls` `native-index` keeps `.state` volume in `totalSize` while hiding the name.
    let state_ref = (1u64 << 48) | 20u64;
    assert_eq!(
        snap.lookup_dir(Some(state_ref), Path::new(r"C:\.state")),
        Some(80 * mib)
    );
}

#[test]
fn custom_data_dir_layout_hides_credentials_across_walk_ls_and_mft() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    let custom_data = root.join("profile");
    fs::create_dir_all(root.join(".state")).unwrap();
    fs::create_dir_all(custom_data.join("config")).unwrap();
    fs::create_dir_all(custom_data.join("telemetry")).unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();

    fs::write(root.join(".state/session.lock"), vec![1u8; 30_000]).unwrap();
    fs::write(custom_data.join("config/auth.json"), vec![2u8; 25_000]).unwrap();
    fs::write(custom_data.join("config/models.json"), vec![3u8; 5_000]).unwrap();
    fs::write(custom_data.join("config/settings.json"), vec![4u8; 4_000]).unwrap();
    fs::write(
        custom_data.join("telemetry/identity.json"),
        vec![5u8; 16_000],
    )
    .unwrap();
    fs::write(custom_data.join("telemetry/events.json"), vec![6u8; 6_000]).unwrap();
    fs::write(root.join("notes/readme.txt"), vec![7u8; 14_000]).unwrap();
    let expected_total = 100_000u64;

    let filter = scan::SensitivePathFilter::from_kit_dirs(
        Some(&root.join(".state")),
        Some(&custom_data.join("config")),
        Some(&custom_data.join("telemetry")),
    );

    // 1. Scanning parent root and custom data root via walk::usage_with_filter hides credentials.
    let parent_usage =
        walk::usage_with_filter(root, 20, &control(), walk::WalkLimits::default(), &filter)
            .unwrap();
    assert_eq!(parent_usage["totalGB"], scan::gib(expected_total));
    let parent_files: Vec<&str> = parent_usage["topFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(parent_files.len(), 3);
    assert!(parent_files.iter().any(|p| p.ends_with("readme.txt")));
    assert!(parent_files.iter().any(|p| p.ends_with("events.json")));
    assert!(parent_files.iter().any(|p| p.ends_with("settings.json")));

    let data_usage = walk::usage_with_filter(
        &custom_data,
        20,
        &control(),
        walk::WalkLimits::default(),
        &filter,
    )
    .unwrap();
    assert_eq!(data_usage["totalGB"], scan::gib(56_000));
    let data_files: Vec<&str> = data_usage["topFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(data_files.len(), 2);
    assert!(data_files.iter().any(|p| p.ends_with("events.json")));
    assert!(data_files.iter().any(|p| p.ends_with("settings.json")));

    // 2. ls::run_with_filter on custom config & telemetry dirs hides secrets while keeping totals.
    let mut mock = MockScan::new(scan::ScanSnapshot::default());
    mock.fail = true;
    let index = VolumeIndex::new(Arc::new(mock));
    let cfg_ls = ls::run_with_filter(
        &index,
        &custom_data.join("config"),
        20,
        &control(),
        walk::WalkLimits::default(),
        &filter,
    )
    .unwrap();
    assert_eq!(cfg_ls["totalChildren"], 1);
    assert_eq!(cfg_ls["totalSize"], scan::format_bytes(34_000));
    assert_eq!(cfg_ls["entries"][0]["name"], "settings.json");
    assert!(cfg_ls["notice"].as_str().unwrap().contains("敏感路径名"));

    let tel_ls = ls::run_with_filter(
        &index,
        &custom_data.join("telemetry"),
        20,
        &control(),
        walk::WalkLimits::default(),
        &filter,
    )
    .unwrap();
    assert_eq!(tel_ls["totalChildren"], 1);
    assert_eq!(tel_ls["totalSize"], scan::format_bytes(22_000));
    assert_eq!(tel_ls["entries"][0]["name"], "events.json");
    assert!(tel_ls["notice"].as_str().unwrap().contains("敏感路径名"));

    // 3. Synthetic $MFT scan with custom `C:\profile\config` and `C:\profile\telemetry`.
    let mut dump = vec![0u8; 40 * 1024];
    let put = |buf: &mut [u8], rec_num: usize, rec: [u8; 1024]| {
        buf[rec_num * 1024..(rec_num + 1) * 1024].copy_from_slice(&rec);
    };
    put(
        &mut dump,
        5,
        build_mft_record(5, 5, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "."));
        }),
    );
    put(
        &mut dump,
        20,
        build_mft_record(20, 1, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(5, 5, "profile"));
        }),
    );
    put(
        &mut dump,
        21,
        build_mft_record(21, 1, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(20, 1, "config"));
        }),
    );
    put(
        &mut dump,
        22,
        build_mft_record(22, 1, true, |r, p| {
            push_resident_attr(r, p, 0x30, &encode_file_name(20, 1, "telemetry"));
        }),
    );
    let mib = 1024 * 1024u64;
    put(
        &mut dump,
        23,
        build_mft_record(23, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800));
            push_resident_attr(r, p, 0x30, &encode_file_name(21, 1, "auth.json"));
            push_non_resident_data(r, p, 90 * mib, 90 * mib);
        }),
    );
    put(
        &mut dump,
        24,
        build_mft_record(24, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800));
            push_resident_attr(r, p, 0x30, &encode_file_name(22, 1, "identity.json"));
            push_non_resident_data(r, p, 70 * mib, 70 * mib);
        }),
    );
    put(
        &mut dump,
        25,
        build_mft_record(25, 1, false, |r, p| {
            push_resident_attr(r, p, 0x10, &encode_std_info(1_577_836_800));
            push_resident_attr(r, p, 0x30, &encode_file_name(21, 1, "settings.json"));
            push_non_resident_data(r, p, 60 * mib, 60 * mib);
        }),
    );
    let mft_filter = scan::SensitivePathFilter::from_kit_dirs(
        None,
        Some(Path::new(r"C:\profile\config")),
        Some(Path::new(r"C:\profile\telemetry")),
    );
    let mft_ok = ntfs_ng::mft::Mft::from_file_with_record_size(dump, 1024).unwrap();
    let snap = mft::scan_mft_source_with_filter("C:", &mft_ok, &control(), &mft_filter).unwrap();
    assert_eq!(snap.total, (90 + 70 + 60) * mib);
    assert_eq!(snap.top_files.len(), 1);
    assert_eq!(snap.top_files[0].path, r"C:\profile\config\settings.json");
    assert_eq!(snap.stale.len(), 1);
    assert_eq!(snap.stale[0].path, r"C:\profile\config\settings.json");
}

#[test]
fn ls_combined_incomplete_and_sensitive_notice_reports_known_bytes() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    fs::create_dir_all(root.join(".state")).unwrap();
    fs::create_dir_all(root.join("big")).unwrap();
    fs::write(root.join(".state/s.bin"), vec![1u8; 1024]).unwrap();
    fs::write(root.join("big/a.bin"), vec![2u8; 1024]).unwrap();
    fs::write(root.join("big/b.bin"), vec![3u8; 1024]).unwrap();
    let mut mock = MockScan::new(scan::ScanSnapshot::default());
    mock.fail = true;
    let index = VolumeIndex::new(Arc::new(mock));
    // Budget of 2 entries enumerates direct children (`.state` and `big`), exhausting budget
    // before measuring subdirectories -> both `budget.incomplete` and `hidden_sensitive` are true.
    let res = ls::run(
        &index,
        root,
        20,
        &control(),
        walk::WalkLimits {
            entries: 2,
            timeout: Duration::from_secs(5),
        },
    )
    .unwrap();
    assert!(
        res["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["name"] != ".state")
    );
    assert!(
        res["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["pct"].is_null())
    );
    let notice = res["notice"].as_str().unwrap();
    assert!(notice.contains("下界"));
    assert!(notice.contains("其已知大小仍计入 totalSize"));
}

#[cfg(windows)]
#[test]
fn ls_dedups_same_directory_hardlinks_including_hidden_sensitive_and_cached() {
    // 1. Pure-file directory with two hardlinks to the same file -> counted once, pct sums to 100.
    let pure = Fixture::new();
    let a = pure.0.join("a.bin");
    fs::write(&a, vec![1u8; 10_000]).unwrap();
    fs::hard_link(&a, pure.0.join("b.bin")).unwrap();
    let index = VolumeIndex::new_production(Arc::new(MockScan::new(scan::ScanSnapshot::default())));
    let res = ls::run(&index, &pure.0, 20, &control(), walk::WalkLimits::default()).unwrap();
    assert_eq!(res["totalSize"], scan::format_bytes(10_000));
    let entries = res["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["name"], "a.bin");
    assert_eq!(entries[0]["bytes"], 10_000);
    assert_eq!(entries[0]["pct"], 100.0);
    assert_eq!(entries[1]["name"], "b.bin");
    assert_eq!(entries[1]["bytes"], 0);
    assert_eq!(entries[1]["pct"], 0.0);

    // 2. Hidden sensitive `auth.json` hardlinked to `z_alias.json` inside `data/config` -> counted once.
    let cfg_fixture = Fixture::new();
    let cfg = cfg_fixture.0.join("data/config");
    fs::create_dir_all(&cfg).unwrap();
    let auth = cfg.join("auth.json");
    fs::write(&auth, vec![2u8; 10_000]).unwrap();
    fs::hard_link(&auth, cfg.join("z_alias.json")).unwrap();
    fs::write(cfg.join("settings.json"), vec![3u8; 10_000]).unwrap();
    let cfg_res = ls::run(&index, &cfg, 20, &control(), walk::WalkLimits::default()).unwrap();
    assert_eq!(cfg_res["totalSize"], scan::format_bytes(20_000));
    let cfg_entries = cfg_res["entries"].as_array().unwrap();
    assert!(cfg_entries.iter().all(|e| e["name"] != "auth.json"));
    let settings = cfg_entries
        .iter()
        .find(|e| e["name"] == "settings.json")
        .unwrap();
    let alias = cfg_entries
        .iter()
        .find(|e| e["name"] == "z_alias.json")
        .unwrap();
    assert_eq!(settings["bytes"], 10_000);
    assert_eq!(settings["pct"], 50.0);
    assert_eq!(alias["bytes"], 0);
    assert_eq!(alias["pct"], 0.0);

    // 3. Warm `native-index` branch also deduplicates same-directory direct-file hardlinks.
    let cached_fixture = Fixture::new();
    fs::create_dir_all(cached_fixture.0.join("sub")).unwrap();
    let h1 = cached_fixture.0.join("first.bin");
    fs::write(&h1, vec![4u8; 10_000]).unwrap();
    fs::hard_link(&h1, cached_fixture.0.join("second.bin")).unwrap();
    let root_s = display_path(&cached_fixture.0);
    let sub_s = display_path(&cached_fixture.0.join("sub"));
    let snap = mock_snapshot(&root_s, &[(&root_s, 30_000), (&sub_s, 20_000)], &[]);
    let cached_index = VolumeIndex::new(Arc::new(MockScan::new(snap)));
    cached_index
        .scan(&cached_fixture.0, true, &control())
        .unwrap();
    let cached_res = ls::run(
        &cached_index,
        &cached_fixture.0,
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(cached_res["method"], "native-index");
    assert_eq!(cached_res["totalSize"], scan::format_bytes(30_000));
    let ce = cached_res["entries"].as_array().unwrap();
    assert_eq!(
        ce.iter().find(|e| e["name"] == "first.bin").unwrap()["bytes"],
        10_000
    );
    assert_eq!(
        ce.iter().find(|e| e["name"] == "second.bin").unwrap()["bytes"],
        0
    );
}

#[cfg(not(windows))]
#[test]
fn unix_literal_backslash_dirname_is_not_misclassified_as_sensitive() {
    let filter = scan::SensitivePathFilter::default();
    assert!(!filter.is_sensitive_dir_path(Path::new(r"/tmp/foo\.state")));
    assert!(!filter.is_sensitive_entry_path(Path::new(r"/tmp/foo\data\config"), "auth.json"));
    assert!(!filter.is_sensitive_full_path(Path::new(r"/tmp/foo\.state/bar.txt")));
}

use cst_pilot::diagnostics::win_native::Control;
use cst_pilot::diagnostics::wz_index::{ScanSource, VolumeIdentity, WzIndex, path_key};
use cst_pilot::diagnostics::{ls, scan, walk};
#[cfg(windows)]
use cst_pilot::diagnostics::{native_job, volume::NativeScanSource};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn control() -> Control {
    Control::new(CancellationToken::new(), Duration::from_secs(20))
}
fn id(serial: u32) -> VolumeIdentity {
    VolumeIdentity {
        serial,
        filesystem: "NTFS".into(),
        root_created: 100 + serial as u64,
    }
}
struct MockScan {
    identity: Mutex<Option<VolumeIdentity>>,
    csv: String,
    calls: AtomicUsize,
    delay: Duration,
    switch: bool,
    fail: bool,
}
impl MockScan {
    fn new(csv: String) -> Self {
        Self {
            identity: Mutex::new(Some(id(1))),
            csv,
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
    fn export(&self, _: &Path, control: &Control) -> Result<Box<dyn Read + Send>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        control.sleep(self.delay)?;
        if self.switch {
            *self.identity.lock().unwrap() = Some(id(2));
        }
        if self.fail {
            Err("模拟 WizTree 失败。".into())
        } else {
            Ok(Box::new(Cursor::new(self.csv.clone().into_bytes())))
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
fn row(path: &str, bytes: u64) -> String {
    format!("\"{path}\",{bytes},0,2020/01/01 00:00:00\r\n")
}
fn parse(csv: String, root: &str) -> Result<scan::ScanSnapshot, String> {
    scan::parse_export(
        Box::new(Cursor::new(csv.into_bytes())),
        Path::new(root),
        &control(),
    )
}

#[test]
fn csv_streams_quoted_commas_topn_extensions_stale_and_zero_root() {
    let csv = format!(
        "\u{feff}File Name,Size,Allocated,Modified\r\n{}{}{}{}",
        row("C:\\", 200 * 1024 * 1024),
        row("C:\\a,b\\", 100 * 1024 * 1024),
        row("C:\\a,b\\large.BIN", 100 * 1024 * 1024),
        row("C:\\tiny", 1)
    );
    let result = parse(csv, "C:\\").unwrap();
    assert_eq!(result.rows, 4);
    assert_eq!(result.file_count, 2);
    assert_eq!(result.top_files[0].path, "C:\\a,b\\large.BIN");
    assert_eq!(result.files.len(), 1);
    assert_eq!(result.stale.len(), 1);
    assert_eq!(result.extensions["bin"], (100 * 1024 * 1024, 1));
    let usage = result.usage(Path::new("C:\\"), 1, Some("exFAT"));
    assert_eq!(usage["method"], "wiztree-walk");
    assert_eq!(usage["topFiles"][0]["pct"], 50.0);
    let zero = parse(row("C:\\", 0), "C:\\")
        .unwrap()
        .usage(Path::new("C:\\"), 20, Some("NTFS"));
    assert_eq!(zero["totalGB"], 0.0);
    assert!(scan::gib(1) > 0.0);
}
#[test]
fn csv_rejects_oversize_outside_missing_root_and_duplicate_directory() {
    assert!(parse("x".repeat(70_000), "C:\\").is_err());
    assert!(parse(row("D:\\", 1), "C:\\").is_err());
    assert!(parse(row("C:\\x", 1), "C:\\").is_err());
    assert!(parse(format!("{}{}", row("C:\\", 1), row("C:\\", 1)), "C:\\").is_err());
    assert!(parse(row("C:\\..\\secret", 1), "C:\\").is_err());
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
fn volume_change_and_unidentified_volume_invalidate_cache_and_failures() {
    let source = Arc::new(MockScan::new(row("C:\\", 12)));
    let index = WzIndex::new(source.clone());
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
fn identity_change_during_export_never_publishes() {
    let mut mock = MockScan::new(row("C:\\", 12));
    mock.switch = true;
    let index = WzIndex::new(Arc::new(mock));
    assert!(index.scan(Path::new("C:\\"), true, &control()).is_err());
    assert!(index.cached(Path::new("C:\\")).is_none());
}
#[test]
fn concurrent_same_volume_requests_share_one_scan() {
    let mut mock = MockScan::new(row("C:\\", 12));
    mock.delay = Duration::from_millis(100);
    let source = Arc::new(mock);
    let index = Arc::new(WzIndex::new(source.clone()));
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
fn failed_export_is_not_repeated_until_volume_changes_and_cancel_does_not_poison() {
    let mut mock = MockScan::new(row("C:\\", 12));
    mock.fail = true;
    let source = Arc::new(mock);
    let index = WzIndex::new(source.clone());
    assert!(index.scan(Path::new("C:\\"), false, &control()).is_err());
    assert!(
        index
            .scan(Path::new("C:\\"), false, &control())
            .unwrap()
            .is_none()
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    *source.identity.lock().unwrap() = Some(id(2));
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
fn subtree_refresh_removes_stale_descendants_and_invalidates_ancestor_totals() {
    let mut first = parse(
        format!(
            "{}{}{}",
            row("C:\\", 100),
            row("C:\\data\\", 100),
            row("C:\\data\\old.bin", 2 * 1024 * 1024)
        ),
        "C:\\",
    )
    .unwrap();
    first.identity = Some(id(1));
    let next = parse(
        format!(
            "{}{}",
            row("C:\\data\\", 20),
            row("C:\\data\\new.bin", 3 * 1024 * 1024)
        ),
        "C:\\data",
    )
    .unwrap();
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
    assert_eq!(value["method"], "node-walk");
    assert_eq!(value["topDirs"].as_array().unwrap().len(), 2);
    assert_eq!(value["topDirs"][0]["pct"], 83.3);
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
fn ls_preserves_omitted_counts_and_never_publishes_walk() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("dir")).unwrap();
    fs::write(fixture.0.join("dir/x"), [0u8; 30]).unwrap();
    fs::write(fixture.0.join("file"), [0u8; 10]).unwrap();
    let mut mock = MockScan::new(String::new());
    mock.fail = true;
    let index = WzIndex::new(Arc::new(mock));
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
fn ls_uses_complete_shared_index_and_large_file_fallback() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("dir")).unwrap();
    fs::write(fixture.0.join("pagefile.sys"), []).unwrap();
    let csv = format!(
        "{}{}{}",
        row(&format!("{}\\", fixture.0.display()), 3 * 1024 * 1024),
        row(
            &format!("{}\\", fixture.0.join("dir").display()),
            1024 * 1024
        ),
        row(
            &fixture.0.join("pagefile.sys").to_string_lossy(),
            2 * 1024 * 1024
        )
    );
    let source = Arc::new(MockScan::new(csv));
    let index = WzIndex::new(source.clone());
    index.scan(&fixture.0, true, &control()).unwrap();
    let value = ls::run(
        &index,
        &fixture.0,
        20,
        &control(),
        walk::WalkLimits::default(),
    )
    .unwrap();
    assert_eq!(value["method"], "wiztree-index");
    assert_eq!(value["entries"][0]["bytes"], 2 * 1024 * 1024);
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn unc_never_builds_a_volume_index() {
    let source = Arc::new(MockScan::new(String::new()));
    let index = WzIndex::new(source.clone());
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
#[cfg(windows)]
#[test]
fn native_temp_junction_is_rejected_before_any_export() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("wiztree")).unwrap();
    fs::write(fixture.0.join("wiztree/WizTree64.exe"), []).unwrap();
    let external = Fixture::new();
    let link = fixture.0.join("wiztree/tmp");
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&link)
        .arg(&external.0)
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(
        NativeScanSource::new(&fixture.0)
            .export(&fixture.0, &control())
            .is_err()
    );
    assert_eq!(fs::read_dir(&external.0).unwrap().count(), 0);
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
    // Run another copy of this test binary; no system configuration or hardware is touched.
    let exe = std::env::current_exe().unwrap();
    let args = [
        "--ignored".into(),
        "--exact".into(),
        "job_child".into(),
        "--nocapture".into(),
    ];
    let out = native_job::run(&exe, &["--list".into()], &control(), 1024 * 1024).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("csv_streams"));
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
fn cancelled_owner_releases_pending_without_poisoning_other_waiters() {
    let mut mock = MockScan::new(row("C:\\", 12));
    mock.delay = Duration::from_millis(150);
    let source = Arc::new(mock);
    let index = Arc::new(WzIndex::new(source.clone()));
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
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}
#[test]
fn csv_reader_is_released_on_success_failure_and_cancel() {
    struct Reader {
        data: Cursor<Vec<u8>>,
        drops: Arc<AtomicUsize>,
    }
    impl Read for Reader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.data.read(bytes)
        }
    }
    impl Drop for Reader {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    for (csv, cancelled) in [
        (row("C:\\", 0), false),
        ("bad data".into(), false),
        (row("C:\\", 0), true),
    ] {
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        let _ = scan::parse_export(
            Box::new(Reader {
                data: Cursor::new(csv.into_bytes()),
                drops: drops.clone(),
            }),
            Path::new("C:\\"),
            &Control::new(token, Duration::from_secs(1)),
        );
    }
    assert_eq!(drops.load(Ordering::SeqCst), 3);
    assert!(parse(format!("{}\"C:\\x\",oops\r\n", row("C:\\", 0)), "C:\\").is_err());
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
}

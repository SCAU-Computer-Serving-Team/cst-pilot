#![cfg(windows)]

//! Real Windows reliability test suite (`L2`) for all 17 open tools in `cst-kit`.
//!
//! Run manually on a Windows host:
//! ```powershell
//! $keep = [Math]::Min(62, [Math]::Max(1, [Environment]::ProcessorCount - 2))
//! (Get-Process -Id $PID).ProcessorAffinity = [IntPtr](([int64]1 -shl $keep) - 1)
//! cargo test -p cst-kit --test live_windows_tools --locked -- --ignored --nocapture
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cst_kit::bitlocker::{
    Executor as BitLockerExecutor, core::BitLockerSource, credentials::Vault,
    model as bitlocker_model, wmi_encryptable::WindowsBitLocker,
};
use cst_kit::diagnostics::hardware::NativeHardware;
use cst_kit::diagnostics::volume::NativeScanSource;
use cst_kit::diagnostics::volume_index::VolumeIndex;
use cst_kit::diagnostics::walk::WalkLimits;
use cst_kit::diagnostics::win_native::{Control, WindowsSource};
use cst_kit::diagnostics::{disk, driver, eventlog, ls, startup, sys};
use cst_kit::runbook::RunbookStore;
use cst_kit::runbook::model::{DeliverInput, Level, PlanInput, RunbookInput, RunbookItem, Shell};
use cst_kit::tools::fff::{SearchInput, search as fff_search};
use cst_kit::tools::path_guard::{GuardedReadOperations, PathGuard};
use cst_kit::tools::ssrf::{HttpRequest, HttpResponse, HttpTransport};
use cst_kit::tools::web_access::{
    ContentInput, FetchInput, SearchInput as WebSearchInput, WebAccess,
};
use cst_kit::{OPEN_TOOL_NAMES, SKILL_NAMES};
use pi_runtime::ReadOperations;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct TempKit {
    root: PathBuf,
}

impl TempKit {
    fn new(tag: &str) -> Self {
        let base = if Path::new(r"E:\tmp").is_dir() {
            PathBuf::from(r"E:\tmp\2026-11-23\live-tools")
        } else {
            std::env::temp_dir().join("cst-pilot-live-tools")
        };
        let root = base.join(format!("{tag}-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("app/prompts")).unwrap();
        fs::create_dir_all(root.join("data/config")).unwrap();
        fs::create_dir_all(root.join("data/telemetry")).unwrap();
        fs::create_dir_all(root.join(".state")).unwrap();
        fs::write(root.join("VERSION"), "cst-pilot v0.9.5\n").unwrap();
        fs::write(root.join("app/prompts/APPEND_SYSTEM.md"), "test").unwrap();
        for skill in SKILL_NAMES {
            let p = root.join(format!("app/skills/{skill}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(
                p,
                format!("---\nname: {skill}\ndescription: {skill}\n---\n"),
            )
            .unwrap();
        }
        Self { root }
    }
}

impl Drop for TempKit {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn control(secs: u64) -> Control {
    Control::new(CancellationToken::new(), Duration::from_secs(secs))
}

#[test]
#[ignore = "live Windows API reliability test"]
fn live_disk_space_info_health_all_and_usage() {
    let source = WindowsSource;

    // 1. disk(space) all volumes
    let t0 = Instant::now();
    let space = disk::run(
        &source,
        disk::DiskInput {
            scope: disk::DiskScope::Space,
            drive: None,
            path: None,
            top: None,
        },
        &control(30),
    )
    .expect("disk(space) should succeed");
    let space_ms = t0.elapsed().as_millis();
    let rows = space["space"].as_array().expect("space array");
    assert!(!rows.is_empty(), "expected at least one volume");
    for row in rows {
        let total = row["totalGB"].as_f64().unwrap();
        let free = row["freeGB"].as_f64().unwrap();
        assert!(free <= total + 0.1, "freeGB ({free}) > totalGB ({total})");
        if let Some(pct) = row["usedPct"].as_f64() {
            assert!((0.0..=100.0).contains(&pct), "usedPct out of bounds: {pct}");
        }
    }
    assert!(space_ms < 5_000, "disk(space) too slow: {space_ms}ms");
    println!(
        "[live] disk(space): {} volumes in {}ms (degraded={:?})",
        rows.len(),
        space_ms,
        space.get("degraded")
    );

    // 2. disk(info, drive="C:")
    let t1 = Instant::now();
    let info = disk::run(
        &source,
        disk::DiskInput {
            scope: disk::DiskScope::Info,
            drive: Some("C:".into()),
            path: None,
            top: None,
        },
        &control(30),
    )
    .expect("disk(info, C:) should succeed");
    let info_ms = t1.elapsed().as_millis();
    let vols = info["volumes"].as_array().expect("volumes array");
    assert_eq!(vols.len(), 1, "drive=C: should return exactly 1 volume");
    assert_eq!(vols[0]["drive"].as_str(), Some("C:"));
    assert!(
        !info["physicalDisks"].as_array().unwrap().is_empty(),
        "physicalDisks should not be empty"
    );
    println!(
        "[live] disk(info, C:): {} physicalDisks in {}ms (infoNotice={:?})",
        info["physicalDisks"].as_array().unwrap().len(),
        info_ms,
        info.get("infoNotice")
    );

    // 3. disk(health) & disk(all)
    let t2 = Instant::now();
    let health = disk::run(
        &source,
        disk::DiskInput {
            scope: disk::DiskScope::Health,
            drive: None,
            path: None,
            top: None,
        },
        &control(30),
    )
    .expect("disk(health) should succeed");
    let health_ms = t2.elapsed().as_millis();
    assert!(
        health["smartNotice"].is_string(),
        "smartNotice must be present"
    );
    if let Some(smart_rows) = health["smart"].as_array() {
        let allowed = std::collections::BTreeSet::from([
            "DeviceId",
            "Wear",
            "Temperature",
            "PowerOnHours",
            "ReadErrorsTotal",
            "WriteErrorsTotal",
        ]);
        for row in smart_rows {
            let keys: std::collections::BTreeSet<&str> = row
                .as_object()
                .expect("smart row must be object")
                .keys()
                .map(String::as_str)
                .collect();
            assert!(
                keys.is_subset(&allowed),
                "unexpected smart keys outside API.md contract: {keys:?}"
            );
        }
    }
    println!(
        "[live] disk(health): smart_rows={} errors={} in {}ms",
        health["smart"].as_array().map_or(0, Vec::len),
        health["smartErrors"].as_array().map_or(0, Vec::len),
        health_ms
    );

    let all = disk::run(
        &source,
        disk::DiskInput {
            scope: disk::DiskScope::All,
            drive: Some("C:".into()),
            path: None,
            top: None,
        },
        &control(30),
    )
    .expect("disk(all, C:) should succeed");
    assert!(all.get("space").is_some());
    assert!(all.get("physicalDisks").is_some());
    assert!(all.get("smartNotice").is_some());

    // 4. disk(usage) through production `storage_tools::run_usage` defaults to `native-walk`
    //    (zero WizTree temp writes, no spurious `degradedFrom`, strict `API.md` `topFiles` keys)
    let kit = TempKit::new("disk-usage");
    let sample = kit.root.join("测试 目录 (中文)");
    fs::create_dir_all(sample.join("sub_a")).unwrap();
    fs::write(sample.join("sub_a/big.bin"), vec![0xABu8; 256 * 1024]).unwrap();
    fs::write(sample.join("empty.txt"), b"").unwrap();

    let prod_index = VolumeIndex::new_production(Arc::new(NativeScanSource::new()));
    let t3 = Instant::now();
    let usage_val =
        cst_kit::diagnostics::storage_tools::run_usage(&prod_index, &sample, 10, &control(30))
            .expect("native walk usage");
    let usage_ms = t3.elapsed().as_millis();
    assert_eq!(usage_val["method"], "native-walk");
    assert!(usage_val.get("degradedFrom").is_none());
    assert!(usage_val["topDirs"].is_array());
    let top_files = usage_val["topFiles"].as_array().expect("topFiles array");
    for f in top_files {
        let keys: std::collections::BTreeSet<&str> =
            f.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from(["modified", "path", "sizeGB"])
        );
    }
    assert!(usage_val["extAgg"].is_array());
    assert!(usage_val["totalGB"].is_number());
    assert!(
        !kit.root.join(".state/tmp/wiztree").exists(),
        ".state/tmp/wiztree must not be created"
    );
    println!(
        "[live] disk(usage): method={} topDirs={} topFiles={} in {}ms",
        usage_val["method"],
        usage_val["topDirs"].as_array().unwrap().len(),
        usage_val["topFiles"].as_array().unwrap().len(),
        usage_ms
    );

    // Nonexistent path must error promptly
    let missing = kit.root.join("does-not-exist-xyz");
    let err = cst_kit::diagnostics::walk::usage(&missing, 10, &control(10), WalkLimits::default());
    assert!(err.is_err(), "nonexistent path must return Err");
}

#[test]
#[ignore = "live Windows API reliability test"]
fn live_ls_sorting_omitted_zero_byte_and_junction() {
    let kit = TempKit::new("ls-real");
    let target = kit.root.join("浏览目录");
    fs::create_dir_all(target.join("dir_big")).unwrap();
    fs::create_dir_all(target.join("dir_small")).unwrap();
    fs::write(target.join("dir_big/data.bin"), vec![1u8; 128 * 1024]).unwrap();
    fs::write(target.join("dir_small/s.txt"), b"hello").unwrap();
    fs::write(target.join("zero.txt"), b"").unwrap();
    fs::write(target.join("file_mid.bin"), vec![2u8; 16 * 1024]).unwrap();

    // Create a real Windows directory junction inside `target`
    let junc_target = kit.root.join("outside_target");
    fs::create_dir_all(&junc_target).unwrap();
    fs::write(junc_target.join("huge.bin"), vec![9u8; 512 * 1024]).unwrap();
    let junc_link = target.join("link_out");
    let status = std::process::Command::new("cmd")
        .args([
            "/c",
            "mklink",
            "/J",
            &junc_link.to_string_lossy(),
            &junc_target.to_string_lossy(),
        ])
        .status()
        .expect("mklink /J");
    assert!(status.success(), "mklink /J failed");

    let index = VolumeIndex::new(Arc::new(NativeScanSource::new()));
    let t0 = Instant::now();
    let res = ls::run(&index, &target, 3, &control(30), WalkLimits::default())
        .expect("ls::run should succeed");
    let elapsed_ms = t0.elapsed().as_millis();
    assert_eq!(res["totalChildren"].as_u64(), Some(5));
    let entries = res["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["name"].as_str(), Some("dir_big"));
    assert_eq!(entries[1]["name"].as_str(), Some("file_mid.bin"));
    // Junction marks incomplete -> all pct must be null
    assert!(
        entries.iter().all(|e| e["pct"].is_null()),
        "incomplete ls must set all pct to null: {entries:?}"
    );
    assert_eq!(res["omitted"]["count"].as_u64(), Some(2));
    assert_eq!(res["omitted"]["unknownCount"].as_u64(), Some(1));
    assert!(res["notice"].is_string());

    // Now test without junction with top=10 to check exact zero-byte and pct=100 sum
    fs::remove_dir(&junc_link).unwrap();
    let clean =
        ls::run(&index, &target, 10, &control(30), WalkLimits::default()).expect("clean ls::run");
    let clean_entries = clean["entries"].as_array().unwrap();
    let zero_row = clean_entries
        .iter()
        .find(|e| e["name"] == "zero.txt")
        .expect("zero.txt row");
    assert_eq!(zero_row["bytes"].as_u64(), Some(0));
    assert!(clean_entries.iter().all(|e| e["pct"].is_number()));
    println!(
        "[live] ls: 5 entries (with junction) in {}ms, method={}",
        elapsed_ms, res["method"]
    );
}

#[test]
#[ignore = "live Windows API reliability test"]
fn live_sys_all_five_scopes() {
    let source = WindowsSource;
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let hw = NativeHardware::new(Some(repo_root));

    // 1. overview
    let t0 = Instant::now();
    let ov = sys::run_with(
        &source,
        &hw,
        sys::SysInput {
            scope: sys::SysScope::Overview,
            top: None,
        },
        &control(30),
    )
    .expect("sys(overview)");
    let ov_ms = t0.elapsed().as_millis();
    let o = &ov["overview"];
    assert!(o["logicalCores"].as_f64().unwrap_or(0.0) >= 1.0);
    let mem_total = o["mem"]["totalMB"].as_f64().unwrap();
    let mem_used = o["mem"]["usedMB"].as_f64().unwrap();
    assert!(mem_total > 0.0 && mem_used <= mem_total + 1.0);
    if let Some(cpu) = o["cpuTotalPct"].as_f64() {
        assert!(
            (0.0..=100.0).contains(&cpu),
            "cpuTotalPct out of range: {cpu}"
        );
    }
    assert!(o["uptime"]["totalHours"].as_f64().unwrap_or(-1.0) >= 0.0);
    println!(
        "[live] sys(overview): {}ms, cpu={:?}%, mem={}/{}MB, model={:?}, degraded={:?}",
        ov_ms,
        o["cpuTotalPct"],
        mem_used,
        mem_total,
        o["machine"]["model"],
        o.get("degraded")
    );

    // 2. proc
    let t1 = Instant::now();
    let proc_res = sys::run_with(
        &source,
        &hw,
        sys::SysInput {
            scope: sys::SysScope::Proc,
            top: Some(15),
        },
        &control(30),
    )
    .expect("sys(proc)");
    let proc_ms = t1.elapsed().as_millis();
    let p = &proc_res["proc"];
    assert!(p["totalProcs"].as_u64().unwrap_or(0) > 0);
    assert!(p["byCpu"].as_array().unwrap().len() <= 15);
    assert!(p["byMem"].as_array().unwrap().len() <= 15);
    for row in p["byCpu"].as_array().unwrap() {
        let pct = row["cpuPct"].as_f64().unwrap();
        assert!(
            (0.0..=100.0).contains(&pct),
            "process cpuPct out of range: {pct} ({row})"
        );
    }
    println!(
        "[live] sys(proc): {}ms, totalProcs={}, intervalSec={}",
        proc_ms, p["totalProcs"], p["intervalSec"]
    );

    // 3. io
    let t2 = Instant::now();
    let io_res = sys::run_with(
        &source,
        &hw,
        sys::SysInput {
            scope: sys::SysScope::Io,
            top: Some(10),
        },
        &control(30),
    )
    .expect("sys(io)");
    let io_ms = t2.elapsed().as_millis();
    let io = &io_res["io"];
    for d in io["disks"].as_array().unwrap() {
        assert_ne!(d["disk"].as_str(), Some("_Total"));
        if let Some(busy) = d["busyPct"].as_f64() {
            assert!((0.0..=100.0).contains(&busy));
        }
    }
    println!(
        "[live] sys(io): {}ms, disks={}, byIo={}, degraded={:?}",
        io_ms,
        io["disks"].as_array().unwrap().len(),
        io["byIo"].as_array().unwrap().len(),
        io.get("degraded")
    );

    // 4. gpu
    let t3 = Instant::now();
    let gpu_res = sys::run_with(
        &source,
        &hw,
        sys::SysInput {
            scope: sys::SysScope::Gpu,
            top: Some(10),
        },
        &control(30),
    )
    .expect("sys(gpu)");
    let gpu_ms = t3.elapsed().as_millis();
    let g = &gpu_res["gpu"];
    assert!(!g["adapters"].as_array().unwrap().is_empty());
    for row in g["byGpuPct"].as_array().unwrap() {
        let pct = row["gpuPct"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&pct));
    }
    println!(
        "[live] sys(gpu): {}ms, adapters={}, nvidia_is_arr={}, engineSamples={}, counterErrors={}, degraded={:?}",
        gpu_ms,
        g["adapters"].as_array().unwrap().len(),
        g["nvidia"].is_array(),
        g["engineSamples"],
        g["counterErrors"],
        g.get("degraded")
    );

    // 5. sensor
    let t4 = Instant::now();
    let sensor_res = sys::run_with(
        &source,
        &hw,
        sys::SysInput {
            scope: sys::SysScope::Sensor,
            top: None,
        },
        &control(30),
    )
    .expect("sys(sensor)");
    let sensor_ms = t4.elapsed().as_millis();
    let s = &sensor_res["sensor"];
    let sensors = s["sensors"].as_array().unwrap();
    assert!(sensors.len() <= 200);
    assert_eq!(s["sensorCount"].as_u64(), Some(sensors.len() as u64));
    for tz in s["thermalZones"].as_array().unwrap() {
        let temp = tz["tempC"].as_f64().unwrap();
        assert!((-50.0..=150.0).contains(&temp));
    }
    println!(
        "[live] sys(sensor): {}ms, admin={}, pawnio={}, sensorCount={}, thermalZones={}, freqCores={}, counterErrors={}, collectionErrors={:?}",
        sensor_ms,
        s["admin"],
        s["pawnio"],
        sensors.len(),
        s["thermalZones"].as_array().unwrap().len(),
        s["frequency"]["cores"],
        s["counterErrors"],
        s.get("collectionErrors")
    );
}

#[test]
#[ignore = "live Windows API reliability test"]
fn live_startup_and_driver_all_scopes() {
    let source = WindowsSource;

    // 1. startup
    let t0 = Instant::now();
    let st = startup::run(&source, startup::StartupInput {}, &control(30)).expect("startup()");
    let st_ms = t0.elapsed().as_millis();
    let s = &st["startup"];
    assert!(s["regItems"].is_array());
    assert!(s["startupFolders"].is_array());
    let services = s["services"].as_array().unwrap();
    assert!(!services.is_empty());
    // Verify Running services appear before non-Running services
    let mut seen_non_running = false;
    for svc in services {
        if svc["state"] == "Running" {
            assert!(
                !seen_non_running,
                "Running service appeared after Stopped service"
            );
        } else {
            seen_non_running = true;
        }
        if let Some(p) = svc["path"].as_str() {
            assert!(
                p.chars().count() <= 143,
                "service path exceeded 140+3 chars"
            );
        }
    }
    println!(
        "[live] startup: {}ms, regItems={}, folders={}, services={}, degraded={:?}",
        st_ms,
        s["regItems"].as_array().unwrap().len(),
        s["startupFolders"].as_array().unwrap().len(),
        services.len(),
        s.get("degraded")
    );

    // 2. driver(problem / core / external / find)
    let t1 = Instant::now();
    let prob = driver::run(
        &source,
        driver::DriverInput {
            scope: driver::DriverScope::Problem,
            ..Default::default()
        },
        &control(30),
    )
    .expect("driver(problem)");
    assert_eq!(
        prob["problem"]["count"].as_u64(),
        Some(prob["problem"]["devices"].as_array().unwrap().len() as u64)
    );

    let core = driver::run(
        &source,
        driver::DriverInput {
            scope: driver::DriverScope::Core,
            ..Default::default()
        },
        &control(30),
    )
    .expect("driver(core)");
    assert!(!core["core"]["display"].as_array().unwrap().is_empty());
    assert!(!core["core"]["drivers"].as_array().unwrap().is_empty());

    let ext = driver::run(
        &source,
        driver::DriverInput {
            scope: driver::DriverScope::External,
            ..Default::default()
        },
        &control(30),
    )
    .expect("driver(external)");
    assert!(ext["external"]["devices"].is_array());
    assert!(ext["external"]["removable"].is_array());

    let find = driver::run(
        &source,
        driver::DriverInput {
            scope: driver::DriverScope::Find,
            class: Some("Display".into()),
            ..Default::default()
        },
        &control(30),
    )
    .expect("driver(find, class=Display)");
    assert!(find["find"]["count"].as_u64().unwrap_or(0) >= 1);

    let empty_find = driver::run(
        &source,
        driver::DriverInput {
            scope: driver::DriverScope::Find,
            ..Default::default()
        },
        &control(10),
    );
    assert!(
        empty_find.is_err(),
        "driver(find) with empty args must fail"
    );
    println!(
        "[live] driver(4 scopes): {}ms, problem={}, core.drivers={}, external.devices={}, find(Display)={}",
        t1.elapsed().as_millis(),
        prob["problem"]["count"],
        core["core"]["drivers"].as_array().unwrap().len(),
        ext["external"]["devices"].as_array().unwrap().len(),
        find["find"]["count"]
    );
}

#[test]
#[ignore = "live Windows API reliability test"]
fn live_eventlog_all_scopes_and_detail() {
    let source = WindowsSource;

    let t0 = Instant::now();
    let recent = eventlog::run(
        &source,
        eventlog::EventInput {
            scope: eventlog::EventScope::Recent,
            hours: Some(24),
            top: Some(20),
            ..Default::default()
        },
        &control(30),
    )
    .expect("eventlog(recent)");
    let recent_ms = t0.elapsed().as_millis();
    let r = &recent["recent"];
    let events = r["events"].as_array().unwrap();
    assert!(events.len() <= 20);
    for e in events {
        if let Some(m) = e["msg"].as_str() {
            assert!(
                m.chars().count() <= 205,
                "event msg not truncated to ~200 chars"
            );
        }
    }
    if !r["countsTruncated"].as_bool().unwrap_or(false)
        && !r["scanTruncated"].as_bool().unwrap_or(false)
    {
        let sum: u64 = r["counts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["n"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(sum, r["total"].as_u64().unwrap(), "sum(counts.n) != total");
    }
    println!(
        "[live] eventlog(recent, 24h): {}ms, total={}, shown={}, unreadable={}, scanTruncated={:?}",
        recent_ms,
        r["total"],
        events.len(),
        r["unreadable"],
        r.get("scanTruncated")
    );

    // Other built-in scopes
    for (label, input) in [
        (
            "boot(all)",
            eventlog::EventInput {
                scope: eventlog::EventScope::Boot,
                hours: Some(72),
                top: Some(20),
                ..Default::default()
            },
        ),
        (
            "crash",
            eventlog::EventInput {
                scope: eventlog::EventScope::Crash,
                hours: Some(72),
                top: Some(20),
                ..Default::default()
            },
        ),
        (
            "service",
            eventlog::EventInput {
                scope: eventlog::EventScope::Service,
                hours: Some(24),
                top: Some(20),
                ..Default::default()
            },
        ),
        (
            "disk",
            eventlog::EventInput {
                scope: eventlog::EventScope::Disk,
                hours: Some(72),
                top: Some(20),
                ..Default::default()
            },
        ),
        (
            "security",
            eventlog::EventInput {
                scope: eventlog::EventScope::Security,
                hours: Some(24),
                top: Some(10),
                ..Default::default()
            },
        ),
        (
            "query(16-ids+level)",
            eventlog::EventInput {
                scope: eventlog::EventScope::Query,
                log_name: Some("System".into()),
                level: Some("warn".into()),
                ids: Some(vec![
                    7, 11, 12, 13, 19, 41, 50, 51, 55, 98, 129, 140, 153, 157, 6005, 6008,
                ]),
                hours: Some(24),
                top: Some(10),
                ..Default::default()
            },
        ),
    ] {
        let ts = Instant::now();
        let out = eventlog::run(&source, input, &control(30))
            .unwrap_or_else(|e| panic!("eventlog({label}) failed: {e}"));
        let scope_obj = out.as_object().unwrap().values().next().unwrap();
        println!(
            "[live] eventlog({label}): {}ms, total={}, degraded={:?}",
            ts.elapsed().as_millis(),
            scope_obj["total"],
            scope_obj.get("degraded")
        );
    }

    // 17 IDs must be rejected before touching the Windows Event Log channel
    let too_many_ids = eventlog::run(
        &source,
        eventlog::EventInput {
            scope: eventlog::EventScope::Query,
            log_name: Some("System".into()),
            ids: Some((1..=17).collect()),
            hours: Some(24),
            top: Some(10),
            ..Default::default()
        },
        &control(10),
    );
    assert!(too_many_ids.is_err(), "17 event IDs must be rejected");

    // Detail lookup by exact recordId (if any recent event exists) + nonexistent recordId
    if let Some(first) = events.first() {
        let log_name = first["logName"].as_str().unwrap().to_owned();
        let record_id = first["recordId"].as_u64().unwrap();
        let det = eventlog::run(
            &source,
            eventlog::EventInput {
                scope: eventlog::EventScope::Detail,
                log_name: Some(log_name),
                record_id: Some(record_id),
                ..Default::default()
            },
            &control(15),
        )
        .expect("eventlog(detail) by recordId");
        assert_eq!(det["detail"]["found"].as_bool(), Some(true));
        assert_eq!(det["detail"]["recordId"].as_u64(), Some(record_id));
    }

    let missing_det = eventlog::run(
        &source,
        eventlog::EventInput {
            scope: eventlog::EventScope::Detail,
            log_name: Some("System".into()),
            record_id: Some(u64::MAX - 1),
            ..Default::default()
        },
        &control(15),
    )
    .expect("eventlog(detail) nonexistent");
    assert_eq!(missing_det["detail"]["found"].as_bool(), Some(false));
}

#[test]
#[ignore = "live Windows API reliability test"]
fn live_read_fffind_ffgrep_perf_encoding_and_security() {
    let kit = TempKit::new("fff-perf");
    let guard = PathGuard::new(&kit.root).unwrap();
    let reader = GuardedReadOperations(guard.clone());

    // Secrets that must NEVER be readable or searchable
    let secret = kit.root.join("data/config/auth.json");
    fs::write(&secret, r#"{"deepseek":{"key":"LIVE_SECRET_NEEDLE"}}"#).unwrap();
    let state_file = kit.root.join(".state/secret.txt");
    fs::write(&state_file, "LIVE_STATE_NEEDLE").unwrap();

    // Workspace with 2,500 files + hardlink alias + UTF-16 LE + GBK + binary + long line
    let ws = kit.root.join("workspace");
    fs::create_dir_all(&ws).unwrap();
    fs::hard_link(&secret, ws.join("alias_auth.txt")).unwrap();
    fs::hard_link(&state_file, ws.join("alias_state.txt")).unwrap();

    for d in 0..25 {
        let sub = ws.join(format!("dir_{d:02}"));
        fs::create_dir_all(&sub).unwrap();
        for f in 0..100 {
            let p = sub.join(format!("log_{f:03}.txt"));
            if d == 12 && f == 42 {
                fs::write(&p, "line1\nNEEDLE_DIAG_TOKEN found here\nline3\n").unwrap();
            } else {
                fs::write(&p, "normal diagnostic line without target\n").unwrap();
            }
        }
    }
    // Binary file with NUL
    fs::write(ws.join("bin.dat"), b"\x00\x01NEEDLE_DIAG_TOKEN\x00").unwrap();
    // Oversized >8 KiB line
    let long_line = format!("{}NEEDLE_DIAG_TOKEN\n", "a".repeat(9000));
    fs::write(ws.join("long.log"), long_line).unwrap();
    // UTF-16 LE text file (common Windows log encoding)
    let utf16_text = "Windows UTF16 NEEDLE_UTF16_TEST\r\n";
    let mut utf16_bytes = vec![0xFFu8, 0xFEu8];
    for u in utf16_text.encode_utf16() {
        utf16_bytes.extend_from_slice(&u.to_le_bytes());
    }
    fs::write(ws.join("utf16.log"), utf16_bytes).unwrap();

    // 1. read security & ADS checks
    assert!(reader.open_reader(&secret).is_err());
    assert!(reader.open_reader(&ws.join("alias_auth.txt")).is_err());
    assert!(reader.open_reader(&ws.join("alias_state.txt")).is_err());
    assert!(
        reader
            .open_reader(Path::new(&format!(
                "{}:stream",
                ws.join("long.log").display()
            )))
            .is_err()
    );
    assert!(
        reader
            .open_reader(Path::new(r"\\.\PhysicalDrive0"))
            .is_err()
    );

    // 2. fffind on 2,500+ files
    let cancel = CancellationToken::new();
    let t0 = Instant::now();
    let found = fff_search(
        &guard,
        &ws,
        SearchInput {
            pattern: "log_042".into(),
            path: None,
            exclude: None,
            limit: Some(50),
            context: None,
            case_sensitive: None,
            literal: false,
        },
        false,
        &cancel,
    )
    .expect("fffind");
    let find_ms = t0.elapsed().as_millis();
    assert_eq!(found["matches"].as_array().unwrap().len(), 25);
    assert!(
        !found["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["path"].as_str().unwrap_or("").contains("alias_")),
        "secret hardlinks leaked into fffind"
    );

    // 3. ffgrep on 2,500+ files
    let t1 = Instant::now();
    let grepped = fff_search(
        &guard,
        &ws,
        SearchInput {
            pattern: "NEEDLE_DIAG_TOKEN".into(),
            path: None,
            exclude: None,
            limit: Some(20),
            context: Some(1),
            case_sensitive: Some(true),
            literal: true,
        },
        true,
        &cancel,
    )
    .expect("ffgrep");
    let grep_ms = t1.elapsed().as_millis();
    let matches = grepped["matches"].as_array().unwrap();
    assert!(
        matches
            .iter()
            .any(|m| m["path"].as_str() == Some("dir_12/log_042.txt") && m["matched"] == true)
    );
    assert!(
        grepped["skipped"].as_u64().unwrap_or(0) >= 1,
        "oversized line should increment skipped"
    );

    // Verify zero secret leakage via content search
    let leak = fff_search(
        &guard,
        &ws,
        SearchInput {
            pattern: "LIVE_SECRET_NEEDLE|LIVE_STATE_NEEDLE".into(),
            path: None,
            exclude: None,
            limit: Some(10),
            context: None,
            case_sensitive: Some(true),
            literal: false,
        },
        true,
        &cancel,
    )
    .expect("ffgrep secret check");
    assert!(leak["matches"].as_array().unwrap().is_empty());

    // Record UTF-16 LE search behavior explicitly
    let utf16_res = fff_search(
        &guard,
        &ws,
        SearchInput {
            pattern: "NEEDLE_UTF16_TEST".into(),
            path: None,
            exclude: None,
            limit: Some(10),
            context: None,
            case_sensitive: Some(true),
            literal: true,
        },
        true,
        &cancel,
    )
    .expect("ffgrep utf16 probe");

    // Verify relative glob matching with unextended Windows cwd (regression from sol's fix)
    let globbed = fff_search(
        &guard,
        &ws,
        SearchInput {
            pattern: "NEEDLE_DIAG_TOKEN".into(),
            path: Some("dir_12/*.txt".into()),
            exclude: Some(vec!["dir_00/**".into()]),
            limit: Some(10),
            context: None,
            case_sensitive: Some(true),
            literal: true,
        },
        true,
        &cancel,
    )
    .expect("ffgrep relative glob");
    assert_eq!(
        globbed["matches"][0]["path"].as_str(),
        Some("dir_12/log_042.txt")
    );

    println!(
        "[live] fff (2,500+ files): fffind={}ms, ffgrep={}ms, skipped={}, utf16_hits={}",
        find_ms,
        grep_ms,
        grepped["skipped"],
        utf16_res["matches"].as_array().unwrap().len()
    );
    assert!(
        find_ms < 10_000,
        "fffind too slow on 2,500 files: {find_ms}ms"
    );
    assert!(
        grep_ms < 10_000,
        "ffgrep too slow on 2,500 files: {grep_ms}ms"
    );
}

#[tokio::test]
#[ignore = "live Windows API reliability test"]
async fn live_runbook_bitlocker_status_and_web_contract() {
    // 1. Runbook plan -> revise -> deliver -> standalone runbook
    let kit = TempKit::new("runbook-live");
    let store = RunbookStore::new(&kit.root).unwrap();
    let cancel = CancellationToken::new();

    let v1 = store
        .plan(
            PlanInput {
                title: "网卡重置方案".into(),
                level: Level::Low,
                date: Some("2026-11-23".into()),
                runbook_id: None,
                base_version: None,
                items: vec![RunbookItem {
                    summary: "刷新 DNS 缓存".into(),
                    command: "ipconfig /flushdns".into(),
                    shell: Shell::Cmd,
                    admin: false,
                }],
            },
            cancel.clone(),
        )
        .await
        .expect("command_plan v1");
    let rb_id = v1["runbook"]["runbookId"].as_str().unwrap().to_owned();
    assert_eq!(v1["runbook"]["version"].as_u64(), Some(1));

    let delivered = store
        .deliver(
            DeliverInput {
                runbook_id: rb_id.clone(),
                version: 1,
            },
            cancel.clone(),
        )
        .await
        .expect("runbook_deliver v1");
    let file_path = PathBuf::from(delivered["runbook"]["file"].as_str().unwrap());
    let raw = fs::read(&file_path).unwrap();
    assert!(raw.starts_with(&[0xEF, 0xBB, 0xBF]), "missing UTF-8 BOM");
    let text = std::str::from_utf8(&raw[3..]).unwrap();
    assert!(text.contains("\r\n"), "missing CRLF");

    let standalone = store
        .write(
            RunbookInput {
                title: "即时清理".into(),
                level: Level::Safe,
                sequence: None,
                date: Some("2026-11-23".into()),
                items: vec![RunbookItem {
                    summary: "查看当前目录".into(),
                    command: "Get-Location".into(),
                    shell: Shell::Powershell,
                    admin: false,
                }],
            },
            cancel.clone(),
        )
        .await
        .expect("runbook standalone");
    assert_eq!(standalone["runbook"]["sequence"].as_u64(), Some(1));
    assert_eq!(
        standalone["runbook"]["commands"][0]["shell"].as_str(),
        Some("PowerShell")
    );
    println!(
        "[live] runbook: plan/deliver/standalone OK ({})",
        file_path.display()
    );

    // 2. Real WindowsBitLocker status (all volumes & drive="C:")
    let t0 = Instant::now();
    let bl_source = WindowsBitLocker;
    let snap = bl_source.snapshot(&control(30));
    match &snap {
        Ok(s) => println!(
            "[live] bitlocker(snapshot): {}ms, admin={}, volumes={}, collectionErrors={:?}",
            t0.elapsed().as_millis(),
            s.admin,
            s.volumes.len(),
            s.collection_errors
        ),
        Err(e) => println!(
            "[live] bitlocker(snapshot) error ({}ms): {e:?}",
            t0.elapsed().as_millis()
        ),
    }

    let exec = BitLockerExecutor::new(Arc::new(WindowsBitLocker));
    let vault = Vault::default();
    let ui = Arc::new(pi_session::NoOpUiContext::new());
    let bl_c = exec
        .execute(
            "live-bl-c",
            bitlocker_model::Input {
                scope: bitlocker_model::Scope::Status,
                drive: Some("C:".into()),
                recovery_password: None,
            },
            &vault,
            ui,
            cancel.clone(),
        )
        .await;
    println!(
        "[live] bitlocker(status, C:): {:?}",
        bl_c.as_ref()
            .map(|v| v["bitlocker"]["volumes"].as_array().map(Vec::len))
            .map_err(|e| e.as_str())
    );

    // 3. Web tools cache + pagination + offline contract
    struct StubTransport;
    impl HttpTransport for StubTransport {
        fn request(
            &self,
            req: HttpRequest,
            _: CancellationToken,
        ) -> pi_agent::BoxFuture<'static, Result<HttpResponse, String>> {
            Box::pin(async move {
                let body = if req.url.contains("exa.ai") {
                    json!({"results":[{"title":"Win11 Diag","url":"https://example.com/a","text":"中文测试内容 Alpha Beta Gamma"}]})
                        .to_string()
                } else {
                    "<html><head><script>evil()</script></head><body><p>Hello Web</p></body></html>"
                        .to_owned()
                };
                Ok(HttpResponse { status: 200, body })
            })
        }
    }

    let web = WebAccess::with_transport(Arc::new(StubTransport));
    let sr = web
        .search(
            WebSearchInput {
                query: Some("Win11 诊断".into()),
                claim: None,
                queries: vec![],
                num_results: Some(3),
            },
            cancel.clone(),
        )
        .await
        .expect("web_search");
    let resp_id = sr["responseId"].as_str().unwrap().to_owned();
    let page = web
        .get(ContentInput {
            response_id: resp_id,
            offset: Some(0),
            limit: Some(200),
            find_text: Some("中文测试".into()),
        })
        .expect("get_search_content");
    assert!(page["content"].as_str().unwrap().contains("中文测试"));

    let fetched = web
        .fetch(
            FetchInput {
                url: Some("https://example.com/page".into()),
                urls: vec![],
            },
            cancel.clone(),
        )
        .await
        .expect("fetch_content");
    assert!(
        fetched["urls"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("Hello Web")
    );
    assert!(
        !fetched["urls"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("evil")
    );
    assert_eq!(OPEN_TOOL_NAMES.len(), 17);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live Windows API reliability test"]
async fn live_diagnostics_concurrency_and_cancellation() {
    let source: Arc<dyn cst_kit::diagnostics::win_native::DiagnosticSource> =
        Arc::new(WindowsSource);
    let cancel_fast = CancellationToken::new();
    let cancel_soon = CancellationToken::new();

    let t0 = Instant::now();
    let f1 = cst_kit::diagnostics::execute(
        source.clone(),
        disk::DiskInput {
            scope: disk::DiskScope::Space,
            drive: None,
            path: None,
            top: None,
        },
        cancel_fast.clone(),
        Duration::from_secs(30),
        disk::run,
    );
    let f2 = cst_kit::diagnostics::execute(
        source.clone(),
        startup::StartupInput {},
        cancel_fast.clone(),
        Duration::from_secs(30),
        startup::run,
    );
    let f3 = cst_kit::diagnostics::execute(
        source.clone(),
        driver::DriverInput {
            scope: driver::DriverScope::Problem,
            ..Default::default()
        },
        cancel_fast.clone(),
        Duration::from_secs(30),
        driver::run,
    );
    let f4 = cst_kit::diagnostics::execute(
        source.clone(),
        sys::SysInput {
            scope: sys::SysScope::Proc,
            top: Some(5),
        },
        cancel_soon.clone(),
        Duration::from_secs(30),
        sys::run,
    );

    let cancel_trigger = cancel_soon.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel_trigger.cancel();
    });

    let (r1, r2, r3, r4) = tokio::join!(f1, f2, f3, f4);
    assert!(r1.is_ok(), "concurrent disk(space) failed: {r1:?}");
    assert!(r2.is_ok(), "concurrent startup failed: {r2:?}");
    assert!(r3.is_ok(), "concurrent driver(problem) failed: {r3:?}");
    assert!(r4.is_err(), "cancelled sys(proc) should return Err");
    println!(
        "[live] concurrency + cancellation (4 tasks): finished in {}ms",
        t0.elapsed().as_millis()
    );
}

fn percentile_ms(samples: &[f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let idx = ((sorted.len() as f64 * p).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[idx]
}

fn current_process_working_set_mb() -> f64 {
    use cst_kit::diagnostics::win_native::DiagnosticSource;
    let pid = std::process::id();
    let source = WindowsSource;
    source
        .processes(&control(10))
        .ok()
        .and_then(|list| list.into_iter().find(|p| p.pid == pid))
        .and_then(|p| p.working_set)
        .map_or(0.0, |b| b as f64 / (1024.0 * 1024.0))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live Windows stage 9.3 1A/1B benchmark gate"]
async fn live_stage93_1a_1b_benchmarks() {
    use cst_kit::diagnostics::result::diagnostic_result;
    use cst_kit::diagnostics::walk;
    use cst_kit::diagnostics::win_native::DiagnosticSource;

    let source: Arc<dyn DiagnosticSource> = Arc::new(WindowsSource);

    // --- 1A: End-to-end queued + JSON-encoded latency (20 rounds after 1 warm-up) ---
    let bench_scope = async |label: &str, scope: disk::DiskScope, drive: Option<&str>| {
        let make_in = || disk::DiskInput {
            scope: match scope {
                disk::DiskScope::Space => disk::DiskScope::Space,
                disk::DiskScope::Info => disk::DiskScope::Info,
                disk::DiskScope::Health => disk::DiskScope::Health,
                disk::DiskScope::All => disk::DiskScope::All,
                disk::DiskScope::Usage => disk::DiskScope::Usage,
            },
            drive: drive.map(str::to_owned),
            path: None,
            top: None,
        };
        // Warm-up
        let warm = cst_kit::diagnostics::execute(
            source.clone(),
            make_in(),
            CancellationToken::new(),
            Duration::from_secs(30),
            disk::run,
        )
        .await
        .expect("warm-up failed");
        let _ = diagnostic_result(warm).expect("warm-up encode");

        let mut times = Vec::with_capacity(20);
        for _ in 0..20 {
            let t0 = Instant::now();
            let val = cst_kit::diagnostics::execute(
                source.clone(),
                make_in(),
                CancellationToken::new(),
                Duration::from_secs(30),
                disk::run,
            )
            .await
            .expect("disk bench failed");
            let _ = diagnostic_result(val).expect("diagnostic_result encode");
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        let min = percentile_ms(&times, 0.0);
        let p50 = percentile_ms(&times, 0.50);
        let p95 = percentile_ms(&times, 0.95);
        let max = percentile_ms(&times, 1.0);
        println!("[1A] {label}: min={min:.1}ms p50={p50:.1}ms p95={p95:.1}ms max={max:.1}ms");
        p95
    };

    let p95_space = bench_scope("disk(space)", disk::DiskScope::Space, None).await;
    let p95_health = bench_scope("disk(health)", disk::DiskScope::Health, None).await;
    let p95_info = bench_scope("disk(info, C:)", disk::DiskScope::Info, Some("C:")).await;
    let p95_all = bench_scope("disk(all, C:)", disk::DiskScope::All, Some("C:")).await;
    assert!(
        p95_space <= 50.0,
        "disk(space) P95 exceeded 50ms: {p95_space}"
    );
    assert!(
        p95_health <= 50.0,
        "disk(health) P95 exceeded 50ms: {p95_health}"
    );
    println!(
        "[1A summary] space_p95={p95_space:.1}ms health_p95={p95_health:.1}ms info_p95={p95_info:.1}ms all_p95={p95_all:.1}ms"
    );

    // --- 1B: Production `walk::usage` hot-scan benchmark & cancellation slot recovery ---
    let pf = Path::new(r"C:\Program Files");
    let _ = walk::usage(pf, 20, &control(60), WalkLimits::default()).expect("warm Program Files");
    let mut pf_times = Vec::new();
    let mut pf_scanned = 0u64;
    let mut pf_total = 0.0f64;
    for _ in 0..3 {
        let t0 = Instant::now();
        let val = walk::usage(pf, 20, &control(60), WalkLimits::default()).expect("Program Files");
        let _ = diagnostic_result(serde_json::json!({"usage": val})).expect("encode usage");
        pf_times.push(t0.elapsed().as_secs_f64() * 1000.0);
        pf_scanned = val["stats"]["filesScanned"].as_u64().unwrap_or(0);
        pf_total = val["totalGB"].as_f64().unwrap_or(0.0);
    }
    let pf_best = percentile_ms(&pf_times, 0.0);
    let pf_med = percentile_ms(&pf_times, 0.5);
    println!(
        "[1B] C:\\Program Files (3 runs): best={pf_best:.1}ms median={pf_med:.1}ms scanned={pf_scanned} totalGB={pf_total}"
    );
    assert!(
        pf_med <= 500.0,
        "1B gate failed: C:\\Program Files hot walk median {pf_med:.1}ms > 500ms"
    );

    for dir in [r"C:\Windows\System32", r"C:\Windows\WinSxS"] {
        let p = Path::new(dir);
        if p.is_dir() {
            let t0 = Instant::now();
            let val = walk::usage(p, 20, &control(60), WalkLimits::default()).expect("walk sysdir");
            println!(
                "[1B] {dir}: {:.1}ms scanned={} truncated={} totalGB={}",
                t0.elapsed().as_secs_f64() * 1000.0,
                val["stats"]["filesScanned"],
                val["truncated"],
                val["totalGB"]
            );
        }
    }

    // Cancellation & 4-slot recovery: launch 4 concurrent walks on C:\Windows, cancel at 30ms,
    // verify all 4 abort promptly and a follow-up `disk(space)` executes immediately.
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        trigger.cancel();
    });
    let t_cancel = Instant::now();
    let mut futs = Vec::new();
    for _ in 0..4 {
        let src = source.clone();
        let tok = cancel.clone();
        futs.push(cst_kit::diagnostics::execute(
            src,
            (),
            tok,
            Duration::from_secs(10),
            |_, (), ctl| walk::usage(Path::new(r"C:\Windows"), 10, ctl, WalkLimits::default()),
        ));
    }
    let results = futures_util::future::join_all(futs).await;
    let cancel_ms = t_cancel.elapsed().as_secs_f64() * 1000.0;
    assert!(
        results.iter().all(Result::is_err),
        "all 4 walks must cancel"
    );
    let t_after = Instant::now();
    let after = cst_kit::diagnostics::execute(
        source.clone(),
        disk::DiskInput {
            scope: disk::DiskScope::Space,
            drive: None,
            path: None,
            top: None,
        },
        CancellationToken::new(),
        Duration::from_secs(10),
        disk::run,
    )
    .await
    .expect("slot recovery after 4 cancelled walks");
    assert!(!after["space"].as_array().unwrap().is_empty());
    println!(
        "[1B] 4-slot cancellation ({cancel_ms:.1}ms) -> follow-up disk(space) recovered in {:.1}ms",
        t_after.elapsed().as_secs_f64() * 1000.0
    );
}

#[test]
#[ignore = "live Windows stage 9.3 1C elevated $MFT gate (requires Admin + CST_ENABLE_MFT=1)"]
fn live_stage93_1c_elevated_mft_gate() {
    use cst_kit::diagnostics::result::diagnostic_result;
    use cst_kit::diagnostics::{mft, storage_tools, walk};

    assert!(
        cst_os::is_elevated().unwrap_or(false),
        "live_stage93_1c_elevated_mft_gate must run in an elevated Administrator process"
    );
    assert!(
        mft::mft_enabled(),
        "set CST_ENABLE_MFT=1 before running the 1C elevated gate"
    );

    let ws_before = current_process_working_set_mb();
    let prod_index = VolumeIndex::new_production(Arc::new(NativeScanSource::new()));
    assert!(prod_index.allows_mft());

    // 1. Cold full-volume `$MFT` scan on C:\
    let t_c = Instant::now();
    let c_usage = storage_tools::run_usage(&prod_index, Path::new(r"C:\"), 20, &control(180))
        .expect("elevated C:\\ $MFT scan must succeed");
    let c_encoded = diagnostic_result(serde_json::json!({"usage": c_usage.clone()}))
        .expect("encode C:\\ usage");
    let c_ms = t_c.elapsed().as_secs_f64() * 1000.0;
    let ws_after_c = current_process_working_set_mb();

    assert_eq!(
        c_usage["method"], "native-mft",
        "C:\\ must use native-mft: {c_usage}"
    );
    assert!(
        c_usage.get("degradedFrom").is_none(),
        "unexpected degradation: {c_usage}"
    );
    let c_total_gb = c_usage["totalGB"].as_f64().unwrap_or(0.0);
    assert!(
        c_total_gb > 1.0,
        "C:\\ totalGB unexpectedly low: {c_total_gb}"
    );
    for f in c_usage["topFiles"].as_array().unwrap() {
        let keys: std::collections::BTreeSet<&str> =
            f.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from(["modified", "path", "sizeGB"]),
            "topFiles must strictly match API.md keys"
        );
    }
    let cached_c = prod_index
        .cached(Path::new(r"C:\"))
        .expect("C:\\ snapshot must be cached");
    let cached_c_mb = cached_c.estimated_bytes() as f64 / (1024.0 * 1024.0);
    println!(
        "[1C] C:\\ cold $MFT: {:.1}ms totalGB={:.2} rows={} files={} est_cache={:.2}MiB ws={:.1}->{:.1}MiB json_bytes={}",
        c_ms,
        c_total_gb,
        cached_c.rows,
        cached_c.file_count,
        cached_c_mb,
        ws_before,
        ws_after_c,
        c_encoded.content.len()
    );

    // 2. Warm `ls` drill-down with live FileReference + PathPolicy::LIVE verification
    let mut ls_reports = Vec::new();
    for dir in [r"C:\", r"C:\Windows", r"C:\Program Files"] {
        let p = Path::new(dir);
        if !p.is_dir() {
            continue;
        }
        let t_ls = Instant::now();
        let ls_val = ls::run(&prod_index, p, 20, &control(30), WalkLimits::default())
            .expect("warm ls failed");
        let _ = diagnostic_result(ls_val.clone()).expect("encode ls");
        let ls_ms = t_ls.elapsed().as_secs_f64() * 1000.0;
        println!(
            "[1C] warm ls({dir}): {:.1}ms method={} totalChildren={} totalSize={}",
            ls_ms, ls_val["method"], ls_val["totalChildren"], ls_val["totalSize"]
        );
        ls_reports.push(serde_json::json!({
            "path": dir,
            "ms": (ls_ms * 10.0).round() / 10.0,
            "method": ls_val["method"],
            "totalChildren": ls_val["totalChildren"],
            "totalSize": ls_val["totalSize"],
            "notice": ls_val.get("notice"),
        }));
    }

    // Cross-check C:\Program Files between MFT-backed ls/cache and live elevated native-walk
    let pf_path = Path::new(r"C:\Program Files");
    let t_pf_walk = Instant::now();
    let pf_walk = walk::usage(pf_path, 20, &control(60), WalkLimits::default())
        .expect("elevated walk C:\\Program Files");
    let pf_walk_ms = t_pf_walk.elapsed().as_secs_f64() * 1000.0;
    let pf_mft_gb = {
        let ls_c = ls::run(
            &prod_index,
            Path::new(r"C:\"),
            50,
            &control(30),
            WalkLimits::default(),
        )
        .expect("ls C:\\");
        ls_c["entries"]
            .as_array()
            .and_then(|arr| {
                arr.iter().find(|e| {
                    e["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case("Program Files"))
                })
            })
            .and_then(|e| e["bytes"].as_u64())
            .map(|b| b as f64 / 1073741824.0)
    };
    println!(
        "[1C] C:\\Program Files cross-check: mft_gb={:?} vs elevated_walk_gb={} ({:.1}ms, truncated={})",
        pf_mft_gb, pf_walk["totalGB"], pf_walk_ms, pf_walk["truncated"]
    );

    // 3. Second volume cancellation + recovery + multi-volume memory check
    let mut second_vol_report = serde_json::Value::Null;
    for candidate in [r"D:\", r"E:\", r"F:\", r"G:\"] {
        let p = Path::new(candidate);
        if !p.is_dir() {
            continue;
        }
        // Cancel mid-scan after 10ms
        let tok = CancellationToken::new();
        let trig = tok.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            trig.cancel();
        });
        let t_cancel = Instant::now();
        let cancel_res = storage_tools::run_usage(
            &prod_index,
            p,
            20,
            &Control::new(tok, Duration::from_secs(60)),
        );
        let cancel_ms = t_cancel.elapsed().as_secs_f64() * 1000.0;
        h.join().unwrap();
        println!(
            "[1C] {candidate} mid-scan cancel: elapsed={:.1}ms cancelled={}",
            cancel_ms,
            cancel_res.is_err()
        );

        // Full cold scan on second volume
        let t_v2 = Instant::now();
        let v2_usage = storage_tools::run_usage(&prod_index, p, 20, &control(180))
            .expect("second volume $MFT scan");
        let v2_ms = t_v2.elapsed().as_secs_f64() * 1000.0;
        println!(
            "[1C] {candidate} cold result: {:.1}ms method={} degradedFrom={:?} totalGB={}",
            v2_ms,
            v2_usage["method"],
            v2_usage.get("degradedFrom"),
            v2_usage["totalGB"]
        );
        assert_eq!(
            v2_usage["method"], "native-mft",
            "{candidate} should succeed with native-mft after prior cancel: {v2_usage}"
        );
        let v2_cached = prod_index.cached(p).expect("second volume cached");
        let ws_after_v2 = current_process_working_set_mb();
        println!(
            "[1C] {candidate} cold $MFT: {:.1}ms method={} totalGB={} rows={} files={} est_cache={:.2}MiB ws={:.1}MiB",
            v2_ms,
            v2_usage["method"],
            v2_usage["totalGB"],
            v2_cached.rows,
            v2_cached.file_count,
            v2_cached.estimated_bytes() as f64 / (1024.0 * 1024.0),
            ws_after_v2
        );
        second_vol_report = serde_json::json!({
            "drive": candidate,
            "cancelMs": (cancel_ms * 10.0).round() / 10.0,
            "cancelReturnedErr": cancel_res.is_err(),
            "coldMs": (v2_ms * 10.0).round() / 10.0,
            "method": v2_usage["method"],
            "totalGB": v2_usage["totalGB"],
            "rows": v2_cached.rows,
            "files": v2_cached.file_count,
            "estCacheMiB": ((v2_cached.estimated_bytes() as f64 / (1024.0 * 1024.0)) * 100.0).round() / 100.0,
            "wsAfterMiB": (ws_after_v2 * 10.0).round() / 10.0,
        });
        break;
    }

    // 4. Elevated disk(health) verification on all local physical disks
    let t_h = Instant::now();
    let health = disk::run(
        &WindowsSource,
        disk::DiskInput {
            scope: disk::DiskScope::Health,
            drive: None,
            path: None,
            top: None,
        },
        &control(30),
    )
    .expect("elevated disk(health)");
    let health_ms = t_h.elapsed().as_secs_f64() * 1000.0;
    println!(
        "[1C] elevated disk(health): {:.1}ms rows={} errors={}",
        health_ms,
        health["smart"].as_array().map_or(0, Vec::len),
        health["smartErrors"].as_array().map_or(0, Vec::len)
    );

    if let Ok(out_path) = std::env::var("CST_1C_REPORT_PATH") {
        let report = serde_json::json!({
            "elevated": true,
            "wsBeforeMiB": (ws_before * 10.0).round() / 10.0,
            "cDrive": {
                "coldMs": (c_ms * 10.0).round() / 10.0,
                "method": c_usage["method"],
                "totalGB": c_total_gb,
                "rows": cached_c.rows,
                "files": cached_c.file_count,
                "estCacheMiB": (cached_c_mb * 100.0).round() / 100.0,
                "wsAfterMiB": (ws_after_c * 10.0).round() / 10.0,
                "notice": c_usage["notice"],
            },
            "warmLs": ls_reports,
            "programFilesCrossCheck": {
                "mftGB": pf_mft_gb.map(|v| (v * 100.0).round() / 100.0),
                "elevatedWalkGB": pf_walk["totalGB"],
                "elevatedWalkMs": (pf_walk_ms * 10.0).round() / 10.0,
                "elevatedWalkTruncated": pf_walk["truncated"],
            },
            "secondVolume": second_vol_report,
            "elevatedHealth": {
                "ms": (health_ms * 10.0).round() / 10.0,
                "rows": health["smart"].as_array().map_or(0, Vec::len),
                "errors": health["smartErrors"].as_array().map_or(0, Vec::len),
            }
        });
        let _ = fs::write(&out_path, serde_json::to_string_pretty(&report).unwrap());
    }
}

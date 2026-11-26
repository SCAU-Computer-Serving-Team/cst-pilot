use cst_kit::diagnostics::result::diagnostic_result;
use cst_kit::diagnostics::win_native::{
    CIMV2, Control, DiagnosticSource, EventQuery, EventRead, EventRecord, EventVisitor,
    LazyEventVisitor, NativeError, NativeHealthRow, ProcessSample, STORAGE, SpaceSample,
    SpaceSnapshot, StartupSnapshot, VolumeSnapshot,
};
use cst_kit::diagnostics::{self, disk, driver, eventlog, startup, sys};
use pi_ai::UserContentBlock;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct MockSource {
    rows: HashMap<String, Result<Vec<Value>, NativeError>>,
    process: Mutex<VecDeque<Vec<ProcessSample>>>,
    spaces: Vec<SpaceSample>,
    space_errors: Vec<NativeError>,
    registry: Vec<Value>,
    folders: Vec<Value>,
    events: Vec<EventRecord>,
    admin: bool,
    admin_error: Option<NativeError>,
    formatted: Arc<Mutex<Vec<u64>>>,
    unreadable: usize,
    scan_truncated: bool,
    requests: Mutex<Vec<(String, String)>>,
    space_calls: AtomicUsize,
    volume_calls: AtomicUsize,
    volume_extents: HashMap<String, Vec<u32>>,
    native_volumes: Option<VolumeSnapshot>,
    native_physical_disks: Option<Vec<Value>>,
    native_health: HashMap<u32, Result<Option<NativeHealthRow>, NativeError>>,
    expected_serial_checks: Mutex<Vec<(u32, Option<String>)>>,
}
impl MockSource {
    fn rows(&mut self, class: &str, rows: Vec<Value>) {
        self.rows.insert(class.into(), Ok(rows));
    }
    fn fail(&mut self, class: &str, code: u32) {
        self.rows.insert(class.into(), Err(error(class, code)));
    }
}
impl DiagnosticSource for MockSource {
    fn query(&self, namespace: &str, wql: &str, _: &Control) -> Result<Vec<Value>, NativeError> {
        self.requests
            .lock()
            .unwrap()
            .push((namespace.into(), wql.into()));
        let class = wql
            .split(" FROM ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        self.rows
            .get(wql)
            .or_else(|| self.rows.get(class))
            .cloned()
            .unwrap_or_else(|| Ok(vec![]))
    }
    fn processes(&self, _: &Control) -> Result<Vec<ProcessSample>, NativeError> {
        Ok(self.process.lock().unwrap().pop_front().unwrap_or_default())
    }
    fn space(&self, drive: Option<&str>, _: &Control) -> Result<SpaceSnapshot, NativeError> {
        self.space_calls.fetch_add(1, Ordering::SeqCst);
        Ok(SpaceSnapshot {
            rows: self
                .spaces
                .iter()
                .filter(|r| drive.is_none_or(|d| r.drive == d))
                .cloned()
                .collect(),
            errors: self.space_errors.clone(),
        })
    }
    fn volume_disk_numbers(
        &self,
        drive: &str,
        _: &Control,
    ) -> Result<Option<Vec<u32>>, NativeError> {
        Ok(self.volume_extents.get(drive).cloned())
    }
    fn native_volumes(
        &self,
        _: Option<&str>,
        _: &Control,
    ) -> Result<Option<VolumeSnapshot>, NativeError> {
        self.volume_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.native_volumes.clone())
    }
    fn native_physical_disks(&self, _: &Control) -> Result<Option<Vec<Value>>, NativeError> {
        Ok(self.native_physical_disks.clone())
    }
    fn native_disk_health(
        &self,
        disk_number: u32,
        _: &str,
        expected_serial: Option<&str>,
        _: &Control,
    ) -> Result<Option<NativeHealthRow>, NativeError> {
        self.expected_serial_checks
            .lock()
            .unwrap()
            .push((disk_number, expected_serial.map(str::to_owned)));
        self.native_health
            .get(&disk_number)
            .cloned()
            .unwrap_or(Ok(None))
    }
    fn startup(&self, _: &Control) -> Result<StartupSnapshot, NativeError> {
        Ok(StartupSnapshot {
            reg_items: self.registry.clone(),
            folders: self.folders.clone(),
            errors: vec![],
        })
    }
    fn admin(&self) -> Result<bool, NativeError> {
        self.admin_error.clone().map_or(Ok(self.admin), Err)
    }
    fn events(
        &self,
        query: &EventQuery,
        visitor: &mut EventVisitor<'_>,
        _: &Control,
    ) -> Result<EventRead, NativeError> {
        self.requests
            .lock()
            .unwrap()
            .push((query.log.clone(), query.xpath.clone()));
        if let Some(Err(error)) = self.rows.get(&query.log) {
            return Err(error.clone());
        }
        let exact = regex::Regex::new(r"EventRecordID=(\d+)").unwrap();
        let record = exact
            .captures(&query.xpath)
            .and_then(|c| c[1].parse::<u64>().ok());
        for row in self
            .events
            .iter()
            .filter(|e| e.log == query.log && record.is_none_or(|r| e.record_id == r))
            .take(query.max_events)
        {
            visitor(row.clone()).map_err(|message| NativeError {
                source: "cancel".into(),
                code: None,
                message,
            })?;
        }
        Ok(EventRead {
            unreadable: self.unreadable,
            truncated: self.scan_truncated,
        })
    }
    fn events_lazy(
        &self,
        query: &EventQuery,
        visitor: &mut LazyEventVisitor<'_>,
        control: &Control,
    ) -> Result<EventRead, NativeError> {
        self.events(
            query,
            &mut |mut event| {
                let message = event.message.take();
                let id = event.record_id;
                let formatted = self.formatted.clone();
                visitor(
                    event,
                    Box::new(move || {
                        formatted.lock().unwrap().push(id);
                        message
                    }),
                )
            },
            control,
        )
    }
}
fn control() -> Control {
    Control::new(CancellationToken::new(), Duration::from_secs(30))
}
fn input<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}
fn error(source: &str, code: u32) -> NativeError {
    NativeError {
        source: source.into(),
        code: Some(code),
        message: "模拟采集错误".into(),
    }
}
fn process(pid: u32, created: u64, cpu: u64, mem: u64) -> ProcessSample {
    ProcessSample {
        pid,
        created,
        cpu_100ns: cpu,
        name: format!("p{pid}"),
        working_set: Some(mem),
        path: None,
        read_bytes: None,
        write_bytes: None,
    }
}
fn event(record: u64, id: u16, provider: &str) -> EventRecord {
    EventRecord {
        log: "System".into(),
        record_id: record,
        id,
        provider: provider.into(),
        time: format!("2026-11-08T01:{:02}:00.000Z", record % 60),
        time_raw: 134_000_000_000_000_000 + record * 600_000_000,
        level: 2,
        message: Some("应用 崩溃\n详情".repeat(100)),
        machine: Some("mock".into()),
    }
}

#[test]
fn model_json_is_bounded_utf8_and_details_are_complete() {
    let value = json!({"sys":{"rows":(0..1000).map(|n|json!({"n":n,"message":"中文🙂".repeat(300)})).collect::<Vec<_>>()}});
    let result = diagnostic_result(value.clone()).unwrap();
    assert_eq!(result.details, value);
    let UserContentBlock::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.len() <= 51200);
    let clipped: Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(clipped["outputTruncated"], true);
    assert!(clipped["originalBytes"].as_u64().unwrap() > 51200);
    assert!(clipped["sys"]["rows"].as_array().unwrap().len() < 1000);
    let small = diagnostic_result(json!({"value":0})).unwrap();
    let UserContentBlock::Text(text) = &small.content[0] else {
        unreachable!()
    };
    assert_eq!(text.text, "{\"value\":0}");
}
#[test]
fn overview_projects_units_and_preserves_partial_failures() {
    let mut source = MockSource::default();
    source.rows("Win32_OperatingSystem",vec![json!({"TotalVisibleMemorySize":8388608,"FreePhysicalMemory":2097152,"LastBootUpTime":"20261107080000.000000+480"})]);
    source.rows(
        "Win32_PerfFormattedData_PerfOS_Processor",
        vec![json!({"Name":"_Total","PercentProcessorTime":25})],
    );
    source.rows(
        "Win32_ComputerSystem",
        vec![json!({"Manufacturer":"Mock","Model":"Test","NumberOfLogicalProcessors":8})],
    );
    source.rows(
        "Win32_Processor",
        vec![json!({"Name":"Mock  CPU","NumberOfCores":4})],
    );
    source.fail("Win32_PerfFormattedData_PerfOS_Memory", 0x80041010);
    let result = sys::run(&source, input(json!({})), &control()).unwrap();
    let value = &result["overview"];
    assert_eq!(value["mem"]["totalMB"], 8192.0);
    assert_eq!(value["mem"]["freeMB"], 2048.0);
    assert_eq!(value["mem"]["usedPct"], 75.0);
    assert_eq!(value["machine"]["cpu"], "Mock CPU");
    assert_eq!(value["machine"]["physicalCores"], 4.0);
    assert!(value["uptime"]["bootTime"].is_string());
    assert!(value["pool"]["nonpagedMB"].is_null());
    assert_eq!(value["degraded"], true);
    assert_eq!(value["collectionErrors"][0]["code"], 0x80041010u32);
    assert!(
        source
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(namespace, _)| namespace == CIMV2)
    );
}
#[test]
fn process_differences_use_elapsed_cores_and_creation_identity() {
    let first = vec![
        process(10, 1, 10_000_000, 1),
        process(20, 1, 10, 1),
        process(30, 1, 900, 1),
    ];
    let second = vec![
        process(10, 1, 30_000_000, 32 * 1048576),
        process(20, 2, 1_000_000, 99 * 1048576),
        process(30, 1, 100, 1),
        process(40, 1, 0, 1),
    ];
    let value = sys::process_result(&first, &second, 2.0, 4, 10);
    assert_eq!(value["totalProcs"], 1);
    assert_eq!(value["byCpu"][0]["cpuPct"], 25.0);
    assert_eq!(value["byMem"][0]["wsMB"], 32.0);
    assert!(value["byCpu"][0]["path"].is_null());
}
#[test]
fn space_preserves_full_disks_zero_free_and_partial_errors() {
    let source = MockSource {
        spaces: vec![SpaceSample {
            drive: "C:".into(),
            total: 100 * 1073741824,
            free: 0,
        }],
        space_errors: vec![error("D:", 21)],
        ..Default::default()
    };
    let result = disk::run(&source, input(json!({"scope":"space"})), &control()).unwrap();
    assert_eq!(result["space"][0]["freeGB"], 0.0);
    assert_eq!(result["space"][0]["usedPct"], 100.0);
    assert_eq!(result["degraded"], true);
    for drive in ["", "C:/", "C;anything", "AA", "1", "C:\\"] {
        assert!(disk::normalize_drive(Some(drive)).is_err());
    }
    assert_eq!(disk::normalize_drive(Some("c")).unwrap(), Some("C:".into()));
}
#[test]
fn disk_association_filters_data_and_failure_keeps_info_but_blocks_smart() {
    let mut source = MockSource::default();
    source.rows(
        "MSFT_PhysicalDisk",
        vec![
            json!({"DeviceId":"1","Size":1073741824,"FriendlyName":"A"}),
            json!({"DeviceId":"2","Size":2147483648u64,"FriendlyName":"B"}),
        ],
    );
    source.rows("Win32_LogicalDiskToPartition",vec![json!({"Dependent":"Win32_LogicalDisk.DeviceID=\"C:\"","Antecedent":"Win32_DiskPartition.DeviceID=\"Disk #2, Partition #1\""})]);
    source.rows(
        "Win32_LogicalDisk",
        vec![json!({"DeviceID":"C:","FreeSpace":0,"Size":2147483648u64})],
    );
    source.rows(
        "MSFT_StorageReliabilityCounter",
        vec![
            json!({"DeviceId":2,"Temperature":30}),
            json!({"DeviceId":1,"Temperature":32}),
        ],
    );
    let result = disk::run(
        &source,
        input(json!({"scope":"all","drive":"c:"})),
        &control(),
    )
    .unwrap();
    assert_eq!(result["physicalDisks"].as_array().unwrap().len(), 1);
    assert_eq!(result["physicalDisks"][0]["sizeGB"], 2.0);
    assert_eq!(result["smart"].as_array().unwrap().len(), 1);
    assert_eq!(result["volumes"][0]["freeGB"], 0.0);
    source.fail("Win32_LogicalDiskToPartition", 0x80041010);
    source.requests.lock().unwrap().clear();
    let result = disk::run(
        &source,
        input(json!({"scope":"all","drive":"c"})),
        &control(),
    )
    .unwrap();
    assert_eq!(result["physicalDisks"].as_array().unwrap().len(), 2);
    assert_eq!(result["collectionErrors"].as_array().unwrap().len(), 1);
    assert!(result["infoNotice"].is_string());
    assert!(result["smart"].is_null());
    assert_eq!(result["degraded"], true);
    assert!(
        !source
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, q)| q.contains("FROM MSFT_StorageReliabilityCounter"))
    );
    assert!(
        source
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(ns, q)| ns == STORAGE && q.contains("FROM MSFT_PhysicalDisk"))
    );
}
#[test]
fn smart_admin_hint_requires_an_actual_access_denial() {
    for code in [0x80070005, 0x80041003, 0x80041010] {
        let mut source = MockSource::default();
        source.rows("MSFT_PhysicalDisk", vec![json!({"DeviceId":"1"})]);
        source.fail("MSFT_StorageReliabilityCounter", code);
        let result = disk::run(&source, input(json!({"scope":"health"})), &control()).unwrap();
        assert_eq!(
            result["smartNotice"].as_str().unwrap().contains("管理员"),
            matches!(code, 0x80070005 | 0x80041003)
        );
        assert!(result["smart"].is_null());
    }
}
#[test]
fn driver_core_reports_failure_when_all_five_queries_fail() {
    let mut source = MockSource::default();
    for class in [
        "Win32_NetworkAdapter",
        "Win32_PnPEntity",
        "Win32_VideoController",
        "Win32_Service",
        "Win32_PnPSignedDriver",
    ] {
        source.fail(class, 5);
    }
    let result = driver::run(&source, input(json!({"scope":"core"})), &control());
    assert!(result.is_err());
    assert_eq!(source.requests.lock().unwrap().len(), 5);
}

#[test]
fn proc_cpu_percentage_cannot_exceed_one_hundred() {
    let value = sys::process_result(
        &[process(1, 0, 0, 1)],
        &[process(1, 0, 12_000_000, 1)],
        1.0,
        1,
        10,
    );
    assert_eq!(value["byCpu"][0]["cpuPct"], 100.0);
}

#[test]
fn overview_preserves_bios_version_without_manufacturer() {
    let mut source = MockSource::default();
    for vendor in [json!(null), json!("  ")] {
        source.rows(
            "Win32_BIOS",
            vec![json!({"Manufacturer":vendor,"SMBIOSBIOSVersion":"v1.2"})],
        );
        let result = sys::run(&source, input(json!({"scope":"overview"})), &control()).unwrap();
        assert_eq!(result["overview"]["machine"]["bios"], "v1.2");
    }
}

#[test]
fn driver_find_is_and_composed_and_includes_hardware_ids() {
    let mut source = MockSource::default();
    source.rows("Win32_PnPEntity",vec![json!({"Name":"Mock USB Audio","PNPClass":"MEDIA","DeviceID":"USB\\one","HardwareID":["VID_1234"],"Status":"OK","ConfigManagerErrorCode":0}),json!({"Name":"Mock USB Audio","PNPClass":"Net","DeviceID":"USB\\two","HardwareID":["VID_1234"]})]);
    let result = driver::run(
        &source,
        input(json!({"scope":"find","name":"USB","class":"media","id":"vid_1234"})),
        &control(),
    )
    .unwrap();
    assert_eq!(result["find"]["count"], 1);
    assert_eq!(result["find"]["devices"][0]["hardwareIds"][0], "VID_1234");
    assert!(driver::run(&source, input(json!({"scope":"find"})), &control()).is_err());
    driver::run(
        &source,
        input(json!({"scope":"find","name":"x' OR 1=1"})),
        &control(),
    )
    .unwrap();
    assert!(
        !source
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("x'")
    );
}
#[test]
fn driver_core_preserves_other_sources_when_a_class_is_unavailable() {
    let mut source = MockSource::default();
    source.rows(
        "Win32_NetworkAdapter",
        vec![json!({"Name":"Mock","NetConnectionID":"Ethernet","PhysicalAdapter":true})],
    );
    source.fail("Win32_VideoController", 0x80041010);
    let result = driver::run(&source, input(json!({"scope":"core"})), &control()).unwrap();
    assert_eq!(result["core"]["net"][0]["physical"], true);
    assert_eq!(result["core"]["degraded"], true);
    assert_eq!(result["core"]["display"], json!([]));
}
#[test]
fn startup_disabled_tristate_and_running_service_order_are_stable() {
    assert_eq!(startup::approval_disabled(false, Some(&[3])), Some(true));
    assert_eq!(startup::approval_disabled(false, Some(&[2])), Some(false));
    assert_eq!(startup::approval_disabled(false, None), None);
    assert_eq!(startup::approval_disabled(true, Some(&[3])), None);
    let mut source = MockSource {
        registry: vec![
            json!({"source":"HKCU\\RunOnce","name":"same","command":"test","disabled":null}),
        ],
        ..Default::default()
    };
    source.rows(
        "Win32_Service",
        vec![
            json!({"Name":"a","State":"Stopped","PathName":"x"}),
            json!({"Name":"z","State":"Running","PathName":"文".repeat(160)}),
        ],
    );
    let result = startup::run(&source, input(json!({})), &control()).unwrap();
    let value = &result["startup"];
    assert_eq!(value["services"][0]["name"], "z");
    assert_eq!(
        value["services"][0]["path"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        141
    );
    assert!(
        value["services"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with('…')
    );
    assert!(value["regItems"][0]["disabled"].is_null());
}
#[test]
fn event_queries_keep_whea_wer_and_hang_and_dedupe_before_top() {
    let mut source = MockSource {
        admin: true,
        ..Default::default()
    };
    source.events = vec![
        event(2, 19, "Microsoft-Windows-WHEA-Logger"),
        event(2, 19, "Microsoft-Windows-WHEA-Logger"),
        event(3, 41, "Kernel"),
    ];
    let result =
        eventlog::run(&source, input(json!({"scope":"boot","top":1})), &control()).unwrap();
    assert_eq!(result["boot"]["total"], 2);
    assert_eq!(result["boot"]["events"].as_array().unwrap().len(), 1);
    assert_eq!(result["boot"]["truncated"], true);
    assert!(
        result["boot"]["events"][0]["msg"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 200
    );
    let queries = source.requests.lock().unwrap();
    assert!(queries[0].1.contains("Microsoft-Windows-WHEA-Logger"));
    assert!(queries[0].1.contains("EventID=7045"));
    drop(queries);
    eventlog::run(
        &source,
        input(json!({"scope":"crash","app":"APP"})),
        &control(),
    )
    .unwrap();
    let queries = source.requests.lock().unwrap();
    let query = &queries.last().unwrap().1;
    assert!(query.contains("Windows Error Reporting"));
    assert!(query.contains("Application Hang"));
    assert!(query.contains("EventID=1002"));
}
#[test]
fn security_denied_never_queries_the_log_or_claims_no_matches() {
    let source = MockSource::default();
    let result = eventlog::run(&source, input(json!({"scope":"security"})), &control()).unwrap();
    assert_eq!(result["security"]["admin"], false);
    assert_eq!(result["security"]["degraded"], true);
    assert!(result["security"]["noMatch"].is_null());
    assert!(source.requests.lock().unwrap().is_empty());
}
#[test]
fn event_filters_counts_bounds_errors_and_detail_contract() {
    let source = MockSource {
        events: (0..120).map(|n| event(n, n as u16, "Provider")).collect(),
        admin: true,
        ..Default::default()
    };
    let result = eventlog::run(
        &source,
        input(json!({"scope":"query","provider":"^provider$","msg":"崩溃","hours":9999})),
        &control(),
    )
    .unwrap();
    let value = &result["query"];
    assert_eq!(value["total"], 120);
    assert_eq!(value["counts"].as_array().unwrap().len(), 100);
    assert_eq!(value["countsTruncated"], true);
    assert_eq!(value["hours"], 720);
    let result = eventlog::run(
        &source,
        input(json!({"scope":"detail","logName":"System","recordId":42})),
        &control(),
    )
    .unwrap();
    assert_eq!(result["detail"]["found"], true);
    assert_eq!(result["detail"]["recordId"], 42);
    assert!(result["detail"]["msg"].as_str().unwrap().contains('\n'));
    assert!(
        source
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("EventRecordID=42")
    );
    for value in [
        json!({"scope":"query","logName":"System']|*"}),
        json!({"scope":"query","provider":"("}),
        json!({"scope":"detail","logName":"System"}),
        json!({"scope":"detail","logName":"System","id":1,"recordId":2}),
    ] {
        assert!(eventlog::run(&source, input(value), &control()).is_err());
    }
    let mut source = MockSource::default();
    source.fail("System", 15007);
    assert!(eventlog::run(&source, input(json!({"scope":"query"})), &control()).is_err());
    // Partial channel failure in `recent` (System fails, Application succeeds) returns degraded result.
    let partial = eventlog::run(&source, input(json!({"scope":"recent"})), &control()).unwrap();
    assert_eq!(partial["recent"]["degraded"], true);
    assert_eq!(partial["recent"]["collectionErrors"][0]["code"], 15007);
    assert!(partial["recent"]["noMatch"].is_null());
}
#[test]
fn event_formats_only_final_top_after_provider_filter_and_joining_logs() {
    let mut old = event(1, 1, "Keep");
    old.message = None;
    let mut newest = event(2, 2, "Keep");
    newest.log = "Application".into();
    newest.time = "2026-11-09T01:02:03Z".into();
    let mut filtered = event(3, 3, "Drop");
    filtered.message = None;
    let source = MockSource {
        events: vec![old, newest, filtered],
        admin: true,
        ..Default::default()
    };
    let result = eventlog::run(
        &source,
        input(json!({"scope":"recent","provider":"^Keep$","top":1})),
        &control(),
    )
    .unwrap();
    let recent = &result["recent"];
    assert_eq!(recent["events"][0]["recordId"], 2);
    assert_eq!(recent["total"], 2);
    assert_eq!(recent["unreadable"], 0);
    assert_eq!(*source.formatted.lock().unwrap(), [2]);
}

#[test]
fn event_message_filter_formats_candidates_only_and_reports_unreadable() {
    let mut bad = event(1, 1, "Keep");
    bad.message = None;
    let source = MockSource {
        events: vec![bad, event(2, 2, "Keep"), event(3, 3, "Drop")],
        admin: true,
        ..Default::default()
    };
    let value = eventlog::run(
        &source,
        input(json!({"scope":"query","msg":"崩溃","provider":"^Keep$"})),
        &control(),
    )
    .unwrap();
    assert_eq!(value["query"]["total"], 1);
    assert_eq!(value["query"]["unreadable"], 1);
    assert_eq!(*source.formatted.lock().unwrap(), [1, 2]);
}

#[test]
fn security_token_failure_keeps_unknown_admin_and_the_original_error() {
    let source = MockSource {
        admin_error: Some(error("token", 6)),
        ..Default::default()
    };
    let result = eventlog::run(&source, input(json!({"scope":"security"})), &control()).unwrap();
    assert!(result["security"]["admin"].is_null());
    assert_eq!(result["security"]["collectionErrors"][0]["code"], 6);
    assert!(
        result["security"]["notice"]
            .as_str()
            .unwrap()
            .contains("无法确认")
    );
    assert!(source.requests.lock().unwrap().is_empty());
}

#[test]
fn event_query_bounds_ids_before_opening_channel() {
    let source = MockSource::default();
    let oversized = input(json!({"scope":"query", "ids":(1..=17).collect::<Vec<_>>() }));
    assert!(
        eventlog::run(&source, oversized, &control())
            .unwrap_err()
            .contains("16")
    );
    assert!(source.requests.lock().unwrap().is_empty());
    let allowed =
        input(json!({"scope":"query", "level":"warn", "ids":(1..=16).collect::<Vec<_>>() }));
    eventlog::run(&source, allowed, &control()).unwrap();
    assert!(source.requests.lock().unwrap()[0].1.contains("EventID=16"));
}

#[test]
fn event_scan_limits_and_unreadable_records_are_reported() {
    let source = MockSource {
        scan_truncated: true,
        unreadable: 3,
        ..Default::default()
    };
    let result = eventlog::run(&source, input(json!({})), &control()).unwrap();
    assert_eq!(result["recent"]["scanTruncated"], true);
    assert_eq!(result["recent"]["degraded"], true);
    assert_eq!(result["recent"]["unreadable"], 6);
    assert!(result["recent"]["noMatch"].is_null());
    let result = eventlog::run(
        &source,
        input(json!({"scope":"detail","logName":"System","recordId":42})),
        &control(),
    )
    .unwrap();
    assert_eq!(result["detail"]["found"], false);
    assert_eq!(result["detail"]["degraded"], true);
}
#[test]
fn smart_keeps_successful_devices_when_another_device_fails() {
    let mut source = MockSource::default();
    source.rows(
        "MSFT_PhysicalDisk",
        vec![json!({"DeviceId":"1"}), json!({"DeviceId":"2"})],
    );
    source.rows(
        "MSFT_StorageReliabilityCounter",
        vec![json!({"DeviceId":"1","Temperature":30})],
    );
    source
        .native_health
        .insert(2, Err(error("SMART_RCV_DRIVE_DATA", 0x80070005)));
    let result = disk::run(&source, input(json!({"scope":"health"})), &control()).unwrap();
    assert_eq!(result["smart"].as_array().unwrap().len(), 1);
    assert_eq!(result["smart"][0]["DeviceId"], "1");
    assert_eq!(result["smartErrors"][0]["deviceId"], "2");
    assert_eq!(result["degraded"], true);
    assert!(result["smartNotice"].as_str().unwrap().contains("管理员"));
    let wmi_smart_queries = source
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, q)| q.contains("MSFT_StorageReliabilityCounter"))
        .count();
    assert_eq!(wmi_smart_queries, 1);
}

#[test]
fn total_wmi_failure_returns_an_error_and_unsupported_scopes_are_rejected() {
    let mut source = MockSource::default();
    for class in [
        "Win32_OperatingSystem",
        "Win32_PageFileUsage",
        "Win32_PerfFormattedData_PerfOS_Memory",
        "Win32_PerfFormattedData_PerfOS_Processor",
        "Win32_ComputerSystem",
        "Win32_BIOS",
        "Win32_Processor",
        "Win32_PnPEntity",
        "MSFT_PhysicalDisk",
        "Win32_LogicalDisk",
    ] {
        source.fail(class, 0x80041010);
    }
    assert!(sys::run(&source, input(json!({})), &control()).is_err());
    assert!(driver::run(&source, input(json!({})), &control()).is_err());
    assert!(disk::run(&source, input(json!({"scope":"info"})), &control()).is_err());
    assert!(serde_json::from_value::<sys::SysInput>(json!({"scope":"invalidscope"})).is_err());
    assert!(serde_json::from_value::<disk::DiskInput>(json!({"scope":"invalid"})).is_err());
    assert!(disk::run(&source, input(json!({"scope":"usage"})), &control()).is_err());
}

#[test]
fn event_detail_keeps_newlines_and_caps_twenty_thousand_characters() {
    let source = MockSource {
        events: vec![EventRecord {
            message: Some("中文\n".repeat(10000)),
            ..event(42, 41, "Kernel")
        }],
        ..Default::default()
    };
    let result = eventlog::run(
        &source,
        input(json!({"scope":"detail","logName":"System","recordId":42})),
        &control(),
    )
    .unwrap();
    let message = result["detail"]["msg"].as_str().unwrap();
    assert_eq!(message.chars().count(), 20000);
    assert!(message.contains('\n'));
    assert_eq!(result["detail"]["msgTruncated"], true);
    assert_eq!(result["detail"]["originalChars"], 30000);
}

#[tokio::test]
async fn worker_cancellation_timeout_and_registration_are_bounded() {
    let source = Arc::new(MockSource::default());
    let names = diagnostics::definitions_with(source.clone());
    assert_eq!(names.len(), 5);
    assert!(names.iter().all(|d| d.prompt_snippet.is_some()));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        diagnostics::execute(
            source.clone(),
            (),
            cancel,
            Duration::from_secs(1),
            |_, _, _| Ok(json!({}))
        )
        .await
        .unwrap_err()
        .contains("取消")
    );
    let error = diagnostics::execute(
        source.clone(),
        (),
        CancellationToken::new(),
        Duration::from_millis(30),
        |_, _, c| {
            c.sleep(Duration::from_secs(2))?;
            Ok(json!({}))
        },
    )
    .await
    .unwrap_err();
    assert!(error.contains("超时") || error.contains("取消"));
    let result = diagnostics::execute(
        source,
        input(json!({"scope":"proc"})),
        CancellationToken::new(),
        Duration::from_secs(3),
        sys::run,
    )
    .await
    .unwrap();
    assert_eq!(result["proc"]["totalProcs"], 0);
}

#[test]
fn disk_prefers_native_extents_volumes_ioctl_health_and_single_batch_wmi_fallback() {
    let mut source = MockSource::default();
    source.rows(
        "MSFT_PhysicalDisk",
        vec![
            json!({"DeviceId":"0","SerialNumber":"SN-NVME-0","Size":1073741824000u64,"FriendlyName":"NVMe 0","MediaType":4,"BusType":17}),
            json!({"DeviceId":"1","SerialNumber":"SN-SATA-1","Size":2147483648000u64,"FriendlyName":"SATA 1","MediaType":3,"BusType":11}),
            json!({"DeviceId":"2","SerialNumber":"SN-USB-2","Size":536870912000u64,"FriendlyName":"USB 2","MediaType":0,"BusType":7}),
        ],
    );
    source.volume_extents.insert("C:".into(), vec![0]);
    source.native_volumes = Some(VolumeSnapshot {
        rows: vec![
            json!({"DeviceID":"C:","VolumeName":"OS","FileSystem":"NTFS","DriveType":3,"Size":107374182400u64,"FreeSpace":53687091200u64}),
        ],
        errors: vec![],
    });
    source.native_health.insert(
        0,
        Ok(Some(NativeHealthRow::device(json!({
            "DeviceId":"0",
            "Wear":3,
            "Temperature":41,
            "PowerOnHours":1200,
            "ReadErrorsTotal":null,
            "WriteErrorsTotal":null
        })))),
    );
    source.native_health.insert(1, Ok(None));
    source.native_health.insert(2, Ok(None));
    source.rows(
        "MSFT_StorageReliabilityCounter",
        vec![json!({"DeviceId":"1","Temperature":35,"PowerOnHours":4000})],
    );

    // 1. info(drive=C:) uses native extents + native volumes and skips CIMV2 WMI completely
    let info = disk::run(
        &source,
        input(json!({"scope":"info","drive":"C:"})),
        &control(),
    )
    .unwrap();
    assert_eq!(info["physicalDisks"].as_array().unwrap().len(), 1);
    assert_eq!(info["physicalDisks"][0]["DeviceId"], "0");
    assert_eq!(info["volumes"][0]["drive"], "C:");
    assert_eq!(info["volumes"][0]["fs"], "NTFS");
    assert!(
        source
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(ns, _)| ns != CIMV2),
        "native extents + volumes must avoid CIMV2 WMI"
    );

    // 2. all(drive=C:) reuses one `native_volumes` probe across both `space` and `info`
    //    without issuing a duplicate `space()` probe.
    source.space_calls.store(0, Ordering::SeqCst);
    source.volume_calls.store(0, Ordering::SeqCst);
    let all = disk::run(
        &source,
        input(json!({"scope":"all","drive":"C:"})),
        &control(),
    )
    .unwrap();
    assert_eq!(all["space"][0]["drive"], "C:");
    assert_eq!(all["volumes"][0]["drive"], "C:");
    assert_eq!(source.volume_calls.load(Ordering::SeqCst), 1);
    assert_eq!(source.space_calls.load(Ordering::SeqCst), 0);

    // 3. health(all disks) passes expected_serial for cross-checking, gets disk 0 from native IOCTL,
    //    queries batch WMI ONCE for disks 1 & 2, preserves disk 0 and disk 1, and reports missing counter for disk 2.
    source.requests.lock().unwrap().clear();
    source.expected_serial_checks.lock().unwrap().clear();
    let health = disk::run(&source, input(json!({"scope":"health"})), &control()).unwrap();
    let smart = health["smart"].as_array().unwrap();
    assert_eq!(smart.len(), 2);
    assert_eq!(smart[0]["DeviceId"], "0");
    assert_eq!(smart[0]["Wear"], 3);
    let keys0: BTreeSet<&str> = smart[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys0,
        BTreeSet::from([
            "DeviceId",
            "PowerOnHours",
            "ReadErrorsTotal",
            "Temperature",
            "Wear",
            "WriteErrorsTotal",
        ])
    );
    assert_eq!(smart[1]["DeviceId"], "1");
    assert_eq!(health["smartErrors"].as_array().unwrap().len(), 1);
    assert_eq!(health["smartErrors"][0]["deviceId"], "2");
    assert_eq!(health["degraded"], true);
    let checked = source.expected_serial_checks.lock().unwrap();
    assert!(
        checked
            .iter()
            .any(|(n, s)| *n == 0 && s.as_deref() == Some("SN-NVME-0"))
    );
    let wmi_smart_queries = source
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, q)| q.contains("MSFT_StorageReliabilityCounter"))
        .count();
    assert_eq!(
        wmi_smart_queries, 1,
        "fallback must issue one batch WMI query instead of per-disk loop"
    );

    // 4. Partial extent match (disk 0 exists in WMI, disk 5 does NOT) must NOT silently drop disk 5.
    let mut partial = MockSource::default();
    partial.rows(
        "MSFT_PhysicalDisk",
        vec![json!({"DeviceId":"0","FriendlyName":"Disk 0"})],
    );
    partial.volume_extents.insert("D:".into(), vec![0, 5]);
    partial.native_volumes = Some(VolumeSnapshot {
        rows: vec![
            json!({"DeviceID":"D:","VolumeName":"Span","FileSystem":"NTFS","DriveType":3,"Size":100u64,"FreeSpace":50u64}),
        ],
        errors: vec![],
    });
    let span_info = disk::run(
        &partial,
        input(json!({"scope":"info","drive":"D:"})),
        &control(),
    )
    .unwrap();
    assert!(
        span_info["infoNotice"].is_string(),
        "partial extent overlap must fail safe with infoNotice instead of dropping missing extent"
    );
    let span_health = disk::run(
        &partial,
        input(json!({"scope":"health","drive":"D:"})),
        &control(),
    )
    .unwrap();
    assert!(span_health["smart"].is_null());
    assert!(
        span_health["smartNotice"]
            .as_str()
            .unwrap()
            .contains("停止查询")
    );

    // 5. WinPE fallback: Storage WMI fails -> native_physical_disks takes over cleanly
    let mut winpe = MockSource::default();
    winpe.fail("MSFT_PhysicalDisk", 0x8004100E);
    winpe.native_physical_disks = Some(vec![
        json!({"DeviceId":"0","FriendlyName":"WinPE NVMe","SerialNumber":"S1","MediaType":4,"BusType":17,"HealthStatus":null,"OperationalStatus":null,"Size":536870912000u64}),
    ]);
    winpe.native_volumes = Some(VolumeSnapshot {
        rows: vec![
            json!({"DeviceID":"X:","VolumeName":"Boot","FileSystem":"NTFS","DriveType":3,"Size":1073741824u64,"FreeSpace":536870912u64}),
        ],
        errors: vec![],
    });
    let pe_info = disk::run(&winpe, input(json!({"scope":"info"})), &control()).unwrap();
    assert_eq!(pe_info["physicalDisks"][0]["FriendlyName"], "WinPE NVMe");
    assert!(pe_info["physicalDisks"][0]["HealthStatus"].is_null());
    assert!(pe_info.get("degraded").is_none());
}

#[test]
fn eventlog_provider_regex_matches_substrings_and_orders_by_filetime_across_dst() {
    let mut source = MockSource::default();
    let older = EventRecord {
        log: "System".into(),
        record_id: 10,
        id: 100,
        provider: "Microsoft-Windows-DiskDiagnostic".into(),
        // Before DST fallback: local 01:30-04:00 (05:30Z)
        time: "2026-11-01T01:30:00-04:00".into(),
        time_raw: 134_065_350_000_000_000,
        level: 2,
        message: Some("older event".into()),
        machine: Some("mock".into()),
    };
    let newer = EventRecord {
        log: "System".into(),
        record_id: 11,
        id: 100,
        provider: "Microsoft-Windows-DiskDiagnostic".into(),
        // After DST fallback: local 01:15-05:00 (06:15Z, lexically smaller than 01:30-04:00!)
        time: "2026-11-01T01:15:00-05:00".into(),
        time_raw: 134_065_377_000_000_000,
        level: 2,
        message: Some("newer event".into()),
        machine: Some("mock".into()),
    };
    source.events = vec![older, newer];

    // Substring literal provider regex must match locally (not be pushed down as exact XPath equality).
    let res = eventlog::run(
        &source,
        input(json!({"scope":"query","logName":"System","provider":"Disk"})),
        &control(),
    )
    .unwrap();
    let q = &res["query"];
    assert_eq!(q["total"], 2);
    assert_eq!(q["events"][0]["recordId"], 11);
    assert_eq!(q["events"][1]["recordId"], 10);
    assert_eq!(q["firstTime"], "2026-11-01T01:30:00-04:00");
    assert_eq!(q["lastTime"], "2026-11-01T01:15:00-05:00");
}

#[test]
fn driver_core_dmtf_date_rejects_non_ascii_and_invalid_calendar_without_panicking() {
    let mut source = MockSource::default();
    source.rows(
        "Win32_PnPSignedDriver",
        vec![
            json!({"DeviceClass":"NET","DeviceName":"NIC1","DriverVersion":"1.0","DriverDate":"20260228000000.000000+000","DriverProviderName":"Vendor"}),
            json!({"DeviceClass":"NET","DeviceName":"NIC2","DriverVersion":"1.0","DriverDate":"2026年02月28日","DriverProviderName":"Vendor"}),
            json!({"DeviceClass":"NET","DeviceName":"NIC3","DriverVersion":"1.0","DriverDate":"20261345000000.000000+000","DriverProviderName":"Vendor"}),
        ],
    );
    let res = driver::run(&source, input(json!({"scope":"core"})), &control()).unwrap();
    let drivers = res["core"]["drivers"].as_array().unwrap();
    assert_eq!(drivers[0]["date"], "2026-02-28");
    assert_eq!(drivers[1]["date"], "2026年02月28日");
    assert_eq!(drivers[2]["date"], "20261345000000.000000+000");
}

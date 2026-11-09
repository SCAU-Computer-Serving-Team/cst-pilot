use cst_pilot::diagnostics::result::diagnostic_result;
use cst_pilot::diagnostics::win_native::{
    CIMV2, Control, DiagnosticSource, EventQuery, EventRead, EventRecord, EventVisitor,
    NativeError, ProcessSample, STORAGE, SpaceSample, SpaceSnapshot, StartupSnapshot,
};
use cst_pilot::diagnostics::{self, disk, driver, eventlog, startup, sys};
use pi_ai::UserContentBlock;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
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
    unreadable: usize,
    scan_truncated: bool,
    requests: Mutex<Vec<(String, String)>>,
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
    fn startup(&self, _: &Control) -> Result<StartupSnapshot, NativeError> {
        Ok(StartupSnapshot {
            reg_items: self.registry.clone(),
            folders: self.folders.clone(),
            errors: vec![],
        })
    }
    fn admin(&self) -> Result<bool, NativeError> {
        Ok(self.admin)
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
    }
}
fn event(record: u64, id: u16, provider: &str) -> EventRecord {
    EventRecord {
        log: "System".into(),
        record_id: record,
        id,
        provider: provider.into(),
        time: format!("2026-11-08T01:{:02}:00.000Z", record % 60),
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
        143
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
    let result = eventlog::run(&source, input(json!({"scope":"query"})), &control()).unwrap();
    assert_eq!(result["query"]["degraded"], true);
    assert_eq!(result["query"]["collectionErrors"][0]["code"], 15007);
    assert!(result["query"]["noMatch"].is_null());
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
    source.fail("SELECT DeviceId,Wear,Temperature,PowerOnHours,ReadErrorsTotal,WriteErrorsTotal FROM MSFT_StorageReliabilityCounter WHERE DeviceId='2'",0x80070005);
    let result = disk::run(&source, input(json!({"scope":"health"})), &control()).unwrap();
    assert_eq!(result["smart"].as_array().unwrap().len(), 1);
    assert_eq!(result["smart"][0]["DeviceId"], "1");
    assert_eq!(result["smartErrors"][0]["deviceId"], "2");
    assert_eq!(result["degraded"], true);
    assert!(result["smartNotice"].as_str().unwrap().contains("管理员"));
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

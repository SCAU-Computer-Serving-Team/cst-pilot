use cst_pilot::diagnostics::hardware::{
    CounterBatch, CounterSample, CounterSet, HardwareSource, LhmMode, NativeHardware, parse_nvidia,
};
use cst_pilot::diagnostics::win_native::{
    Control, DiagnosticSource, EventQuery, EventRead, EventVisitor, NativeError, ProcessSample,
    SpaceSnapshot, StartupSnapshot,
};
use cst_pilot::diagnostics::{sys, sys_load};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn ctl() -> Control {
    Control::new(CancellationToken::new(), Duration::from_secs(30))
}
fn err(source: &str) -> NativeError {
    NativeError {
        source: source.into(),
        code: Some(5),
        message: "模拟拒绝访问".into(),
    }
}
type QueryReplies = HashMap<String, VecDeque<Result<Vec<Value>, NativeError>>>;
#[derive(Default)]
struct Source {
    queries: Mutex<QueryReplies>,
    requests: Mutex<Vec<String>>,
    admin: bool,
}
impl DiagnosticSource for Source {
    fn query(&self, _: &str, wql: &str, _: &Control) -> Result<Vec<Value>, NativeError> {
        let class = wql.split(" FROM ").nth(1).unwrap().to_owned();
        self.requests.lock().unwrap().push(class.clone());
        self.queries
            .lock()
            .unwrap()
            .get_mut(&class)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| Ok(vec![]))
    }
    fn processes(&self, _: &Control) -> Result<Vec<ProcessSample>, NativeError> {
        Ok(vec![])
    }
    fn space(&self, _: Option<&str>, _: &Control) -> Result<SpaceSnapshot, NativeError> {
        Ok(SpaceSnapshot::default())
    }
    fn startup(&self, _: &Control) -> Result<StartupSnapshot, NativeError> {
        Ok(StartupSnapshot::default())
    }
    fn admin(&self) -> Result<bool, NativeError> {
        Ok(self.admin)
    }
    fn events(
        &self,
        _: &EventQuery,
        _: &mut EventVisitor<'_>,
        _: &Control,
    ) -> Result<EventRead, NativeError> {
        Ok(EventRead::default())
    }
}
struct Hardware {
    batches: Mutex<VecDeque<Result<CounterBatch, String>>>,
    calls: AtomicUsize,
    nvidia: Result<Option<Value>, String>,
    lhm: Result<Value, String>,
    lhm_calls: AtomicUsize,
    pawnio: Result<bool, NativeError>,
}
impl Default for Hardware {
    fn default() -> Self {
        Self {
            batches: Mutex::new(VecDeque::new()),
            calls: AtomicUsize::new(0),
            nvidia: Ok(None),
            lhm: Ok(json!({"hardware":[],"sensors":[]})),
            lhm_calls: AtomicUsize::new(0),
            pawnio: Ok(false),
        }
    }
}
impl HardwareSource for Hardware {
    fn counters(&self, _: CounterSet, control: &Control) -> Result<CounterBatch, String> {
        control.check()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batches
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(CounterBatch::default()))
    }
    fn pawnio(&self) -> Result<bool, NativeError> {
        self.pawnio.clone()
    }
    fn nvidia(&self, control: &Control) -> Result<Option<Value>, String> {
        control.check()?;
        self.nvidia.clone()
    }
    fn lhm(&self, _: LhmMode, control: &Control) -> Result<Value, String> {
        control.check()?;
        self.lhm_calls.fetch_add(1, Ordering::SeqCst);
        self.lhm.clone()
    }
}
fn sample(counter: &str, instance: &str, value: f64) -> CounterSample {
    CounterSample {
        counter: counter.into(),
        instance: instance.into(),
        value,
    }
}
fn process(pid: u32, created: &str, read: u64, write: u64) -> Value {
    json!({"ProcessId":pid,"CreationDate":created,"ReadTransferCount":read.to_string(),"WriteTransferCount":write.to_string(),"Name":"worker.exe"})
}
#[test]
fn io_differences_use_real_interval_creation_identity_and_individual_monotonic_counters() {
    let first = vec![
        process(1, "a", 1024, 0),
        process(2, "a", 100, 200),
        process(3, "a", 500, 500),
        process(4, "a", 0, 0),
    ];
    let second = vec![
        process(1, "a", 5120, 4096),
        process(2, "b", 50000, 60000),
        process(3, "a", 400, 50000),
        process(4, "a", 2048, 0),
        process(5, "a", 10000, 0),
    ];
    let result = sys_load::io_result(
        &first,
        &second,
        &[
            json!({"Name":"_Total"}),
            json!({"Name":"0 C:","CurrentDiskQueueLength":2,"PercentIdleTime":20,"DiskReadBytesPersec":10240,"DiskWriteBytesPersec":2048}),
        ],
        2.0,
        1,
    );
    assert_eq!(result["byIo"].as_array().unwrap().len(), 1);
    assert_eq!(result["byIo"][0]["pid"], 1);
    assert_eq!(result["byIo"][0]["ioKBs"], 4.0);
    assert_eq!(result["totalProcs"], 5);
    assert_eq!(result["disks"].as_array().unwrap().len(), 1);
    assert_eq!(result["disks"][0]["busyPct"], 80.0);
    assert_eq!(result["disks"][0]["readKBs"], 10.0);
    assert!(
        sys_load::io_result(&first, &second, &[], 0.0, 50)["byIo"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn io_warms_disk_provider_keeps_healthy_disk_when_process_source_fails() {
    let source = Source::default();
    source.queries.lock().unwrap().insert(
        "Win32_Process".into(),
        VecDeque::from([Err(err("process")), Err(err("process"))]),
    );
    source.queries.lock().unwrap().insert(
        "Win32_PerfFormattedData_PerfDisk_PhysicalDisk".into(),
        VecDeque::from([
            Ok(vec![json!({"Name":"warmup"})]),
            Ok(vec![json!({"Name":"0 C:","PercentIdleTime":0})]),
        ]),
    );
    let value = sys_load::io(&source, 10, &ctl()).unwrap();
    assert_eq!(value["disks"][0]["disk"], "0 C:");
    assert_eq!(value["degraded"], true);
    assert!(value["intervalSec"].as_f64().unwrap() >= 1.2);
    assert_eq!(
        *source.requests.lock().unwrap(),
        [
            "Win32_Process",
            "Win32_PerfFormattedData_PerfDisk_PhysicalDisk",
            "Win32_Process",
            "Win32_PerfFormattedData_PerfDisk_PhysicalDisk"
        ]
    );
}
#[test]
fn gpu_uses_busiest_engine_across_adapters_and_filters_one_mib_instances() {
    let counters = CounterBatch {
        samples: vec![
            sample(
                "engine",
                "pid_42_luid_0x1_0x2_phys_0_eng_0_engtype_3D",
                65.0,
            ),
            sample(
                "engine",
                "pid_42_luid_0x3_0x4_phys_0_eng_1_engtype_Copy",
                45.0,
            ),
            sample("engine", "pid_9_luid_a_phys_0_eng_0_engtype_3D", 110.0),
            sample("engine", "pid_2_engtype_3D", f64::NAN),
            sample("memory", "pid_42_luid_a_phys_0", 2.0 * 1048576.0),
            sample("memory", "pid_42_luid_b_phys_0", 3.0 * 1048576.0),
            sample("memory", "pid_9_luid_b_phys_0", 1048576.0),
        ],
        errors: vec![],
    };
    let value = sys_load::gpu_result(
        &counters,
        &[
            json!({"Name":"GPU","PNPDeviceID":"PCI\\VEN_123"}),
            json!({"Name":"Virtual","PNPDeviceID":"ROOT\\VID"}),
        ],
        &[json!({"ProcessId":42,"Name":"paint.exe"})],
        50,
    );
    assert_eq!(value["byGpuPct"][0]["gpuPct"], 100.0);
    assert_eq!(value["byGpuPct"][1]["gpuPct"], 65.0);
    assert_eq!(value["byGpuPct"][1]["engtypes"], "3D+Copy");
    assert_eq!(value["byGpuPct"][1]["engines"].as_array().unwrap().len(), 2);
    assert_eq!(value["byDedicatedMB"][0]["dedicatedMB"], 5.0);
    assert_eq!(value["byDedicatedMB"].as_array().unwrap().len(), 1);
    assert_eq!(value["adapters"][1]["bus"], "ROOT");
}
#[test]
fn gpu_retries_engine_once_but_does_not_repeat_nvidia_or_discard_lhm_errors() {
    let hardware = Hardware {
        batches: Mutex::new(VecDeque::from([
            Ok(CounterBatch {
                samples: vec![],
                errors: vec![err("engine")],
            }),
            Ok(CounterBatch::default()),
        ])),
        nvidia: Err("驱动无响应".into()),
        lhm: Ok(
            json!({"hardware":["integrated"],"sensors":[{"hw":"integrated","name":"temperature","type":"Temperature","value":44}],"collectionErrors":[{"source":"Update","message":"partial"}]}),
        ),
        ..Hardware::default()
    };
    let value = sys_load::gpu(&Source::default(), &hardware, 10, &ctl()).unwrap();
    assert_eq!(hardware.calls.load(Ordering::SeqCst), 2);
    assert_eq!(hardware.lhm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(value["lhmGpu"]["sensors"][0]["value"], 44.0);
    assert_eq!(value["degraded"], true);
    assert_eq!(value["collectionErrors"].as_array().unwrap().len(), 2);
}
#[test]
fn gpu_memory_error_does_not_retry_and_nvidia_success_skips_lhm() {
    let hardware = Hardware {
        batches: Mutex::new(VecDeque::from([Ok(CounterBatch {
            samples: vec![],
            errors: vec![err("memory")],
        })])),
        nvidia: Ok(Some(json!([{"uuid":"a"}]))),
        ..Hardware::default()
    };
    let value = sys_load::gpu(&Source::default(), &hardware, 10, &ctl()).unwrap();
    assert_eq!(hardware.calls.load(Ordering::SeqCst), 1);
    assert_eq!(hardware.lhm_calls.load(Ordering::SeqCst), 0);
    assert!(value.get("lhmGpu").is_none());
    assert_eq!(value["degraded"], true);
}
#[test]
fn sensor_filters_nonfinite_types_and_limits_readings_without_inferring_from_driver() {
    let batch = CounterBatch {
        samples: vec![
            sample("thermal", "a", 303.15),
            sample("passive", "a", 30.1),
            sample("thermal", "bad", 999.0),
            sample("thermal", "nan", f64::NAN),
            sample("passive", "no_temp", 3.0),
            sample("frequency", "0,0", 55.0),
            sample("frequency", "0,1", 125.0),
            sample("frequency", "_Total", 10.0),
            sample("frequency", "0,_Total", 5.0),
        ],
        errors: vec![],
    };
    let mut sensors: Vec<_> = (0..250)
        .map(|i| json!({"hw":"CPU","name":format!("T{i}"),"type":"Temperature","value":51.123}))
        .collect();
    sensors.insert(
        0,
        json!({"hw":"CPU","name":"N","type":"Voltage","value":null}),
    );
    sensors.insert(0, json!({"hw":"CPU","name":"N","type":"Load","value":1}));
    let hardware = Hardware {
        batches: Mutex::new(VecDeque::from([Ok(batch)])),
        lhm: Ok(json!({"hardware":["CPU"],"sensors":sensors})),
        pawnio: Ok(false),
        ..Hardware::default()
    };
    let value = sys_load::sensor(&Source::default(), &hardware, &ctl()).unwrap();
    assert_eq!(value["pawnio"], false);
    assert_eq!(value["admin"], false);
    assert_eq!(value["sensorCount"], 200);
    assert_eq!(value["sensors"][0]["value"], 51.12);
    assert_eq!(value["thermalZones"].as_array().unwrap().len(), 1);
    assert_eq!(value["thermalZones"][0]["tempC"], 30.0);
    assert_eq!(value["frequency"]["cores"], 2);
    assert_eq!(value["frequency"]["avgPctOfMax"], 90.0);
    assert_eq!(value["frequency"]["minPctOfMax"], 55.0);
}
#[test]
fn missing_lhm_keeps_counter_data_and_pawnio_error_is_not_reported_as_absent() {
    let hardware = Hardware {
        lhm: Err("DLL 缺失".into()),
        pawnio: Err(err("PawnIO")),
        batches: Mutex::new(VecDeque::from([Ok(CounterBatch {
            samples: vec![sample("thermal", "acpi", 300.15)],
            errors: vec![err("frequency")],
        })])),
        ..Hardware::default()
    };
    let value = sys_load::sensor(&Source::default(), &hardware, &ctl()).unwrap();
    assert!(value["pawnio"].is_null());
    assert_eq!(value["thermalZones"][0]["tempC"], 27.0);
    assert_eq!(value["frequency"]["cores"], 0);
    assert!(value["frequency"]["minPctOfMax"].is_null());
    assert_eq!(value["collectionErrors"].as_array().unwrap().len(), 3);
    assert_eq!(value["degraded"], true);
}
#[test]
fn sensor_retains_lhm_partial_failures_alongside_counter_errors() {
    let hardware = Hardware {
        lhm: Ok(
            json!({"hardware":["CPU"],"sensors":[],"collectionErrors":[{"source":"CPU","message":"update error"}]}),
        ),
        batches: Mutex::new(VecDeque::from([Ok(CounterBatch {
            samples: vec![],
            errors: vec![err("thermal")],
        })])),
        ..Hardware::default()
    };
    let value = sys_load::sensor(&Source::default(), &hardware, &ctl()).unwrap();
    assert_eq!(value["collectionErrors"].as_array().unwrap().len(), 2);
    assert_eq!(value["degraded"], true);
}
#[test]
fn nvidia_csv_retains_multiple_cards_and_unavailable_numeric_values() {
    let value=parse_nvidia(b"uuid-a,0000:01:00.0,Card A,55,100.5,2048,8192,76,555.1\nuuid-b,0000:02:00.0,\"Card, B\",N/A,[Not Supported],0,4096,0,555.2\n").unwrap();
    assert_eq!(value.as_array().unwrap().len(), 2);
    assert_eq!(value[1]["name"], "Card, B");
    assert!(value[1]["tempC"].is_null());
    assert!(value[1]["powerW"].is_null());
    assert_eq!(value[1]["vramUsedMB"], 0.0);
    assert!(parse_nvidia(b"broken\n").is_err());
    assert!(parse_nvidia(b"").is_err());
}
#[test]
fn all_sys_scopes_are_registered_and_cancel_before_collection() {
    for scope in ["overview", "proc", "io", "gpu", "sensor"] {
        let input: sys::SysInput = serde_json::from_value(json!({"scope":scope})).unwrap();
        let control = ctl();
        control.cancel.cancel();
        let hardware = Hardware::default();
        assert!(
            sys::run_with(&Source::default(), &hardware, input, &control)
                .unwrap_err()
                .contains("取消")
        );
        assert_eq!(hardware.calls.load(Ordering::SeqCst), 0);
    }
    assert!(
        NativeHardware::new(None)
            .lhm(LhmMode::Sensor, &ctl())
            .unwrap_err()
            .contains("未配置")
    );
}

#[tokio::test]
async fn dropped_diagnostic_request_cancels_worker_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let (started, start) = tokio::sync::oneshot::channel();
    let (done, finish) = tokio::sync::oneshot::channel();
    let source = std::sync::Arc::new(Source::default());
    let task = tokio::spawn(cst_pilot::diagnostics::execute(
        source,
        (),
        parent.clone(),
        Duration::from_secs(30),
        move |_, _, control| {
            let _ = started.send(());
            let result = control.sleep(Duration::from_secs(25));
            let _ = done.send(result.is_err());
            result.map(|_| json!({}))
        },
    ));
    start.await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), finish)
            .await
            .unwrap()
            .unwrap()
    );
    assert!(!parent.is_cancelled());
}

#[cfg(windows)]
#[test]
#[ignore = "由 lhm-host/test.ps1 构建模拟 DLL 后显式执行"]
fn lhm_host_fixture_is_loaded_through_real_job_object() {
    let root = std::env::var_os("CST_TEST_LHM_ROOT").expect("fixture root");
    let hardware = NativeHardware::new(Some(root.into()));
    let sensor = hardware.lhm(LhmMode::Sensor, &ctl()).unwrap();
    assert_eq!(sensor["sensors"].as_array().unwrap().len(), 200);
    assert_eq!(sensor["sensors"][0]["type"], "Temperature");
    let gpu = hardware.lhm(LhmMode::Gpu, &ctl()).unwrap();
    assert_eq!(gpu["sensors"][0]["type"], "Load");
}

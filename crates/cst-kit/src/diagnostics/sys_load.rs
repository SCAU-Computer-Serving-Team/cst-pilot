use super::hardware::{CounterBatch, CounterSet, HardwareSource, LhmMode};
use super::win_native::{
    CIMV2, Control, DiagnosticSource, NativeError, ProcessSample, add_errors, collect, number,
    rounded,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

const IO_PROCESS: &str =
    "SELECT ProcessId,Name,CreationDate,ReadTransferCount,WriteTransferCount FROM Win32_Process";
const IO_DISK: &str = "SELECT Name,CurrentDiskQueueLength,PercentIdleTime,DiskReadBytesPersec,DiskWriteBytesPersec FROM Win32_PerfFormattedData_PerfDisk_PhysicalDisk";

pub fn io(
    source: &dyn DiagnosticSource,
    hardware: &dyn HardwareSource,
    top: usize,
    control: &Control,
) -> Result<Value, String> {
    let mut errors = Vec::new();
    match source.processes(control) {
        Ok(first) if !first.is_empty() && first.iter().any(|p| p.read_bytes.is_some()) => {
            let started = Instant::now();
            let pdh_batch = hardware.counters(CounterSet::DiskIo, control);
            let remaining = Duration::from_millis(1200).saturating_sub(started.elapsed());
            if !remaining.is_zero() {
                control.sleep(remaining)?;
            }
            let second = match source.processes(control) {
                Ok(rows) if rows.iter().any(|p| p.read_bytes.is_some()) => rows,
                Ok(_) => {
                    errors.push(error(
                        "Process",
                        "第二次进程快照回退到无 I/O 计数器模式，无法完成进程读写差分。",
                    ));
                    Vec::new()
                }
                Err(e) => {
                    errors.push(e);
                    Vec::new()
                }
            };
            let elapsed = started.elapsed().as_secs_f64();
            let mut disk_ok = false;
            let mut physical = match pdh_batch {
                Ok(b) => {
                    errors.extend(b.errors.clone());
                    let rows = pdh_disks(&b);
                    if !rows.is_empty() {
                        disk_ok = true;
                    }
                    rows
                }
                Err(e) => {
                    errors.push(error("PhysicalDisk", &e));
                    Vec::new()
                }
            };
            if !disk_ok {
                let err_before = errors.len();
                let wmi_disks = collect(source, CIMV2, IO_DISK, control, &mut errors);
                if errors.len() == err_before {
                    disk_ok = true;
                }
                physical = wmi_physical_disks(&wmi_disks);
            }
            control.check()?;
            if second.is_empty() && !disk_ok {
                return Err("进程与物理盘 I/O 采集全部失败。".into());
            }
            let mut result = io_native_result(&first, &second, physical, elapsed, top);
            add_errors(&mut result, errors);
            return Ok(result);
        }
        _ => {}
    }
    let first = collect(source, CIMV2, IO_PROCESS, control, &mut errors);
    let started = Instant::now();
    // The formatted provider's first read warms the sampling window.
    let _ = collect(source, CIMV2, IO_DISK, control, &mut errors);
    control.sleep(Duration::from_millis(1200))?;
    let second = collect(source, CIMV2, IO_PROCESS, control, &mut errors);
    let elapsed = started.elapsed().as_secs_f64();
    let disks = collect(source, CIMV2, IO_DISK, control, &mut errors);
    control.check()?;
    if errors.len() == 4 {
        return Err("进程与物理盘 I/O 采集全部失败。".into());
    }
    let mut result = io_result(&first, &second, &disks, elapsed, top);
    add_errors(&mut result, errors);
    Ok(result)
}

fn pdh_disks(batch: &CounterBatch) -> Vec<Value> {
    #[derive(Default)]
    struct DiskRow {
        queue: Option<f64>,
        idle: Option<f64>,
        read: Option<f64>,
        write: Option<f64>,
    }
    let mut map: BTreeMap<String, DiskRow> = BTreeMap::new();
    for s in &batch.samples {
        if s.instance == "_Total" || !s.value.is_finite() {
            continue;
        }
        let row = map.entry(s.instance.clone()).or_default();
        match s.counter.as_str() {
            "queue" => row.queue = Some(rounded(s.value.max(0.0), 1)),
            "idle" => row.idle = Some(s.value),
            "read" => row.read = Some(rounded(s.value.max(0.0) / 1024.0, 0)),
            "write" => row.write = Some(rounded(s.value.max(0.0) / 1024.0, 0)),
            _ => {}
        }
    }
    let mut physical: Vec<Value> = map
        .into_iter()
        .map(|(disk, r)| {
            json!({
                "disk": disk,
                "queueLen": r.queue,
                "busyPct": r.idle.map(|v| rounded((100.0 - v).clamp(0.0, 100.0), 1)),
                "readKBs": r.read,
                "writeKBs": r.write,
            })
        })
        .collect();
    descending(&mut physical, "busyPct");
    physical
}

fn wmi_physical_disks(disks: &[Value]) -> Vec<Value> {
    let mut physical: Vec<_> = disks
        .iter()
        .filter(|d| d["Name"] != "_Total")
        .map(|d| {
            json!({
                "disk":d["Name"],"queueLen":number(d,"CurrentDiskQueueLength"),
                "busyPct":number(d,"PercentIdleTime").map(|v|rounded((100.0-v).clamp(0.0,100.0),1)),
                "readKBs":number(d,"DiskReadBytesPersec").map(|v|rounded(v/1024.0,0)),
                "writeKBs":number(d,"DiskWriteBytesPersec").map(|v|rounded(v/1024.0,0))
            })
        })
        .collect();
    descending(&mut physical, "busyPct");
    physical
}

pub fn io_native_result(
    first: &[ProcessSample],
    second: &[ProcessSample],
    physical: Vec<Value>,
    interval: f64,
    top: usize,
) -> Value {
    let before: HashMap<_, _> = first.iter().map(|p| ((p.pid, p.created), p)).collect();
    let mut by_io: Vec<_> = second
        .iter()
        .filter_map(|p| {
            let old = before.get(&(p.pid, p.created))?;
            let read = p.read_bytes?.checked_sub(old.read_bytes?)?;
            let write = p.write_bytes?.checked_sub(old.write_bytes?)?;
            if !interval.is_finite() || interval <= 0.0 {
                return None;
            }
            let rate = (read as f64 + write as f64) / interval / 1024.0;
            if rate <= 0.0 {
                return None;
            }
            let name = p.name.strip_suffix(".exe").unwrap_or(&p.name);
            Some(json!({"name": name, "pid": p.pid, "ioKBs": rounded(rate, 1)}))
        })
        .collect();
    descending(&mut by_io, "ioKBs");
    by_io.truncate(top);
    json!({
        "intervalSec": rounded(interval, 2),
        "totalProcs": second.len(),
        "disks": physical,
        "byIo": by_io,
        "notice": "disks 为各物理盘队列、忙碌百分比与读写吞吐（KiB/s）；byIo 为相同 PID 与创建时间的进程读写字节差分（KiB/s），按实际采样间隔计算。两套统计口径不同，不可直接相加。新建、退出、PID 复用或计数回退的进程不参与差分。"
    })
}
fn uint(row: &Value, key: &str) -> Option<u64> {
    row[key]
        .as_u64()
        .or_else(|| row[key].as_str()?.parse().ok())
}
pub fn io_result(
    first: &[Value],
    second: &[Value],
    disks: &[Value],
    interval: f64,
    top: usize,
) -> Value {
    let before: HashMap<_, _> = first
        .iter()
        .filter_map(|p| {
            Some((
                (
                    uint(p, "ProcessId")?,
                    p["CreationDate"].as_str()?.to_owned(),
                ),
                p,
            ))
        })
        .collect();
    let mut by_io:Vec<_>=second.iter().filter_map(|p|{
        let pid=uint(p,"ProcessId")?;
        let old=before.get(&(pid,p["CreationDate"].as_str()?.to_owned()))?;
        let read=uint(p,"ReadTransferCount")?.checked_sub(uint(old,"ReadTransferCount")?)?;
        let write=uint(p,"WriteTransferCount")?.checked_sub(uint(old,"WriteTransferCount")?)?;
        if !interval.is_finite() || interval<=0.0 { return None; }
        let rate=(read as f64+write as f64)/interval/1024.0;
        if rate<=0.0 { return None; }
        Some(json!({"name":p["Name"].as_str().map(|n|n.strip_suffix(".exe").unwrap_or(n)),"pid":pid,"ioKBs":rounded(rate,1)}))
    }).collect();
    descending(&mut by_io, "ioKBs");
    by_io.truncate(top);
    let physical = wmi_physical_disks(disks);
    json!({"intervalSec":rounded(interval,2),"totalProcs":second.len(),"disks":physical,"byIo":by_io,
        "notice":"disks 为各物理盘队列、忙碌百分比与读写吞吐（KiB/s）；byIo 为相同 PID 与创建时间的进程读写字节差分（KiB/s），按实际采样间隔计算。两套统计口径不同，不可直接相加。新建、退出、PID 复用或计数回退的进程不参与差分。"})
}
fn descending(rows: &mut [Value], key: &str) {
    rows.sort_by(|a, b| {
        b[key]
            .as_f64()
            .unwrap_or(-1.0)
            .total_cmp(&a[key].as_f64().unwrap_or(-1.0))
    });
}
fn batch(source: &dyn HardwareSource, set: CounterSet, control: &Control) -> CounterBatch {
    match source.counters(set, control) {
        Ok(b) => b,
        Err(message) => CounterBatch {
            samples: vec![],
            errors: match set {
                CounterSet::Gpu => vec![error("engine", &message), error("memory", &message)],
                CounterSet::Sensor => {
                    vec![error("thermal", &message), error("frequency", &message)]
                }
                CounterSet::DiskIo => vec![error("PhysicalDisk", &message)],
            },
        },
    }
}
fn error(key: &str, message: &str) -> NativeError {
    NativeError {
        source: key.into(),
        code: None,
        message: message.into(),
    }
}
fn counter_errors(batch: &CounterBatch) -> Value {
    let mut errors = BTreeMap::<String, String>::new();
    for e in &batch.errors {
        errors
            .entry(e.source.clone())
            .and_modify(|m| {
                m.push('；');
                m.push_str(&e.message);
            })
            .or_insert_with(|| e.message.clone());
    }
    json!(errors)
}
fn has_non_nvidia_physical_adapter(adapters: &[Value]) -> bool {
    adapters.iter().any(|r| {
        let bus = r["PNPDeviceID"]
            .as_str()
            .and_then(|v| v.split('\\').next())
            .unwrap_or_default();
        let is_physical = bus.eq_ignore_ascii_case("PCI") || bus.eq_ignore_ascii_case("ACPI");
        if !is_physical {
            return false;
        }
        let vendor = r["AdapterCompatibility"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let name = r["Name"].as_str().unwrap_or_default().to_ascii_lowercase();
        !vendor.contains("nvidia") && !name.contains("nvidia")
    })
}

pub fn gpu(
    source: &dyn DiagnosticSource,
    hardware: &dyn HardwareSource,
    top: usize,
    control: &Control,
) -> Result<Value, String> {
    let mut errors = Vec::new();
    let adapters_res = source.query(
        CIMV2,
        "SELECT Name,AdapterCompatibility,DriverVersion,Status,PNPDeviceID FROM Win32_VideoController",
        control,
    );
    let adapters_failed = adapters_res.is_err();
    let adapters = match adapters_res {
        Ok(rows) => rows,
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };
    let probe_lhm_parallel = adapters_failed || has_non_nvidia_physical_adapter(&adapters);
    let (mut counters, nvidia, lhm_parallel) = std::thread::scope(|scope| {
        let nv = scope.spawn(|| hardware.nvidia(control));
        let lhm_handle =
            probe_lhm_parallel.then(|| scope.spawn(|| hardware.lhm(LhmMode::Gpu, control)));
        let first = batch(hardware, CounterSet::Gpu, control);
        let nv_res = nv
            .join()
            .unwrap_or_else(|_| Err("NVIDIA 采集子任务异常退出。".into()));
        let lhm_res = lhm_handle.map(|h| {
            h.join()
                .unwrap_or_else(|_| Err("LHM 采集子任务异常退出。".into()))
        });
        (first, nv_res, lhm_res)
    });
    control.check()?;
    if counters.errors.iter().any(|e| e.source == "engine")
        && !counters.samples.iter().any(|s| s.counter == "engine")
    {
        let retry = hardware
            .gpu_engine(control)
            .unwrap_or_else(|message| CounterBatch {
                samples: vec![],
                errors: vec![error("engine", &message)],
            });
        counters.errors.retain(|e| e.source != "engine");
        counters
            .errors
            .extend(retry.errors.into_iter().filter(|e| e.source == "engine"));
        counters
            .samples
            .extend(retry.samples.into_iter().filter(|s| s.counter == "engine"));
    }
    let name_map: HashMap<u64, String> = match source.native_process_names(control) {
        Ok(Some(map)) => map,
        Ok(None) => wmi_process_names(&collect(
            source,
            CIMV2,
            "SELECT ProcessId,Name FROM Win32_Process",
            control,
            &mut errors,
        )),
        Err(e) => {
            errors.push(e);
            wmi_process_names(&collect(
                source,
                CIMV2,
                "SELECT ProcessId,Name FROM Win32_Process",
                control,
                &mut errors,
            ))
        }
    };
    let pdh_ok = !counters.samples.is_empty() || counters.errors.is_empty();
    let mut value = gpu_result_with_names(&counters, &adapters, &name_map, top);
    let nvidia_has_data = matches!(nvidia, Ok(Some(Value::Array(_))));
    let nvidia = match nvidia {
        Ok(v) => v.unwrap_or(Value::Null),
        Err(e) => {
            errors.push(error("nvidia-smi", &e));
            json!({"error":e})
        }
    };
    let lhm_outcome =
        lhm_parallel.or_else(|| (!nvidia.is_array()).then(|| hardware.lhm(LhmMode::Gpu, control)));
    let mut lhm_failed = false;
    if let Some(outcome) = lhm_outcome {
        match outcome {
            Ok(v) => {
                errors.extend(lhm_errors(&v));
                value["lhmGpu"] = sanitize_lhm(&v, LhmMode::Gpu);
            }
            Err(e) => {
                lhm_failed = true;
                value["lhmGpu"] = json!({"hardware":[],"sensors":[],"error":e});
                errors.push(error("LHM", &e));
            }
        }
    }
    value["nvidia"] = nvidia;
    errors.extend(counters.errors);
    if !pdh_ok && adapters_failed && !nvidia_has_data && lhm_failed {
        return Err("GPU 诊断所有采集源均失败。".into());
    }
    add_errors(&mut value, errors);
    control.check()?;
    Ok(value)
}
fn wmi_process_names(names: &[Value]) -> HashMap<u64, String> {
    names
        .iter()
        .filter_map(|r| {
            Some((
                uint(r, "ProcessId")?,
                r["Name"].as_str()?.trim_end_matches(".exe").to_owned(),
            ))
        })
        .collect()
}
fn pid(instance: &str) -> Option<u64> {
    let rest = instance.strip_prefix("pid_")?;
    rest.split('_').next()?.parse().ok()
}
pub fn gpu_result(
    counters: &CounterBatch,
    adapters: &[Value],
    names: &[Value],
    top: usize,
) -> Value {
    gpu_result_with_names(counters, adapters, &wmi_process_names(names), top)
}
fn gpu_result_with_names(
    counters: &CounterBatch,
    adapters: &[Value],
    names: &HashMap<u64, String>,
    top: usize,
) -> Value {
    let mut engine: BTreeMap<u64, Vec<Value>> = BTreeMap::new();
    let mut memory: BTreeMap<u64, f64> = BTreeMap::new();
    for sample in &counters.samples {
        if !sample.value.is_finite() {
            continue;
        }
        let Some(id) = pid(&sample.instance) else {
            continue;
        };
        if sample.counter == "engine" {
            let pct = rounded(sample.value.clamp(0.0, 100.0), 1);
            if pct <= 0.0 {
                continue;
            }
            let kind = sample
                .instance
                .split("engtype_")
                .nth(1)
                .and_then(|s| s.split('_').next())
                .unwrap_or("unknown");
            let adapter = sample
                .instance
                .find("luid_")
                .and_then(|start| {
                    sample.instance[start..]
                        .find("_eng_")
                        .map(|end| &sample.instance[start..start + end])
                })
                .unwrap_or("unknown");
            engine
                .entry(id)
                .or_default()
                .push(json!({"adapter":adapter,"type":kind,"gpuPct":pct}));
        } else if sample.counter == "memory" && sample.value > 1048576.0 {
            *memory.entry(id).or_default() += rounded(sample.value / 1048576.0, 0);
        }
    }
    let mut by_pct:Vec<Value>=engine.into_iter().map(|(id,engines)|{
        let maximum=engines.iter().filter_map(|e|e["gpuPct"].as_f64()).fold(0.0,f64::max);
        let kinds:BTreeSet<_>=engines.iter().filter_map(|e|e["type"].as_str()).collect();
        json!({"pid":id,"name":names.get(&id),"gpuPct":maximum,"engtypes":kinds.into_iter().collect::<Vec<_>>().join("+"),"engines":engines})
    }).collect();
    let mut by_mem: Vec<Value> = memory
        .into_iter()
        .map(|(id, mb)| json!({"pid":id,"name":names.get(&id),"dedicatedMB":mb}))
        .collect();
    descending(&mut by_pct, "gpuPct");
    by_pct.truncate(top);
    descending(&mut by_mem, "dedicatedMB");
    by_mem.truncate(top);
    let mut out = json!({"engineSamples":counters.samples.iter().filter(|s|s.counter=="engine").count(),"byGpuPct":by_pct,"byDedicatedMB":by_mem,
        "adapters":adapters.iter().map(|r|json!({"name":r["Name"],"vendor":r["AdapterCompatibility"],"driver":r["DriverVersion"],"status":r["Status"],"bus":r["PNPDeviceID"].as_str().and_then(|v|v.split('\\').next())})).collect::<Vec<_>>(),
        "notice":"GPU 利用率取每进程跨适配器最繁忙的单一引擎，不累加并行引擎；engines 仅保留非零负载引擎样本。byDedicatedMB 仅聚合大于 1 MiB 的专用显存实例。nvidia 为按 uuid/pciBusId 区分的独显状态；检测到非 NVIDIA 物理显卡或 nvidia 缺失时由 lhmGpu 补充。空 hardware 不代表没有显卡，失败来源见 counterErrors/collectionErrors。"});
    if !counters.errors.is_empty() {
        out["counterErrors"] = counter_errors(counters);
    }
    out
}
pub fn sensor(
    source: &dyn DiagnosticSource,
    hardware: &dyn HardwareSource,
    control: &Control,
) -> Result<Value, String> {
    let (counters, lhm) = std::thread::scope(|scope| {
        let lhm = scope.spawn(|| hardware.lhm(LhmMode::Sensor, control));
        (
            batch(hardware, CounterSet::Sensor, control),
            lhm.join()
                .unwrap_or_else(|_| Err("LHM 采集子任务异常退出。".into())),
        )
    });
    control.check()?;
    let mut errors = counters.errors.clone();
    let mut status = |r: Result<bool, NativeError>| match r {
        Ok(v) => json!(v),
        Err(e) => {
            errors.push(e);
            Value::Null
        }
    };
    let admin = status(source.admin());
    let pawnio = status(hardware.pawnio());
    let pdh_ok = !counters.samples.is_empty() || counters.errors.is_empty();
    let lhm_ok = lhm.is_ok();
    if !pdh_ok && !lhm_ok {
        return Err("传感器与硬件监控采集全部失败。".into());
    }
    let mut value = sensor_result(
        &counters,
        &lhm.as_ref()
            .ok()
            .cloned()
            .unwrap_or_else(|| json!({"hardware":[],"sensors":[]})),
    );
    value["admin"] = admin;
    value["pawnio"] = pawnio;
    match lhm {
        Err(e) => errors.push(error("LHM", &e)),
        Ok(v) => errors.extend(lhm_errors(&v)),
    }
    add_errors(&mut value, errors);
    control.check()?;
    Ok(value)
}
fn lhm_errors(value: &Value) -> Vec<NativeError> {
    value["collectionErrors"]
        .as_array()
        .into_iter()
        .flatten()
        .take(16)
        .map(|e| NativeError {
            source: format!("LHM/{}", e["source"].as_str().unwrap_or("hardware")),
            code: None,
            message: e["message"]
                .as_str()
                .unwrap_or("传感器采集失败。")
                .to_owned(),
        })
        .collect()
}
fn lhm_unit(sensor_type: &str) -> &'static str {
    match sensor_type {
        "Temperature" => "°C",
        "Load" => "%",
        "Clock" => "MHz",
        "Power" => "W",
        "Fan" => "RPM",
        "Voltage" => "V",
        "SmallData" => "MB",
        "Data" => "GB",
        _ => "",
    }
}

pub fn sanitize_lhm(value: &Value, mode: LhmMode) -> Value {
    let types: &[&str] = match mode {
        LhmMode::Sensor => &["Temperature", "Fan", "Voltage"],
        LhmMode::Gpu => &[
            "Temperature",
            "Load",
            "Clock",
            "Power",
            "Fan",
            "SmallData",
            "Data",
        ],
    };
    let sensors:Vec<Value>=value["sensors"].as_array().into_iter().flatten().filter(|s|{
        s["hw"].is_string() && s["name"].is_string() && s["type"].as_str().is_some_and(|t|types.contains(&t)) && s["value"].as_f64().is_some_and(f64::is_finite)
    }).take(200).map(|s|{
        let ty = s["type"].as_str().unwrap_or_default();
        json!({"hw":s["hw"],"name":s["name"],"type":ty,"unit":lhm_unit(ty),"value":rounded(s["value"].as_f64().unwrap(),2)})
    }).collect();
    let hardware: Vec<_> = value["hardware"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(200)
        .collect();
    let mut out = json!({"hardware":hardware,"sensors":sensors});
    if value["collectionErrors"].is_array()
        && !value["collectionErrors"].as_array().unwrap().is_empty()
    {
        out["collectionErrors"] = value["collectionErrors"].clone();
        out["degraded"] = json!(true);
    }
    out
}
pub fn sensor_result(batch: &CounterBatch, lhm: &Value) -> Value {
    let mut out = sanitize_lhm(lhm, LhmMode::Sensor);
    let mut zones = BTreeMap::<String, (Option<f64>, Option<f64>)>::new();
    let mut frequency = Vec::new();
    for s in &batch.samples {
        if !s.value.is_finite() {
            continue;
        }
        match s.counter.as_str() {
            "thermal" => {
                let c = s.value - 273.15;
                if (-50.0..=150.0).contains(&c) {
                    zones.entry(s.instance.clone()).or_default().0 = Some(rounded(c, 1));
                }
            }
            "passive" => {
                zones.entry(s.instance.clone()).or_default().1 = Some(rounded(s.value, 0));
            }
            "frequency" => {
                if let Some((group, core)) = s.instance.split_once(',')
                    && group.parse::<u32>().is_ok()
                    && core.parse::<u32>().is_ok()
                {
                    frequency.push(s.value);
                }
            }
            _ => {}
        }
    }
    out["sensorCount"] = json!(out["sensors"].as_array().map_or(0, Vec::len));
    out["thermalZones"] = json!(
        zones
            .into_iter()
            .filter_map(|(zone, (temp, passive))| Some(
                json!({"zone":zone,"tempC":temp?,"passivePct":passive})
            ))
            .collect::<Vec<_>>()
    );
    out["frequency"] = json!({"cores":frequency.len(),"minPctOfMax":frequency.iter().copied().reduce(f64::min).map(|v|rounded(v,1)),"avgPctOfMax":(!frequency.is_empty()).then(||rounded(frequency.iter().sum::<f64>()/frequency.len() as f64,1))});
    if !batch.errors.is_empty() {
        out["counterErrors"] = counter_errors(batch);
    }
    out["notice"] = json!(
        "sensors 为 LHM 返回的温度（°C）、风扇（RPM）与电压（V），仅保留有限值，最多 200 项。thermalZones 为 ACPI 热区（K 转 °C，仅保留 -50～150°C），frequency 为各逻辑核频率占最大频率的百分比。admin 与 pawnio 只描述权限和注册项；可读项以返回的读数为准，空值为采集缺失。本工具不安装驱动。"
    );
    out
}

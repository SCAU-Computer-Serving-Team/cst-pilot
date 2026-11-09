use super::hardware::{CounterBatch, CounterSet, HardwareSource, LhmMode};
use super::win_native::{
    CIMV2, Control, DiagnosticSource, NativeError, add_errors, collect, number, rounded,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

const IO_PROCESS: &str =
    "SELECT ProcessId,Name,CreationDate,ReadTransferCount,WriteTransferCount FROM Win32_Process";
const IO_DISK: &str = "SELECT Name,CurrentDiskQueueLength,PercentIdleTime,DiskReadBytesPersec,DiskWriteBytesPersec FROM Win32_PerfFormattedData_PerfDisk_PhysicalDisk";

pub fn io(source: &dyn DiagnosticSource, top: usize, control: &Control) -> Result<Value, String> {
    let mut errors = Vec::new();
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
pub fn gpu(
    source: &dyn DiagnosticSource,
    hardware: &dyn HardwareSource,
    top: usize,
    control: &Control,
) -> Result<Value, String> {
    let (mut counters, nvidia) = std::thread::scope(|scope| {
        let nv = scope.spawn(|| hardware.nvidia(control));
        let first = batch(hardware, CounterSet::Gpu, control);
        (
            first,
            nv.join()
                .unwrap_or_else(|_| Err("NVIDIA 采集子任务异常退出。".into())),
        )
    });
    control.check()?;
    if counters.errors.iter().any(|e| e.source == "engine") {
        counters = batch(hardware, CounterSet::Gpu, control);
    }
    let mut errors = Vec::new();
    let adapters = collect(
        source,
        CIMV2,
        "SELECT Name,AdapterCompatibility,DriverVersion,Status,PNPDeviceID FROM Win32_VideoController",
        control,
        &mut errors,
    );
    let names = collect(
        source,
        CIMV2,
        "SELECT ProcessId,Name FROM Win32_Process",
        control,
        &mut errors,
    );
    let mut value = gpu_result(&counters, &adapters, &names, top);
    let nvidia = match nvidia {
        Ok(v) => v.unwrap_or(Value::Null),
        Err(e) => {
            errors.push(error("nvidia-smi", &e));
            json!({"error":e})
        }
    };
    if !nvidia.is_array() {
        match hardware.lhm(LhmMode::Gpu, control) {
            Ok(v) => {
                errors.extend(lhm_errors(&v));
                value["lhmGpu"] = sanitize_lhm(&v, LhmMode::Gpu);
            }
            Err(e) => {
                value["lhmGpu"] = json!({"hardware":[],"sensors":[],"notice":e});
                errors.push(error("LHM", &e));
            }
        }
    }
    value["nvidia"] = nvidia;
    errors.extend(counters.errors);
    add_errors(&mut value, errors);
    control.check()?;
    Ok(value)
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
    let names: HashMap<_, _> = names
        .iter()
        .filter_map(|r| {
            Some((
                uint(r, "ProcessId")?,
                r["Name"].as_str()?.trim_end_matches(".exe").to_owned(),
            ))
        })
        .collect();
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
            engine.entry(id).or_default().push(json!({"adapter":adapter,"type":kind,"gpuPct":rounded(sample.value.clamp(0.0,100.0),1)}));
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
    json!({"engineSamples":counters.samples.iter().filter(|s|s.counter=="engine").count(),"byGpuPct":by_pct,"byDedicatedMB":by_mem,
        "adapters":adapters.iter().map(|r|json!({"name":r["Name"],"vendor":r["AdapterCompatibility"],"driver":r["DriverVersion"],"status":r["Status"],"bus":r["PNPDeviceID"].as_str().and_then(|v|v.split('\\').next())})).collect::<Vec<_>>(),
        "counterErrors":counter_errors(counters),
        "notice":"GPU 利用率取每进程跨适配器最繁忙的单一引擎，不累加并行引擎；engines 保留适配器和引擎样本。byDedicatedMB 仅聚合大于 1 MiB 的专用显存实例。nvidia 为按 uuid/pciBusId 区分的显卡状态；缺失或失败时由 lhmGpu 补充。空 hardware 不代表没有显卡，失败来源见 counterErrors/collectionErrors。"})
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
    }).take(200).map(|s|json!({"hw":s["hw"],"name":s["name"],"type":s["type"],"value":rounded(s["value"].as_f64().unwrap(),2)})).collect();
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
    out["counterErrors"] = counter_errors(batch);
    out["notice"] = json!(
        "sensors 为 LHM 返回的温度（°C）、风扇（RPM）与电压（V），仅保留有限值，最多 200 项。thermalZones 为 ACPI 热区（K 转 °C，仅保留 -50～150°C），frequency 为各逻辑核频率占最大频率的百分比。admin 与 pawnio 只描述权限和注册项；可读项以返回的读数为准，空值为采集缺失。本工具不安装驱动。"
    );
    out
}

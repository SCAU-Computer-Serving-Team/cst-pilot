use super::win_native::{
    CIMV2, Control, DiagnosticSource, ProcessSample, add_errors, collect, number, rounded,
};
use chrono::{FixedOffset, NaiveDateTime, TimeZone, Utc};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SysScope {
    #[default]
    Overview,
    Proc,
    Io,
    Gpu,
    Sensor,
}
#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SysInput {
    #[serde(default)]
    pub scope: SysScope,
    pub top: Option<usize>,
}

pub fn run(
    source: &dyn DiagnosticSource,
    input: SysInput,
    control: &Control,
) -> Result<Value, String> {
    run_with(
        source,
        &super::hardware::NativeHardware::new(None),
        input,
        control,
    )
}

pub fn run_with(
    source: &dyn DiagnosticSource,
    hardware: &dyn super::hardware::HardwareSource,
    input: SysInput,
    control: &Control,
) -> Result<Value, String> {
    let top = input.top.unwrap_or(10).clamp(1, 50);
    control.check()?;
    match input.scope {
        SysScope::Io => Ok(json!({"io":super::sys_load::io(source,top,control)?})),
        SysScope::Gpu => Ok(json!({"gpu":super::sys_load::gpu(source,hardware,top,control)?})),
        SysScope::Sensor => Ok(json!({"sensor":super::sys_load::sensor(source,hardware,control)?})),
        SysScope::Overview => Ok(json!({"overview": overview(source,control)?})),
        SysScope::Proc => {
            let first = source.processes(control).map_err(|e| e.to_string())?;
            let started = Instant::now();
            control.sleep(Duration::from_millis(1200))?;
            let second = source.processes(control).map_err(|e| e.to_string())?;
            let elapsed = started.elapsed().as_secs_f64();
            Ok(
                json!({"proc": process_result(&first,&second,elapsed,std::thread::available_parallelism().map_or(1,usize::from),input.top.unwrap_or(10).clamp(1,50))}),
            )
        }
    }
}

pub fn process_result(
    first: &[ProcessSample],
    second: &[ProcessSample],
    interval: f64,
    cores: usize,
    top: usize,
) -> Value {
    let first: HashMap<_, _> = first.iter().map(|p| ((p.pid, p.created), p)).collect();
    let rows:Vec<Value>=second.iter().filter_map(|p| {
        let previous=first.get(&(p.pid,p.created))?;
        let delta=p.cpu_100ns.checked_sub(previous.cpu_100ns)?;
        Some(json!({"name":p.name,"pid":p.pid,"wsMB":p.working_set.map(|v|rounded(v as f64/1048576.0,0)),"cpuPct":rounded(100.0*delta as f64/10_000_000.0/interval/cores.max(1) as f64,1),"path":p.path}))
    }).collect();
    let mut cpu = rows.clone();
    let mut mem = rows.clone();
    cpu.sort_by(|a, b| {
        b["cpuPct"]
            .as_f64()
            .unwrap_or_default()
            .total_cmp(&a["cpuPct"].as_f64().unwrap_or_default())
    });
    mem.sort_by(|a, b| {
        b["wsMB"]
            .as_f64()
            .unwrap_or(-1.0)
            .total_cmp(&a["wsMB"].as_f64().unwrap_or(-1.0))
    });
    cpu.truncate(top);
    mem.truncate(top);
    json!({"cores":cores,"totalProcs":rows.len(),"intervalSec":rounded(interval,2),"byCpu":cpu,"byMem":mem,"notice":"CPU 按实际采样间隔与逻辑核数计算平均占用率；wsMB 为工作集 MiB。权限不足时 path 为 null，新建、退出或 PID 已复用的进程不参与差分。"})
}

fn overview(source: &dyn DiagnosticSource, control: &Control) -> Result<Value, String> {
    let mut errors = Vec::new();
    let cpu = collect(
        source,
        CIMV2,
        "SELECT Name,PercentProcessorTime FROM Win32_PerfFormattedData_PerfOS_Processor",
        control,
        &mut errors,
    );
    let os=collect(source,CIMV2,"SELECT TotalVisibleMemorySize,FreePhysicalMemory,LastBootUpTime FROM Win32_OperatingSystem",control,&mut errors).into_iter().next().unwrap_or_default();
    let pages = collect(
        source,
        CIMV2,
        "SELECT Name,AllocatedBaseSize,CurrentUsage,PeakUsage FROM Win32_PageFileUsage",
        control,
        &mut errors,
    );
    let pool = collect(
        source,
        CIMV2,
        "SELECT PoolNonpagedBytes,PoolPagedBytes FROM Win32_PerfFormattedData_PerfOS_Memory",
        control,
        &mut errors,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let cs = collect(
        source,
        CIMV2,
        "SELECT Manufacturer,Model,NumberOfLogicalProcessors FROM Win32_ComputerSystem",
        control,
        &mut errors,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let bios = collect(
        source,
        CIMV2,
        "SELECT Manufacturer,SMBIOSBIOSVersion,ReleaseDate FROM Win32_BIOS",
        control,
        &mut errors,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let processors = collect(
        source,
        CIMV2,
        "SELECT Name,NumberOfCores FROM Win32_Processor",
        control,
        &mut errors,
    );
    control.check()?;
    if errors.len() == 7 {
        return Err("整机信息采集全部失败，请检查 WMI 服务状态。".into());
    }
    let total = number(&os, "TotalVisibleMemorySize").map(|v| v / 1024.0);
    let free = number(&os, "FreePhysicalMemory").map(|v| v / 1024.0);
    let used = total.zip(free).map(|(t, f)| (t - f).max(0.0));
    let boot = os["LastBootUpTime"].as_str().and_then(parse_dmtf);
    let uptime=boot.map(|boot|{
        let minutes=(Utc::now()-boot).num_minutes().max(0);
        json!({"bootTime":boot.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),"text":format!("{}天{}小时{}分",minutes/1440,minutes%1440/60,minutes%60),"totalHours":rounded(minutes as f64/60.0,1)})
    });
    let cores: Option<f64> = processors
        .iter()
        .map(|p| number(p, "NumberOfCores"))
        .try_fold(0.0, |sum, v| v.map(|n| sum + n));
    let mut result = json!({
        "cpuTotalPct":cpu.iter().find(|r|r["Name"]=="_Total").and_then(|r|number(r,"PercentProcessorTime")),
        "logicalCores":number(&cs,"NumberOfLogicalProcessors"),
        "mem":{"totalMB":total.map(|v|rounded(v,0)),"freeMB":free.map(|v|rounded(v,0)),"usedMB":used.map(|v|rounded(v,0)),"usedPct":used.zip(total).filter(|(_,t)|*t>0.0).map(|(u,t)|rounded(u/t*100.0,1))},
        "pagefile":pages.iter().map(|r|super::win_native::selected(r,&[("name","Name"),("allocMB","AllocatedBaseSize"),("usedMB","CurrentUsage"),("peakMB","PeakUsage")])).collect::<Vec<_>>(),
        "pool":{"nonpagedMB":number(&pool,"PoolNonpagedBytes").map(|v|rounded(v/1048576.0,0)),"pagedMB":number(&pool,"PoolPagedBytes").map(|v|rounded(v/1048576.0,0))},
        "machine":{"vendor":cs["Manufacturer"],"model":cs["Model"],"cpu":processors.first().and_then(|p|p["Name"].as_str()).map(|s|s.split_whitespace().collect::<Vec<_>>().join(" ")),"physicalCores":cores.filter(|v|*v>0.0),"bios":bios["Manufacturer"].as_str().zip(bios["SMBIOSBIOSVersion"].as_str()).map(|(a,b)|format!("{a} {b}")),"biosDate":bios["ReleaseDate"].as_str().and_then(parse_dmtf).map(|d|d.format("%Y-%m-%d").to_string())},
        "uptime":uptime,"notice":"Windows 原生 WMI 快照。内存与内核池单位为 MiB；cpuTotalPct 为系统格式化计数器当前值。null 表示未采集到数据，部分失败见 collectionErrors。"
    });
    add_errors(&mut result, errors);
    Ok(result)
}

pub fn definition(
    root: &std::path::Path,
    source: std::sync::Arc<dyn DiagnosticSource>,
) -> pi_session::ToolDefinition {
    let hardware = std::sync::Arc::new(super::hardware::NativeHardware::new(Some(
        root.to_path_buf(),
    )));
    let mut tool = pi_session::ToolDefinition::from_typed(
        "sys",
        "sys",
        "只读系统检查：overview 整机概况、proc 进程 CPU/内存排行、io 物理盘与进程 I/O、gpu GPU 引擎与显存、sensor 温度/风扇/电压与频率。",
        move |_, input: SysInput, cancel, _, _| {
            let source = source.clone();
            let hardware = hardware.clone();
            async move {
                let result = super::execute(
                    source,
                    input,
                    cancel.unwrap_or_default(),
                    Duration::from_secs(30),
                    move |source, input, control| {
                        run_with(source, hardware.as_ref(), input, control)
                    },
                )
                .await?;
                super::result::diagnostic_result(result)
            }
        },
    );
    tool.prompt_snippet=Some("只读查询整机负载、进程、磁盘 I/O、GPU 与可读传感器。遵循 notice 的采样口径，缺失或降级项不表示正常。".into());
    tool
}

fn parse_dmtf(text: &str) -> Option<chrono::DateTime<Utc>> {
    if text.len() < 25 || !text.is_ascii() {
        return None;
    }
    let date = NaiveDateTime::parse_from_str(&text[..14], "%Y%m%d%H%M%S").ok()?;
    let minutes: i32 = text[22..25].parse().ok()?;
    let sign = match &text[21..22] {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    FixedOffset::east_opt(sign * minutes * 60)?
        .from_local_datetime(&date)
        .single()
        .map(|d| d.with_timezone(&Utc))
}

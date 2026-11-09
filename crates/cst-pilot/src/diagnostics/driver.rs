use super::win_native::{
    CIMV2, Control, DiagnosticSource, add_errors, collect, number, rounded, selected,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DriverScope {
    #[default]
    Problem,
    Core,
    External,
    Find,
}
#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverInput {
    #[serde(default)]
    pub scope: DriverScope,
    pub name: Option<String>,
    pub class: Option<String>,
    pub id: Option<String>,
}

pub fn run(
    source: &dyn DiagnosticSource,
    input: DriverInput,
    control: &Control,
) -> Result<Value, String> {
    let mut errors = Vec::new();
    let (scope, mut result) = match input.scope {
        DriverScope::Problem | DriverScope::Find => {
            let find = matches!(input.scope, DriverScope::Find);
            if find
                && [&input.name, &input.class, &input.id]
                    .iter()
                    .all(|s| s.as_ref().is_none_or(|s| s.trim().is_empty()))
            {
                return Err("find 至少提供 name、class 或 id 条件之一。".into());
            }
            for value in [&input.name, &input.class, &input.id].into_iter().flatten() {
                if value.len() > 512 {
                    return Err("查找条件过长。".into());
                }
            }
            let wql = if find {
                "SELECT Name,PNPClass,Status,ConfigManagerErrorCode,DeviceID,HardwareID FROM Win32_PnPEntity"
            } else {
                "SELECT Name,PNPClass,Status,ConfigManagerErrorCode,DeviceID,HardwareID FROM Win32_PnPEntity WHERE Status='Error' OR Status='Unknown'"
            };
            let rows = collect(source, CIMV2, wql, control, &mut errors);
            let devices: Vec<_> = rows
                .iter()
                .filter(|r| {
                    (!find && matches!(r["Status"].as_str(), Some("Error" | "Unknown")))
                        || find
                            && contains(&r["Name"], input.name.as_deref())
                            && contains(&r["PNPClass"], input.class.as_deref())
                            && input.id.as_deref().is_none_or(|id| {
                                contains(&r["DeviceID"], Some(id))
                                    || r["HardwareID"].as_array().is_some_and(|ids| {
                                        ids.iter().any(|v| contains(v, Some(id)))
                                    })
                            })
                })
                .map(|r| {
                    let mut row = device(r);
                    row["hardwareIds"] = r["HardwareID"]
                        .as_array()
                        .map(|a| json!(a))
                        .unwrap_or_else(|| json!([]));
                    row
                })
                .collect();
            (
                if find { "find" } else { "problem" },
                json!({"count":devices.len(),"devices":devices,"notice":"Windows PnP 设备只读查询；多个条件按 AND 组合，id 匹配设备 ID 与硬件 ID。状态与错误码保留系统原值。"}),
            )
        }
        DriverScope::Core => {
            let net = collect(
                source,
                CIMV2,
                "SELECT Name,NetConnectionID,PhysicalAdapter,NetConnectionStatus FROM Win32_NetworkAdapter WHERE NetConnectionID IS NOT NULL",
                control,
                &mut errors,
            );
            let pnp = collect(
                source,
                CIMV2,
                "SELECT Name,PNPClass,Status,ConfigManagerErrorCode FROM Win32_PnPEntity WHERE PNPClass='Bluetooth' OR PNPClass='MEDIA'",
                control,
                &mut errors,
            );
            let display = collect(
                source,
                CIMV2,
                "SELECT Name,AdapterCompatibility,DriverVersion,Status,PNPDeviceID FROM Win32_VideoController",
                control,
                &mut errors,
            );
            let services = collect(
                source,
                CIMV2,
                "SELECT Name,State FROM Win32_Service WHERE Name='bthserv' OR Name='Audiosrv'",
                control,
                &mut errors,
            );
            let drivers = collect(
                source,
                CIMV2,
                "SELECT DeviceClass,DeviceName,DriverVersion,DriverDate,DriverProviderName FROM Win32_PnPSignedDriver WHERE DeviceClass='NET' OR DeviceClass='BLUETOOTH' OR DeviceClass='MEDIA' OR DeviceClass='DISPLAY'",
                control,
                &mut errors,
            );
            let group = |class: &str| {
                pnp.iter()
                    .filter(|r| {
                        r["PNPClass"]
                            .as_str()
                            .is_some_and(|v| v.eq_ignore_ascii_case(class))
                    })
                    .map(|r| {
                        selected(
                            r,
                            &[
                                ("name", "Name"),
                                ("status", "Status"),
                                ("errorCode", "ConfigManagerErrorCode"),
                            ],
                        )
                    })
                    .collect::<Vec<_>>()
            };
            (
                "core",
                json!({"net":net.iter().filter(|r|r["NetConnectionID"].as_str().is_some_and(|s|!s.is_empty())).map(|r|{let mut v=selected(r,&[("name","Name"),("connId","NetConnectionID"),("connStatus","NetConnectionStatus")]);v["physical"]=json!(r["PhysicalAdapter"].as_bool().unwrap_or(false));v}).collect::<Vec<_>>(),"bluetooth":group("Bluetooth"),"audio":group("MEDIA"),"display":display.iter().map(|r|{let mut v=selected(r,&[("name","Name"),("vendor","AdapterCompatibility"),("driver","DriverVersion"),("status","Status")]);v["bus"]=json!(r["PNPDeviceID"].as_str().and_then(|s|s.split('\\').next()));v}).collect::<Vec<_>>(),"services":services.iter().map(|r|selected(r,&[("name","Name"),("state","State")])).collect::<Vec<_>>(),"drivers":drivers.iter().map(|r|selected(r,&[("class","DeviceClass"),("device","DeviceName"),("version","DriverVersion"),("date","DriverDate"),("provider","DriverProviderName")])).collect::<Vec<_>>(),"notice":"网卡含虚拟适配器；蓝牙、音频、显示设备及四类签名驱动均来自原生 WMI。空结果需结合 collectionErrors 判读。"}),
            )
        }
        DriverScope::External => {
            let devices = collect(
                source,
                CIMV2,
                "SELECT Name,PNPClass,Status,ConfigManagerErrorCode,DeviceID FROM Win32_PnPEntity WHERE DeviceID LIKE 'USB%' OR DeviceID LIKE 'BTHENUM%' OR DeviceID LIKE 'DISPLAY%'",
                control,
                &mut errors,
            );
            let disks = collect(
                source,
                CIMV2,
                "SELECT Model,InterfaceType,MediaType,Size FROM Win32_DiskDrive WHERE MediaType LIKE '%Removable%'",
                control,
                &mut errors,
            );
            (
                "external",
                json!({"devices":devices.iter().map(device).collect::<Vec<_>>(),"removable":disks.iter().map(|r|{let mut v=selected(r,&[("model","Model"),("interface","InterfaceType"),("mediaType","MediaType")]);v["sizeGB"]=json!(number(r,"Size").map(|n|rounded(n/1073741824.0,1)));v}).collect::<Vec<_>>(),"notice":"外设按 USB、BTHENUM、DISPLAY 总线前缀及可移动磁盘筛选，可能包含内置摄像头和内置屏。"}),
            )
        }
    };
    control.check()?;
    let sources = match scope {
        "core" => 6,
        "external" => 2,
        _ => 1,
    };
    if errors.len() == sources {
        return Err("设备与驱动信息采集全部失败，请检查 WMI 服务状态。".into());
    }
    add_errors(&mut result, errors);
    Ok(json!({scope:result}))
}
fn contains(value: &Value, needle: Option<&str>) -> bool {
    needle.is_none_or(|n| {
        value
            .as_str()
            .is_some_and(|s| s.to_lowercase().contains(&n.to_lowercase()))
    })
}
fn device(row: &Value) -> Value {
    selected(
        row,
        &[
            ("name", "Name"),
            ("class", "PNPClass"),
            ("status", "Status"),
            ("errorCode", "ConfigManagerErrorCode"),
            ("deviceId", "DeviceID"),
        ],
    )
}

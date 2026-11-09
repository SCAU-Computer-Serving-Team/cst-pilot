use super::win_native::{
    CIMV2, Control, DiagnosticSource, NativeError, STORAGE, add_errors, collect, number, rounded,
    selected,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DiskScope {
    Space,
    Info,
    Health,
    Usage,
    All,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiskInput {
    pub scope: DiskScope,
    pub drive: Option<String>,
    pub path: Option<String>,
    pub top: Option<usize>,
}
const PHYSICAL: &str = "SELECT FriendlyName,SerialNumber,MediaType,BusType,HealthStatus,OperationalStatus,Size,DeviceId FROM MSFT_PhysicalDisk";
const SMART: &str = "SELECT DeviceId,Wear,Temperature,PowerOnHours,ReadErrorsTotal,WriteErrorsTotal FROM MSFT_StorageReliabilityCounter";

pub fn normalize_drive(drive: Option<&str>) -> Result<Option<String>, String> {
    drive
        .map(|s| {
            let bytes = s.as_bytes();
            if !(bytes.len() == 1 || bytes.len() == 2 && bytes[1] == b':')
                || !bytes[0].is_ascii_alphabetic()
            {
                return Err("drive 只接受盘符，如 C 或 C:。".into());
            }
            Ok(format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
        })
        .transpose()
}
pub fn run(
    source: &dyn DiagnosticSource,
    input: DiskInput,
    control: &Control,
) -> Result<Value, String> {
    if matches!(input.scope, DiskScope::Usage) {
        return Err("usage 需要工具包共享扫描实例。".into());
    }
    let drive = normalize_drive(input.drive.as_deref())?;
    let mut result = json!({});
    let mut errors = Vec::new();
    if matches!(input.scope, DiskScope::Space | DiskScope::All) {
        match source.space(drive.as_deref(), control) {
            Ok(snapshot) => {
                if snapshot.rows.is_empty()
                    && !snapshot.errors.is_empty()
                    && matches!(input.scope, DiskScope::Space)
                {
                    return Err(snapshot.errors[0].to_string());
                }
                errors.extend(snapshot.errors);
                result["space"]=json!(snapshot.rows.iter().map(|r|json!({"drive":r.drive,"totalGB":rounded(r.total as f64/1073741824.0,1),"freeGB":rounded(r.free as f64/1073741824.0,1),"usedPct":if r.total>0{Some(rounded((1.0-r.free as f64/r.total as f64)*100.0,1))}else{None}})).collect::<Vec<_>>());
            }
            Err(error) if matches!(input.scope, DiskScope::Space) => return Err(error.to_string()),
            Err(error) => {
                errors.push(error);
                result["space"] = json!([]);
            }
        }
    }
    let need_info = matches!(input.scope, DiskScope::Info | DiskScope::All);
    let need_health = matches!(input.scope, DiskScope::Health | DiskScope::All);
    let ids = if (need_info || need_health) && drive.is_some() {
        physical_ids(source, drive.as_deref().unwrap_or_default(), control).map(Some)
    } else {
        Ok(None)
    };
    let previous_errors = errors.len();
    let physical = if need_info || need_health && ids.is_ok() {
        collect(source, STORAGE, PHYSICAL, control, &mut errors)
    } else {
        Vec::new()
    };
    let physical_failed = errors.len() > previous_errors;
    let included = |row: &Value| {
        ids.as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_none_or(|ids| ids.contains(&device_id(row)))
    };
    if need_info {
        let before = errors.len();
        let volumes = collect(
            source,
            CIMV2,
            "SELECT DeviceID,VolumeName,FileSystem,DriveType,Size,FreeSpace FROM Win32_LogicalDisk",
            control,
            &mut errors,
        );
        if matches!(input.scope, DiskScope::Info) && physical_failed && errors.len() > before {
            return Err("物理盘与卷信息采集全部失败，请检查 WMI 服务状态。".into());
        }
        result["physicalDisks"] = json!(
            physical
                .iter()
                .filter(|r| included(r))
                .map(|r| {
                    let mut row = selected(
                        r,
                        &[
                            ("FriendlyName", "FriendlyName"),
                            ("SerialNumber", "SerialNumber"),
                            ("MediaType", "MediaType"),
                            ("BusType", "BusType"),
                            ("HealthStatus", "HealthStatus"),
                            ("OperationalStatus", "OperationalStatus"),
                            ("DeviceId", "DeviceId"),
                        ],
                    );
                    row["sizeGB"] = json!(number(r, "Size").map(|n| rounded(n / 1073741824.0, 1)));
                    row
                })
                .collect::<Vec<_>>()
        );
        result["volumes"] = json!(
            volumes
                .iter()
                .filter(|r| drive.as_ref().is_none_or(|d| r["DeviceID"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(d))))
                .map(|r| {
                    let mut row = selected(
                        r,
                        &[
                            ("drive", "DeviceID"),
                            ("label", "VolumeName"),
                            ("fs", "FileSystem"),
                            ("driveType", "DriveType"),
                        ],
                    );
                    row["totalGB"] = json!(number(r, "Size").map(|v| rounded(v / 1073741824.0, 1)));
                    row["freeGB"] =
                        json!(number(r, "FreeSpace").map(|v| rounded(v / 1073741824.0, 1)));
                    row
                })
                .collect::<Vec<_>>()
        );
        if let Err(error) = &ids {
            result["infoNotice"] =
                json!("无法确定目标卷对应的物理盘，physicalDisks 保留全量设备。");
            errors.push(error.clone());
        }
    }
    if need_health {
        if let Err(error) = &ids {
            result["smart"] = Value::Null;
            result["smartNotice"] = json!("无法确定目标卷对应的物理盘，已停止查询 SMART。");
            result["smartErrors"] = json!([{"deviceId":drive,"error":error.message}]);
            errors.push(error.clone());
        } else {
            let targets: Vec<_> = physical
                .iter()
                .filter(|r| included(r))
                .map(device_id)
                .collect();
            let mut rows = Vec::new();
            let mut smart_errors = Vec::new();
            let mut denied = false;
            for id in targets.iter().take(256) {
                control.check()?;
                let escaped = id.replace('\\', "\\\\").replace('\'', "\\'");
                let query = format!("{SMART} WHERE DeviceId='{escaped}'");
                let outcome = source.query(STORAGE, &query, control).and_then(|values| {
                    let values: Vec<_> =
                        values.into_iter().filter(|r| device_id(r) == *id).collect();
                    if values.is_empty() {
                        Err(NativeError {
                            source: "MSFT_StorageReliabilityCounter".into(),
                            code: None,
                            message: "设备未返回可靠性计数器。".into(),
                        })
                    } else {
                        Ok(values)
                    }
                });
                match outcome {
                    Ok(values) => rows.extend(values),
                    Err(error) => {
                        denied |= error.denied();
                        smart_errors
                            .push(json!({"deviceId":id,"error":error.message,"code":error.code}));
                        errors.push(error);
                    }
                }
            }
            if targets.is_empty() {
                smart_errors.push(json!({"deviceId":null,"error":"未找到可查询的物理盘。"}));
                denied |= errors.iter().any(NativeError::denied);
            }
            if targets.len() > 256 {
                smart_errors.push(json!({"deviceId":null,"error":"超过单次 256 个物理盘的查询上限，请指定 drive。"}));
            }
            result["smart"] = if rows.is_empty() {
                Value::Null
            } else {
                json!(rows)
            };
            result["smartNotice"] = json!(if denied {
                "部分可靠性计数器读取被拒绝，请使用管理员权限重试。"
            } else {
                "可靠性计数器来自 Windows 存储提供程序；未返回的字段表示设备不支持或采集缺失，无法据此判断磁盘健康。"
            });
            if !smart_errors.is_empty() {
                result["smartErrors"] = json!(smart_errors);
                result["degraded"] = json!(true);
            }
        }
    }
    control.check()?;
    add_errors(&mut result, errors);
    Ok(result)
}
fn device_id(row: &Value) -> String {
    row["DeviceId"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| row["DeviceId"].to_string())
}
fn physical_ids(
    source: &dyn DiagnosticSource,
    drive: &str,
    control: &Control,
) -> Result<HashSet<String>, NativeError> {
    let rows = source.query(
        CIMV2,
        "SELECT Antecedent,Dependent FROM Win32_LogicalDiskToPartition",
        control,
    )?;
    let volume = regex::Regex::new(r#"(?i)DeviceID\s*=\s*"([a-z]:)""#).expect("static regex");
    let disk = regex::Regex::new(r"(?i)Disk #(\d+)").expect("static regex");
    let ids: HashSet<String> = rows
        .iter()
        .filter_map(|r| {
            let dep = r["Dependent"].as_str()?;
            let ant = r["Antecedent"].as_str()?;
            if !volume
                .captures(dep)?
                .get(1)?
                .as_str()
                .eq_ignore_ascii_case(drive)
            {
                return None;
            }
            Some(disk.captures(ant)?.get(1)?.as_str().to_owned())
        })
        .collect();
    if ids.is_empty() {
        Err(NativeError {
            source: "Win32_LogicalDiskToPartition".into(),
            code: None,
            message: "无法确定目标卷对应的物理盘。".into(),
        })
    } else {
        Ok(ids)
    }
}

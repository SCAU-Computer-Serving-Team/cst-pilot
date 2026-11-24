use super::win_native::{
    CIMV2, Control, DiagnosticSource, NativeError, STORAGE, VolumeSnapshot, add_errors, collect,
    number, rounded, selected,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

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

fn normalize_serial(row: &Value) -> Option<String> {
    let raw = row["SerialNumber"].as_str()?;
    let norm: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\0')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (!norm.is_empty()).then_some(norm)
}

fn project_space_from_volumes(snapshot: &VolumeSnapshot, drive: Option<&str>) -> Vec<Value> {
    snapshot
        .rows
        .iter()
        .filter(|r| {
            drive.is_none_or(|d| {
                r["DeviceID"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(d))
            })
        })
        .filter_map(|r| {
            let d = r["DeviceID"].as_str()?;
            let total = number(r, "Size")?;
            let free = number(r, "FreeSpace")?;
            let clamped_free = free.min(total);
            Some(json!({
                "drive": d,
                "totalGB": rounded(total / 1073741824.0, 1),
                "freeGB": rounded(clamped_free / 1073741824.0, 1),
                "usedPct": if total > 0.0 {
                    Some(rounded((1.0 - clamped_free / total) * 100.0, 1))
                } else {
                    None
                }
            }))
        })
        .collect()
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
    let need_space = matches!(input.scope, DiskScope::Space | DiskScope::All);
    let need_info = matches!(input.scope, DiskScope::Info | DiskScope::All);
    let need_health = matches!(input.scope, DiskScope::Health | DiskScope::All);

    // Reuse one native volume probe across `space` and `info` when running `all` so remote SMB
    // volumes are never probed twice.
    let mut cached_volumes: Option<VolumeSnapshot> = None;
    if need_space && need_info {
        match source.native_volumes(drive.as_deref(), control) {
            Ok(Some(snapshot)) => {
                let space_rows = project_space_from_volumes(&snapshot, drive.as_deref());
                errors.extend(snapshot.errors.clone());
                result["space"] = json!(space_rows);
                cached_volumes = Some(snapshot);
            }
            Ok(None) => {}
            Err(error) => {
                errors.push(error);
            }
        }
    }

    if need_space && result.get("space").is_none() {
        match source.space(drive.as_deref(), control) {
            Ok(snapshot) => {
                if snapshot.rows.is_empty()
                    && !snapshot.errors.is_empty()
                    && matches!(input.scope, DiskScope::Space)
                {
                    return Err(snapshot.errors[0].to_string());
                }
                errors.extend(snapshot.errors);
                result["space"] = json!(
                    snapshot
                        .rows
                        .iter()
                        .map(|r| {
                            let free = r.free.min(r.total);
                            json!({
                                "drive": r.drive,
                                "totalGB": rounded(r.total as f64 / 1073741824.0, 1),
                                "freeGB": rounded(free as f64 / 1073741824.0, 1),
                                "usedPct": if r.total > 0 {
                                    Some(rounded((1.0 - free as f64 / r.total as f64) * 100.0, 1))
                                } else {
                                    None
                                }
                            })
                        })
                        .collect::<Vec<_>>()
                );
            }
            Err(error) if matches!(input.scope, DiskScope::Space) => return Err(error.to_string()),
            Err(error) => {
                errors.push(error);
                result["space"] = json!([]);
            }
        }
    }

    let previous_errors = errors.len();
    let mut physical_from_native = false;
    let physical = if !need_info && need_health {
        // Health-only queries prefer SetupAPI native enumeration to avoid cold Storage WMI when
        // every disk can be answered directly by native IOCTL SMART.
        match source.native_physical_disks(control) {
            Ok(Some(native_rows)) if !native_rows.is_empty() => {
                physical_from_native = true;
                native_rows
            }
            _ => collect(source, STORAGE, PHYSICAL, control, &mut errors),
        }
    } else if need_info || need_health {
        let mut wmi_errors = Vec::new();
        let wmi_rows = collect(source, STORAGE, PHYSICAL, control, &mut wmi_errors);
        if !wmi_errors.is_empty() && wmi_rows.is_empty() {
            match source.native_physical_disks(control) {
                Ok(Some(native_rows)) if !native_rows.is_empty() => {
                    physical_from_native = true;
                    native_rows
                }
                _ => {
                    errors.extend(wmi_errors);
                    Vec::new()
                }
            }
        } else {
            errors.extend(wmi_errors);
            wmi_rows
        }
    } else {
        Vec::new()
    };
    let physical_failed = errors.len() > previous_errors;
    let ids = if (need_info || need_health) && drive.is_some() {
        physical_ids(
            source,
            drive.as_deref().unwrap_or_default(),
            &physical,
            physical_from_native,
            control,
        )
        .map(Some)
    } else {
        Ok(None)
    };
    let included = |row: &Value| {
        ids.as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_none_or(|ids| ids.contains(&device_id(row)))
    };

    if need_info {
        let before = errors.len();
        let volumes = if let Some(snapshot) = cached_volumes.take() {
            snapshot.rows
        } else {
            match source.native_volumes(drive.as_deref(), control) {
                Ok(Some(snapshot)) => {
                    errors.extend(snapshot.errors);
                    snapshot.rows
                }
                Ok(None) => collect(
                    source,
                    CIMV2,
                    "SELECT DeviceID,VolumeName,FileSystem,DriveType,Size,FreeSpace FROM Win32_LogicalDisk",
                    control,
                    &mut errors,
                ),
                Err(err) => {
                    errors.push(err);
                    collect(
                        source,
                        CIMV2,
                        "SELECT DeviceID,VolumeName,FileSystem,DriveType,Size,FreeSpace FROM Win32_LogicalDisk",
                        control,
                        &mut errors,
                    )
                }
            }
        };
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
        if ids.is_err() {
            result["infoNotice"] =
                json!("无法确定目标卷对应的物理盘，physicalDisks 保留全量设备。");
        }
    }
    if let Err(error) = &ids {
        errors.push(error.clone());
    }

    if need_health {
        if let Err(error) = &ids {
            result["smart"] = Value::Null;
            result["smartNotice"] = json!("无法确定目标卷对应的物理盘，已停止查询 SMART。");
            result["smartErrors"] = json!([{"deviceId":drive,"error":error.message}]);
        } else {
            let target_rows: Vec<&Value> = physical.iter().filter(|r| included(r)).collect();
            let mut rows = Vec::new();
            let mut smart_errors = Vec::new();
            let mut denied = false;
            let mut native_denied_ids: HashSet<String> = HashSet::new();
            let mut fallback_targets: Vec<(String, Option<String>)> = Vec::new();
            let mut used_native_ioctl = false;
            let mut used_controller_scope = false;
            for target_row in target_rows.iter().take(256) {
                control.check()?;
                let id = device_id(target_row);
                let serial = normalize_serial(target_row);
                if let Ok(disk_num) = id.trim().parse::<u32>() {
                    match source.native_disk_health(disk_num, &id, serial.as_deref(), control) {
                        Ok(Some(native_row)) => {
                            used_native_ioctl = true;
                            used_controller_scope |= native_row.controller_scope;
                            rows.push(native_row.row);
                            continue;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            if error.denied() {
                                native_denied_ids.insert(id.clone());
                            }
                        }
                    }
                }
                fallback_targets.push((id, serial));
            }
            if !fallback_targets.is_empty() {
                control.check()?;
                // If physical targets came from native SetupAPI (OS disk numbers), map them to
                // WMI `MSFT_PhysicalDisk.DeviceId` via verified serial numbers before querying
                // `MSFT_StorageReliabilityCounter` so we never guess WMI IDs on Storage Spaces.
                let wmi_id_map: Option<HashMap<String, String>> = if physical_from_native {
                    match source.query(STORAGE, PHYSICAL, control) {
                        Ok(wmi_disks) => {
                            let mut by_serial: HashMap<String, Vec<String>> = HashMap::new();
                            let mut all_wmi_ids = HashSet::new();
                            for wd in &wmi_disks {
                                let wid = device_id(wd);
                                all_wmi_ids.insert(wid.clone());
                                if let Some(s) = normalize_serial(wd) {
                                    by_serial.entry(s).or_default().push(wid);
                                }
                            }
                            let mut map = HashMap::new();
                            for (native_id, serial) in &fallback_targets {
                                if let Some(s) = serial
                                    && let Some(matches) = by_serial.get(s)
                                    && matches.len() == 1
                                {
                                    map.insert(native_id.clone(), matches[0].clone());
                                } else if serial.is_none() && all_wmi_ids.contains(native_id) {
                                    map.insert(native_id.clone(), native_id.clone());
                                }
                            }
                            Some(map)
                        }
                        Err(_) => None,
                    }
                } else {
                    None
                };

                match source.query(STORAGE, SMART, control) {
                    Ok(all_rows) => {
                        for (id, _) in &fallback_targets {
                            let lookup_id = if let Some(map) = &wmi_id_map {
                                map.get(id).cloned()
                            } else {
                                Some(id.clone())
                            };
                            let matched: Vec<_> = match lookup_id.as_deref() {
                                Some(wid) => all_rows
                                    .iter()
                                    .filter(|r| device_id(r) == wid)
                                    .cloned()
                                    .collect(),
                                None => Vec::new(),
                            };
                            if matched.is_empty() {
                                let is_denied = native_denied_ids.contains(id);
                                denied |= is_denied;
                                let message = if is_denied {
                                    "设备可靠性计数器访问被拒绝。"
                                } else if physical_from_native
                                    && wmi_id_map.is_some()
                                    && lookup_id.is_none()
                                {
                                    "无法确认该物理盘与 WMI 可靠性计数器的唯一对应关系，已跳过猜测匹配。"
                                } else {
                                    "设备未返回可靠性计数器。"
                                };
                                let error = NativeError {
                                    source: "MSFT_StorageReliabilityCounter".into(),
                                    code: is_denied.then_some(5),
                                    message: message.into(),
                                };
                                smart_errors.push(
                                    json!({"deviceId":id,"error":error.message,"code":error.code}),
                                );
                                errors.push(error);
                            } else {
                                rows.extend(matched);
                            }
                        }
                    }
                    Err(error) => {
                        denied |= error.denied() || !native_denied_ids.is_empty();
                        for (id, _) in &fallback_targets {
                            smart_errors.push(
                                json!({"deviceId":id,"error":error.message,"code":error.code}),
                            );
                        }
                        errors.push(error);
                    }
                }
            }
            if target_rows.is_empty() {
                smart_errors.push(json!({"deviceId":null,"error":"未找到可查询的物理盘。"}));
                denied |= errors.iter().any(NativeError::denied);
            }
            if target_rows.len() > 256 {
                smart_errors.push(json!({"deviceId":null,"error":"超过单次 256 个物理盘的查询上限，请指定 drive。"}));
            }
            result["smart"] = if rows.is_empty() {
                Value::Null
            } else {
                json!(rows)
            };
            let scope_note = if used_controller_scope {
                "部分 NVMe 指标来自控制器级日志，多命名空间可能共享计数。"
            } else {
                ""
            };
            result["smartNotice"] = json!(if denied {
                "部分可靠性计数器读取被拒绝，请使用管理员权限重试。".to_owned()
            } else if used_native_ioctl && fallback_targets.is_empty() {
                format!(
                    "可靠性数据来自 Windows 原生存储 IOCTL；{scope_note}未返回的字段表示设备不支持或采集缺失，无法据此判断磁盘健康。"
                )
            } else {
                format!(
                    "可靠性计数器来自 Windows 存储提供程序与原生存储 IOCTL；{scope_note}未返回的字段表示设备不支持或采集缺失，无法据此判断磁盘健康。"
                )
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
    physical: &[Value],
    physical_from_native: bool,
    control: &Control,
) -> Result<HashSet<String>, NativeError> {
    if let Ok(Some(nums)) = source.volume_disk_numbers(drive, control)
        && !nums.is_empty()
    {
        let mapped: HashSet<String> = nums.into_iter().map(|n| n.to_string()).collect();
        let all_numeric_physical = physical
            .iter()
            .all(|row| device_id(row).trim().parse::<u32>().is_ok());
        // Require EVERY extent disk number to match a known disk id (never `.any()`), and only
        // trust OS DiskNumber == DeviceId directly when rows came from native SetupAPI or every
        // physical row uses standard numeric DeviceIds without partial overlap.
        let known_ids: HashSet<String> = physical.iter().map(device_id).collect();
        if physical.is_empty()
            || ((physical_from_native || all_numeric_physical)
                && mapped.iter().all(|id| known_ids.contains(id)))
        {
            return Ok(mapped);
        }
    }
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

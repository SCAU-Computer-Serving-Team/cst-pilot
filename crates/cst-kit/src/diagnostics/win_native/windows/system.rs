use std::ffi::c_void;
use std::mem::size_of;
use std::time::Duration;

use chrono::{Local, TimeZone};
use serde_json::{Value, json};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDevicePropertyW,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_Class, DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_DriverDate,
    DEVPKEY_Device_DriverProvider, DEVPKEY_Device_DriverVersion, DEVPKEY_Device_FriendlyName,
    DEVPROPTYPE,
};
use windows::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, FILETIME,
};
use windows::Win32::System::ProcessStatus::{
    ENUM_PAGE_FILE_INFORMATION, K32EnumPageFilesW, K32GetPerformanceInfo, PERFORMANCE_INFORMATION,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW, OpenSCManagerW,
    OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW, QueryServiceStatus,
    SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE,
    SERVICE_AUTO_START, SERVICE_CONTINUE_PENDING, SERVICE_PAUSE_PENDING, SERVICE_PAUSED,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_WIN32,
};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX,
    RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::core::{BOOL, PCWSTR};

use super::{Control, NativeError, NativeOverview, native_error, stopped, wide};
use crate::diagnostics::win_native::{format_dmtf_date, rounded};

struct RegKey(HKEY);
impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

struct ScHandle(SC_HANDLE);
impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

struct DevInfo(HDEVINFO);
impl Drop for DevInfo {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn ft_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

fn sample_cpu_pct(control: &Control) -> Option<f64> {
    let mut idle1 = FILETIME::default();
    let mut kernel1 = FILETIME::default();
    let mut user1 = FILETIME::default();
    unsafe { GetSystemTimes(Some(&mut idle1), Some(&mut kernel1), Some(&mut user1)) }.ok()?;
    control.sleep(Duration::from_millis(200)).ok()?;
    let mut idle2 = FILETIME::default();
    let mut kernel2 = FILETIME::default();
    let mut user2 = FILETIME::default();
    unsafe { GetSystemTimes(Some(&mut idle2), Some(&mut kernel2), Some(&mut user2)) }.ok()?;

    let idle_d = ft_to_u64(idle2).saturating_sub(ft_to_u64(idle1));
    let kernel_d = ft_to_u64(kernel2).saturating_sub(ft_to_u64(kernel1));
    let user_d = ft_to_u64(user2).saturating_sub(ft_to_u64(user1));
    let total = kernel_d.saturating_add(user_d);
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle_d);
    Some(rounded(
        (busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0),
        1,
    ))
}

fn read_reg_sz(subkey: &str, value_name: &str) -> Option<String> {
    let key_w = wide(subkey);
    let mut hkey = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key_w.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    }
    .ok()
    .ok()?;
    let _guard = RegKey(hkey);
    let val_w = wide(value_name);
    let mut bytes = 0u32;
    let status = unsafe {
        RegGetValueW(
            hkey,
            PCWSTR::null(),
            PCWSTR(val_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if status.is_err() || !(2..=16 * 1024).contains(&bytes) {
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let status = unsafe {
        RegGetValueW(
            hkey,
            PCWSTR::null(),
            PCWSTR(val_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if status.is_err() {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    let text = String::from_utf16_lossy(&buf[..end]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn physical_core_count() -> Option<u32> {
    let mut len = 0u32;
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len) };
    if len == 0 || len > 4 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(
                buf.as_mut_ptr()
                    .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(),
            ),
            &mut len,
        )
    }
    .ok()?;
    let mut offset = 0usize;
    let mut cores = 0u32;
    while offset + size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() <= len as usize {
        let info = unsafe {
            &*(buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)
        };
        if info.Size == 0 {
            break;
        }
        cores += 1;
        offset = offset.saturating_add(info.Size as usize);
    }
    (cores > 0).then_some(cores)
}

struct PageFileCollect {
    page_size: u64,
    rows: Vec<Value>,
}

unsafe extern "system" fn pagefile_cb(
    ctx: *mut c_void,
    info: *mut ENUM_PAGE_FILE_INFORMATION,
    filename: PCWSTR,
) -> BOOL {
    if ctx.is_null() || info.is_null() {
        return BOOL(1);
    }
    let acc = unsafe { &mut *(ctx as *mut PageFileCollect) };
    let info = unsafe { &*info };
    let name = if filename.is_null() {
        String::new()
    } else {
        unsafe { filename.to_string() }.unwrap_or_default()
    };
    // Convert `\??\C:\pagefile.sys` -> `C:\pagefile.sys`.
    let clean = name.strip_prefix(r"\??\").unwrap_or(&name).to_owned();
    let to_mb = |pages: usize| rounded((pages as f64 * acc.page_size as f64) / 1_048_576.0, 0);
    acc.rows.push(json!({
        "name": clean,
        "allocMB": to_mb(info.TotalSize),
        "usedMB": to_mb(info.TotalInUse),
        "peakMB": to_mb(info.PeakUsage),
    }));
    BOOL(1)
}

pub fn overview(control: &Control) -> Result<NativeOverview, NativeError> {
    stopped("Overview", control)?;

    // 1. CPU short 200ms dual-sample (`GetSystemTimes`)
    let cpu_total_pct = sample_cpu_pct(control);
    stopped("Overview", control)?;

    // 2. Logical & physical cores
    let mut out = NativeOverview {
        cpu_total_pct,
        logical_cores: std::thread::available_parallelism()
            .ok()
            .and_then(|n| u32::try_from(n.get()).ok()),
        physical_cores: physical_core_count(),
        ..Default::default()
    };

    // 3. Memory (`GlobalMemoryStatusEx`)
    let mut mem = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
        let total = mem.ullTotalPhys as f64 / 1_048_576.0;
        let free = mem.ullAvailPhys as f64 / 1_048_576.0;
        let used = (total - free).max(0.0);
        out.total_mb = Some(rounded(total, 0));
        out.free_mb = Some(rounded(free, 0));
        out.used_mb = Some(rounded(used, 0));
        out.used_pct = (total > 0.0).then(|| rounded(used / total * 100.0, 1));
    }

    // 4. Kernel pool & page size (`GetPerformanceInfo`) + Pagefiles (`EnumPageFilesW`)
    let mut perf = PERFORMANCE_INFORMATION {
        cb: size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    let page_size = if unsafe { K32GetPerformanceInfo(&mut perf, perf.cb) }.as_bool() {
        let ps = perf.PageSize as u64;
        out.nonpaged_mb = Some(rounded(
            (perf.KernelNonpaged as f64 * ps as f64) / 1_048_576.0,
            0,
        ));
        out.paged_mb = Some(rounded(
            (perf.KernelPaged as f64 * ps as f64) / 1_048_576.0,
            0,
        ));
        ps
    } else {
        4096
    };

    let mut pf_ctx = PageFileCollect {
        page_size,
        rows: Vec::new(),
    };
    if unsafe { K32EnumPageFilesW(Some(pagefile_cb), (&raw mut pf_ctx).cast()) }.as_bool() {
        out.pagefile = Some(pf_ctx.rows);
    }

    // 5. Uptime (`GetTickCount64`)
    let uptime_ms = unsafe { GetTickCount64() };
    let uptime_secs = (uptime_ms / 1000) as i64;
    let boot = Local::now() - chrono::Duration::seconds(uptime_secs);
    let minutes = (uptime_secs / 60).max(0);
    out.uptime = Some(json!({
        "bootTime": boot.format("%Y-%m-%d %H:%M").to_string(),
        "text": format!("{}天{}小时{}分", minutes / 1440, minutes % 1440 / 60, minutes % 60),
        "totalHours": rounded(minutes as f64 / 60.0, 1),
    }));

    // 6. Machine / BIOS / CPU from HKLM registry (sub-millisecond, zero WMI)
    const BIOS_KEY: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
    const CPU_KEY: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    out.vendor = read_reg_sz(BIOS_KEY, "SystemManufacturer");
    out.model = read_reg_sz(BIOS_KEY, "SystemProductName");
    out.cpu = read_reg_sz(CPU_KEY, "ProcessorNameString")
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "));
    let bios_parts: Vec<String> = [
        read_reg_sz(BIOS_KEY, "BIOSVendor"),
        read_reg_sz(BIOS_KEY, "BIOSVersion"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !bios_parts.is_empty() {
        out.bios = Some(bios_parts.join(" "));
    }
    out.bios_date = read_reg_sz(BIOS_KEY, "BIOSReleaseDate").and_then(|raw| format_dmtf_date(&raw));

    Ok(out)
}

fn service_state_label(state: u32) -> &'static str {
    if state == SERVICE_RUNNING.0 {
        "Running"
    } else if state == SERVICE_STOPPED.0 {
        "Stopped"
    } else if state == SERVICE_PAUSED.0 {
        "Paused"
    } else if state == SERVICE_START_PENDING.0 {
        "Start Pending"
    } else if state == SERVICE_STOP_PENDING.0 {
        "Stop Pending"
    } else if state == SERVICE_CONTINUE_PENDING.0 {
        "Continue Pending"
    } else if state == SERVICE_PAUSE_PENDING.0 {
        "Pause Pending"
    } else {
        "Unknown"
    }
}

pub fn auto_services(control: &Control) -> Result<Vec<Value>, NativeError> {
    stopped("SCM", control)?;
    let scm = ScHandle(
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) }
            .map_err(|e| native_error("OpenSCManagerW", e))?,
    );

    let mut needed = 0u32;
    let mut count = 0u32;
    let mut resume = 0u32;
    let _ = unsafe {
        EnumServicesStatusExW(
            scm.0,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            None,
            &mut needed,
            &mut count,
            Some(&mut resume),
            PCWSTR::null(),
        )
    };
    if needed == 0 || needed > 16 * 1024 * 1024 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize + 4096];
    resume = 0;
    loop {
        stopped("SCM", control)?;
        let res = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                Some(&mut buf),
                &mut needed,
                &mut count,
                Some(&mut resume),
                PCWSTR::null(),
            )
        };
        match res {
            Ok(()) => break,
            Err(e) if (e.code().0 as u32 & 0xffff) == ERROR_MORE_DATA.0 => {
                let next = buf
                    .len()
                    .saturating_add(needed as usize)
                    .saturating_add(4096);
                if next > 16 * 1024 * 1024 {
                    return Err(native_error("EnumServicesStatusExW", e));
                }
                buf.resize(next, 0);
                resume = 0;
            }
            Err(e) => return Err(native_error("EnumServicesStatusExW", e)),
        }
    }

    let entries = unsafe {
        std::slice::from_raw_parts(
            buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
            count as usize,
        )
    };
    let mut cfg_buf = vec![0u8; 8192];
    let mut rows = Vec::new();
    for entry in entries {
        stopped("SCM", control)?;
        let name = if entry.lpServiceName.is_null() {
            continue;
        } else {
            unsafe { entry.lpServiceName.to_string() }.unwrap_or_default()
        };
        if name.is_empty() {
            continue;
        }
        let Ok(svc_raw) =
            (unsafe { OpenServiceW(scm.0, PCWSTR(entry.lpServiceName.0), SERVICE_QUERY_CONFIG) })
        else {
            continue;
        };
        let svc = ScHandle(svc_raw);
        let mut cfg_needed = 0u32;
        let ok = unsafe {
            QueryServiceConfigW(
                svc.0,
                Some(cfg_buf.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()),
                cfg_buf.len() as u32,
                &mut cfg_needed,
            )
        };
        let queried = match ok {
            Ok(()) => true,
            Err(e)
                if (e.code().0 as u32 & 0xffff) == ERROR_INSUFFICIENT_BUFFER.0
                    && cfg_needed <= 64 * 1024 =>
            {
                cfg_buf.resize(cfg_needed as usize, 0);
                unsafe {
                    QueryServiceConfigW(
                        svc.0,
                        Some(cfg_buf.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()),
                        cfg_buf.len() as u32,
                        &mut cfg_needed,
                    )
                }
                .is_ok()
            }
            Err(_) => false,
        };
        if !queried {
            continue;
        }
        let cfg = unsafe { &*(cfg_buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>()) };
        if cfg.dwStartType != SERVICE_AUTO_START {
            continue;
        }
        let display = if entry.lpDisplayName.is_null() {
            name.clone()
        } else {
            unsafe { entry.lpDisplayName.to_string() }.unwrap_or_else(|_| name.clone())
        };
        let path_name = if cfg.lpBinaryPathName.is_null() {
            Value::Null
        } else {
            json!(unsafe { cfg.lpBinaryPathName.to_string() }.unwrap_or_default())
        };
        rows.push(json!({
            "Name": name,
            "DisplayName": display,
            "State": service_state_label(entry.ServiceStatusProcess.dwCurrentState.0),
            "PathName": path_name,
        }));
    }
    Ok(rows)
}

pub fn services_by_name(names: &[&str], control: &Control) -> Result<Vec<Value>, NativeError> {
    stopped("SCM", control)?;
    let scm = ScHandle(
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
            .map_err(|e| native_error("OpenSCManagerW", e))?,
    );
    let mut rows = Vec::with_capacity(names.len());
    for &svc_name in names {
        stopped("SCM", control)?;
        let w = wide(svc_name);
        let Ok(svc_raw) =
            (unsafe { OpenServiceW(scm.0, PCWSTR(w.as_ptr()), SERVICE_QUERY_STATUS) })
        else {
            continue;
        };
        let svc = ScHandle(svc_raw);
        let mut status = SERVICE_STATUS::default();
        if unsafe { QueryServiceStatus(svc.0, &mut status) }.is_ok() {
            rows.push(json!({
                "Name": svc_name,
                "State": service_state_label(status.dwCurrentState.0),
            }));
        }
    }
    Ok(rows)
}

fn devprop_string(
    dev_info: HDEVINFO,
    dev_data: &SP_DEVINFO_DATA,
    key: &windows::Win32::Foundation::DEVPROPKEY,
    buf: &mut Vec<u8>,
) -> Option<String> {
    let mut prop_type = DEVPROPTYPE::default();
    let mut req = 0u32;
    let res = unsafe {
        SetupDiGetDevicePropertyW(
            dev_info,
            dev_data,
            key,
            &mut prop_type,
            Some(buf.as_mut_slice()),
            Some(&mut req),
            0,
        )
    };
    if let Err(e) = res {
        if (e.code().0 as u32 & 0xffff) == ERROR_INSUFFICIENT_BUFFER.0 && req <= 64 * 1024 {
            buf.resize(req as usize, 0);
            unsafe {
                SetupDiGetDevicePropertyW(
                    dev_info,
                    dev_data,
                    key,
                    &mut prop_type,
                    Some(buf.as_mut_slice()),
                    Some(&mut req),
                    0,
                )
            }
            .ok()?;
        } else {
            return None;
        }
    }
    let bytes = (req as usize).min(buf.len());
    if bytes < 2 {
        return None;
    }
    let units: Vec<u16> = buf[..bytes]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let end = units.iter().position(|c| *c == 0).unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..end]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn devprop_filetime_date(
    dev_info: HDEVINFO,
    dev_data: &SP_DEVINFO_DATA,
    key: &windows::Win32::Foundation::DEVPROPKEY,
) -> Option<String> {
    let mut prop_type = DEVPROPTYPE::default();
    let mut raw = [0u8; 8];
    let mut req = 0u32;
    unsafe {
        SetupDiGetDevicePropertyW(
            dev_info,
            dev_data,
            key,
            &mut prop_type,
            Some(&mut raw),
            Some(&mut req),
            0,
        )
    }
    .ok()?;
    if req < 8 {
        return None;
    }
    let ticks = u64::from_le_bytes(raw);
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    let unix_secs = ticks.checked_sub(EPOCH_DIFF_100NS)? / 10_000_000;
    let dt = chrono::Utc.timestamp_opt(unix_secs as i64, 0).single()?;
    Some(dt.format("%Y-%m-%d").to_string())
}

pub fn signed_drivers(classes: &[&str], control: &Control) -> Result<Vec<Value>, NativeError> {
    stopped("SetupAPI", control)?;
    let dev_info = DevInfo(
        unsafe {
            SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES | DIGCF_PRESENT)
        }
        .map_err(|e| native_error("SetupDiGetClassDevsW", e))?,
    );
    let mut buf = vec![0u8; 1024];
    let mut rows = Vec::new();
    let mut idx = 0u32;
    loop {
        stopped("SetupAPI", control)?;
        let mut dev_data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        match unsafe { SetupDiEnumDeviceInfo(dev_info.0, idx, &mut dev_data) } {
            Ok(()) => {}
            Err(e) if (e.code().0 as u32 & 0xffff) == ERROR_NO_MORE_ITEMS.0 => break,
            Err(e) => return Err(native_error("SetupDiEnumDeviceInfo", e)),
        }
        idx += 1;

        let Some(dev_class) =
            devprop_string(dev_info.0, &dev_data, &DEVPKEY_Device_Class, &mut buf)
        else {
            continue;
        };
        if !classes.iter().any(|c| c.eq_ignore_ascii_case(&dev_class)) {
            continue;
        }
        let version = devprop_string(
            dev_info.0,
            &dev_data,
            &DEVPKEY_Device_DriverVersion,
            &mut buf,
        );
        let provider = devprop_string(
            dev_info.0,
            &dev_data,
            &DEVPKEY_Device_DriverProvider,
            &mut buf,
        );
        let date = devprop_filetime_date(dev_info.0, &dev_data, &DEVPKEY_Device_DriverDate);
        if version.is_none() && provider.is_none() && date.is_none() {
            continue;
        }
        let device = devprop_string(
            dev_info.0,
            &dev_data,
            &DEVPKEY_Device_FriendlyName,
            &mut buf,
        )
        .or_else(|| devprop_string(dev_info.0, &dev_data, &DEVPKEY_Device_DeviceDesc, &mut buf));
        rows.push(json!({
            "DeviceClass": dev_class.to_uppercase(),
            "DeviceName": device,
            "DriverVersion": version,
            "DriverDate": date,
            "DriverProviderName": provider,
        }));
    }
    Ok(rows)
}

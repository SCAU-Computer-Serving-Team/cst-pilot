use super::super::{
    Control, NativeError, NativeHealthRow, SpaceSample, SpaceSnapshot, VolumeSnapshot,
};
use super::{Handle, native_error, stopped, wide};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::CStr;
use std::mem::{offset_of, size_of, zeroed};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DEVICE_NOT_CONNECTED, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
    ERROR_NO_MEDIA_IN_DRIVE, ERROR_NO_MORE_ITEMS, ERROR_NOT_READY, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW,
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
};
use windows::Win32::System::Diagnostics::Debug::{
    GetThreadErrorMode, SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_EXTENT, DISK_GEOMETRY_EX, GUID_DEVINTERFACE_DISK,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    IOCTL_STORAGE_QUERY_PROPERTY, NVMeDataTypeLogPage, PropertyStandardQuery, ProtocolTypeNvme,
    SMART_RCV_DRIVE_DATA, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY,
    STORAGE_PROTOCOL_DATA_DESCRIPTOR, STORAGE_PROTOCOL_SPECIFIC_DATA,
    STORAGE_TEMPERATURE_DATA_DESCRIPTOR, StorageAdapterProtocolSpecificProperty,
    StorageDeviceProperty, StorageDeviceProtocolSpecificProperty, StorageDeviceSeekPenaltyProperty,
    StorageDeviceTemperatureProperty, VOLUME_DISK_EXTENTS,
};
use windows::core::PCWSTR;

const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const MAX_EXTENT_BUFFER_BYTES: usize = 64 * 1024;
const MAX_SETUPAPI_DISKS: u32 = 256;
const MAX_NETWORK_PROBES: usize = 4;
const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

static ACTIVE_NETWORK_PROBES: AtomicUsize = AtomicUsize::new(0);

struct NetworkProbeGuard;
impl NetworkProbeGuard {
    fn try_acquire() -> Option<Self> {
        let mut current = ACTIVE_NETWORK_PROBES.load(Ordering::Relaxed);
        loop {
            if current >= MAX_NETWORK_PROBES {
                return None;
            }
            match ACTIVE_NETWORK_PROBES.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self),
                Err(actual) => current = actual,
            }
        }
    }
}
impl Drop for NetworkProbeGuard {
    fn drop(&mut self) {
        ACTIVE_NETWORK_PROBES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Suppresses modal Windows error dialogs (e.g. empty optical drives / disconnected card readers)
/// on the calling thread while preserving existing thread error-mode flags, and restores the
/// previous mode on drop.
pub(super) struct ThreadErrorModeGuard {
    prev: THREAD_ERROR_MODE,
    active: bool,
}
impl ThreadErrorModeGuard {
    pub(super) fn suppress_critical_errors() -> Self {
        // SAFETY: `GetThreadErrorMode` reads the calling thread's mode without side effects.
        let existing = unsafe { GetThreadErrorMode() };
        let desired = THREAD_ERROR_MODE(existing | SEM_FAILCRITICALERRORS.0);
        let mut prev = THREAD_ERROR_MODE(0);
        // SAFETY: `prev` is valid for the duration of the call.
        let active = unsafe { SetThreadErrorMode(desired, Some(&mut prev)) }.is_ok();
        Self { prev, active }
    }
}
impl Drop for ThreadErrorModeGuard {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: restores the exact thread error mode previously returned by Windows.
            let _ = unsafe { SetThreadErrorMode(self.prev, None) };
        }
    }
}

fn open_device(path: &str, access: u32) -> Result<Handle, windows::core::Error> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated and lives across the Win32 call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }?;
    if handle == INVALID_HANDLE_VALUE {
        return Err(windows::core::Error::from_thread());
    }
    Ok(Handle(handle))
}

type VolumeMeta = (Option<String>, Option<String>);

struct VolumeProbe {
    space: Result<(u64, u64), NativeError>,
    meta: Option<Result<VolumeMeta, NativeError>>,
}

fn query_drive_space_sync(drive_root: &str) -> Result<(u64, u64), windows::core::Error> {
    let path = wide(drive_root);
    let mut avail_to_caller = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    // SAFETY: `path` is NUL-terminated and output pointers are valid stack locals.
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(path.as_ptr()),
            Some(&mut avail_to_caller),
            Some(&mut total),
            Some(&mut total_free),
        )?;
    }
    // Pair caller-quota available bytes with caller-quota total bytes so `free <= total`.
    Ok((total, avail_to_caller.min(total)))
}

fn query_volume_label_and_fs_sync(
    drive_root: &str,
) -> Result<(Option<String>, Option<String>), windows::core::Error> {
    let root_wide = wide(drive_root);
    let mut vol_name = [0u16; 261];
    let mut fs_name = [0u16; 64];
    // SAFETY: `root_wide` is NUL-terminated and output buffers live across the call.
    unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut vol_name),
            None,
            None,
            None,
            Some(&mut fs_name),
        )?;
    }
    let label = utf16_z(&vol_name);
    let fs = utf16_z(&fs_name);
    Ok((Some(label), if fs.is_empty() { None } else { Some(fs) }))
}

fn probe_volume(name: &str, drive_type: u32, need_meta: bool, control: &Control) -> VolumeProbe {
    let drive_root = format!("{name}\\");
    if drive_type != DRIVE_REMOTE {
        let space = query_drive_space_sync(&drive_root)
            .map_err(|e| native_error(&format!("GetDiskFreeSpaceExW({name})"), e));
        let meta = if need_meta {
            Some(
                query_volume_label_and_fs_sync(&drive_root)
                    .map_err(|e| native_error(&format!("GetVolumeInformationW({name})"), e)),
            )
        } else {
            None
        };
        return VolumeProbe { space, meta };
    }

    let Some(guard) = NetworkProbeGuard::try_acquire() else {
        let err = NativeError {
            source: format!("RemoteVolume({name})"),
            code: None,
            message: "网络卷探测并发已达上限，跳过慢速网络盘。".into(),
        };
        return VolumeProbe {
            space: Err(err.clone()),
            meta: need_meta.then_some(Err(err)),
        };
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let root_owned = drive_root.clone();
    let name_owned = name.to_owned();
    std::thread::spawn(move || {
        let _guard = guard;
        let _err_mode = ThreadErrorModeGuard::suppress_critical_errors();
        let space = query_drive_space_sync(&root_owned)
            .map_err(|e| native_error(&format!("GetDiskFreeSpaceExW({name_owned})"), e));
        let meta =
            if need_meta {
                if space.is_ok() {
                    Some(query_volume_label_and_fs_sync(&root_owned).map_err(|e| {
                        native_error(&format!("GetVolumeInformationW({name_owned})"), e)
                    }))
                } else {
                    None
                }
            } else {
                None
            };
        let _ = tx.send(VolumeProbe { space, meta });
    });

    let deadline = Instant::now() + control.remaining().min(NETWORK_PROBE_TIMEOUT);
    loop {
        if let Err(message) = control.check() {
            let err = NativeError {
                source: "Storage".into(),
                code: None,
                message,
            };
            return VolumeProbe {
                space: Err(err.clone()),
                meta: need_meta.then_some(Err(err)),
            };
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(probe) => return probe,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    let err = NativeError {
                        source: format!("RemoteVolume({name})"),
                        code: None,
                        message: "网络卷响应超时，已跳过以防阻塞。".into(),
                    };
                    return VolumeProbe {
                        space: Err(err.clone()),
                        meta: need_meta.then_some(Err(err)),
                    };
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let err = NativeError {
                    source: format!("RemoteVolume({name})"),
                    code: None,
                    message: "网络卷探测线程异常退出。".into(),
                };
                return VolumeProbe {
                    space: Err(err.clone()),
                    meta: need_meta.then_some(Err(err)),
                };
            }
        }
    }
}

fn is_empty_media_error(err: &NativeError) -> bool {
    matches!(
        err.code.map(|c| c & 0xFFFF),
        Some(c)
            if c == ERROR_NOT_READY.0
                || c == ERROR_DEVICE_NOT_CONNECTED.0
                || c == ERROR_NO_MEDIA_IN_DRIVE.0
    )
}

pub(super) fn space(drive: Option<&str>, control: &Control) -> Result<SpaceSnapshot, NativeError> {
    let _err_mode = ThreadErrorModeGuard::suppress_critical_errors();
    let drives = unsafe { GetLogicalDrives() };
    if drives == 0 {
        return Err(native_error(
            "GetLogicalDrives",
            windows::core::Error::from_thread(),
        ));
    }
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for index in 0..26u8 {
        stopped("Storage", control)?;
        let letter = (b'A' + index) as char;
        let name = format!("{letter}:");
        if drives & (1 << index) == 0 || drive.is_some_and(|d| d != name) {
            continue;
        }
        let root = wide(&format!("{name}\\"));
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
        let probe = probe_volume(&name, drive_type, false, control);
        stopped("Storage", control)?;
        match probe.space {
            Ok((total, free)) => rows.push(SpaceSample {
                drive: name,
                total,
                free,
            }),
            Err(err)
                if drive.is_none() && drive_type == DRIVE_CDROM && is_empty_media_error(&err) =>
            {
                // Skip optical drives with no inserted media during all-drive enumeration.
            }
            Err(err) => errors.push(err),
        }
    }
    if drive.is_some() && rows.is_empty() && errors.is_empty() {
        return Err(NativeError {
            source: "Storage".into(),
            code: None,
            message: "指定盘符不存在或无法读取。".into(),
        });
    }
    Ok(SpaceSnapshot { rows, errors })
}

pub(super) fn native_volumes(
    drive: Option<&str>,
    control: &Control,
) -> Result<Option<VolumeSnapshot>, NativeError> {
    let _err_mode = ThreadErrorModeGuard::suppress_critical_errors();
    let drives = unsafe { GetLogicalDrives() };
    if drives == 0 {
        return Err(native_error(
            "GetLogicalDrives",
            windows::core::Error::from_thread(),
        ));
    }
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for index in 0..26u8 {
        stopped("Storage", control)?;
        let letter = (b'A' + index) as char;
        let name = format!("{letter}:");
        if drives & (1 << index) == 0 || drive.is_some_and(|d| d != name) {
            continue;
        }
        let root_wide = wide(&format!("{name}\\"));
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };
        let probe = probe_volume(&name, drive_type, true, control);
        stopped("Storage", control)?;
        let mut skipped_empty_cdrom = false;
        let (size, free_space) = match probe.space {
            Ok((total, free)) => (Some(total), Some(free)),
            Err(err)
                if drive.is_none() && drive_type == DRIVE_CDROM && is_empty_media_error(&err) =>
            {
                skipped_empty_cdrom = true;
                (None, None)
            }
            Err(err) => {
                errors.push(err);
                (None, None)
            }
        };
        let (label, fs) = match probe.meta {
            Some(Ok((l, f))) => (l, f),
            Some(Err(err)) => {
                if !(skipped_empty_cdrom
                    || (drive.is_none() && drive_type == DRIVE_CDROM && is_empty_media_error(&err)))
                {
                    errors.push(err);
                }
                (None, None)
            }
            None => (None, None),
        };
        rows.push(json!({
            "DeviceID": name,
            "VolumeName": label,
            "FileSystem": fs,
            "DriveType": drive_type,
            "Size": size,
            "FreeSpace": free_space,
        }));
    }
    Ok(Some(VolumeSnapshot { rows, errors }))
}

fn utf16_z(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Parses a `VOLUME_DISK_EXTENTS` output buffer strictly: rejects truncated responses rather than
/// silently dropping extents.
fn parse_volume_disk_extents(buf: &[u8]) -> Option<Vec<u32>> {
    let extents_off = offset_of!(VOLUME_DISK_EXTENTS, Extents);
    if buf.len() < extents_off {
        return None;
    }
    let count = u32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
    if count == 0 {
        return None;
    }
    let needed = count
        .checked_mul(size_of::<DISK_EXTENT>())?
        .checked_add(extents_off)?;
    if needed > buf.len() {
        return None;
    }
    let mut nums = BTreeSet::new();
    for idx in 0..count {
        let off = extents_off + idx * size_of::<DISK_EXTENT>();
        // SAFETY: `off + size_of::<DISK_EXTENT>() <= needed <= buf.len()`.
        let extent =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const DISK_EXTENT) };
        nums.insert(extent.DiskNumber);
    }
    (!nums.is_empty()).then(|| nums.into_iter().collect())
}

pub(super) fn volume_disk_numbers(
    drive: &str,
    control: &Control,
) -> Result<Option<Vec<u32>>, NativeError> {
    stopped("Storage", control)?;
    let _err_mode = ThreadErrorModeGuard::suppress_critical_errors();
    let path = format!(r"\\.\{drive}");
    let handle = match open_device(&path, 0) {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    let mut buf_len = 512usize;
    loop {
        stopped("Storage", control)?;
        let mut buf = vec![0u8; buf_len];
        let mut returned = 0u32;
        // SAFETY: `handle` is valid and `buf` has `buf_len` writable bytes.
        let res = unsafe {
            DeviceIoControl(
                handle.0,
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                None,
                0,
                Some(buf.as_mut_ptr().cast()),
                buf.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        match res {
            Ok(()) => {
                let returned = returned as usize;
                if returned > buf.len() {
                    return Ok(None);
                }
                return Ok(parse_volume_disk_extents(&buf[..returned]));
            }
            Err(e)
                if matches!(
                    e.code().0 as u32 & 0xFFFF,
                    c if c == ERROR_MORE_DATA.0 || c == ERROR_INSUFFICIENT_BUFFER.0
                ) =>
            {
                if buf_len >= MAX_EXTENT_BUFFER_BYTES {
                    return Ok(None);
                }
                buf_len = (buf_len * 2).min(MAX_EXTENT_BUFFER_BYTES);
            }
            Err(_) => return Ok(None),
        }
    }
}

/// Strictly extracts a NUL-terminated UTF-16 `DevicePath` from `SP_DEVICE_INTERFACE_DETAIL_DATA_W`
/// raw bytes without creating an unaligned `u16` slice.
fn parse_setupapi_device_path(buf: &[u8], required: usize) -> Option<String> {
    let path_off = offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
    if required < path_off + 2 || required > buf.len() {
        return None;
    }
    let slice = &buf[path_off..required];
    let mut units = Vec::with_capacity(slice.len() / 2);
    let mut found_nul = false;
    for &chunk in slice.as_chunks::<2>().0 {
        let unit = u16::from_le_bytes(chunk);
        if unit == 0 {
            found_nul = true;
            break;
        }
        units.push(unit);
    }
    if !found_nul || units.is_empty() {
        return None;
    }
    Some(String::from_utf16_lossy(&units))
}

fn enumerate_disk_numbers(control: &Control) -> Option<Vec<u32>> {
    if let Some(nums) = enumerate_disk_numbers_setupapi(control)
        && !nums.is_empty()
    {
        return Some(nums);
    }
    let mut fallback = Vec::new();
    for idx in 0..32u32 {
        if control.check().is_err() {
            return None;
        }
        if open_device(&format!(r"\\.\PhysicalDrive{idx}"), 0).is_ok() {
            fallback.push(idx);
        }
    }
    (!fallback.is_empty()).then_some(fallback)
}

fn enumerate_disk_numbers_setupapi(control: &Control) -> Option<Vec<u32>> {
    // SAFETY: standard SetupAPI read-only enumeration for present disk interfaces.
    let dev_info = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_DISK),
            None,
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }
    .ok()?;
    struct DevInfoGuard(windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO);
    impl Drop for DevInfoGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }
    let _guard = DevInfoGuard(dev_info);
    let mut numbers = BTreeSet::new();
    let mut reached_end = false;
    for index in 0..MAX_SETUPAPI_DISKS {
        if control.check().is_err() {
            return None;
        }
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        let res = unsafe {
            SetupDiEnumDeviceInterfaces(dev_info, None, &GUID_DEVINTERFACE_DISK, index, &mut iface)
        };
        if let Err(e) = res {
            if (e.code().0 as u32 & 0xFFFF) == ERROR_NO_MORE_ITEMS.0 {
                reached_end = true;
                break;
            }
            return None;
        }
        let mut required = 0u32;
        let first = unsafe {
            SetupDiGetDeviceInterfaceDetailW(dev_info, &iface, None, 0, Some(&mut required), None)
        };
        if let Err(e) = first
            && (e.code().0 as u32 & 0xFFFF) != ERROR_INSUFFICIENT_BUFFER.0
        {
            return None;
        }
        let min_required = offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath) + 2;
        if (required as usize) < min_required || required > 8192 {
            return None;
        }
        let mut detail_buf = vec![0u64; (required as usize).div_ceil(8)];
        let detail_ptr = detail_buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        // SAFETY: `detail_buf` is 8-byte aligned and has at least `required` bytes.
        let ok = unsafe {
            (*detail_ptr).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(
                dev_info,
                &iface,
                Some(detail_ptr),
                required,
                None,
                None,
            )
        };
        if ok.is_err() {
            return None;
        }
        // SAFETY: `detail_buf` is initialized and lives for the whole slice borrow.
        let raw_bytes = unsafe {
            std::slice::from_raw_parts(
                detail_buf.as_ptr() as *const u8,
                std::mem::size_of_val(detail_buf.as_slice()),
            )
        };
        let dev_path = parse_setupapi_device_path(raw_bytes, required as usize)?;
        if let Ok(handle) = open_device(&dev_path, 0)
            && let Some(num) = query_device_number(handle.0)
        {
            numbers.insert(num);
        }
    }
    if !reached_end {
        return None;
    }
    Some(numbers.into_iter().collect())
}

fn query_device_number(handle: HANDLE) -> Option<u32> {
    let mut num: STORAGE_DEVICE_NUMBER = unsafe { zeroed() };
    let mut ret = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some((&raw mut num).cast()),
            size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut ret),
            None,
        )
        .ok()?;
    }
    if (ret as usize) >= size_of::<STORAGE_DEVICE_NUMBER>() {
        Some(num.DeviceNumber)
    } else {
        None
    }
}

pub(super) fn native_physical_disks(control: &Control) -> Result<Option<Vec<Value>>, NativeError> {
    stopped("Storage", control)?;
    let _err_mode = ThreadErrorModeGuard::suppress_critical_errors();
    let Some(numbers) = enumerate_disk_numbers(control) else {
        return Ok(None);
    };
    let mut rows = Vec::new();
    for num in numbers {
        stopped("Storage", control)?;
        let Ok(handle) = open_device(&format!(r"\\.\PhysicalDrive{num}"), 0) else {
            continue;
        };
        let desc = query_device_descriptor(handle.0);
        let seek_penalty = query_seek_penalty(handle.0);
        let size = query_disk_size(handle.0);
        let (friendly, serial, bus_type_raw) = match desc {
            Some(d) => {
                let name = match (d.vendor.is_empty(), d.product.is_empty()) {
                    (false, false) => format!("{} {}", d.vendor, d.product),
                    (true, false) => d.product,
                    (false, true) => d.vendor,
                    (true, true) => format!("PhysicalDrive{num}"),
                };
                (
                    name,
                    if d.serial.is_empty() {
                        Value::Null
                    } else {
                        Value::String(d.serial)
                    },
                    d.bus_type_raw,
                )
            }
            None => (format!("PhysicalDrive{num}"), Value::Null, 0),
        };
        // Standard MSFT_PhysicalDisk MediaType values: 0=Unspecified, 3=HDD, 4=SSD.
        // Never assume SSD/HDD for USB bridges or virtual disks from seek penalty alone.
        let media_type: u32 = if bus_type_raw == 17 {
            4
        } else if matches!(bus_type_raw, 3 | 10 | 11) {
            match seek_penalty {
                Some(false) => 4,
                Some(true) => 3,
                None => 0,
            }
        } else {
            0
        };
        rows.push(json!({
            "FriendlyName": friendly,
            "SerialNumber": serial,
            "MediaType": media_type,
            "BusType": bus_type_raw,
            "HealthStatus": Value::Null,
            "OperationalStatus": Value::Null,
            "DeviceId": num.to_string(),
            "Size": size,
        }));
    }
    if rows.is_empty() {
        Ok(None)
    } else {
        Ok(Some(rows))
    }
}

fn normalize_serial(serial: &str) -> String {
    serial
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\0')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub(super) fn native_disk_health(
    disk_number: u32,
    device_id: &str,
    expected_serial: Option<&str>,
    control: &Control,
) -> Result<Option<NativeHealthRow>, NativeError> {
    stopped("Storage", control)?;
    let _err_mode = ThreadErrorModeGuard::suppress_critical_errors();
    let dev_path = format!(r"\\.\PhysicalDrive{disk_number}");
    let handle = match open_device(&dev_path, 0) {
        Ok(h) => h,
        Err(e) => {
            if (e.code().0 as u32 & 0xFFFF) == ERROR_ACCESS_DENIED.0 {
                return Err(native_error("PhysicalDrive", e));
            }
            return Ok(None);
        }
    };
    let desc = query_device_descriptor(handle.0);
    // Cross-check serial number when both caller (WMI) and native device report one,
    // preventing accidental reads from the wrong physical drive on Storage Spaces / RAID.
    if let Some(expected) = expected_serial
        .map(normalize_serial)
        .filter(|s| !s.is_empty())
        && let Some(actual) = desc
            .as_ref()
            .map(|d| normalize_serial(&d.serial))
            .filter(|s| !s.is_empty())
        && expected != actual
    {
        return Ok(None);
    }
    let bus_type = desc.map(|d| d.bus_type_raw).unwrap_or(0);
    // 1. NVMe Log Page 0x02 (works without elevation on standard Windows NVMe driver)
    if (bus_type == 17 || bus_type == 0)
        && let Some(row) = query_nvme_health_row(handle.0, device_id)
    {
        return Ok(Some(row));
    }
    // 2. Local ATA/SATA read-only SMART_RCV_DRIVE_DATA (requires separate R/W handle per SDK winioctl.h)
    if matches!(bus_type, 3 | 11) {
        return query_ata_smart_row(&dev_path, disk_number, device_id);
    }
    Ok(None)
}

struct DeviceDesc {
    vendor: String,
    product: String,
    serial: String,
    bus_type_raw: u32,
}

fn cstr_at(buf: &[u8], offset: u32) -> String {
    let off = offset as usize;
    if off == 0 || off >= buf.len() {
        return String::new();
    }
    let slice = &buf[off..];
    let nul = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    CStr::from_bytes_with_nul(&slice[..=nul.min(slice.len().saturating_sub(1))])
        .ok()
        .and_then(|c| c.to_str().ok())
        .unwrap_or_else(|| std::str::from_utf8(&slice[..nul]).unwrap_or(""))
        .trim()
        .to_owned()
}

fn query_device_descriptor(handle: HANDLE) -> Option<DeviceDesc> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut buf = vec![0u8; 4096];
    let mut ret = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const query).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr().cast()),
            buf.len() as u32,
            Some(&mut ret),
            None,
        )
        .ok()?;
    }
    let ret = ret as usize;
    if ret < size_of::<STORAGE_DEVICE_DESCRIPTOR>() || ret > buf.len() {
        return None;
    }
    let d = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
    let valid = &buf[..ret];
    Some(DeviceDesc {
        vendor: cstr_at(valid, d.VendorIdOffset),
        product: cstr_at(valid, d.ProductIdOffset),
        serial: cstr_at(valid, d.SerialNumberOffset),
        bus_type_raw: d.BusType.0 as u32,
    })
}

/// Parses `DEVICE_SEEK_PENALTY_DESCRIPTOR` from raw bytes so an out-of-range Win32 `BOOLEAN` byte
/// can never cause Rust `bool` undefined behavior.
fn parse_seek_penalty_bytes(buf: &[u8]) -> Option<bool> {
    let bool_off = offset_of!(DEVICE_SEEK_PENALTY_DESCRIPTOR, IncursSeekPenalty);
    if buf.len() < size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() || buf.len() <= bool_off {
        return None;
    }
    let size = u32::from_ne_bytes(buf[4..8].try_into().ok()?) as usize;
    if size <= bool_off {
        return None;
    }
    match buf[bool_off] {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn query_seek_penalty(handle: HANDLE) -> Option<bool> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut raw = [0u8; size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>()];
    let mut ret = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const query).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(raw.as_mut_ptr().cast()),
            raw.len() as u32,
            Some(&mut ret),
            None,
        )
        .ok()?;
    }
    let ret = (ret as usize).min(raw.len());
    parse_seek_penalty_bytes(&raw[..ret])
}

fn query_disk_size(handle: HANDLE) -> Option<u64> {
    let mut out: DISK_GEOMETRY_EX = unsafe { zeroed() };
    let mut ret = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            None,
            0,
            Some((&raw mut out).cast()),
            size_of::<DISK_GEOMETRY_EX>() as u32,
            Some(&mut ret),
            None,
        )
        .ok()?;
    }
    ((ret as usize) >= size_of::<DISK_GEOMETRY_EX>() && out.DiskSize >= 0)
        .then_some(out.DiskSize as u64)
}

fn query_temperature_property(handle: HANDLE) -> Option<i32> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceTemperatureProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut buf = vec![0u8; 1024];
    let mut ret = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const query).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr().cast()),
            buf.len() as u32,
            Some(&mut ret),
            None,
        )
        .ok()?;
    }
    let ret = ret as usize;
    if ret < size_of::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>() || ret > buf.len() {
        return None;
    }
    let desc = unsafe {
        std::ptr::read_unaligned(buf.as_ptr() as *const STORAGE_TEMPERATURE_DATA_DESCRIPTOR)
    };
    if desc.InfoCount > 0 {
        let temp = i32::from(desc.TemperatureInfo[0].Temperature);
        if (-50..=150).contains(&temp) {
            return Some(temp);
        }
    }
    None
}

fn build_nvme_health_query(
    prop_id: windows::Win32::System::Ioctl::STORAGE_PROPERTY_ID,
) -> (Vec<u64>, usize) {
    let header_off = offset_of!(STORAGE_PROPERTY_QUERY, AdditionalParameters);
    let proto_size = size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>();
    let payload_len = 512usize;
    let total_len = header_off + proto_size + payload_len;
    let mut buf = vec![0u64; total_len.div_ceil(8)];
    let q = STORAGE_PROPERTY_QUERY {
        PropertyId: prop_id,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let p = STORAGE_PROTOCOL_SPECIFIC_DATA {
        ProtocolType: ProtocolTypeNvme,
        DataType: NVMeDataTypeLogPage.0 as u32,
        ProtocolDataRequestValue: 0x02, // NVME_LOG_PAGE_HEALTH_INFO
        ProtocolDataRequestSubValue: 0,
        ProtocolDataOffset: proto_size as u32,
        ProtocolDataLength: payload_len as u32,
        ..Default::default()
    };
    // SAFETY: `buf` is 8-byte aligned and sized for `header_off + proto_size + payload_len`;
    // `write_unaligned` does not require sub-field alignment assumptions.
    unsafe {
        let base = buf.as_mut_ptr() as *mut u8;
        std::ptr::write_unaligned(base as *mut STORAGE_PROPERTY_QUERY, q);
        std::ptr::write_unaligned(
            base.add(header_off) as *mut STORAGE_PROTOCOL_SPECIFIC_DATA,
            p,
        );
    }
    (buf, total_len)
}

/// Strictly validates a `STORAGE_PROTOCOL_DATA_DESCRIPTOR` NVMe Health Log Page (`0x02`) response
/// and projects only the 6 authorized `API.md` SMART fields.
fn parse_nvme_health_response(
    buf: &[u8],
    ret: usize,
    device_id: &str,
    fallback_temp: Option<i32>,
) -> Option<Value> {
    let min_desc = size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>();
    if ret < min_desc || ret > buf.len() {
        return None;
    }
    // SAFETY: `ret >= size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>()` and `ret <= buf.len()`.
    let descr = unsafe {
        std::ptr::read_unaligned(buf.as_ptr() as *const STORAGE_PROTOCOL_DATA_DESCRIPTOR)
    };
    if descr.Version == 0
        || (descr.Size as usize) < min_desc
        || descr.ProtocolSpecificData.ProtocolType != ProtocolTypeNvme
        || descr.ProtocolSpecificData.DataType != NVMeDataTypeLogPage.0 as u32
    {
        return None;
    }
    let proto_off = descr.ProtocolSpecificData.ProtocolDataOffset as usize;
    if proto_off < size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>() {
        return None;
    }
    let data_off = offset_of!(STORAGE_PROTOCOL_DATA_DESCRIPTOR, ProtocolSpecificData)
        .checked_add(proto_off)?;
    let data_len = descr.ProtocolSpecificData.ProtocolDataLength as usize;
    if data_len < 512 || data_off.checked_add(data_len).is_none_or(|end| end > ret) {
        return None;
    }
    let log = &buf[data_off..data_off + 512];
    let temp_k = u16::from_le_bytes([log[1], log[2]]);
    let temperature = if (100..=600).contains(&temp_k) {
        Some(i32::from(temp_k) - 273)
    } else {
        fallback_temp
    };
    let wear = log[5];
    let power_on_hours = u128::from_le_bytes(log[128..144].try_into().ok()?);
    let power_on_hours_u64 = u64::try_from(power_on_hours).ok();
    Some(json!({
        "DeviceId": device_id,
        "Wear": wear,
        "Temperature": temperature,
        "PowerOnHours": power_on_hours_u64,
        "ReadErrorsTotal": Value::Null,
        "WriteErrorsTotal": Value::Null,
    }))
}

fn query_nvme_health_row(handle: HANDLE, device_id: &str) -> Option<NativeHealthRow> {
    // Prefer device/namespace scope first; fall back to adapter/controller scope when needed.
    for (prop_id, controller_scope) in [
        (StorageDeviceProtocolSpecificProperty, false),
        (StorageAdapterProtocolSpecificProperty, true),
    ] {
        let (mut buf, total_len) = build_nvme_health_query(prop_id);
        let mut ret = 0u32;
        let ptr = buf.as_mut_ptr().cast();
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(ptr),
                total_len as u32,
                Some(ptr),
                total_len as u32,
                Some(&mut ret),
                None,
            )
        };
        if ok.is_err() {
            continue;
        }
        // SAFETY: `buf` is initialized and sized for at least `total_len` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, total_len) };
        let fallback_temp = query_temperature_property(handle);
        if let Some(row) = parse_nvme_health_response(bytes, ret as usize, device_id, fallback_temp)
        {
            return Some(NativeHealthRow {
                row,
                controller_scope,
            });
        }
    }
    None
}

// Packed Win32 SENDCMDINPARAMS / SENDCMDOUTPARAMS layout per Windows SDK um/ntdddisk.h.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdeRegs {
    features: u8,
    sector_count: u8,
    sector_number: u8,
    cyl_low: u8,
    cyl_high: u8,
    drive_head: u8,
    command: u8,
    reserved: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SendCmdInParams {
    buffer_size: u32,
    drive_regs: IdeRegs,
    drive_number: u8,
    reserved: [u8; 3],
    dw_reserved: [u32; 4],
    buffer: [u8; 1],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DriverStatus {
    driver_error: u8,
    ide_error: u8,
    reserved: [u8; 2],
    dw_reserved: [u32; 2],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SendCmdOutParams {
    buffer_size: u32,
    driver_status: DriverStatus,
    buffer: [u8; 512],
}

/// Projects strictly validated ATA SMART page fields into the 6 authorized `API.md` SMART keys.
fn parse_ata_smart_response(data: &[u8], device_id: &str) -> Option<Value> {
    if data.len() < 512 {
        return None;
    }
    // Validate 8-bit SMART page checksum (mod 256 == 0) when present; reject all-zero pages.
    if data.iter().all(|&b| b == 0) {
        return None;
    }
    let checksum: u8 = data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if checksum != 0 {
        return None;
    }
    let mut power_on_hours: Option<u64> = None;
    let mut temperature: Option<i32> = None;
    for slot in 0..30usize {
        let off = 2 + slot * 12;
        if off + 12 > data.len() {
            break;
        }
        let id = data[off];
        if id == 0 {
            continue;
        }
        let raw = &data[off + 5..off + 11];
        match id {
            0x09 => {
                // Only accept low 32-bit hours when upper 2 bytes are zero (exclude vendor-packed minutes/seconds).
                if raw[4] == 0 && raw[5] == 0 {
                    let hrs = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64;
                    if hrs <= 500_000 {
                        power_on_hours = Some(hrs);
                    }
                }
            }
            0xC2 | 0xBE if temperature.is_none() => {
                let c = i32::from(raw[0]);
                if (1..=120).contains(&c) {
                    temperature = Some(c);
                }
            }
            _ => {}
        }
    }
    if power_on_hours.is_none() && temperature.is_none() {
        return None;
    }
    Some(json!({
        "DeviceId": device_id,
        "Wear": Value::Null,
        "Temperature": temperature,
        "PowerOnHours": power_on_hours,
        "ReadErrorsTotal": Value::Null,
        "WriteErrorsTotal": Value::Null,
    }))
}

fn query_ata_smart_row(
    dev_path: &str,
    disk_number: u32,
    device_id: &str,
) -> Result<Option<NativeHealthRow>, NativeError> {
    let Ok(drive_number) = u8::try_from(disk_number) else {
        return Ok(None);
    };
    // SMART_RCV_DRIVE_DATA requires FILE_READ_ACCESS | FILE_WRITE_ACCESS per Windows SDK winioctl.h.
    let handle = match open_device(dev_path, (GENERIC_READ | GENERIC_WRITE).0) {
        Ok(h) => h,
        Err(e) => {
            if (e.code().0 as u32 & 0xFFFF) == ERROR_ACCESS_DENIED.0 {
                return Err(native_error("SMART_RCV_DRIVE_DATA", e));
            }
            return Ok(None);
        }
    };
    let in_params = SendCmdInParams {
        buffer_size: 512,
        drive_regs: IdeRegs {
            features: 0xD0, // READ_ATTRIBUTES (strictly read-only)
            sector_count: 1,
            sector_number: 1,
            cyl_low: 0x4F,
            cyl_high: 0xC2,
            drive_head: 0xA0,
            command: 0xB0, // SMART_CMD
            reserved: 0,
        },
        drive_number,
        reserved: [0; 3],
        dw_reserved: [0; 4],
        buffer: [0],
    };
    let mut out_params: SendCmdOutParams = unsafe { zeroed() };
    let mut ret = 0u32;
    let res = unsafe {
        DeviceIoControl(
            handle.0,
            SMART_RCV_DRIVE_DATA,
            Some((&raw const in_params).cast()),
            (size_of::<SendCmdInParams>() - 1) as u32,
            Some((&raw mut out_params).cast()),
            size_of::<SendCmdOutParams>() as u32,
            Some(&mut ret),
            None,
        )
    };
    if let Err(e) = res {
        if (e.code().0 as u32 & 0xFFFF) == ERROR_ACCESS_DENIED.0 {
            return Err(native_error("SMART_RCV_DRIVE_DATA", e));
        }
        return Ok(None);
    }
    let out_size = out_params.buffer_size;
    if (ret as usize) < size_of::<SendCmdOutParams>()
        || out_size < 512
        || out_params.driver_status.driver_error != 0
    {
        return Ok(None);
    }
    Ok(parse_ata_smart_response(&out_params.buffer, device_id).map(NativeHealthRow::device))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn seek_penalty_parser_rejects_invalid_boolean_bytes() {
        let sz = size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>();
        let off = offset_of!(DEVICE_SEEK_PENALTY_DESCRIPTOR, IncursSeekPenalty);
        let mut buf = vec![0u8; sz];
        buf[0..4].copy_from_slice(&(sz as u32).to_ne_bytes());
        buf[4..8].copy_from_slice(&(sz as u32).to_ne_bytes());
        buf[off] = 0;
        assert_eq!(parse_seek_penalty_bytes(&buf), Some(false));
        buf[off] = 1;
        assert_eq!(parse_seek_penalty_bytes(&buf), Some(true));
        buf[off] = 0xFF;
        assert_eq!(parse_seek_penalty_bytes(&buf), None);
        assert_eq!(parse_seek_penalty_bytes(&buf[..4]), None);
    }

    #[test]
    fn volume_disk_extents_parser_rejects_truncated_buffers() {
        let extents_off = offset_of!(VOLUME_DISK_EXTENTS, Extents);
        let mut buf = vec![0u8; extents_off + 2 * size_of::<DISK_EXTENT>()];
        buf[0..4].copy_from_slice(&2u32.to_ne_bytes());
        let e0 = DISK_EXTENT {
            DiskNumber: 3,
            StartingOffset: 0,
            ExtentLength: 1024,
        };
        let e1 = DISK_EXTENT {
            DiskNumber: 1,
            StartingOffset: 1024,
            ExtentLength: 1024,
        };
        unsafe {
            std::ptr::write_unaligned(buf.as_mut_ptr().add(extents_off) as *mut DISK_EXTENT, e0);
            std::ptr::write_unaligned(
                buf.as_mut_ptr().add(extents_off + size_of::<DISK_EXTENT>()) as *mut DISK_EXTENT,
                e1,
            );
        }
        assert_eq!(parse_volume_disk_extents(&buf), Some(vec![1, 3]));
        // Truncate by 1 byte -> must reject rather than silently dropping disk 1
        assert_eq!(parse_volume_disk_extents(&buf[..buf.len() - 1]), None);
    }

    #[test]
    fn setupapi_device_path_requires_nul_and_bounds() {
        let path_off = offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
        let wide_str: Vec<u16> = r"\\?\scsi#disk0".encode_utf16().chain(Some(0)).collect();
        let mut buf = vec![0u8; path_off + wide_str.len() * 2];
        for (i, u) in wide_str.iter().enumerate() {
            buf[path_off + i * 2..path_off + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        assert_eq!(
            parse_setupapi_device_path(&buf, buf.len()),
            Some(r"\\?\scsi#disk0".into())
        );
        // Missing NUL terminator -> reject
        assert_eq!(parse_setupapi_device_path(&buf, buf.len() - 2), None);
        // Smaller than header + 1 u16 -> reject
        assert_eq!(parse_setupapi_device_path(&buf, path_off), None);
    }

    #[test]
    fn nvme_parser_validates_descriptor_and_emits_only_contract_keys() {
        let (mut qbuf, total) = build_nvme_health_query(StorageDeviceProtocolSpecificProperty);
        let bytes = unsafe { std::slice::from_raw_parts_mut(qbuf.as_mut_ptr() as *mut u8, total) };
        let desc_size = size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>();
        let proto_off = offset_of!(STORAGE_PROTOCOL_DATA_DESCRIPTOR, ProtocolSpecificData);
        let proto_size = size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>();
        let mut descr: STORAGE_PROTOCOL_DATA_DESCRIPTOR = unsafe { zeroed() };
        descr.Version = desc_size as u32;
        descr.Size = desc_size as u32;
        descr.ProtocolSpecificData.ProtocolType = ProtocolTypeNvme;
        descr.ProtocolSpecificData.DataType = NVMeDataTypeLogPage.0 as u32;
        descr.ProtocolSpecificData.ProtocolDataOffset = proto_size as u32;
        descr.ProtocolSpecificData.ProtocolDataLength = 512;
        unsafe {
            std::ptr::write_unaligned(
                bytes.as_mut_ptr() as *mut STORAGE_PROTOCOL_DATA_DESCRIPTOR,
                descr,
            );
        }
        let log_off = proto_off + proto_size;
        assert!(log_off + 512 <= bytes.len());
        bytes[log_off + 1..log_off + 3].copy_from_slice(&(315u16).to_le_bytes()); // 42 C
        bytes[log_off + 5] = 7; // Wear 7%
        bytes[log_off + 128..log_off + 144].copy_from_slice(&(1234u128).to_le_bytes());

        let row = parse_nvme_health_response(bytes, log_off + 512, "0", None).unwrap();
        let keys: BTreeSet<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "DeviceId",
                "PowerOnHours",
                "ReadErrorsTotal",
                "Temperature",
                "Wear",
                "WriteErrorsTotal",
            ])
        );
        assert_eq!(row["Temperature"], 42);
        assert_eq!(row["Wear"], 7);
        assert_eq!(row["PowerOnHours"], 1234);

        // Overflowing u64 PowerOnHours becomes null instead of string
        bytes[log_off + 128..log_off + 144].copy_from_slice(&(u128::MAX).to_le_bytes());
        let row2 = parse_nvme_health_response(bytes, log_off + 512, "0", None).unwrap();
        assert!(row2["PowerOnHours"].is_null());

        // Overlapping ProtocolDataOffset must be rejected
        descr.ProtocolSpecificData.ProtocolDataOffset = 4;
        unsafe {
            std::ptr::write_unaligned(
                bytes.as_mut_ptr() as *mut STORAGE_PROTOCOL_DATA_DESCRIPTOR,
                descr,
            );
        }
        assert!(parse_nvme_health_response(bytes, log_off + 512, "0", None).is_none());
    }

    #[test]
    fn ata_parser_validates_checksum_and_emits_only_contract_keys() {
        let mut page = [0u8; 512];
        // Attribute 0x09 (PowerOnHours = 2500)
        page[2] = 0x09;
        page[7..11].copy_from_slice(&2500u32.to_le_bytes());
        // Attribute 0xC2 (Temp = 36 C)
        page[14] = 0xC2;
        page[19] = 36;
        let sum: u8 = page[..511].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        page[511] = 0u8.wrapping_sub(sum);

        let row = parse_ata_smart_response(&page, "1").unwrap();
        let keys: BTreeSet<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "DeviceId",
                "PowerOnHours",
                "ReadErrorsTotal",
                "Temperature",
                "Wear",
                "WriteErrorsTotal",
            ])
        );
        assert_eq!(row["PowerOnHours"], 2500);
        assert_eq!(row["Temperature"], 36);
        // Corrupt checksum -> reject
        page[511] ^= 1;
        assert!(parse_ata_smart_response(&page, "1").is_none());
    }
}

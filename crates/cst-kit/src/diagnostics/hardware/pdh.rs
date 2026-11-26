use super::{Control, CounterBatch, CounterSample, CounterSet, NativeError};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
use windows::Win32::System::Performance::*;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW,
};
use windows::core::{PCWSTR, PWSTR};

static LOCALIZED_PATHS: OnceLock<Mutex<HashMap<&'static str, Vec<u16>>>> = OnceLock::new();

struct Query(PDH_HQUERY);
impl Query {
    fn new() -> Result<Self, String> {
        let mut handle = PDH_HQUERY::default();
        check(unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) })?;
        Ok(Self(handle))
    }
}
impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.0);
        }
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
fn check(code: u32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("PDH 错误 0x{code:08X}"))
    }
}
fn aligned(bytes: u32) -> Result<Vec<usize>, String> {
    if bytes == 0 || bytes > 8 * 1024 * 1024 {
        return Err("PDH 缓冲区超过采集上限。".into());
    }
    Ok(vec![
        0;
        (bytes as usize).div_ceil(std::mem::size_of::<usize>())
    ])
}
// AddEnglishCounter/GetCounterInfo localizes both classic and v2 counters. It
// avoids dependence on a complete English Perflib registry on the host machine.
fn localize(path: &'static str) -> Result<Vec<u16>, String> {
    let cache = LOCALIZED_PATHS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.get(path)
    {
        return Ok(cached.clone());
    }
    let query = Query::new()?;
    let mut counter = PDH_HCOUNTER::default();
    check(unsafe { PdhAddEnglishCounterW(query.0, PCWSTR(wide(path).as_ptr()), 0, &mut counter) })?;
    let mut size = 0;
    let status = unsafe { PdhGetCounterInfoW(counter, false, &mut size, None) };
    if status != PDH_MORE_DATA {
        check(status)?;
    }
    let mut buffer = aligned(size)?;
    check(unsafe {
        PdhGetCounterInfoW(counter, false, &mut size, Some(buffer.as_mut_ptr().cast()))
    })?;
    let size = size as usize;
    if size < std::mem::size_of::<PDH_COUNTER_INFO_W>()
        || size > std::mem::size_of_val(buffer.as_slice())
    {
        return Err("PDH 计数器元数据长度无效。".into());
    }
    let info = unsafe { &*buffer.as_ptr().cast::<PDH_COUNTER_INFO_W>() };
    let localized = wide(&bounded_string(info.szFullPath, &buffer, size)?);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path, localized.clone());
    }
    Ok(localized)
}
// All string pointers must point into the PDH-owned result buffer, with a NUL
// inside that allocation; no unbounded pointer-to-string scan is needed.
fn bounded_string(pointer: PWSTR, buffer: &[usize], written: usize) -> Result<String, String> {
    let base = buffer.as_ptr().cast::<u16>();
    let off = (pointer.0 as usize).checked_sub(base as usize);
    let Some(off) = off.filter(|&off| off < written && off.is_multiple_of(2)) else {
        return Err("PDH 返回无效的字符串指针。".into());
    };
    if written > std::mem::size_of_val(buffer) {
        return Err("PDH 返回的字符串缓冲区长度无效。".into());
    }
    // Derive the pointer from the allocation and exclude any rounding padding.
    let units = unsafe { std::slice::from_raw_parts(base.add(off / 2), (written - off) / 2) };
    let len = units
        .iter()
        .position(|c| *c == 0)
        .ok_or("PDH 返回未终止的字符串。")?;
    Ok(String::from_utf16_lossy(&units[..len]))
}
fn values(counter: PDH_HCOUNTER, key: &str) -> Result<(Vec<CounterSample>, usize), String> {
    let mut bytes = 0;
    let mut count = 0;
    // pdh.h PDH_FMT_NOCAP100: keep genuine >100% frequency values.
    let format = PDH_FMT(PDH_FMT_DOUBLE.0 | 0x00008000);
    let status =
        unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut bytes, &mut count, None) };
    if status == PDH_CSTATUS_NO_INSTANCE {
        return Ok((vec![], 0));
    }
    if status != PDH_MORE_DATA {
        check(status)?;
    }
    if count == 0 {
        return Ok((vec![], 0));
    }
    for _ in 0..3 {
        let mut buffer = aligned(bytes)?;
        let capacity = std::mem::size_of_val(buffer.as_slice());
        bytes = capacity as u32;
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                format,
                &mut bytes,
                &mut count,
                Some(buffer.as_mut_ptr().cast()),
            )
        };
        if status == PDH_MORE_DATA {
            continue;
        }
        check(status)?;
        let written = bytes as usize;
        if written > capacity
            || count as usize > 100_000
            || count as usize * std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() > written
        {
            return Err("PDH 计数器实例超过采集上限。".into());
        }
        let items = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                count as usize,
            )
        };
        let mut samples = Vec::new();
        let mut invalid = 0;
        for item in items {
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            if !matches!(
                item.FmtValue.CStatus,
                PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
            ) || !value.is_finite()
            {
                invalid += 1;
                continue;
            }
            samples.push(CounterSample {
                counter: key.into(),
                instance: bounded_string(item.szName, &buffer, written)?,
                value,
            });
        }
        return Ok((samples, invalid));
    }
    Err("PDH 实例持续变化，无法读取完整快照。".into())
}
pub fn sample(
    set: CounterSet,
    engine_only: bool,
    control: &Control,
) -> Result<CounterBatch, String> {
    control.check()?;
    let specs: &[(&'static str, &'static str)] = match set {
        CounterSet::Gpu => &[
            ("engine", r"\GPU Engine(*)\Utilization Percentage"),
            ("memory", r"\GPU Process Memory(*)\Dedicated Usage"),
        ],
        CounterSet::Sensor => &[
            ("thermal", r"\Thermal Zone Information(*)\Temperature"),
            ("passive", r"\Thermal Zone Information(*)\% Passive Limit"),
            (
                "frequency",
                r"\Processor Information(*)\% of Maximum Frequency",
            ),
        ],
        CounterSet::DiskIo => &[
            ("queue", r"\PhysicalDisk(*)\Current Disk Queue Length"),
            ("idle", r"\PhysicalDisk(*)\% Idle Time"),
            ("read", r"\PhysicalDisk(*)\Disk Read Bytes/sec"),
            ("write", r"\PhysicalDisk(*)\Disk Write Bytes/sec"),
        ],
    };
    let query = Query::new()?;
    let mut batch = CounterBatch::default();
    let mut handles = Vec::new();
    for (key, path) in specs {
        if engine_only && *key != "engine" {
            continue;
        }
        control.check()?;
        let add = || {
            let localized = localize(path)?;
            let mut handle = PDH_HCOUNTER::default();
            check(unsafe { PdhAddCounterW(query.0, PCWSTR(localized.as_ptr()), 0, &mut handle) })?;
            Ok::<_, String>(handle)
        };
        match add() {
            Ok(handle) => handles.push((*key, handle)),
            Err(e) => batch.errors.push(error(key, e)),
        }
    }
    if handles.is_empty() {
        return Ok(batch);
    }
    check(unsafe { PdhCollectQueryData(query.0) })?;
    control.sleep(Duration::from_millis(1200))?;
    check(unsafe { PdhCollectQueryData(query.0) })?;
    for (key, handle) in handles {
        control.check()?;
        match values(handle, key) {
            Ok((values, invalid)) => {
                batch.samples.extend(values);
                if invalid > 0 {
                    batch
                        .errors
                        .push(error(key, format!("{invalid} 个计数器实例无有效样本。")));
                }
            }
            Err(e) => batch.errors.push(error(key, e)),
        }
    }
    Ok(batch)
}
fn error(key: &str, message: String) -> NativeError {
    NativeError {
        source: key.into(),
        code: None,
        message,
    }
}
pub fn pawnio() -> Result<bool, NativeError> {
    let mut key = HKEY::default();
    let path = wide(r"SYSTEM\CurrentControlSet\Services\PawnIO");
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            Some(0),
            KEY_READ,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        return Ok(false);
    }
    if status.is_err() {
        return Err(NativeError {
            source: "PawnIO".into(),
            code: Some(status.0),
            message: "PawnIO 服务注册项无法读取。".into(),
        });
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_terminator_must_be_within_reported_result() {
        let buffer = [usize::from(u16::from_ne_bytes([b'A', 0]))];
        let pointer = PWSTR(buffer.as_ptr().cast_mut().cast());
        assert!(bounded_string(pointer, &buffer, 2).is_err());
        assert!(bounded_string(pointer, &buffer, std::mem::size_of_val(&buffer) + 1).is_err());
        #[cfg(target_endian = "little")]
        assert_eq!(bounded_string(pointer, &buffer, 4).unwrap(), "A");
    }
}

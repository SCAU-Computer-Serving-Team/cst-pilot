use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{offset_of, size_of};

use super::{Control, Handle, NativeError, ProcessSample, native_error, stopped};
use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, FILETIME};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const MAX_NT_BUF_BYTES: usize = 64 * 1024 * 1024;

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        system_information_class: u32,
        system_information: *mut c_void,
        system_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(offset_of!(SystemProcessInformation, create_time) == 0x20);
    assert!(offset_of!(SystemProcessInformation, user_time) == 0x28);
    assert!(offset_of!(SystemProcessInformation, kernel_time) == 0x30);
    assert!(offset_of!(SystemProcessInformation, image_name) == 0x38);
    assert!(offset_of!(SystemProcessInformation, unique_process_id) == 0x50);
    assert!(offset_of!(SystemProcessInformation, working_set_size) == 0x90);
    assert!(offset_of!(SystemProcessInformation, read_transfer_count) == 0xE8);
    assert!(offset_of!(SystemProcessInformation, write_transfer_count) == 0xF0);
    assert!(size_of::<SystemProcessInformation>() == 0x100);
};

#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(offset_of!(SystemProcessInformation, create_time) == 0x20);
    assert!(offset_of!(SystemProcessInformation, user_time) == 0x28);
    assert!(offset_of!(SystemProcessInformation, kernel_time) == 0x30);
    assert!(offset_of!(SystemProcessInformation, image_name) == 0x38);
    assert!(offset_of!(SystemProcessInformation, unique_process_id) == 0x44);
    assert!(offset_of!(SystemProcessInformation, working_set_size) == 0x6C);
    assert!(offset_of!(SystemProcessInformation, read_transfer_count) == 0xA0);
    assert!(offset_of!(SystemProcessInformation, write_transfer_count) == 0xA8);
    assert!(size_of::<SystemProcessInformation>() == 0xB8);
};

fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// Primary fast snapshot via `NtQuerySystemInformation(SystemProcessInformation)`:
/// 100% process coverage (including SYSTEM & PPL processes) with CPU, WorkingSet, and IO transfer
/// counters in a single syscall (~9 ms). Falls back to `Toolhelp32` if `ntdll` rejects the query.
pub fn snapshot(control: &Control) -> Result<Vec<ProcessSample>, NativeError> {
    stopped("Process", control)?;
    match nt_snapshot(control) {
        Ok(rows) if !rows.is_empty() => Ok(rows),
        Ok(_) | Err(_) => toolhelp_snapshot(control),
    }
}

fn nt_snapshot(control: &Control) -> Result<Vec<ProcessSample>, NativeError> {
    let mut cap = 512 * 1024usize;
    let mut buf: Vec<u64> = vec![0; cap.div_ceil(8)];
    let mut returned: u32 = 0;

    for _ in 0..8 {
        stopped("Process", control)?;
        let byte_len = (buf.len() * 8) as u32;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_INFORMATION_CLASS,
                buf.as_mut_ptr().cast(),
                byte_len,
                &mut returned,
            )
        };
        if status == 0 {
            let valid_bytes = if returned == 0 {
                buf.len() * 8
            } else {
                (returned as usize).min(buf.len() * 8)
            };
            return parse_nt_process_buffer(&buf, valid_bytes, control);
        }
        if status == STATUS_INFO_LENGTH_MISMATCH {
            let needed = (returned as usize).max(cap.saturating_mul(2));
            let next = needed.saturating_add(64 * 1024);
            if next > MAX_NT_BUF_BYTES {
                return Err(NativeError {
                    source: "NtQuerySystemInformation".into(),
                    code: Some(status as u32),
                    message: "系统进程快照超出缓冲区上限。".into(),
                });
            }
            cap = next;
            buf.resize(cap.div_ceil(8), 0);
            continue;
        }
        return Err(NativeError {
            source: "NtQuerySystemInformation".into(),
            code: Some(status as u32),
            message: format!("NtQuerySystemInformation 返回状态 0x{:08X}", status as u32),
        });
    }
    Err(NativeError {
        source: "NtQuerySystemInformation".into(),
        code: None,
        message: "系统进程快照重试次数超限。".into(),
    })
}

fn parse_nt_process_buffer(
    buf: &[u64],
    valid_bytes: usize,
    control: &Control,
) -> Result<Vec<ProcessSample>, NativeError> {
    let buf_bytes = unsafe {
        std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), std::mem::size_of_val(buf))
    };
    let base = buf_bytes.as_ptr() as usize;
    let valid_bytes = valid_bytes.min(buf_bytes.len());
    let mut offset = 0usize;
    let mut rows = Vec::with_capacity(512);
    let min_entry = size_of::<SystemProcessInformation>();

    while offset + min_entry <= valid_bytes {
        stopped("Process", control)?;
        let entry = unsafe { &*((base + offset) as *const SystemProcessInformation) };
        let pid = entry.unique_process_id as u32;
        let raw_name = if !entry.image_name.buffer.is_null() && entry.image_name.length > 0 {
            let ptr = entry.image_name.buffer as usize;
            let bytes = entry.image_name.length as usize;
            if let Some(rel) = ptr.checked_sub(base)
                && let Some(end_rel) = rel.checked_add(bytes)
                && end_rel <= valid_bytes
                && bytes.is_multiple_of(2)
            {
                let units: Vec<u16> = buf_bytes[rel..end_rel]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                String::from_utf16_lossy(&units)
            } else {
                String::new()
            }
        } else if pid == 0 {
            "System Idle Process".to_owned()
        } else if pid == 4 {
            "System".to_owned()
        } else {
            String::new()
        };

        let name = raw_name
            .strip_suffix(".exe")
            .or_else(|| raw_name.strip_suffix(".EXE"))
            .unwrap_or(&raw_name)
            .to_owned();

        // Skip Idle process (PID 0) from CPU/IO competition to match Toolhelp32/WMI semantics.
        if pid != 0 {
            let created = entry.create_time.max(0) as u64;
            let cpu_100ns =
                (entry.kernel_time.max(0) as u64).saturating_add(entry.user_time.max(0) as u64);
            let read_bytes = u64::try_from(entry.read_transfer_count).ok();
            let write_bytes = u64::try_from(entry.write_transfer_count).ok();
            rows.push(ProcessSample {
                pid,
                name,
                created,
                cpu_100ns,
                working_set: Some(entry.working_set_size as u64),
                path: None,
                read_bytes,
                write_bytes,
            });
        }

        if entry.next_entry_offset == 0 {
            return Ok(rows);
        }
        let step = entry.next_entry_offset as usize;
        if step < min_entry || !step.is_multiple_of(8) {
            return Err(NativeError {
                source: "NtQuerySystemInformation".into(),
                code: None,
                message: "系统进程链表步长异常。".into(),
            });
        }
        let Some(next) = offset.checked_add(step) else {
            return Err(NativeError {
                source: "NtQuerySystemInformation".into(),
                code: None,
                message: "系统进程链表偏移溢出。".into(),
            });
        };
        if next + min_entry > valid_bytes {
            return Err(NativeError {
                source: "NtQuerySystemInformation".into(),
                code: None,
                message: "系统进程链表偏移越界。".into(),
            });
        }
        offset = next;
    }

    Err(NativeError {
        source: "NtQuerySystemInformation".into(),
        code: None,
        message: "系统进程快照缓冲区未正常终止。".into(),
    })
}

/// Fill image paths ONLY for the final Top-N `(pid, created_ticks)` pairs using a single reused
/// path buffer, re-verifying process creation time on the same handle to prevent PID reuse.
pub fn fill_paths(targets: &mut [(u32, u64, Option<String>)]) {
    let mut path_buffer = vec![0u16; 32768];
    for (pid, expected_created, slot) in targets.iter_mut() {
        if slot.is_some() || *pid == 0 {
            continue;
        }
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, *pid) })
        else {
            continue;
        };
        let handle = Handle(handle);
        if *expected_created != 0 {
            let mut created = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            if unsafe { GetProcessTimes(handle.0, &mut created, &mut exit, &mut kernel, &mut user) }
                .is_err()
                || ticks(created) != *expected_created
            {
                continue;
            }
        }
        let mut length = path_buffer.len() as u32;
        if unsafe {
            QueryFullProcessImageNameW(
                handle.0,
                PROCESS_NAME_WIN32,
                PWSTR(path_buffer.as_mut_ptr()),
                &mut length,
            )
        }
        .is_ok()
        {
            let used = (length as usize).min(path_buffer.len());
            *slot = Some(String::from_utf16_lossy(&path_buffer[..used]));
        }
    }
}

/// Fast PID -> image name mapping via `CreateToolhelp32Snapshot` (~18 ms vs 723 ms WMI).
pub fn process_names(control: &Control) -> Result<HashMap<u64, String>, NativeError> {
    let snapshot = Handle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|e| native_error("CreateToolhelp32Snapshot", e))?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry) }
        .map_err(|e| native_error("Process32FirstW", e))?;
    let mut names = HashMap::with_capacity(512);
    loop {
        stopped("Process", control)?;
        let name_end = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        let raw = String::from_utf16_lossy(&entry.szExeFile[..name_end]);
        let trimmed = raw
            .strip_suffix(".exe")
            .or_else(|| raw.strip_suffix(".EXE"))
            .unwrap_or(&raw)
            .to_owned();
        names.insert(u64::from(entry.th32ProcessID), trimmed);
        match unsafe { Process32NextW(snapshot.0, &mut entry) } {
            Ok(()) => {}
            Err(e) if e.code().0 as u32 & 0xffff == ERROR_NO_MORE_FILES.0 => break,
            Err(e) => return Err(native_error("Process32NextW", e)),
        }
    }
    Ok(names)
}

fn toolhelp_snapshot(control: &Control) -> Result<Vec<ProcessSample>, NativeError> {
    let snapshot = Handle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|e| native_error("CreateToolhelp32Snapshot", e))?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry) }
        .map_err(|e| native_error("Process32FirstW", e))?;
    let mut rows = Vec::new();
    let mut path_buffer = vec![0u16; 32768];
    loop {
        stopped("Process", control)?;
        if let Some(row) = read_toolhelp(&entry, &mut path_buffer) {
            rows.push(row);
        }
        match unsafe { Process32NextW(snapshot.0, &mut entry) } {
            Ok(()) => {}
            Err(e) if e.code().0 as u32 & 0xffff == ERROR_NO_MORE_FILES.0 => break,
            Err(e) => return Err(native_error("Process32NextW", e)),
        }
    }
    Ok(rows)
}

fn read_toolhelp(entry: &PROCESSENTRY32W, path_buffer: &mut [u16]) -> Option<ProcessSample> {
    let handle = Handle(
        unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                entry.th32ProcessID,
            )
        }
        .ok()?,
    );
    let mut created = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle.0, &mut created, &mut exit, &mut kernel, &mut user) }.ok()?;
    let created_ticks = ticks(created);
    let mut length = path_buffer.len() as u32;
    let path = unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(path_buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .ok()
    .map(|_| {
        let used = (length as usize).min(path_buffer.len());
        String::from_utf16_lossy(&path_buffer[..used])
    });
    let memory = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            entry.th32ProcessID,
        )
    }
    .ok()
    .map(Handle)
    .and_then(|mem_handle| {
        let mut c2 = FILETIME::default();
        let mut e2 = FILETIME::default();
        let mut k2 = FILETIME::default();
        let mut u2 = FILETIME::default();
        if unsafe { GetProcessTimes(mem_handle.0, &mut c2, &mut e2, &mut k2, &mut u2) }.is_err()
            || ticks(c2) != created_ticks
        {
            return None;
        }
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if unsafe { K32GetProcessMemoryInfo(mem_handle.0, &mut counters, size) }.as_bool() {
            Some(counters.WorkingSetSize as u64)
        } else {
            None
        }
    });
    let name_end = entry
        .szExeFile
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(entry.szExeFile.len());
    let name = String::from_utf16_lossy(&entry.szExeFile[..name_end]);
    Some(ProcessSample {
        pid: entry.th32ProcessID,
        name: name.strip_suffix(".exe").unwrap_or(&name).into(),
        created: ticks(created),
        cpu_100ns: ticks(kernel) + ticks(user),
        working_set: memory,
        path,
        read_bytes: None,
        write_bytes: None,
    })
}

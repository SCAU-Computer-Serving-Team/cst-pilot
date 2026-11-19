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

fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}
pub fn snapshot(control: &Control) -> Result<Vec<ProcessSample>, NativeError> {
    let snapshot = Handle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|e| native_error("CreateToolhelp32Snapshot", e))?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry) }
        .map_err(|e| native_error("Process32FirstW", e))?;
    let mut rows = Vec::new();
    loop {
        stopped("Process", control)?;
        if let Some(row) = read(&entry) {
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
fn read(entry: &PROCESSENTRY32W) -> Option<ProcessSample> {
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
    let mut path_buffer = vec![0u16; 32768];
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
    .map(|_| String::from_utf16_lossy(&path_buffer[..length as usize]));
    let memory = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            entry.th32ProcessID,
        )
    }
    .ok()
    .map(Handle)
    .and_then(|handle| {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if unsafe { K32GetProcessMemoryInfo(handle.0, &mut counters, size) }.as_bool() {
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
    })
}

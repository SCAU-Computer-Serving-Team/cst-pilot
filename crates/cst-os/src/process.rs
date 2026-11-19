use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Stable OS process identity combining PID with kernel creation timestamp (100ns FILETIME on Windows).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub created: u64,
}

impl ProcessIdentity {
    pub fn capture(pid: u32) -> Result<Option<Self>, String> {
        #[cfg(windows)]
        {
            capture_windows(pid)
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            Err("运行态升级仅支持 Windows。".into())
        }
    }

    pub fn current() -> Result<Self, String> {
        Self::capture(std::process::id())?.ok_or_else(|| "当前进程无法核验。".into())
    }
}

#[cfg(windows)]
fn capture_windows(pid: u32) -> Result<Option<ProcessIdentity>, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::{
        Foundation::{FILETIME, HANDLE},
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        },
    };
    if pid == 0 {
        return Err("升级父进程标识无效。".into());
    }
    let handle = match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    } {
        Ok(handle) => unsafe { OwnedHandle::from_raw_handle(handle.0) },
        Err(error) if error.code().0 as u32 == 0x8007_0057 => return Ok(None),
        Err(_) => return Err("升级父进程无法核验。".into()),
    };
    let (mut created, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(
            HANDLE(handle.as_raw_handle()),
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .map_err(|_| "升级父进程创建时间无法读取。")?;
    let created = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Ok(Some(ProcessIdentity { pid, created }))
}

/// Checks whether the current process token is a member of the local Administrators group (UAC elevated).
pub fn is_elevated() -> Result<bool, String> {
    #[cfg(windows)]
    {
        use windows::Win32::Security::{
            CheckTokenMembership, CreateWellKnownSid, PSID, WinBuiltinAdministratorsSid,
        };
        use windows::core::BOOL;
        let mut buffer = [0u32; 17];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        let sid = PSID(buffer.as_mut_ptr().cast());
        let mut member = BOOL::default();
        unsafe {
            CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(sid), &mut size)
                .map_err(|e| format!("CreateWellKnownSid failed: {e}"))?;
            CheckTokenMembership(None, sid, &mut member)
                .map_err(|e| format!("CheckTokenMembership failed: {e}"))?;
        }
        Ok(member.as_bool())
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

/// Best-effort liveness probe for a legacy PID-only lock owner.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        };
        match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                true
            }
            Err(error) => error.code().0 as u32 != 0x8007_0057,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        true
    }
}

/// Spawns a detached helper process in its own process group with null stdio handles.
pub fn launch_detached(exe: &Path, args: &[OsString], cwd: &Path) -> Result<u32, String> {
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
    }
    command
        .spawn()
        .map(|child| child.id())
        .map_err(|_| "升级辅助进程无法启动。".into())
}

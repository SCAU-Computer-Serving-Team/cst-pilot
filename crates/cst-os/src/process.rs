use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

/// Queries the Win32 executable image path of `pid` (`None` if the process has exited).
pub fn query_process_image_path(pid: u32) -> Result<Option<PathBuf>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows::Win32::{
            Foundation::HANDLE,
            System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        };
        use windows::core::PWSTR;

        if pid == 0 {
            return Err("进程标识无效。".into());
        }
        let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => unsafe { OwnedHandle::from_raw_handle(handle.0) },
            Err(error) if error.code().0 as u32 == 0x8007_0057 => return Ok(None),
            Err(_) => return Err("进程路径无法核验。".into()),
        };
        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                HANDLE(handle.as_raw_handle()),
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut len,
            )
        }
        .map_err(|_| "进程可执行文件路径无法读取。")?;
        Ok(Some(PathBuf::from(OsString::from_wide(
            &buffer[..len as usize],
        ))))
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err("进程路径核验仅支持 Windows。".into())
    }
}

/// Resolves the trusted Windows directory (e.g. `C:\Windows`) via `GetWindowsDirectoryW`,
/// ignoring untrusted runtime `SystemRoot` / `WINDIR` environment variables.
pub fn windows_directory() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;

        let mut buffer = vec![0u16; 4096];
        let len = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
        if len == 0 || len >= buffer.len() {
            return Err("无法读取系统 Windows 目录。".into());
        }
        Ok(PathBuf::from(OsString::from_wide(&buffer[..len])))
    }
    #[cfg(not(windows))]
    {
        Err("系统目录解析仅支持 Windows。".into())
    }
}

/// Resolves the trusted System32 directory (e.g. `C:\Windows\System32`) via `GetSystemDirectoryW`,
/// ignoring untrusted runtime `SystemRoot` / `PATH` environment variables.
pub fn system_directory() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

        let mut buffer = vec![0u16; 4096];
        let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if len == 0 || len >= buffer.len() {
            return Err("无法读取系统 System32 目录。".into());
        }
        Ok(PathBuf::from(OsString::from_wide(&buffer[..len])))
    }
    #[cfg(not(windows))]
    {
        Err("系统目录解析仅支持 Windows。".into())
    }
}

/// Queries `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\<exe_name>` for a machine-installed executable.
pub fn query_machine_app_path(exe_name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
        use windows::core::PCWSTR;

        if exe_name.is_empty() || exe_name.contains(['\\', '/', '\0']) {
            return None;
        }
        let subkey = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe_name}");
        let subkey_wide: Vec<u16> = std::ffi::OsStr::new(&subkey)
            .encode_wide()
            .chain([0])
            .collect();
        let mut buffer = vec![0u16; 4096];
        let mut byte_len = (buffer.len() * std::mem::size_of::<u16>()) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(subkey_wide.as_ptr()),
                PCWSTR::null(),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut byte_len),
            )
        };
        if status.is_err() || byte_len < 2 {
            return None;
        }
        let u16_len = (byte_len as usize) / 2;
        let trimmed = buffer[..u16_len]
            .iter()
            .position(|&c| c == 0)
            .map_or(&buffer[..u16_len], |nul| &buffer[..nul]);
        let raw = OsString::from_wide(trimmed);
        let cleaned = raw.to_string_lossy();
        let cleaned = cleaned.trim().trim_matches('"');
        if cleaned.is_empty() {
            None
        } else {
            Some(PathBuf::from(cleaned))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = exe_name;
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn trusted_windows_and_process_image_queries_succeed() {
        let win_dir = windows_directory().unwrap();
        let sys_dir = system_directory().unwrap();
        assert!(win_dir.join("explorer.exe").exists());
        assert!(sys_dir.join("notepad.exe").exists());

        let current_img = query_process_image_path(std::process::id())
            .unwrap()
            .unwrap();
        let expected_img = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(
            crate::strip_windows_verbatim_prefix(&current_img.canonicalize().unwrap()),
            crate::strip_windows_verbatim_prefix(&expected_img)
        );
    }
}

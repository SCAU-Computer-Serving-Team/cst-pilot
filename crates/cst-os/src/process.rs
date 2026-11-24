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
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    if pid == 0 {
        return Err("升级父进程标识无效。".into());
    }
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
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
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };
        match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
            Ok(handle) => {
                // The process handle is signalled on exit, including exit code 259 (STILL_ACTIVE).
                let alive = unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0;
                unsafe {
                    let _ = CloseHandle(handle);
                }
                alive
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

/// Token security metadata for a running process (`session_id`, `logon_id`, user SID bytes, and elevation state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessTokenInfo {
    pub session_id: u32,
    pub logon_id: u64,
    pub user_sid: Vec<u8>,
    pub elevated: bool,
}

impl ProcessTokenInfo {
    /// Returns true if `other` belongs to the exact same OS user SID, Terminal Services session,
    /// and (when sharing the same elevation split-token tier) the same `AuthenticationId` logon session.
    pub fn same_user_and_session(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.user_sid == other.user_sid
            && (self.elevated != other.elevated || self.logon_id == other.logon_id)
    }
}

/// Queries the access token of `pid` (`None` if `pid` has exited).
pub fn query_process_token_info(pid: u32) -> Result<Option<ProcessTokenInfo>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows::Win32::{
            Foundation::HANDLE,
            Security::{
                GetLengthSid, GetTokenInformation, IsValidSid, TOKEN_ELEVATION, TOKEN_QUERY,
                TOKEN_STATISTICS, TOKEN_USER, TokenElevation, TokenSessionId, TokenStatistics,
                TokenUser,
            },
            System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
        };

        if pid == 0 {
            return Err("进程标识无效。".into());
        }
        let proc_handle =
            match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
                Ok(h) => unsafe { OwnedHandle::from_raw_handle(h.0) },
                Err(error) if error.code().0 as u32 == 0x8007_0057 => return Ok(None),
                Err(_) => return Err("无法打开目标进程句柄核验权限。".into()),
            };
        let mut raw_token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                HANDLE(proc_handle.as_raw_handle()),
                TOKEN_QUERY,
                &mut raw_token,
            )
        }
        .map_err(|_| "无法读取目标进程访问令牌。".to_owned())?;
        let token = unsafe { OwnedHandle::from_raw_handle(raw_token.0) };
        let token_handle = HANDLE(token.as_raw_handle());

        let mut session_id = 0u32;
        let mut ret_len = 0u32;
        unsafe {
            GetTokenInformation(
                token_handle,
                TokenSessionId,
                Some((&mut session_id as *mut u32).cast()),
                std::mem::size_of::<u32>() as u32,
                &mut ret_len,
            )
        }
        .map_err(|_| "无法读取进程登录会话标识。".to_owned())?;

        let mut stats = TOKEN_STATISTICS::default();
        let mut stats_len = 0u32;
        unsafe {
            GetTokenInformation(
                token_handle,
                TokenStatistics,
                Some((&mut stats as *mut TOKEN_STATISTICS).cast()),
                std::mem::size_of::<TOKEN_STATISTICS>() as u32,
                &mut stats_len,
            )
        }
        .map_err(|_| "无法读取进程登录凭据会话统计。".to_owned())?;
        let logon_id = (u64::from(stats.AuthenticationId.HighPart as u32) << 32)
            | u64::from(stats.AuthenticationId.LowPart);

        let mut elevation = TOKEN_ELEVATION::default();
        let mut elev_len = 0u32;
        unsafe {
            GetTokenInformation(
                token_handle,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut elev_len,
            )
        }
        .map_err(|_| "无法读取进程令牌提权状态。".to_owned())?;

        let mut user_len = 0u32;
        let _ = unsafe { GetTokenInformation(token_handle, TokenUser, None, 0, &mut user_len) };
        let min_user_len = std::mem::size_of::<TOKEN_USER>();
        if (user_len as usize) < min_user_len || user_len > 4096 {
            return Err("无法读取进程用户安全标识符长度。".into());
        }
        let mut user_buf = vec![0u64; (user_len as usize).div_ceil(std::mem::size_of::<u64>())];
        let mut returned_len = 0u32;
        unsafe {
            GetTokenInformation(
                token_handle,
                TokenUser,
                Some(user_buf.as_mut_ptr().cast()),
                user_len,
                &mut returned_len,
            )
        }
        .map_err(|_| "无法读取进程用户安全标识符。".to_owned())?;
        if (returned_len as usize) < min_user_len {
            return Err("目标进程用户安全标识符数据不完整。".into());
        }

        // SAFETY: `user_buf` is 8-byte aligned (satisfying `TOKEN_USER`) and at least `size_of::<TOKEN_USER>()` bytes were written.
        let token_user: &TOKEN_USER = unsafe { &*(user_buf.as_ptr().cast::<TOKEN_USER>()) };
        let sid = token_user.User.Sid;
        if sid.is_invalid() || !unsafe { IsValidSid(sid) }.as_bool() {
            return Err("目标进程用户安全标识符无效。".into());
        }
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        let sid_bytes = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), sid_len) }.to_vec();

        Ok(Some(ProcessTokenInfo {
            session_id,
            logon_id,
            user_sid: sid_bytes,
            elevated: elevation.TokenIsElevated != 0,
        }))
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Ok(Some(ProcessTokenInfo {
            session_id: 0,
            logon_id: 0,
            user_sid: Vec::new(),
            elevated: false,
        }))
    }
}

/// Queries the access token of the current process.
pub fn current_process_token_info() -> Result<ProcessTokenInfo, String> {
    query_process_token_info(std::process::id())?.ok_or_else(|| "当前进程访问令牌无法读取。".into())
}

/// Returns `true` when the current process runs under a UAC split-token administrator account
/// (`TokenElevationTypeLimited`), meaning `ShellExecuteExW("runas")` on `cst-server.exe` elevates
/// under the exact same user SID while keeping the GUI window at Medium Integrity Level (avoiding UIPI).
pub fn can_elevate_same_user() -> Result<bool, String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows::Win32::{
            Foundation::HANDLE,
            Security::{
                GetTokenInformation, TOKEN_ELEVATION_TYPE, TOKEN_QUERY, TokenElevationType,
                TokenElevationTypeLimited,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };

        let mut raw_token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) }
            .map_err(|_| "无法读取当前进程访问令牌。".to_owned())?;
        let token = unsafe { OwnedHandle::from_raw_handle(raw_token.0) };
        let mut elev_type = TOKEN_ELEVATION_TYPE::default();
        let mut ret_len = 0u32;
        unsafe {
            GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenElevationType,
                Some((&mut elev_type as *mut TOKEN_ELEVATION_TYPE).cast()),
                std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
                &mut ret_len,
            )
        }
        .map_err(|_| "无法读取当前进程提权令牌类型。".to_owned())?;
        Ok(elev_type == TokenElevationTypeLimited)
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn exited_process_with_still_active_exit_code_is_not_live() {
        let mut process = std::process::Command::new(system_directory().unwrap().join("cmd.exe"))
            .args(["/d", "/c", "exit", "259"])
            .spawn()
            .unwrap();
        let pid = process.id();
        assert_eq!(process.wait().unwrap().code(), Some(259));
        assert!(!is_process_alive(pid));
    }

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

        let token_info = current_process_token_info().unwrap();
        assert!(!token_info.user_sid.is_empty());
        assert!(token_info.same_user_and_session(&token_info));
    }
}

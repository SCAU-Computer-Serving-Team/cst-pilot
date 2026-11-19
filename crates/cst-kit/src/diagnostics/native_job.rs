use super::win_native::Control;
use std::ffi::OsString;
use std::path::Path;

/// Execute a fixed trusted helper without a shell. The process is created
/// suspended, assigned to a kill-on-close Job Object, then allowed to run.
#[cfg(windows)]
pub fn run(
    exe: &Path,
    args: &[OsString],
    control: &Control,
    max_output: usize,
) -> Result<Vec<u8>, String> {
    use std::fs::File;
    use std::io::Read;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Foundation::{
        CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::JobObjects::*;
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::*;
    use windows::core::{PCWSTR, PWSTR};
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    struct Attributes {
        ptr: LPPROC_THREAD_ATTRIBUTE_LIST,
        _memory: Vec<usize>,
    }
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.ptr);
            }
        }
    }
    fn err(_: windows::core::Error) -> String {
        "受控采集子进程无法启动。".into()
    }
    fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>, String> {
        let mut text: Vec<u16> = value.encode_wide().collect();
        if text.contains(&0) {
            return Err("采集子进程参数包含空字符。".into());
        }
        text.push(0);
        Ok(text)
    }
    control.check()?;
    let application = wide(exe.as_os_str())?;
    let mut command = OsString::new();
    for value in std::iter::once(exe.as_os_str()).chain(args.iter().map(OsString::as_os_str)) {
        if !command.is_empty() {
            command.push(" ");
        }
        command.push(quote(value));
    }
    let mut command = wide(&command)?;
    if command.len() > 32767 {
        return Err("采集子进程命令行超过系统上限。".into());
    }
    // SAFETY: all structures are initialized PODs; handles are RAII-owned and
    // only explicitly listed pipe handles are inherited by the child.
    unsafe {
        let job = Handle(CreateJobObjectW(None, PCWSTR::null()).map_err(err)?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        )
        .map_err(err)?;
        let security = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: true.into(),
            ..Default::default()
        };
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        CreatePipe(&mut read, &mut write, Some(&security), 0).map_err(err)?;
        let reader = Handle(read);
        let writer = Handle(write);
        SetHandleInformation(reader.0, HANDLE_FLAG_INHERIT.0, Default::default()).map_err(err)?;
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut memory = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let ptr = LPPROC_THREAD_ATTRIBUTE_LIST(memory.as_mut_ptr().cast());
        InitializeProcThreadAttributeList(Some(ptr), 1, None, &mut size).map_err(err)?;
        let attributes = Attributes {
            ptr,
            _memory: memory,
        };
        let handles = [writer.0];
        UpdateProcThreadAttribute(
            attributes.ptr,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(handles.as_ptr().cast()),
            std::mem::size_of_val(&handles),
            None,
            None,
        )
        .map_err(err)?;
        let mut startup = STARTUPINFOEXW {
            lpAttributeList: attributes.ptr,
            ..Default::default()
        };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdOutput = writer.0;
        startup.StartupInfo.hStdError = writer.0;
        let mut info = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            &startup.StartupInfo,
            &mut info,
        )
        .map_err(err)?;
        let process = Handle(info.hProcess);
        let thread = Handle(info.hThread);
        if AssignProcessToJobObject(job.0, process.0).is_err() {
            let _ = TerminateProcess(process.0, 1);
            return Err("采集子进程无法加入受控任务。".into());
        }
        if ResumeThread(thread.0) == u32::MAX {
            let _ = TerminateJobObject(job.0, 1);
            return Err("采集子进程无法恢复运行。".into());
        }
        drop(writer);
        let raw = reader.0;
        std::mem::forget(reader);
        let mut file = File::from_raw_handle(raw.0);
        let output = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut block = [0u8; 8192];
            let mut overflow = false;
            loop {
                match file.read(&mut block) {
                    Ok(0) => break,
                    Ok(n) => {
                        if bytes.len() + n <= max_output {
                            bytes.extend_from_slice(&block[..n]);
                        } else {
                            overflow = true;
                        }
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            if overflow {
                Err("采集子进程输出超过预算。".into())
            } else {
                Ok(bytes)
            }
        });
        let outcome = loop {
            if let Err(error) = control.check() {
                break Err(error);
            }
            match WaitForSingleObject(process.0, 20) {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    GetExitCodeProcess(process.0, &mut code).map_err(err)?;
                    break if code == 0 {
                        Ok(())
                    } else {
                        Err(format!("采集子进程退出码：{code}。"))
                    };
                }
                WAIT_TIMEOUT => (),
                _ => break Err("等待采集子进程失败。".into()),
            }
        };
        // Also terminate any descendant still holding the output pipe.
        let _ = TerminateJobObject(job.0, 1);
        let bytes = output.join().map_err(|_| "采集输出线程异常。")?;
        outcome?;
        bytes
    }
}
#[cfg(not(windows))]
pub fn run(_: &Path, _: &[OsString], _: &Control, _: usize) -> Result<Vec<u8>, String> {
    Err("受控采集子进程仅支持 Windows。".into())
}

#[cfg(windows)]
fn quote(value: &std::ffi::OsStr) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for unit in value.encode_wide() {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            b'\\' as u16,
            if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        out.push(unit);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    OsString::from_wide(&out)
}

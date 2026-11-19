use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

pub use cst_os::{ProcessIdentity, RestartUi};

use crate::applier::UpdateHost;

pub struct NativeHost {
    pub ui: RestartUi,
}

impl UpdateHost for NativeHost {
    fn health_check(&self, root: &Path, cancel: &CancellationToken) -> Result<(), String> {
        let expected_version = crate::layout::read_version(root)?;
        let expected_line = format!("cst-pilot {expected_version}");
        run_health_checks_in_shared_job(
            &[root.join("cst-server.exe"), root.join("cst-pilot.exe")],
            &[OsString::from("--health-check")],
            &expected_line,
            Duration::from_secs(15),
            cancel,
            64 * 1024,
        )
        .map_err(|_| "新版程序未在 15 秒内通过健康检查。".into())
    }

    fn restart(&self, root: &Path) -> Result<(), String> {
        launch(
            &root.join("cst-pilot.exe"),
            &["--ui".into(), self.ui.argument().into()],
            root,
        )
        .map(|_| ())
    }
}

pub(crate) fn launch(exe: &Path, args: &[OsString], root: &Path) -> Result<u32, String> {
    cst_os::launch_detached(exe, args, root)
}

#[cfg(windows)]
fn run_health_checks_in_shared_job(
    exes: &[ std::path::PathBuf ],
    args: &[OsString],
    expected_line: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    max_output: usize,
) -> Result<(), String> {
    use std::fs::File;
    use std::io::Read;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::FromRawHandle;
    use std::time::Instant;
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
    fn quote(value: &std::ffi::OsStr) -> OsString {
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
    fn err(_: windows::core::Error) -> String {
        "升级健康检查进程无法启动。".into()
    }
    fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>, String> {
        let mut text: Vec<u16> = value.encode_wide().collect();
        if text.contains(&0) {
            return Err("健康检查参数包含空字符。".into());
        }
        text.push(0);
        Ok(text)
    }

    crate::io::cancelled(cancel)?;
    let deadline = Instant::now() + timeout;

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

        for exe in exes {
            crate::io::cancelled(cancel)?;
            if Instant::now() >= deadline {
                let _ = TerminateJobObject(job.0, 1);
                return Err("健康检查超时。".into());
            }
            let application = wide(exe.as_os_str())?;
            let mut command = OsString::new();
            for value in
                std::iter::once(exe.as_os_str()).chain(args.iter().map(OsString::as_os_str))
            {
                if !command.is_empty() {
                    command.push(" ");
                }
                command.push(quote(value));
            }
            let mut command = wide(&command)?;
            if command.len() > 32767 {
                let _ = TerminateJobObject(job.0, 1);
                return Err("健康检查命令行超过系统上限。".into());
            }

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
            SetHandleInformation(reader.0, HANDLE_FLAG_INHERIT.0, Default::default())
                .map_err(err)?;
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
                let _ = TerminateJobObject(job.0, 1);
                return Err("健康检查进程无法加入受控任务。".into());
            }
            if ResumeThread(thread.0) == u32::MAX {
                let _ = TerminateJobObject(job.0, 1);
                return Err("健康检查进程无法恢复运行。".into());
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
                    Err("健康检查输出超过预算。".into())
                } else {
                    Ok(bytes)
                }
            });
            let outcome = loop {
                if let Err(error) = crate::io::cancelled(cancel) {
                    break Err(error);
                }
                if Instant::now() >= deadline {
                    break Err("健康检查超时。".into());
                }
                match WaitForSingleObject(process.0, 20) {
                    WAIT_OBJECT_0 => {
                        let mut code = 0;
                        GetExitCodeProcess(process.0, &mut code).map_err(err)?;
                        break if code == 0 {
                            Ok(())
                        } else {
                            Err(format!("健康检查进程退出码：{code}。"))
                        };
                    }
                    WAIT_TIMEOUT => (),
                    _ => break Err("等待健康检查进程失败。".into()),
                }
            };
            if let Err(error) = outcome {
                let _ = TerminateJobObject(job.0, 1);
                let _ = output.join();
                return Err(error);
            }
            let bytes = match output.join().map_err(|_| "健康检查输出线程异常。".to_owned())? {
                Ok(b) => b,
                Err(e) => {
                    let _ = TerminateJobObject(job.0, 1);
                    return Err(e);
                }
            };
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                let _ = TerminateJobObject(job.0, 1);
                "健康检查输出编码无效。".to_owned()
            })?;
            if !text.lines().any(|line| line.trim() == expected_line) {
                let _ = TerminateJobObject(job.0, 1);
                return Err("健康检查输出版本与工具包 VERSION 不一致。".into());
            }
        }
        let _ = TerminateJobObject(job.0, 1);
        Ok(())
    }
}

#[cfg(not(windows))]
fn run_health_checks_in_shared_job(
    _: &[std::path::PathBuf],
    _: &[OsString],
    _: &str,
    _: Duration,
    _: &CancellationToken,
    _: usize,
) -> Result<(), String> {
    Err("运行态升级仅支持 Windows。".into())
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        time::Instant,
    };
    use windows::Win32::{
        Foundation::{FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
    };

    struct Parent {
        handle: OwnedHandle,
        identity: ProcessIdentity,
    }

    fn open(pid: u32) -> Result<Option<Parent>, String> {
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
        Ok(Some(Parent {
            handle,
            identity: ProcessIdentity { pid, created },
        }))
    }

    pub(super) fn wait(
        parents: &[ProcessIdentity],
        budget: Duration,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let mut opened = Vec::new();
        for expected in parents {
            if expected.pid == std::process::id() {
                return Err("更新器不能等待自身退出。".into());
            }
            if let Some(parent) = open(expected.pid)? {
                if parent.identity != *expected {
                    return Err("升级父进程标识已被复用，未替换程序。".into());
                }
                opened.push(parent);
            }
        }
        let deadline = Instant::now() + budget;
        for parent in opened {
            loop {
                crate::io::cancelled(cancel)?;
                match unsafe { WaitForSingleObject(HANDLE(parent.handle.as_raw_handle()), 20) } {
                    WAIT_OBJECT_0 => break,
                    WAIT_TIMEOUT if Instant::now() < deadline => (),
                    WAIT_TIMEOUT => return Err("等待原程序退出超时，未替换程序。".into()),
                    _ => return Err("无法等待原程序退出。".into()),
                }
            }
        }
        Ok(())
    }
}

pub fn wait_for_parents(
    parents: &[ProcessIdentity],
    budget: Duration,
    cancel: &CancellationToken,
) -> Result<(), String> {
    if parents.is_empty()
        || parents.len() > 16
        || parents.iter().any(|p| p.pid == 0 || p.created == 0)
    {
        return Err("升级父进程列表无效。".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    if parents.iter().any(|p| !seen.insert(p.pid)) {
        return Err("升级父进程列表重复。".into());
    }
    #[cfg(windows)]
    {
        native::wait(parents, budget, cancel)
    }
    #[cfg(not(windows))]
    {
        let _ = (budget, cancel);
        Err("运行态升级仅支持 Windows。".into())
    }
}

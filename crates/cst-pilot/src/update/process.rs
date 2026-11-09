use super::applier::UpdateHost;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub created: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RestartUi {
    Tui,
    Web,
    Desktop,
}
impl RestartUi {
    fn argument(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Web => "web",
            Self::Desktop => "desktop",
        }
    }
}
pub struct NativeHost {
    pub ui: RestartUi,
}
impl UpdateHost for NativeHost {
    fn health_check(&self, root: &Path, cancel: &CancellationToken) -> Result<(), String> {
        let control = crate::diagnostics::win_native::Control::new(
            cancel.child_token(),
            Duration::from_secs(15),
        );
        crate::diagnostics::native_job::run(
            &root.join("cst-pilot.exe"),
            &["--health-check".into()],
            &control,
            64 * 1024,
        )
        .map(|_| ())
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
pub(super) fn launch(exe: &Path, args: &[std::ffi::OsString], root: &Path) -> Result<u32, String> {
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP; no inherited console exit signal.
    }
    command
        .spawn()
        .map(|child| child.id())
        .map_err(|_| "升级辅助进程无法启动。".into())
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
    pub(super) struct Parent {
        handle: OwnedHandle,
        pub(super) identity: ProcessIdentity,
    }
    pub(super) fn open(pid: u32) -> Result<Option<Parent>, String> {
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
            Err(error) if error.code().0 as u32 == 0x80070057 => return Ok(None),
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
                super::super::io::cancelled(cancel)?;
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
impl ProcessIdentity {
    pub fn capture(pid: u32) -> Result<Option<Self>, String> {
        #[cfg(windows)]
        {
            Ok(native::open(pid)?.map(|p| p.identity))
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

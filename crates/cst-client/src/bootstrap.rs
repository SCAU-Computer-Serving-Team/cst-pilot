use std::ffi::OsString;
use std::fs;
#[cfg(windows)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cst_os::{
    ProcessIdentity, ProcessTokenInfo, current_process_token_info, is_reparse_or_symlink,
    normalize_canonical_path_for_hash, query_process_image_path, query_process_token_info,
    read_discovery_file, strip_windows_verbatim_prefix, system_directory, windows_directory,
};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::error::ClientError;
use crate::http::AppHttpClient;

pub type TokenQueryFn = Arc<dyn Fn(u32) -> Result<Option<ProcessTokenInfo>, String> + Send + Sync>;
pub type CurrentTokenFn = Arc<dyn Fn() -> Result<ProcessTokenInfo, String> + Send + Sync>;

#[derive(Clone)]
pub struct BootstrapOptions {
    pub require_elevation: bool,
    pub allow_uac_server_spawn: bool,
    pub expected_desktop_exe: Option<PathBuf>,
    pub expected_server_exe: Option<PathBuf>,
    pub expected_updater_exe: Option<PathBuf>,
    pub spawn_server_if_missing: bool,
    pub server_extra_args: Vec<OsString>,
    pub startup_timeout: Duration,
    pub query_token_fn: TokenQueryFn,
    pub current_token_fn: CurrentTokenFn,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            require_elevation: true,
            allow_uac_server_spawn: false,
            expected_desktop_exe: None,
            expected_server_exe: None,
            expected_updater_exe: None,
            spawn_server_if_missing: true,
            server_extra_args: Vec::new(),
            startup_timeout: Duration::from_secs(10),
            query_token_fn: Arc::new(query_process_token_info),
            current_token_fn: Arc::new(current_process_token_info),
        }
    }
}

impl BootstrapOptions {
    pub fn desktop_exe_for(&self, canonical_kit_root: &Path) -> PathBuf {
        self.expected_desktop_exe
            .clone()
            .unwrap_or_else(|| canonical_kit_root.join("cst-pilot.exe"))
    }

    pub fn server_exe_for(&self, canonical_kit_root: &Path) -> PathBuf {
        self.expected_server_exe
            .clone()
            .unwrap_or_else(|| canonical_kit_root.join("app").join("cst-server.exe"))
    }

    pub fn updater_exe_for(&self, canonical_kit_root: &Path) -> PathBuf {
        self.expected_updater_exe
            .clone()
            .unwrap_or_else(|| canonical_kit_root.join("app").join("cst-updater.exe"))
    }
}

pub enum DesktopInstanceOutcome {
    Acquired(DesktopInstanceGuard),
    ActivatedExisting { owner_pid: u32 },
}

/// Holds the kernel-owned desktop single-instance named pipe and listens for `"ACTIVATE"` requests.
pub struct DesktopInstanceGuard {
    pipe_address: String,
    stop: Arc<AtomicBool>,
    wakeup_rx: watch::Receiver<u64>,
    #[cfg(windows)]
    listener_thread: Option<std::thread::JoinHandle<()>>,
}

impl DesktopInstanceGuard {
    pub fn pipe_address(&self) -> &str {
        &self.pipe_address
    }

    pub fn subscribe_wakeup(&self) -> watch::Receiver<u64> {
        self.wakeup_rx.clone()
    }
}

impl Drop for DesktopInstanceGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        {
            if let Some(handle) = self.listener_thread.take() {
                let _ = handle.join();
            }
        }
    }
}

pub fn desktop_pipe_address(kit_root: &Path) -> Result<String, ClientError> {
    let normalized =
        normalize_canonical_path_for_hash(kit_root).map_err(ClientError::BootstrapFailed)?;
    let hash = Sha256::digest(normalized.as_bytes());
    Ok(format!(r"\\.\pipe\cst-pilot-desktop-{hash:x}"))
}

/// Acquires the per-kit desktop single-instance lock or wakes an already-running instance in the same user session.
pub fn acquire_or_activate_desktop_instance(
    kit_root: &Path,
    options: &BootstrapOptions,
) -> Result<DesktopInstanceOutcome, ClientError> {
    let canonical_root = canonicalize_kit_root(kit_root)?;
    let self_token = (options.current_token_fn)().map_err(ClientError::BootstrapFailed)?;
    if options.require_elevation && !options.allow_uac_server_spawn && !self_token.elevated {
        return Err(ClientError::NotElevated(
            "桌面端需要管理员权限运行，请通过 UAC 授权后启动。".into(),
        ));
    }

    let address = desktop_pipe_address(&canonical_root)?;

    #[cfg(windows)]
    {
        match try_create_first_desktop_pipe(&address) {
            Ok(first_pipe) => {
                let stop = Arc::new(AtomicBool::new(false));
                let (wakeup_tx, wakeup_rx) = watch::channel(0u64);
                let thread_stop = stop.clone();
                let listener_thread = std::thread::Builder::new()
                    .name("cst-desktop-wakeup".into())
                    .spawn(move || {
                        run_desktop_wakeup_listener(first_pipe, thread_stop, wakeup_tx);
                    })
                    .map_err(|e| {
                        ClientError::BootstrapFailed(format!("无法启动单实例唤醒监听线程：{e}"))
                    })?;

                Ok(DesktopInstanceOutcome::Acquired(DesktopInstanceGuard {
                    pipe_address: address,
                    stop,
                    wakeup_rx,
                    listener_thread: Some(listener_thread),
                }))
            }
            Err(Some(5 | 183 | 231)) => activate_existing_windows_desktop_instance(
                &canonical_root,
                &address,
                &self_token,
                options,
            ),
            Err(code) => Err(ClientError::BootstrapFailed(format!(
                "无法建立桌面端单实例命名管道锁（错误码 {code:?}）。"
            ))),
        }
    }

    #[cfg(not(windows))]
    {
        let _ = (&self_token, options);
        let (_tx, wakeup_rx) = watch::channel(0u64);
        Ok(DesktopInstanceOutcome::Acquired(DesktopInstanceGuard {
            pipe_address: address,
            stop: Arc::new(AtomicBool::new(false)),
            wakeup_rx,
        }))
    }
}

/// Checks `.state/update/transaction.json` before starting network/server connections;
/// if an interrupted upgrade journal exists, synchronously runs `cst-updater.exe --root <kit> --recover`.
pub fn recover_pending_update_transaction(
    kit_root: &Path,
    options: &BootstrapOptions,
) -> Result<bool, ClientError> {
    let canonical_root = canonicalize_kit_root(kit_root)?;
    let tx_path = canonical_root
        .join(".state")
        .join("update")
        .join("transaction.json");
    if !tx_path.exists() {
        return Ok(false);
    }

    let updater_exe = options.updater_exe_for(&canonical_root);
    if !updater_exe.is_file() || path_is_reparse_or_symlink(&updater_exe) {
        return Err(ClientError::UpgradeRecoveryFailed(
            "检测到未完成的软件升级事务，且独立更新程序缺失或无效，无法自动恢复。".into(),
        ));
    }

    let mut cmd = Command::new(&updater_exe);
    apply_portable_sanitized_env(&mut cmd, &canonical_root)?;
    cmd.arg("--root")
        .arg(&canonical_root)
        .arg("--recover")
        .current_dir(&canonical_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = cmd.spawn().map_err(|e| {
        ClientError::UpgradeRecoveryFailed(format!("无法启动独立更新程序执行升级恢复：{e}"))
    })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ClientError::UpgradeRecoveryFailed(
                        "执行升级中断恢复超时，请检查磁盘读写状态后重试。".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => {
                return Err(ClientError::UpgradeRecoveryFailed(format!(
                    "等待升级恢复结果失败：{e}"
                )));
            }
        }
    };

    if !status.success() || tx_path.exists() {
        return Err(ClientError::UpgradeRecoveryFailed(
            "上次软件升级中断且自动回滚未完成，已停止启动以保护本地数据。".into(),
        ));
    }
    Ok(true)
}

#[derive(Debug, Clone)]
pub struct VerifiedServerEndpoint {
    pub kit_root: PathBuf,
    pub pid: u32,
    pub process_created: u64,
    pub port: u16,
    pub version: String,
    pub http: AppHttpClient,
    pub spawned_by_client: bool,
}

/// Discovers and bidirectionally verifies `.state/server.json` + Win32 process/token + `GET /api/health`,
/// or spawns `<kit_root>/cst-server.exe` (`CREATE_NO_WINDOW`) with a sanitized environment whitelist.
pub async fn ensure_server_running(
    kit_root: &Path,
    options: &BootstrapOptions,
) -> Result<VerifiedServerEndpoint, ClientError> {
    let canonical_root = canonicalize_kit_root(kit_root)?;
    let self_token = (options.current_token_fn)().map_err(ClientError::BootstrapFailed)?;
    if options.require_elevation && !options.allow_uac_server_spawn && !self_token.elevated {
        return Err(ClientError::NotElevated(
            "桌面端需要管理员权限运行，请通过 UAC 授权后启动。".into(),
        ));
    }

    // Step 1: Probe existing `server.json` if present
    match probe_existing_server(&canonical_root, &self_token, options).await {
        ProbeOutcome::Verified(endpoint) => return Ok(endpoint),
        ProbeOutcome::Fatal(err) => return Err(err),
        ProbeOutcome::Stopping(old_pid) => {
            // Wait up to 3s for the shutting-down server to release `ServerInstanceLock`
            let wait_deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < wait_deadline && cst_os::is_process_alive(old_pid) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        ProbeOutcome::AbsentOrStale => {}
    }

    if !options.spawn_server_if_missing {
        return Err(ClientError::Disconnected("本机后台服务未运行。".into()));
    }

    // Step 2: Spawn `<kit_root>/cst-server.exe` (via UAC `runas` when desktop runs at Medium IL,
    // or direct `CREATE_NO_WINDOW` child when already elevated / elevation not required).
    let server_exe = options.server_exe_for(&canonical_root);
    if !server_exe.is_file() || path_is_reparse_or_symlink(&server_exe) {
        return Err(ClientError::BootstrapFailed(format!(
            "未找到有效的后台服务可执行文件：{}",
            server_exe.display()
        )));
    }

    let mut spawned = if options.require_elevation && !self_token.elevated {
        spawn_elevated_server_via_uac(&server_exe, &canonical_root, &options.server_extra_args)?
    } else {
        let mut cmd = Command::new(&server_exe);
        apply_portable_sanitized_env(&mut cmd, &canonical_root)?;
        cmd.args(&options.server_extra_args)
            .current_dir(&canonical_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW (0x0800_0000) | CREATE_NEW_PROCESS_GROUP (0x0000_0200)
            cmd.creation_flags(0x0800_0200);
        }

        let child = cmd.spawn().map_err(|e| {
            ClientError::BootstrapFailed(format!("无法启动本机后台服务 cst-server.exe：{e}"))
        })?;
        SpawnedServerHandle::Direct(child)
    };

    let deadline = Instant::now() + options.startup_timeout;
    loop {
        match probe_existing_server(&canonical_root, &self_token, options).await {
            ProbeOutcome::Verified(mut endpoint) => {
                endpoint.spawned_by_client = true;
                return Ok(endpoint);
            }
            ProbeOutcome::Fatal(err) => return Err(err),
            ProbeOutcome::Stopping(_) | ProbeOutcome::AbsentOrStale => {}
        }

        if let Some(exit_desc) = spawned.try_exited() {
            // One final probe in case another paired instance won the race cleanly
            if let ProbeOutcome::Verified(endpoint) =
                probe_existing_server(&canonical_root, &self_token, options).await
            {
                return Ok(endpoint);
            }
            return Err(ClientError::BootstrapFailed(format!(
                "后台服务进程提前退出（{exit_desc}）。"
            )));
        }

        if Instant::now() >= deadline {
            spawned.terminate_best_effort();
            return Err(ClientError::BootstrapFailed(
                "等待本机后台服务就绪超时（10 秒）。".into(),
            ));
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

enum SpawnedServerHandle {
    Direct(std::process::Child),
    #[cfg(windows)]
    Elevated(std::os::windows::io::OwnedHandle),
}

impl SpawnedServerHandle {
    fn try_exited(&mut self) -> Option<String> {
        match self {
            Self::Direct(child) => child
                .try_wait()
                .ok()
                .flatten()
                .map(|st| format!("状态码：{st}")),
            #[cfg(windows)]
            Self::Elevated(handle) => {
                use std::os::windows::io::AsRawHandle;
                use windows::Win32::{
                    Foundation::{HANDLE, WAIT_OBJECT_0},
                    System::Threading::{GetExitCodeProcess, WaitForSingleObject},
                };
                let raw = HANDLE(handle.as_raw_handle());
                if unsafe { WaitForSingleObject(raw, 0) } == WAIT_OBJECT_0 {
                    let mut code = 0u32;
                    let _ = unsafe { GetExitCodeProcess(raw, &mut code) };
                    Some(format!("退出码：{code}"))
                } else {
                    None
                }
            }
        }
    }

    fn terminate_best_effort(&mut self) {
        match self {
            Self::Direct(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            #[cfg(windows)]
            Self::Elevated(handle) => {
                use std::os::windows::io::AsRawHandle;
                use windows::Win32::{Foundation::HANDLE, System::Threading::TerminateProcess};
                let _ = unsafe { TerminateProcess(HANDLE(handle.as_raw_handle()), 1) };
            }
        }
    }
}

fn spawn_elevated_server_via_uac(
    server_exe: &Path,
    canonical_root: &Path,
    extra_args: &[OsString],
) -> Result<SpawnedServerHandle, ClientError> {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows::Win32::UI::Shell::{
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        use windows::core::PCWSTR;

        fn quote_win32_arg(value: &OsStr) -> OsString {
            let mut out = vec![b'"' as u16];
            let mut slashes = 0usize;
            for unit in value.encode_wide() {
                if unit == b'\\' as u16 {
                    slashes += 1;
                    continue;
                }
                let repeat = if unit == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                };
                out.extend(std::iter::repeat_n(b'\\' as u16, repeat));
                slashes = 0;
                out.push(unit);
            }
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
            out.push(b'"' as u16);
            OsString::from_wide(&out)
        }

        let clean_root = strip_windows_verbatim_prefix(canonical_root);
        let clean_exe = strip_windows_verbatim_prefix(server_exe);

        let mut params = OsString::new();
        params.push(quote_win32_arg(OsStr::new("--sanitize-env")));
        params.push(" ");
        params.push(quote_win32_arg(OsStr::new("--kit-root")));
        params.push(" ");
        params.push(quote_win32_arg(clean_root.as_os_str()));
        for arg in extra_args {
            params.push(" ");
            params.push(quote_win32_arg(arg));
        }

        let verb_w: Vec<u16> = OsStr::new("runas").encode_wide().chain([0]).collect();
        let file_w: Vec<u16> = clean_exe.as_os_str().encode_wide().chain([0]).collect();
        let params_w: Vec<u16> = params.as_os_str().encode_wide().chain([0]).collect();
        let dir_w: Vec<u16> = clean_root.as_os_str().encode_wide().chain([0]).collect();

        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb_w.as_ptr()),
            lpFile: PCWSTR(file_w.as_ptr()),
            lpParameters: PCWSTR(params_w.as_ptr()),
            lpDirectory: PCWSTR(dir_w.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };

        unsafe { ShellExecuteExW(&mut sei) }.map_err(|_| {
            ClientError::NotElevated(
                "需要管理员权限以运行后台诊断服务，UAC 授权已取消。".to_owned(),
            )
        })?;

        if sei.hProcess.is_invalid() {
            return Err(ClientError::BootstrapFailed(
                "后台诊断服务 UAC 启动未返回有效进程句柄。".to_owned(),
            ));
        }
        Ok(SpawnedServerHandle::Elevated(unsafe {
            OwnedHandle::from_raw_handle(sei.hProcess.0)
        }))
    }

    #[cfg(not(windows))]
    {
        let _ = (server_exe, canonical_root, extra_args);
        Err(ClientError::NotElevated(
            "桌面端需要管理员权限运行。".into(),
        ))
    }
}

enum ProbeOutcome {
    Verified(VerifiedServerEndpoint),
    Stopping(u32),
    AbsentOrStale,
    Fatal(ClientError),
}

async fn probe_existing_server(
    canonical_root: &Path,
    self_token: &ProcessTokenInfo,
    options: &BootstrapOptions,
) -> ProbeOutcome {
    let Some(record) = read_discovery_file(canonical_root) else {
        return ProbeOutcome::AbsentOrStale;
    };

    let proc_identity = match ProcessIdentity::capture(record.pid) {
        Ok(Some(id)) => id,
        Ok(None) => return ProbeOutcome::AbsentOrStale,
        Err(_) => {
            return ProbeOutcome::Fatal(ClientError::CrossSessionInstance(
                "检测到其他用户或受限会话的后台服务实例正在运行，请先关闭旧实例。".into(),
            ));
        }
    };

    let expected_server = match options.server_exe_for(canonical_root).canonicalize() {
        Ok(p) => strip_windows_verbatim_prefix(&p),
        Err(_) => strip_windows_verbatim_prefix(&options.server_exe_for(canonical_root)),
    };
    let actual_image = match query_process_image_path(record.pid) {
        Ok(Some(img)) => img
            .canonicalize()
            .map(|p| strip_windows_verbatim_prefix(&p))
            .unwrap_or_else(|_| strip_windows_verbatim_prefix(&img)),
        Ok(None) => return ProbeOutcome::AbsentOrStale,
        Err(msg) => return ProbeOutcome::Fatal(ClientError::InvalidServerIdentity(msg)),
    };
    if !paths_equal_windows_ci(&actual_image, &expected_server) {
        // Stale PID reused by an unrelated OS process? Wait: check if `ServerInstanceLock` is actually held!
        // If `ServerInstanceLock` can be acquired, `.state/server.json` is merely a stale leftover from a crash + PID reuse!
        if cst_os::ServerInstanceLock::acquire(canonical_root).is_ok() {
            return ProbeOutcome::AbsentOrStale;
        }
        return ProbeOutcome::Fatal(ClientError::InvalidServerIdentity(
            "后台服务可执行文件路径与当前工具包目录不符，拒绝建立连接。".into(),
        ));
    }

    let server_token_info = match (options.query_token_fn)(record.pid) {
        Ok(Some(info)) => info,
        Ok(None) => return ProbeOutcome::AbsentOrStale,
        Err(_) => {
            return ProbeOutcome::Fatal(ClientError::CrossSessionInstance(
                "无法核验后台服务进程的登录会话令牌，请关闭残留进程后重试。".into(),
            ));
        }
    };

    if !self_token.same_user_and_session(&server_token_info) {
        return ProbeOutcome::Fatal(ClientError::CrossSessionInstance(
            "当前工具包目录已有另一登录会话的后台服务正在运行，请先在对应会话中退出。".into(),
        ));
    }

    if options.require_elevation && !server_token_info.elevated {
        return ProbeOutcome::Fatal(ClientError::PrivilegeMismatch(
            "检测到未提权的残留后台服务进程正在运行，请先安全关闭旧后台服务后再重新打开。".into(),
        ));
    }

    let http = match AppHttpClient::new(record.port, record.token.clone()) {
        Ok(c) => c,
        Err(e) => return ProbeOutcome::Fatal(e),
    };
    let health = match http.health(Duration::from_millis(800)).await {
        Ok(h) => h,
        Err(ClientError::Unauthorized(_)) => {
            if cst_os::ServerInstanceLock::acquire(canonical_root).is_ok() {
                return ProbeOutcome::AbsentOrStale;
            }
            return ProbeOutcome::Fatal(ClientError::InvalidServerIdentity(
                "后台服务访问令牌校验失败，请关闭残留后台进程后重新启动。".into(),
            ));
        }
        Err(_) => return ProbeOutcome::AbsentOrStale,
    };

    if !health.connected
        || health.pid != Some(record.pid)
        || health.process_created != Some(proc_identity.created)
    {
        return ProbeOutcome::Fatal(ClientError::InvalidServerIdentity(
            "后台服务进程身份与健康检查回执不一致，拒绝复用连接。".into(),
        ));
    }

    let expected_norm = match normalize_canonical_path_for_hash(canonical_root) {
        Ok(s) => s,
        Err(e) => return ProbeOutcome::Fatal(ClientError::BootstrapFailed(e)),
    };
    let reported_norm = health
        .kit_root
        .as_deref()
        .and_then(|s| normalize_canonical_path_for_hash(Path::new(s)).ok());
    if reported_norm.as_deref() != Some(expected_norm.as_str()) {
        return ProbeOutcome::Fatal(ClientError::InvalidServerIdentity(
            "后台服务报告的工具包目录与当前程序不一致。".into(),
        ));
    }

    if health.elevated != Some(server_token_info.elevated) {
        return ProbeOutcome::Fatal(ClientError::PrivilegeMismatch(
            "后台服务报告的管理员提权状态不满足桌面端要求，请关闭旧后台后重开。".into(),
        ));
    }

    if health.stopping {
        return ProbeOutcome::Stopping(record.pid);
    }
    let expected_version = fs::read_to_string(canonical_root.join("VERSION"))
        .ok()
        .and_then(|text| text.trim().strip_prefix("cst-pilot ").map(str::to_owned));
    let Some(version) = health.version.filter(|reported| {
        !reported.is_empty()
            && expected_version
                .as_ref()
                .is_none_or(|expected| reported == expected)
    }) else {
        return ProbeOutcome::Fatal(ClientError::InvalidServerIdentity(
            "后台服务版本与工具包 VERSION 不一致，拒绝复用连接。".into(),
        ));
    };

    ProbeOutcome::Verified(VerifiedServerEndpoint {
        kit_root: strip_windows_verbatim_prefix(canonical_root),
        pid: record.pid,
        process_created: proc_identity.created,
        port: record.port,
        version,
        http,
        spawned_by_client: false,
    })
}

/// Applies the strict portable environment whitelist to `app/cst-server.exe` / `app/cst-updater.exe`:
/// - `SystemRoot` / `WINDIR` / `ComSpec` resolved via trusted Win32 APIs (`GetWindowsDirectoryW` / `GetSystemDirectoryW`)
/// - `TEMP` / `TMP` redirected to `<kit_root>/.state/tmp`
/// - `CST_PILOT_ROOT` = `<kit_root>`, `CST_OFFLINE` = `1`
/// - Strips untrusted `PATH`, `PI_*`, `*_API_KEY`, `HTTP_PROXY`, `CST_EXPORT_BROWSER`, `CHROME_PATH`, etc.
pub fn apply_portable_sanitized_env(
    cmd: &mut Command,
    canonical_kit_root: &Path,
) -> Result<(), ClientError> {
    let clean_root = strip_windows_verbatim_prefix(canonical_kit_root);
    let tmp_dir = clean_root.join(".state").join("tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| {
        ClientError::BootstrapFailed(format!(
            "无法创建便携运行临时目录 {}：{e}",
            tmp_dir.display()
        ))
    })?;

    let win_dir = windows_directory().map_err(ClientError::BootstrapFailed)?;
    let sys_dir = system_directory().map_err(ClientError::BootstrapFailed)?;

    let mut path_entries = Vec::with_capacity(6);
    for bundled in [clean_root.join("pwsh"), clean_root.join("bin")] {
        if bundled.is_dir() {
            path_entries.push(bundled);
        }
    }
    path_entries.extend([
        sys_dir.clone(),
        win_dir.clone(),
        sys_dir.join("Wbem"),
        sys_dir.join("WindowsPowerShell").join("v1.0"),
    ]);
    let joined_path = std::env::join_paths(path_entries)
        .map_err(|e| ClientError::BootstrapFailed(format!("组装安全 PATH 失败：{e}")))?;

    cmd.env_clear();
    cmd.env("SystemRoot", &win_dir);
    cmd.env("WINDIR", &win_dir);
    cmd.env("ComSpec", sys_dir.join("cmd.exe"));
    cmd.env("PATH", joined_path);
    cmd.env("PATHEXT", ".COM;.EXE;.BAT;.CMD");
    cmd.env("TEMP", &tmp_dir);
    cmd.env("TMP", &tmp_dir);
    cmd.env("CST_PILOT_ROOT", &clean_root);
    cmd.env("CST_OFFLINE", "1");

    for passthrough in [
        "SYSTEMDRIVE",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "CommonProgramFiles",
        "CommonProgramFiles(x86)",
        "COMPUTERNAME",
        "USERNAME",
        "USERDOMAIN",
        "OS",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ] {
        if let Some(val) = std::env::var_os(passthrough)
            && !val.is_empty()
        {
            cmd.env(passthrough, val);
        }
    }
    Ok(())
}

fn canonicalize_kit_root(kit_root: &Path) -> Result<PathBuf, ClientError> {
    if path_is_reparse_or_symlink(kit_root) {
        return Err(ClientError::BootstrapFailed(
            "工具包根目录禁止使用符号链接或目录联接。".into(),
        ));
    }
    let canonical = kit_root.canonicalize().map_err(|e| {
        ClientError::BootstrapFailed(format!("无法读取工具包根目录 {}：{e}", kit_root.display()))
    })?;
    if path_is_reparse_or_symlink(&canonical) {
        return Err(ClientError::BootstrapFailed(
            "工具包根目录禁止使用符号链接或目录联接。".into(),
        ));
    }
    Ok(canonical)
}

fn path_is_reparse_or_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| is_reparse_or_symlink(&m))
        .unwrap_or(true)
}

fn paths_equal_windows_ci(a: &Path, b: &Path) -> bool {
    let sa = strip_windows_verbatim_prefix(a)
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let sb = strip_windows_verbatim_prefix(b)
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    sa == sb
}

#[cfg(windows)]
fn try_create_first_desktop_pipe(
    address: &str,
) -> Result<std::os::windows::io::OwnedHandle, Option<i32>> {
    use std::os::windows::io::FromRawHandle;
    use windows::{
        Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
            System::Pipes::{
                CreateNamedPipeW, PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE,
            },
        },
        core::PCWSTR,
    };

    let wide: Vec<u16> = address.encode_utf16().chain([0]).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            256,
            256,
            500,
            None,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().raw_os_error());
    }
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) })
}

#[cfg(windows)]
fn run_desktop_wakeup_listener(
    accept_handle: std::os::windows::io::OwnedHandle,
    stop: Arc<AtomicBool>,
    wakeup_tx: watch::Sender<u64>,
) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{ReadFile, WriteFile},
        System::Pipes::{ConnectNamedPipe, DisconnectNamedPipe},
    };

    let raw = HANDLE(accept_handle.as_raw_handle());
    let mut seq = 0u64;

    while !stop.load(Ordering::SeqCst) {
        let connected = match unsafe { ConnectNamedPipe(raw, None) } {
            Ok(()) => true,
            // ERROR_PIPE_CONNECTED (535 -> HRESULT 0x8007_0217)
            Err(e) if e.code().0 as u32 == 0x8007_0217 => true,
            Err(_) => false,
        };
        if !connected {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }

        if stop.load(Ordering::SeqCst) {
            let _ = unsafe { DisconnectNamedPipe(raw) };
            break;
        }

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut buf = [0u8; 9];
        let mut offset = 0;
        while !stop.load(Ordering::SeqCst) && Instant::now() < deadline && offset < buf.len() {
            let mut read = 0;
            match unsafe { ReadFile(raw, Some(&mut buf[offset..]), Some(&mut read), None) } {
                Ok(()) if read > 0 => offset += read as usize,
                Ok(()) => break,
                // ERROR_NO_DATA: the nonblocking pipe has no bytes to read yet.
                Err(e) if e.code().0 as u32 == 0x8007_00E8 => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        if &buf == b"ACTIVATE\n" && !stop.load(Ordering::SeqCst) {
            seq = seq.wrapping_add(1);
            wakeup_tx.send_replace(seq);
            let mut written = 0;
            let _ = unsafe { WriteFile(raw, Some(b"OK\n"), Some(&mut written), None) };
            // Give the client a bounded chance to read the acknowledgement before disconnecting.
            while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                let mut byte = [0];
                let mut read = 0;
                match unsafe { ReadFile(raw, Some(&mut byte), Some(&mut read), None) } {
                    Err(e) if e.code().0 as u32 == 0x8007_00E8 => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => break,
                }
            }
        }
        let _ = unsafe { DisconnectNamedPipe(raw) };
    }
}

#[cfg(windows)]
fn activate_existing_windows_desktop_instance(
    canonical_root: &Path,
    address: &str,
    self_token: &ProcessTokenInfo,
    options: &BootstrapOptions,
) -> Result<DesktopInstanceOutcome, ClientError> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::{
        Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE},
            Storage::FileSystem::{
                CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, OPEN_EXISTING,
            },
            System::Pipes::{GetNamedPipeServerProcessId, PeekNamedPipe, WaitNamedPipeW},
        },
        core::PCWSTR,
    };

    let wide: Vec<u16> = address.encode_utf16().chain([0]).collect();
    let deadline = Instant::now() + Duration::from_secs(3);

    let client_handle: OwnedHandle = loop {
        match unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        } {
            Ok(h) => break unsafe { OwnedHandle::from_raw_handle(h.0) },
            Err(err) => {
                let code = err.code().0 as u32;
                // ERROR_ACCESS_DENIED (0x8007_0005) -> owned by another user / elevation context
                if code == 0x8007_0005 {
                    return Err(ClientError::CrossSessionInstance(
                        "当前工具包目录已有另一权限或登录会话的桌面实例正在运行。".into(),
                    ));
                }
                // ERROR_PIPE_BUSY (0x8007_00E7) or momentary transition -> wait and retry
                if Instant::now() >= deadline {
                    return Err(ClientError::BootstrapFailed(
                        "连接已有桌面实例唤醒通道超时。".into(),
                    ));
                }
                unsafe {
                    let _ = WaitNamedPipeW(PCWSTR(wide.as_ptr()), 150);
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    };

    let mut owner_pid = 0u32;
    unsafe { GetNamedPipeServerProcessId(HANDLE(client_handle.as_raw_handle()), &mut owner_pid) }
        .map_err(|_| ClientError::BootstrapFailed("无法查询已有桌面实例进程标识。".into()))?;

    if ProcessIdentity::capture(owner_pid)
        .map_err(ClientError::BootstrapFailed)?
        .is_none()
    {
        return Err(ClientError::BootstrapFailed(
            "已有桌面实例刚刚退出，请重新启动。".into(),
        ));
    }

    let expected_exe = options.desktop_exe_for(canonical_root);
    let actual_exe = query_process_image_path(owner_pid)
        .map_err(ClientError::InvalidServerIdentity)?
        .ok_or_else(|| ClientError::BootstrapFailed("已有桌面实例已退出，请重新启动。".into()))?;
    if !paths_equal_windows_ci(&actual_exe, &expected_exe) {
        return Err(ClientError::InvalidServerIdentity(
            "持有单实例锁的进程路径与当前工具包不符，拒绝发送唤醒请求。".into(),
        ));
    }

    let owner_token = (options.query_token_fn)(owner_pid)
        .map_err(|_| {
            ClientError::CrossSessionInstance(
                "已有其他用户登录会话的 CST Pilot 实例正在运行。".into(),
            )
        })?
        .ok_or_else(|| ClientError::BootstrapFailed("已有桌面实例已退出。".into()))?;
    if !self_token.same_user_and_session(&owner_token) {
        return Err(ClientError::CrossSessionInstance(
            "已有其他用户或远程会话的 CST Pilot 实例正在运行，请勿重复打开。".into(),
        ));
    }

    let mut file = fs::File::from(client_handle);
    file.write_all(b"ACTIVATE\n")
        .and_then(|_| file.flush())
        .map_err(|e| ClientError::BootstrapFailed(format!("发送窗口置前唤醒指令失败：{e}")))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                HANDLE(file.as_raw_handle()),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
        }
        .is_err()
        {
            break;
        }
        if available >= 3 {
            let mut reply = [0; 3];
            if file.read_exact(&mut reply).is_ok() && &reply == b"OK\n" {
                return Ok(DesktopInstanceOutcome::ActivatedExisting { owner_pid });
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(ClientError::BootstrapFailed(
        "已有桌面实例未确认唤醒请求。".into(),
    ))
}

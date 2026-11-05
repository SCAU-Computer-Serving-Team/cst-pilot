use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;
use tokio_util::sync::CancellationToken;

pub const CONFIG_COMMAND_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigValueSource {
    #[default]
    Global,
    Project,
    Extension {
        from_project: bool,
    },
}

impl ConfigValueSource {
    pub fn from_source_info(source: &pi_session::SourceInfo) -> Self {
        Self::Extension {
            from_project: source.scope == "project"
                || source.source == "project"
                || source.origin == "project",
        }
    }

    pub fn is_project_scoped(self) -> bool {
        match self {
            ConfigValueSource::Global => false,
            ConfigValueSource::Project => true,
            ConfigValueSource::Extension { from_project } => from_project,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigValueReference {
    Command(String),
    Template(Vec<TemplatePart>),
}

fn env_var_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("env var regex"))
}

fn env_var_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*").expect("env var prefix regex"))
}

fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(prev)) = parts.last_mut() {
        prev.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_owned()));
}

fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    let mut parts = Vec::new();
    let mut index = 0usize;

    while index < config.len() {
        let Some(rel_dollar) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar_index = index + rel_dollar;
        append_literal(&mut parts, &config[index..dollar_index]);

        let rest = &config[dollar_index + 1..];
        if let Some(escaped) = rest.chars().next()
            && (escaped == '$' || escaped == '!')
        {
            let mut buf = [0u8; 4];
            append_literal(&mut parts, escaped.encode_utf8(&mut buf));
            index = dollar_index + 1 + escaped.len_utf8();
            continue;
        }

        if let Some(after_brace) = rest.strip_prefix('{') {
            if let Some(rel_end) = after_brace.find('}') {
                let name = &after_brace[..rel_end];
                if env_var_name_re().is_match(name) {
                    parts.push(TemplatePart::Env(name.to_owned()));
                } else {
                    let end_index = dollar_index + 2 + rel_end + 1;
                    append_literal(&mut parts, &config[dollar_index..end_index]);
                }
                index = dollar_index + 2 + rel_end + 1;
                continue;
            }
            append_literal(&mut parts, "$");
            index = dollar_index + 1;
            continue;
        }

        if let Some(m) = env_var_prefix_re().find(rest) {
            parts.push(TemplatePart::Env(m.as_str().to_owned()));
            index = dollar_index + 1 + m.end();
            continue;
        }

        append_literal(&mut parts, "$");
        index = dollar_index + 1;
    }

    parts
}

fn parse_config_value_reference(config: &str) -> ConfigValueReference {
    if config.starts_with('!') {
        ConfigValueReference::Command(config.to_owned())
    } else {
        ConfigValueReference::Template(parse_config_value_template(config))
    }
}

fn resolve_env_config_value(name: &str, env: Option<&BTreeMap<String, String>>) -> Option<String> {
    if let Some(map) = env
        && let Some(val) = map.get(name)
        && !val.is_empty()
    {
        return Some(val.clone());
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn get_template_env_var_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names = Vec::new();
    for part in parts {
        if let TemplatePart::Env(name) = part
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    names
}

fn resolve_template(
    parts: &[TemplatePart],
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(lit) => resolved.push_str(lit),
            TemplatePart::Env(name) => {
                let val = resolve_env_config_value(name, env)?;
                resolved.push_str(&val);
            }
        }
    }
    Some(resolved)
}

pub fn get_config_value_env_var_name(config: &str) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) if parts.len() == 1 => match &parts[0] {
            TemplatePart::Env(name) => Some(name.clone()),
            TemplatePart::Literal(_) => None,
        },
        _ => None,
    }
}

pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) => get_template_env_var_names(&parts),
        ConfigValueReference::Command(_) => Vec::new(),
    }
}

pub fn get_missing_config_value_env_var_names(
    config: &str,
    env: Option<&BTreeMap<String, String>>,
) -> Vec<String> {
    get_config_value_env_var_names(config)
        .into_iter()
        .filter(|name| resolve_env_config_value(name, env).is_none())
        .collect()
}

pub fn is_command_config_value(config: &str) -> bool {
    matches!(
        parse_config_value_reference(config),
        ConfigValueReference::Command(_)
    )
}

/// 纯语法与环境变量存在性检查（只读安全：绝不执行 `!command`）。
pub fn is_config_value_configured(config: &str, env: Option<&BTreeMap<String, String>>) -> bool {
    !config.is_empty() && get_missing_config_value_env_var_names(config, env).is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandExecFailure {
    UntrustedProject,
    TimedOut,
    Cancelled,
    SpawnFailed,
    NonZeroExit(i32),
    EmptyOutput,
    OutputTooLarge,
}

impl CommandExecFailure {
    pub fn format_redacted(&self, description: &str) -> String {
        match self {
            CommandExecFailure::UntrustedProject => {
                format!(
                    "Refused to execute command config for {description} from untrusted project"
                )
            }
            CommandExecFailure::TimedOut => {
                format!("Failed to resolve {description} from shell command (timed out after 10s)")
            }
            CommandExecFailure::Cancelled => {
                format!("Failed to resolve {description} from shell command (cancelled)")
            }
            CommandExecFailure::SpawnFailed => {
                format!("Failed to resolve {description} from shell command (spawn failed)")
            }
            CommandExecFailure::NonZeroExit(code) => {
                format!("Failed to resolve {description} from shell command (exit code: {code})")
            }
            CommandExecFailure::EmptyOutput => {
                format!("Failed to resolve {description} from shell command (empty output)")
            }
            CommandExecFailure::OutputTooLarge => {
                format!("Failed to resolve {description} from shell command (output too large)")
            }
        }
    }
}

#[cfg(windows)]
pub(crate) mod win_job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    type Handle = *mut std::ffi::c_void;
    type Bool = i32;
    type Dword = u32;

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: Dword = 0x0000_2000;
    const TH32CS_SNAPTHREAD: Dword = 0x0000_0004;
    const THREAD_SUSPEND_RESUME: Dword = 0x0002;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    pub const CREATE_SUSPENDED: u32 = 0x0000_0004;

    #[repr(C)]
    struct ThreadEntry32 {
        dw_size: Dword,
        cnt_usage: Dword,
        th32_thread_id: Dword,
        th32_owner_process_id: Dword,
        tp_base_pri: i32,
        tp_delta_pri: i32,
        dw_flags: Dword,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: Dword,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: Dword,
        affinity: usize,
        priority_class: Dword,
        scheduling_class: Dword,
    }

    #[repr(C)]
    #[derive(Default)]
    struct JobObjectExtendedLimitInformation {
        basic_limit_information: JobObjectBasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    unsafe extern "system" {
        fn CreateJobObjectW(security_attributes: *mut std::ffi::c_void, name: *const u16)
        -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            info_class: i32,
            info: *const std::ffi::c_void,
            info_len: Dword,
        ) -> Bool;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> Bool;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> Bool;
        fn CloseHandle(handle: Handle) -> Bool;
        fn CreateToolhelp32Snapshot(dw_flags: Dword, th32_process_id: Dword) -> Handle;
        fn Thread32First(snapshot: Handle, lpte: *mut ThreadEntry32) -> Bool;
        fn Thread32Next(snapshot: Handle, lpte: *mut ThreadEntry32) -> Bool;
        fn OpenThread(desired_access: Dword, inherit_handle: Bool, thread_id: Dword) -> Handle;
        fn ResumeThread(thread: Handle) -> Dword;
    }

    pub fn resume_process_threads(pid: u32) -> bool {
        // SAFETY: Standard Win32 Toolhelp32 snapshot iteration to resume threads of `pid` created with CREATE_SUSPENDED.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap.is_null() || snap == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut resumed = false;
            let mut success = true;
            let mut entry = ThreadEntry32 {
                dw_size: std::mem::size_of::<ThreadEntry32>() as Dword,
                cnt_usage: 0,
                th32_thread_id: 0,
                th32_owner_process_id: 0,
                tp_base_pri: 0,
                tp_delta_pri: 0,
                dw_flags: 0,
            };
            if Thread32First(snap, &raw mut entry) != 0 {
                loop {
                    if entry.th32_owner_process_id == pid {
                        let th = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32_thread_id);
                        if th.is_null() {
                            success = false;
                        } else {
                            let previous = ResumeThread(th);
                            success &= previous != u32::MAX;
                            resumed |= previous != u32::MAX;
                            CloseHandle(th);
                        }
                    }
                    if Thread32Next(snap, &raw mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
            resumed && success
        }
    }

    fn finish_containment(
        child: &mut Child,
        assign: impl FnOnce(&Child) -> bool,
        resume: impl FnOnce(u32) -> bool,
    ) -> bool {
        if assign(child) && resume(child.id()) {
            return true;
        }
        let _ = child.kill();
        let _ = child.wait();
        false
    }

    pub struct WinJobGuard {
        handle: Handle,
    }

    unsafe impl Send for WinJobGuard {}
    unsafe impl Sync for WinJobGuard {}

    impl WinJobGuard {
        pub fn create() -> Option<Self> {
            // SAFETY: Win32 CreateJobObjectW + SetInformationJobObject with valid POD struct.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut info = JobObjectExtendedLimitInformation::default();
                info.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&raw const info).cast(),
                    std::mem::size_of::<JobObjectExtendedLimitInformation>() as Dword,
                );
                if ok == 0 {
                    CloseHandle(handle);
                    return None;
                }
                Some(Self { handle })
            }
        }

        pub fn assign(&self, child: &Child) -> bool {
            // SAFETY: child.as_raw_handle() is a valid process handle.
            unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle() as Handle) != 0 }
        }

        pub fn assign_and_resume(&self, child: &mut Child) -> bool {
            finish_containment(child, |child| self.assign(child), resume_process_threads)
        }

        pub fn terminate(&self) {
            // SAFETY: self.handle is a valid job object handle.
            unsafe {
                TerminateJobObject(self.handle, 1);
            }
        }
    }

    impl Drop for WinJobGuard {
        fn drop(&mut self) {
            // SAFETY: Closing the job object with KILL_ON_JOB_CLOSE terminates all remaining processes.
            unsafe {
                TerminateJobObject(self.handle, 1);
                CloseHandle(self.handle);
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn assignment_and_resume_failures_reap_suspended_child_without_running_it() {
            use std::os::windows::process::CommandExt;
            use std::process::{Command, Stdio};
            let dir = std::path::PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development")
                .join(format!("containment-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            for fail_assignment in [true, false] {
                let marker = dir.join(format!("{fail_assignment}.txt"));
                let mut child = Command::new("cmd.exe")
                    .args(["/d", "/s", "/c"])
                    .raw_arg(format!("\"echo forbidden > \"{}\"\"", marker.display()))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
                    .spawn()
                    .unwrap();
                let mut resume_called = false;
                assert!(!finish_containment(
                    &mut child,
                    |_| !fail_assignment,
                    |_| {
                        resume_called = true;
                        false
                    }
                ));
                assert_eq!(resume_called, !fail_assignment);
                assert!(child.try_wait().unwrap().is_some());
                assert!(!marker.exists());
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

pub fn kill_process_tree_sync(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(unix)]
    {
        // SAFETY: Sending SIGKILL to the child process group (-pid) and process (pid).
        unsafe {
            let _ = libc::kill(-(pid as i32), libc::SIGKILL);
            let _ = libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

fn execute_shell_command_sync(
    command: &str,
    cwd: Option<&Path>,
    env: Option<&BTreeMap<String, String>>,
    timeout_ms: u64,
    cancel: Option<&CancellationToken>,
    scope_cancel: &CancellationToken,
) -> Result<String, CommandExecFailure> {
    if scope_cancel.is_cancelled() || cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(CommandExecFailure::Cancelled);
    }

    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = Command::new("cmd.exe");
        // cmd.exe parses its own quoting rules rather than the C runtime argv format.
        c.args(["/d", "/s", "/c"]);
        c.raw_arg(format!("\"{command}\""));
        c.creation_flags(CREATE_NO_WINDOW);
        c
    };

    #[cfg(not(windows))]
    let mut cmd = {
        use std::os::unix::process::CommandExt;
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c.process_group(0);
        c
    };

    if let Some(dir) = cwd
        && dir.exists()
    {
        cmd.current_dir(dir);
    }
    if let Some(env_map) = env {
        cmd.envs(env_map);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    let job = win_job::WinJobGuard::create().ok_or(CommandExecFailure::SpawnFailed)?;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(win_job::CREATE_NO_WINDOW | win_job::CREATE_SUSPENDED);
    }

    let mut child = cmd.spawn().map_err(|_| CommandExecFailure::SpawnFailed)?;
    let pid = child.id();

    #[cfg(windows)]
    if !job.assign_and_resume(&mut child) {
        let _ = child.kill();
        job.terminate();
        let _ = child.wait();
        return Err(CommandExecFailure::SpawnFailed);
    }

    let stdout_pipe = child.stdout.take();
    let stop_reader = Arc::new(AtomicBool::new(false));
    let reader_stop = stop_reader.clone();
    let reader_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut too_large = false;
        if let Some(out) = stdout_pipe {
            crate::tools::process_io::read_pipe(out, reader_stop, |data| {
                let remaining = (256usize * 1024).saturating_sub(buf.len());
                too_large |= data.len() > remaining;
                buf.extend_from_slice(&data[..data.len().min(remaining)]);
            });
        }
        (buf, too_large)
    });

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let exit_status = loop {
        if scope_cancel.is_cancelled() || cancel.is_some_and(CancellationToken::is_cancelled) {
            #[cfg(windows)]
            job.terminate();
            kill_process_tree_sync(pid);
            let _ = child.wait();
            let _ =
                crate::tools::process_io::finish_pipe_readers(&stop_reader, vec![reader_handle]);
            return Err(CommandExecFailure::Cancelled);
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    #[cfg(windows)]
                    job.terminate();
                    kill_process_tree_sync(pid);
                    let _ = child.wait();
                    let _ = crate::tools::process_io::finish_pipe_readers(
                        &stop_reader,
                        vec![reader_handle],
                    );
                    return Err(CommandExecFailure::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                #[cfg(windows)]
                job.terminate();
                kill_process_tree_sync(pid);
                let _ = child.kill();
                let _ = child.wait();
                let _ = crate::tools::process_io::finish_pipe_readers(
                    &stop_reader,
                    vec![reader_handle],
                );
                return Err(CommandExecFailure::SpawnFailed);
            }
        }
    };

    // Shell 已退出，终止 Job / 进程组中可能残留的后代进程，使 stdout 管道立即闭合
    #[cfg(windows)]
    job.terminate();
    #[cfg(unix)]
    kill_process_tree_sync(pid);

    let (out_bytes, too_large) =
        crate::tools::process_io::finish_pipe_readers(&stop_reader, vec![reader_handle])
            .pop()
            .and_then(Result::ok)
            .unwrap_or_default();
    if too_large {
        return Err(CommandExecFailure::OutputTooLarge);
    }
    if !exit_status.success() {
        return Err(CommandExecFailure::NonZeroExit(
            exit_status.code().unwrap_or(1),
        ));
    }

    let text = String::from_utf8_lossy(&out_bytes).trim().to_owned();
    if text.is_empty() {
        return Err(CommandExecFailure::EmptyOutput);
    }
    Ok(text)
}

fn compute_env_fingerprint(env: Option<&BTreeMap<String, String>>) -> String {
    use sha2::{Digest, Sha256};
    let mut effective: BTreeMap<_, _> = std::env::vars_os().collect();
    if let Some(env) = env {
        for (key, value) in env {
            effective.insert(key.into(), value.into());
        }
    }
    let mut hash = Sha256::new();
    for (key, value) in effective {
        for component in [key, value] {
            let bytes = component.as_encoded_bytes();
            hash.update(bytes.len().to_le_bytes());
            hash.update(bytes);
        }
    }
    format!("{:x}", hash.finalize())
}

/// 绑定到运行时配置身份（`agent_dir` + `cwd` + `project_trusted`）的配置值解析器。
/// 命令结果缓存按实例隔离，在 `reload`、`logout`、`set_runtime_api_key` 或 `dispose` 时清理。
type CommandCache = BTreeMap<(String, String), Option<String>>;

#[derive(Clone)]
pub struct ConfigValueResolver {
    cwd: PathBuf,
    agent_dir: PathBuf,
    project_trusted: Arc<AtomicBool>,
    command_cache: Arc<Mutex<CommandCache>>,
    cache_generation: Arc<AtomicU64>,
    command_cancel: Arc<Mutex<CancellationToken>>,
}

impl std::fmt::Debug for ConfigValueResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigValueResolver")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .field(
                "project_trusted",
                &self.project_trusted.load(Ordering::SeqCst),
            )
            .finish()
    }
}

impl Default for ConfigValueResolver {
    fn default() -> Self {
        Self::new(PathBuf::from("."), PathBuf::from("."), true)
    }
}

impl ConfigValueResolver {
    pub fn new(
        cwd: impl Into<PathBuf>,
        agent_dir: impl Into<PathBuf>,
        project_trusted: bool,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            project_trusted: Arc::new(AtomicBool::new(project_trusted)),
            command_cache: Arc::new(Mutex::new(BTreeMap::new())),
            cache_generation: Arc::new(AtomicU64::new(0)),
            command_cancel: Arc::new(Mutex::new(CancellationToken::new())),
        }
    }

    pub fn is_project_trusted(&self) -> bool {
        self.project_trusted.load(Ordering::SeqCst)
    }

    pub fn set_project_trusted(&self, trusted: bool) {
        let prev = self.project_trusted.swap(trusted, Ordering::SeqCst);
        if prev != trusted {
            self.clear_cache();
        }
    }

    pub fn clear_cache(&self) {
        let mut cache = self.command_cache.lock().expect("cache lock");
        self.cache_generation.fetch_add(1, Ordering::SeqCst);
        let mut cancel = self
            .command_cancel
            .lock()
            .expect("command cancellation lock");
        cancel.cancel();
        *cancel = CancellationToken::new();
        cache.clear();
    }

    /// Commands execute outside the async executor; dropping the waiter cancels its process tree.
    pub async fn resolve_async(
        &self,
        config: String,
        env: Option<BTreeMap<String, String>>,
        source: ConfigValueSource,
    ) -> Result<Option<String>, String> {
        if !is_command_config_value(&config) {
            return Ok(self.resolve(&config, env.as_ref(), source, None));
        }
        let resolver = self.clone();
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            resolver
                .resolve(&config, env.as_ref(), source, Some(&cancel))
                .map(Some)
                .ok_or_else(|| "Failed to resolve credential from shell command".to_owned())
        })
        .await
        .map_err(|_| "Credential command task failed".to_owned())?
    }

    pub fn resolve(
        &self,
        config: &str,
        env: Option<&BTreeMap<String, String>>,
        source: ConfigValueSource,
        cancel: Option<&CancellationToken>,
    ) -> Option<String> {
        match parse_config_value_reference(config) {
            ConfigValueReference::Command(cmd_cfg) => self
                .resolve_command_cached(&cmd_cfg, env, source, cancel)
                .ok(),
            ConfigValueReference::Template(parts) => resolve_template(&parts, env),
        }
    }

    fn resolve_command_cached(
        &self,
        command: &str,
        env: Option<&BTreeMap<String, String>>,
        source: ConfigValueSource,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, CommandExecFailure> {
        if source.is_project_scoped() && !self.is_project_trusted() {
            return Err(CommandExecFailure::UntrustedProject);
        }
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(CommandExecFailure::Cancelled);
        }
        let key = (compute_env_fingerprint(env), command.to_owned());
        let (generation, scope_cancel) = {
            let cache = self.command_cache.lock().expect("cache lock");
            if source.is_project_scoped() && !self.is_project_trusted() {
                return Err(CommandExecFailure::UntrustedProject);
            }
            if let Some(Some(value)) = cache.get(&key) {
                return Ok(value.clone());
            }
            (
                self.cache_generation.load(Ordering::SeqCst),
                self.command_cancel
                    .lock()
                    .expect("command cancellation lock")
                    .clone(),
            )
        };
        let result = execute_shell_command_sync(
            &command[1..],
            Some(&self.cwd),
            env,
            CONFIG_COMMAND_TIMEOUT_MS,
            cancel,
            &scope_cancel,
        )?;
        let mut cache = self.command_cache.lock().expect("cache lock");
        if self.cache_generation.load(Ordering::SeqCst) != generation
            || scope_cancel.is_cancelled()
            || cancel.is_some_and(CancellationToken::is_cancelled)
        {
            return Err(CommandExecFailure::Cancelled);
        }
        cache.insert(key, Some(result.clone()));
        Ok(result)
    }

    pub async fn resolve_or_throw_async(
        &self,
        config: String,
        description: &'static str,
        env: Option<BTreeMap<String, String>>,
        source: ConfigValueSource,
    ) -> Result<String, String> {
        if !is_command_config_value(&config) {
            return self.resolve_or_throw(&config, description, env.as_ref(), source, None);
        }
        let resolver = self.clone();
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            resolver.resolve_or_throw(&config, description, env.as_ref(), source, Some(&cancel))
        })
        .await
        .map_err(|_| "Configuration command task failed".to_owned())?
    }

    pub fn resolve_uncached(
        &self,
        config: &str,
        env: Option<&BTreeMap<String, String>>,
        source: ConfigValueSource,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, CommandExecFailure> {
        match parse_config_value_reference(config) {
            ConfigValueReference::Command(cmd_cfg) => {
                if source.is_project_scoped() && !self.is_project_trusted() {
                    return Err(CommandExecFailure::UntrustedProject);
                }
                let scope_cancel = self
                    .command_cancel
                    .lock()
                    .expect("command cancellation lock")
                    .clone();
                if source.is_project_scoped() && !self.is_project_trusted() {
                    return Err(CommandExecFailure::UntrustedProject);
                }
                execute_shell_command_sync(
                    &cmd_cfg[1..],
                    Some(&self.cwd),
                    env,
                    CONFIG_COMMAND_TIMEOUT_MS,
                    cancel,
                    &scope_cancel,
                )
            }
            ConfigValueReference::Template(parts) => {
                resolve_template(&parts, env).ok_or(CommandExecFailure::EmptyOutput)
            }
        }
    }

    pub fn resolve_or_throw(
        &self,
        config: &str,
        description: &str,
        env: Option<&BTreeMap<String, String>>,
        source: ConfigValueSource,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, String> {
        match parse_config_value_reference(config) {
            ConfigValueReference::Command(cmd) => self
                .resolve_command_cached(&cmd, env, source, cancel)
                .map_err(|failure| failure.format_redacted(description)),
            ConfigValueReference::Template(parts) => {
                if let Some(val) = resolve_template(&parts, env) {
                    return Ok(val);
                }
                let missing = get_missing_config_value_env_var_names(config, env);
                if missing.len() == 1 {
                    Err(format!(
                        "Failed to resolve {description} from environment variable: {}",
                        missing[0]
                    ))
                } else if missing.len() > 1 {
                    Err(format!(
                        "Failed to resolve {description} from environment variables: {}",
                        missing.join(", ")
                    ))
                } else {
                    Err(format!("Failed to resolve {description}"))
                }
            }
        }
    }

    pub fn resolve_headers_or_throw(
        &self,
        headers: Option<&BTreeMap<String, String>>,
        env: Option<&BTreeMap<String, String>>,
        source: ConfigValueSource,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<BTreeMap<String, String>>, String> {
        let Some(headers) = headers else {
            return Ok(None);
        };
        let mut resolved = BTreeMap::new();
        for (key, value) in headers {
            let val =
                self.resolve_or_throw(value, &format!("header \"{key}\""), env, source, cancel)?;
            resolved.insert(key.clone(), val);
        }
        Ok(Some(resolved))
    }
}

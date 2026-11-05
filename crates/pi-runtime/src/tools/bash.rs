use crate::tools::process_io::{finish_pipe_readers, read_pipe};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pi_agent::{AgentTool, AgentToolResult, BoxFuture, ToolReplayPolicy};
use pi_ai::{TextContent, UserContentBlock};
use pi_session::{
    BashExecutor, BashRequest, BashResult, ExtensionToolContext, NoOpUiContext, ToolDefinition,
    ToolExposure,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config_value::kill_process_tree_sync;
#[cfg(windows)]
use crate::config_value::win_job;
use crate::tools::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, OutputAccumulator, OutputAccumulatorOptions,
    OutputSnapshot, STRUCTURED_OUTPUT_MAX_BYTES, TruncatedBy, TruncationResult, format_size,
};

pub const MAX_TIMEOUT_MS: u64 = 2_147_483_647;
pub const MAX_TIMEOUT_SECONDS: f64 = 2_147_483.647;
pub const BASH_UPDATE_THROTTLE_MS: u64 = 100;

pub fn resolve_timeout_ms(timeout_secs: Option<f64>) -> Result<Option<u64>, String> {
    let Some(secs) = timeout_secs else {
        return Ok(None);
    };
    if !secs.is_finite() || secs <= 0.0 {
        return Err("Invalid timeout: must be a finite number of seconds".to_owned());
    }
    let ms_f = secs * 1000.0;
    if ms_f > MAX_TIMEOUT_MS as f64 {
        return Err(format!(
            "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
        ));
    }
    Ok(Some((ms_f.round() as u64).max(1)))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BashToolInput {
    /// Shell command to execute
    pub command: String,
    /// Timeout in seconds (optional, no default timeout)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BashToolOutput {
    pub output: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub exit_code: i32,
    pub wall_time_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BashSpawnContext {
    pub command: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

pub type ShellDataCallback = Arc<dyn Fn(&[u8]) + Send + Sync>;

pub struct BashExecOptions {
    pub on_data: ShellDataCallback,
    pub cancel: CancellationToken,
    pub timeout_secs: Option<f64>,
    pub env: BTreeMap<String, String>,
}

pub trait BashOperations: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<'a, Result<Option<i32>, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    pub shell: String,
    pub args: Vec<String>,
}

pub fn resolve_bash_shell_config(explicit_shell_path: Option<&str>) -> ShellConfig {
    if let Some(custom) = explicit_shell_path
        && !custom.trim().is_empty()
    {
        return ShellConfig {
            shell: custom.trim().to_owned(),
            args: vec!["-c".to_owned()],
        };
    }

    #[cfg(windows)]
    {
        // Prefer Git Bash on Windows if available, otherwise fall back to cmd.exe
        let candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
        ];
        for cand in candidates {
            if Path::new(cand).is_file() {
                return ShellConfig {
                    shell: cand.to_owned(),
                    args: vec!["-c".to_owned()],
                };
            }
        }
        ShellConfig {
            shell: "cmd.exe".to_owned(),
            args: vec!["/d".to_owned(), "/s".to_owned(), "/c".to_owned()],
        }
    }

    #[cfg(not(windows))]
    {
        let sh = if Path::new("/bin/bash").is_file() {
            "/bin/bash"
        } else {
            "sh"
        };
        ShellConfig {
            shell: sh.to_owned(),
            args: vec!["-c".to_owned()],
        }
    }
}

pub(crate) struct LocalShellOperations {
    pub(crate) shell_name: String,
    pub(crate) resolve_shell: Arc<dyn Fn() -> Result<ShellConfig, String> + Send + Sync>,
}

impl BashOperations for LocalShellOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        mut options: BashExecOptions,
    ) -> BoxFuture<'a, Result<Option<i32>, String>> {
        let shell_name = self.shell_name.clone();
        let resolve_shell = self.resolve_shell.clone();
        let command_str = command.to_owned();
        let cwd_str = cwd.to_owned();

        Box::pin(async move {
            let timeout_ms = resolve_timeout_ms(options.timeout_secs)?;
            let child_cancel = options.cancel.child_token();
            let _cancel_on_drop = child_cancel.clone().drop_guard();
            options.cancel = child_cancel;
            if options.cancel.is_cancelled() {
                return Err("aborted".to_owned());
            }
            let cwd_path = Path::new(&cwd_str);
            if !cwd_path.exists() {
                return Err(format!(
                    "Working directory does not exist: {cwd_str}\nCannot execute {shell_name} commands."
                ));
            }

            tokio::task::spawn_blocking(move || -> Result<Option<i32>, String> {
                if options.cancel.is_cancelled() {
                    return Err("aborted".to_owned());
                }

                let shell_cfg = resolve_shell()?;
                let mut cmd = Command::new(&shell_cfg.shell);
                for arg in &shell_cfg.args {
                    cmd.arg(arg);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    if Path::new(&shell_cfg.shell)
                        .file_name()
                        .is_some_and(|n| n.eq_ignore_ascii_case("cmd.exe"))
                    {
                        cmd.raw_arg(format!("\"{command_str}\""));
                    } else {
                        cmd.arg(&command_str);
                    }
                }
                #[cfg(not(windows))]
                cmd.arg(&command_str);
                cmd.current_dir(&cwd_str);
                cmd.env_clear();
                for (k, v) in &options.env {
                    cmd.env(k, v);
                }
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());

                #[cfg(windows)]
                let job = Some(
                    win_job::WinJobGuard::create()
                        .ok_or_else(|| "Failed to create shell process containment".to_owned())?,
                );

                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(win_job::CREATE_NO_WINDOW | win_job::CREATE_SUSPENDED);
                }

                #[cfg(not(windows))]
                {
                    use std::os::unix::process::CommandExt;
                    cmd.process_group(0);
                }

                let mut child = cmd
                    .spawn()
                    .map_err(|e| format!("Failed to spawn {}: {e}", shell_cfg.shell))?;
                let pid = child.id();

                // Windows 零竞态绑定 Job Object：子进程以 CREATE_SUSPENDED 挂起创建，加入 Job 后再恢复主线程
                #[cfg(windows)]
                if let Some(ref j) = job
                    && !j.assign_and_resume(&mut child)
                {
                    let _ = child.kill();
                    j.terminate();
                    let _ = child.wait();
                    return Err("Failed to contain or resume shell process".to_owned());
                }

                let stdout_pipe = child.stdout.take();
                let stderr_pipe = child.stderr.take();

                let stop_readers = Arc::new(AtomicBool::new(false));
                let callback_failed = Arc::new(AtomicBool::new(false));
                let stop_out = stop_readers.clone();
                let on_data_out = options.on_data.clone();
                let failed_out = callback_failed.clone();
                let out_handle = std::thread::spawn(move || {
                    if let Some(out) = stdout_pipe {
                        read_pipe(out, stop_out.clone(), |data| {
                            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                on_data_out(data)
                            }))
                            .is_err()
                            {
                                failed_out.store(true, Ordering::Release);
                                stop_out.store(true, Ordering::Release);
                            }
                        });
                    }
                });
                let stop_err = stop_readers.clone();
                let on_data_err = options.on_data.clone();
                let failed_err = callback_failed.clone();
                let err_handle = std::thread::spawn(move || {
                    if let Some(err) = stderr_pipe {
                        read_pipe(err, stop_err.clone(), |data| {
                            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                on_data_err(data)
                            }))
                            .is_err()
                            {
                                failed_err.store(true, Ordering::Release);
                                stop_err.store(true, Ordering::Release);
                            }
                        });
                    }
                });

                let start_inst = Instant::now();
                let exit_status = loop {
                    if options.cancel.is_cancelled() || callback_failed.load(Ordering::Acquire) {
                        #[cfg(windows)]
                        if let Some(ref j) = job {
                            j.terminate();
                        }
                        kill_process_tree_sync(pid);
                        let _ = child.wait();
                        let _ = finish_pipe_readers(&stop_readers, vec![out_handle, err_handle]);
                        return Err(if callback_failed.load(Ordering::Acquire) {
                            "Shell output callback panicked".to_owned()
                        } else {
                            "aborted".to_owned()
                        });
                    }

                    if let Some(limit_ms) = timeout_ms
                        && start_inst.elapsed() >= Duration::from_millis(limit_ms)
                    {
                        #[cfg(windows)]
                        if let Some(ref j) = job {
                            j.terminate();
                        }
                        kill_process_tree_sync(pid);
                        let _ = child.wait();
                        let _ = finish_pipe_readers(&stop_readers, vec![out_handle, err_handle]);
                        let secs_str = options
                            .timeout_secs
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| (limit_ms / 1000).to_string());
                        return Err(format!("timeout:{secs_str}"));
                    }

                    match child.try_wait() {
                        Ok(Some(status)) => break status,
                        Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                        Err(err) => {
                            #[cfg(windows)]
                            if let Some(ref j) = job {
                                j.terminate();
                            }
                            kill_process_tree_sync(pid);
                            let _ = child.kill();
                            let _ = child.wait();
                            let _ =
                                finish_pipe_readers(&stop_readers, vec![out_handle, err_handle]);
                            return Err(format!("Failed waiting for child process: {err}"));
                        }
                    }
                };

                // 主 Shell 退出后，立即终止受控 Job / 进程组内尚存的所有后代进程，使被孙进程继承的管道写端立刻关闭
                #[cfg(windows)]
                if let Some(ref j) = job {
                    j.terminate();
                }
                #[cfg(unix)]
                kill_process_tree_sync(pid);

                if finish_pipe_readers(&stop_readers, vec![out_handle, err_handle])
                    .iter()
                    .any(Result::is_err)
                {
                    return Err("Shell output reader failed".to_owned());
                }
                if callback_failed.load(Ordering::Acquire) {
                    return Err("Shell output callback panicked".to_owned());
                }

                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(code) = exit_status.code() {
                        return Ok(Some(code));
                    }
                    if let Some(sig) = exit_status.signal() {
                        return Ok(Some(128 + sig));
                    }
                    Ok(Some(1))
                }
                #[cfg(not(unix))]
                {
                    Ok(Some(exit_status.code().unwrap_or(1)))
                }
            })
            .await
            .map_err(|e| format!("Shell execution task panicked: {e}"))?
        })
    }
}

pub fn create_local_bash_operations(shell_path: Option<String>) -> Arc<dyn BashOperations> {
    Arc::new(LocalShellOperations {
        shell_name: "bash".to_owned(),
        resolve_shell: Arc::new(move || Ok(resolve_bash_shell_config(shell_path.as_deref()))),
    })
}

/// 构造供 `AgentSession::execute_bash`（含用户 `!cmd` 与 RPC `"bash"` 命令）共享复用的有界流式 `BashExecutor`。
pub fn create_local_bash_executor(shell_path: Option<String>) -> BashExecutor {
    create_bash_executor(BashToolOptions {
        shell_path,
        ..Default::default()
    })
}

/// Builds the session executor with the same operations and spawn policy as the bash tool.
pub fn create_bash_executor(options: BashToolOptions) -> BashExecutor {
    let ops = options
        .operations
        .clone()
        .unwrap_or_else(|| create_local_bash_operations(options.shell_path.clone()));
    Arc::new(move |req: BashRequest| {
        let (ops, options) = (ops.clone(), options.clone());
        Box::pin(async move {
            let acc = Arc::new(Mutex::new(OutputAccumulator::new(
                OutputAccumulatorOptions {
                    temp_file_prefix: Some("pi-user-bash".to_owned()),
                    temp_dir: options.temp_dir.clone(),
                    ..OutputAccumulatorOptions::default()
                },
            )));
            let acc_cb = acc.clone();
            let on_chunk = req.on_chunk.clone();
            let on_data: ShellDataCallback = Arc::new(move |data: &[u8]| {
                acc_cb.lock().expect("acc lock").append(data);
                if let Some(ref cb) = on_chunk {
                    cb(String::from_utf8_lossy(data).into_owned());
                }
            });

            let mut env: BTreeMap<String, String> = std::env::vars().collect();
            env.remove("PI_SESSION_ID");
            env.remove("PI_SESSION_FILE");
            env.remove("PI_PROVIDER");
            env.remove("PI_MODEL");
            env.remove("PI_REASONING_LEVEL");

            if options.expose_session_environment.unwrap_or(true) {
                env.extend(req.env);
            }
            let command = match options.command_prefix {
                Some(prefix) if !prefix.is_empty() => format!("{prefix}\n{}", req.command),
                _ => req.command,
            };
            let spawn = BashSpawnContext {
                command,
                cwd: req.cwd,
                env,
            };
            let spawn = options
                .spawn_hook
                .as_ref()
                .map_or_else(|| spawn.clone(), |hook| hook(spawn.clone()));
            let exec_res = ops
                .exec(
                    &spawn.command,
                    &spawn.cwd,
                    BashExecOptions {
                        on_data,
                        cancel: req.cancel_token,
                        timeout_secs: None,
                        env: spawn.env,
                    },
                )
                .await;

            let snapshot = {
                let mut guard = acc.lock().expect("acc lock");
                guard.finish();
                guard.snapshot(true)
            };

            match exec_res {
                Ok(exit_code) => Ok(BashResult {
                    output: snapshot.content,
                    exit_code,
                    cancelled: false,
                    truncated: snapshot.truncation.truncated,
                    full_output_path: snapshot.full_output_path,
                }),
                Err(err) if err == "aborted" => Ok(BashResult {
                    output: snapshot.content,
                    exit_code: None,
                    cancelled: true,
                    truncated: snapshot.truncation.truncated,
                    full_output_path: snapshot.full_output_path,
                }),
                Err(err) => Err(err),
            }
        })
    })
}

#[derive(Clone, Default)]
pub struct BashToolOptions {
    pub operations: Option<Arc<dyn BashOperations>>,
    pub command_prefix: Option<String>,
    pub shell_path: Option<String>,
    pub expose_session_environment: Option<bool>,
    pub spawn_hook: Option<BashSpawnHook>,
    pub temp_dir: Option<std::path::PathBuf>,
}

pub(crate) fn resolve_spawn_context(
    command: String,
    cwd: String,
    spawn_hook: Option<&BashSpawnHook>,
    expose_session_environment: bool,
    ctx: &ExtensionToolContext,
) -> BashSpawnContext {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.remove("PI_SESSION_ID");
    env.remove("PI_SESSION_FILE");
    env.remove("PI_PROVIDER");
    env.remove("PI_MODEL");
    env.remove("PI_REASONING_LEVEL");

    if expose_session_environment {
        let sid = ctx.session_id();
        if !sid.is_empty() {
            env.insert("PI_SESSION_ID".to_owned(), sid);
        }
        if let Some(sf) = ctx.session_file() {
            env.insert(
                "PI_SESSION_FILE".to_owned(),
                sf.to_string_lossy().into_owned(),
            );
        }
        if let Some(model) = ctx.model() {
            env.insert("PI_PROVIDER".to_owned(), model.provider);
            env.insert("PI_MODEL".to_owned(), model.id);
        }
        if let Ok(lvl) = ctx.runtime().get_thinking_level() {
            env.insert("PI_REASONING_LEVEL".to_owned(), lvl.as_str().to_owned());
        }
    }

    let base = BashSpawnContext { command, cwd, env };
    match spawn_hook {
        Some(hook) => hook(base),
        None => base,
    }
}

fn format_shell_output(
    snapshot: &OutputSnapshot,
    last_line_bytes: usize,
    empty_text: &str,
) -> (String, Option<BashToolDetails>) {
    let trunc = &snapshot.truncation;
    let mut text = if snapshot.content.is_empty() {
        empty_text.to_owned()
    } else {
        snapshot.content.clone()
    };
    let mut details = None;
    if trunc.truncated {
        details = Some(BashToolDetails {
            truncation: Some(trunc.clone()),
            full_output_path: snapshot.full_output_path.clone(),
        });
        let start_line = trunc.total_lines.saturating_sub(trunc.output_lines) + 1;
        let end_line = trunc.total_lines;
        let full_suffix = match &snapshot.full_output_path {
            Some(path) => format!("Full output: {path}"),
            None => "Full output could not be saved".to_owned(),
        };
        if trunc.last_line_partial {
            let last_size = format_size(last_line_bytes);
            text.push_str(&format!(
                "\n\n[Showing last {} of line {end_line} (line is {last_size}). {full_suffix}]",
                format_size(trunc.output_bytes)
            ));
        } else if trunc.truncated_by == Some(TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {}. {full_suffix}]",
                trunc.total_lines
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). {full_suffix}]",
                trunc.total_lines,
                format_size(DEFAULT_MAX_BYTES)
            ));
        }
    }
    (text, details)
}

fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_owned()
    } else {
        format!("{text}\n\n{status}")
    }
}

pub(crate) struct ShellToolConfig {
    pub(crate) name: &'static str,
    pub(crate) label: &'static str,
    pub(crate) shell_name: &'static str,
    pub(crate) prompt_snippet: &'static str,
    pub(crate) temp_file_prefix: &'static str,
}

pub(crate) fn create_shell_tool_definition(
    cwd: impl Into<String>,
    config: ShellToolConfig,
    options: Option<BashToolOptions>,
) -> ToolDefinition {
    let default_cwd = cwd.into();
    let opts = options.unwrap_or_default();
    let ops: Arc<dyn BashOperations> = opts
        .operations
        .unwrap_or_else(|| create_local_bash_operations(opts.shell_path.clone()));
    let command_prefix = opts.command_prefix;
    let expose_session_env = opts.expose_session_environment.unwrap_or(true);
    let spawn_hook = opts.spawn_hook;
    let temp_dir = opts.temp_dir;
    let temp_prefix = config.temp_file_prefix.to_owned();

    let mut def = ToolDefinition::new_typed_with_context::<BashToolInput, _, _>(
        config.name,
        config.label,
        format!(
            "Execute a {} command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            config.shell_name,
            DEFAULT_MAX_BYTES / 1024
        ),
        move |_tool_call_id, input: BashToolInput, on_update, cancel: CancellationToken, ctx: ExtensionToolContext| {
            let ops = ops.clone();
            let base_cwd = default_cwd.clone();
            let command_prefix = command_prefix.clone();
            let spawn_hook = spawn_hook.clone();
            let temp_dir = temp_dir.clone();
            let temp_prefix = temp_prefix.clone();

            Box::pin(async move {
                resolve_timeout_ms(input.timeout)?;
                if cancel.is_cancelled() { return Err("Command aborted".to_owned()); }
                let resolved_cmd = match &command_prefix {
                    Some(pref) if !pref.is_empty() => format!("{pref}\n{}", input.command),
                    _ => input.command.clone(),
                };
                let effective_cwd = if ctx.cwd().is_empty() {
                    base_cwd
                } else {
                    ctx.cwd()
                };
                let spawn_ctx = resolve_spawn_context(
                    resolved_cmd,
                    effective_cwd,
                    spawn_hook.as_ref(),
                    expose_session_env,
                    &ctx,
                );

                if let Some(ref cb) = on_update {
                    cb(AgentToolResult {
                            is_error: false,
                            terminate: None,
                        content: Vec::new(),
                        details: Value::Null,
                        structured_content: None,
                        usage: None,
                    });
                }

                let acc = Arc::new(Mutex::new((
                    OutputAccumulator::new(OutputAccumulatorOptions {
                        temp_file_prefix: Some(temp_prefix),
                        temp_dir,
                        ..OutputAccumulatorOptions::default()
                    }),
                    Instant::now()
                        .checked_sub(Duration::from_millis(BASH_UPDATE_THROTTLE_MS))
                        .unwrap_or_else(Instant::now),
                )));

                let acc_for_cb = acc.clone();
                let on_update_for_cb = on_update.clone();
                let on_data: ShellDataCallback = Arc::new(move |data: &[u8]| {
                    let mut guard = acc_for_cb.lock().expect("acc lock");
                    guard.0.append(data);
                    if let Some(ref cb) = on_update_for_cb
                        && guard.1.elapsed() >= Duration::from_millis(BASH_UPDATE_THROTTLE_MS)
                    {
                        guard.1 = Instant::now();
                        let snap = guard.0.snapshot(true);
                        let details = BashToolDetails {
                            truncation: snap.truncation.truncated.then_some(snap.truncation),
                            full_output_path: snap.full_output_path,
                        };
                        let update = AgentToolResult {
                            is_error: false,
                            terminate: None,
                            content: vec![UserContentBlock::Text(TextContent::new(snap.content))],
                            details: serde_json::to_value(details).unwrap_or(Value::Null),
                            structured_content: None,
                            usage: None,
                        };
                        drop(guard);
                        cb(update);
                    }
                });

                let started_at = Instant::now();
                let exec_res = ops
                    .exec(
                        &spawn_ctx.command,
                        &spawn_ctx.cwd,
                        BashExecOptions {
                            on_data,
                            cancel,
                            timeout_secs: input.timeout,
                            env: spawn_ctx.env,
                        },
                    )
                    .await;

                let (snapshot, last_line_bytes, full_output) = {
                    let mut guard = acc.lock().expect("acc lock");
                    guard.0.finish();
                    let snap = guard.0.snapshot(true);
                    let llb = guard.0.last_line_bytes();
                    let full = guard.0.read_full_output(STRUCTURED_OUTPUT_MAX_BYTES);
                    (snap, llb, full)
                };

                let exit_code_opt = match exec_res {
                    Ok(code) => code,
                    Err(err) => {
                        let (text, _) = format_shell_output(&snapshot, last_line_bytes, "");
                        if err == "aborted" {
                            return Err(append_status(&text, "Command aborted"));
                        }
                        if let Some(secs) = err.strip_prefix("timeout:") {
                            return Err(append_status(
                                &text,
                                &format!("Command timed out after {secs} seconds"),
                            ));
                        }
                        return Err(err);
                    }
                };

                let (output_text, details_opt) =
                    format_shell_output(&snapshot, last_line_bytes, "(no output)");
                let Some(exit_code) = exit_code_opt else {
                    return Err(append_status(
                        &output_text,
                        "Command terminated without an exit code",
                    ));
                };

                let wall_time_seconds =
                    (started_at.elapsed().as_secs_f64() * 10.0).round() / 10.0;
                let structured = BashToolOutput {
                    output: full_output.content,
                    truncated: full_output.truncated,
                    full_output_path: if full_output.truncated {
                        snapshot.full_output_path.clone()
                    } else {
                        None
                    },
                    exit_code,
                    wall_time_seconds,
                };
                let structured_val = serde_json::to_value(&structured).ok();
                let details_val = details_opt
                    .and_then(|d| serde_json::to_value(d).ok())
                    .unwrap_or(Value::Null);

                if exit_code != 0 {
                    return Err(append_status(
                        &output_text,
                        &format!("Command exited with code {exit_code}"),
                    ));
                }

                Ok(AgentToolResult {
                            is_error: false,
                            terminate: None,
                    content: vec![UserContentBlock::Text(TextContent::new(output_text))],
                    details: details_val,
                    structured_content: structured_val,
                    usage: None,
                })
            }) as BoxFuture<'static, Result<AgentToolResult, String>>
        },
    )
    .with_prompt_snippet(config.prompt_snippet)
    .with_exposure(ToolExposure::Direct)
    .with_replay_policy(ToolReplayPolicy::Never);

    if expose_session_env {
        def = def.with_prompt_guidelines(vec![
            "You can inspect PI_* environment variables for current model and session details."
                .to_owned(),
        ]);
    }
    def.output_schema = serde_json::to_value(schemars::schema_for!(BashToolOutput)).ok();
    def
}

pub fn create_bash_tool_definition(
    cwd: impl Into<String>,
    options: Option<BashToolOptions>,
) -> ToolDefinition {
    create_shell_tool_definition(
        cwd,
        ShellToolConfig {
            name: "bash",
            label: "bash",
            shell_name: "bash",
            prompt_snippet: "Execute bash commands (ls, grep, find, etc.)",
            temp_file_prefix: "pi-bash",
        },
        options,
    )
}

pub fn create_bash_tool(cwd: impl Into<String>, options: Option<BashToolOptions>) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_bash_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

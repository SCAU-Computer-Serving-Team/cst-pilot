use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::Arc;

use pi_agent::{AgentTool, BoxFuture};
use pi_session::{NoOpUiContext, ToolDefinition};

use crate::tools::bash::{
    BashExecOptions, BashOperations, BashSpawnContext, BashSpawnHook, BashToolDetails,
    BashToolInput, BashToolOptions, BashToolOutput, LocalShellOperations, ShellConfig,
    ShellToolConfig, create_shell_tool_definition,
};

pub const POWERSHELL_ARGS: &[&str] = &[
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

pub const UTF8_OUTPUT_PREFIX: &str =
    "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n";

pub type PowerShellToolInput = BashToolInput;
pub type PowerShellToolOutput = BashToolOutput;
pub type PowerShellToolDetails = BashToolDetails;
pub type PowerShellSpawnContext = BashSpawnContext;
pub type PowerShellSpawnHook = BashSpawnHook;
pub type PowerShellExecOptions = BashExecOptions;
pub type PowerShellOperations = dyn BashOperations;

#[derive(Clone, Default)]
pub struct PowerShellToolOptions {
    pub operations: Option<Arc<dyn BashOperations>>,
    pub command_prefix: Option<String>,
    pub expose_session_environment: Option<bool>,
    pub spawn_hook: Option<PowerShellSpawnHook>,
    pub temp_dir: Option<std::path::PathBuf>,
}

/// Checks whether a shell executable path exists without following Windows Store `APPEXECLINK` reparse points.
pub fn shell_path_exists(path: &str) -> bool {
    let trimmed = path.trim();
    !trimmed.is_empty() && std::fs::symlink_metadata(trimmed).is_ok()
}

pub fn find_executable_on_path(executable: &str) -> Option<String> {
    #[cfg(windows)]
    {
        use crate::config_value::win_job;
        use std::os::windows::process::CommandExt;

        let mut cmd = Command::new("where.exe");
        cmd.arg(executable)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(win_job::CREATE_NO_WINDOW);

        let output = cmd.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_match = stdout.trim().lines().next()?.trim();
        if !first_match.is_empty() && shell_path_exists(first_match) {
            return Some(first_match.to_owned());
        }
        None
    }

    #[cfg(not(windows))]
    {
        let output = Command::new("which")
            .arg(executable)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_match = stdout.trim().lines().next()?.trim();
        if !first_match.is_empty() {
            return Some(first_match.to_owned());
        }
        None
    }
}

pub struct PowerShellConfigLookup<'a> {
    pub platform: &'a str,
    pub env: &'a BTreeMap<String, String>,
    pub find_in_path: &'a dyn Fn(&str) -> Option<String>,
    pub path_exists: &'a dyn Fn(&str) -> bool,
}

fn env_get_ci<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    if let Some(v) = env.get(key) {
        return (!v.is_empty()).then_some(v.as_str());
    }
    env.iter()
        .find(|(k, v)| k.eq_ignore_ascii_case(key) && !v.is_empty())
        .map(|(_, v)| v.as_str())
}

fn win32_join(base: &str, segments: &[&str]) -> String {
    let mut out = base.trim_end_matches(['\\', '/']).replace('/', "\\");
    for seg in segments {
        let cleaned = seg.trim_matches(['\\', '/']);
        if !cleaned.is_empty() {
            out.push('\\');
            out.push_str(cleaned);
        }
    }
    out
}

/// Resolves PowerShell executable on Windows according to the 5-tier lookup order:
/// 1. `pwsh.exe` on `PATH`
/// 2. `%ProgramFiles%\PowerShell\7\pwsh.exe`
/// 3. `powershell.exe` on `PATH`
/// 4. `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`
/// 5. Error if none exists
pub fn get_powershell_config_with(
    lookup: &PowerShellConfigLookup<'_>,
) -> Result<ShellConfig, String> {
    if lookup.platform != "win32" {
        return Err("The powershell tool is only available on Windows.".to_owned());
    }

    let mut shell = (lookup.find_in_path)("pwsh.exe");
    if shell.is_none()
        && let Some(program_files) = env_get_ci(lookup.env, "ProgramFiles")
    {
        let pwsh7 = win32_join(program_files, &["PowerShell", "7", "pwsh.exe"]);
        if (lookup.path_exists)(&pwsh7) {
            shell = Some(pwsh7);
        }
    }

    if shell.is_none() {
        shell = (lookup.find_in_path)("powershell.exe");
    }
    if shell.is_none()
        && let Some(system_root) = env_get_ci(lookup.env, "SystemRoot")
    {
        let windows_ps = win32_join(
            system_root,
            &["System32", "WindowsPowerShell", "v1.0", "powershell.exe"],
        );
        if (lookup.path_exists)(&windows_ps) {
            shell = Some(windows_ps);
        }
    }

    let Some(shell) = shell else {
        return Err(
            "No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH."
                .to_owned(),
        );
    };

    Ok(ShellConfig {
        shell,
        args: POWERSHELL_ARGS.iter().map(|s| (*s).to_owned()).collect(),
    })
}

pub fn get_powershell_config() -> Result<ShellConfig, String> {
    let platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let lookup = PowerShellConfigLookup {
        platform,
        env: &env,
        find_in_path: &find_executable_on_path,
        path_exists: &shell_path_exists,
    };
    get_powershell_config_with(&lookup)
}

pub fn resolve_powershell_config() -> ShellConfig {
    get_powershell_config().unwrap_or_else(|_| ShellConfig {
        shell: if cfg!(windows) {
            "powershell.exe".to_owned()
        } else {
            "pwsh".to_owned()
        },
        args: POWERSHELL_ARGS.iter().map(|s| (*s).to_owned()).collect(),
    })
}

struct LocalPowerShellOperationsWrapper {
    inner: Arc<dyn BashOperations>,
}

impl BashOperations for LocalPowerShellOperationsWrapper {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<'a, Result<Option<i32>, String>> {
        let prefixed = format!("{UTF8_OUTPUT_PREFIX}{command}");
        Box::pin(async move { self.inner.exec(&prefixed, cwd, options).await })
    }
}

pub fn create_local_powershell_operations() -> Arc<dyn BashOperations> {
    let base = Arc::new(LocalShellOperations {
        shell_name: "PowerShell".to_owned(),
        resolve_shell: Arc::new(get_powershell_config),
    });
    Arc::new(LocalPowerShellOperationsWrapper { inner: base })
}

pub fn powershell_tool_system_prompt_contribution(
    expose_session_environment: Option<bool>,
) -> (String, Vec<String>) {
    let guidelines = if expose_session_environment.unwrap_or(true) {
        vec![
            "You can inspect PI_* environment variables for current model and session details."
                .to_owned(),
        ]
    } else {
        Vec::new()
    };
    ("Execute PowerShell commands".to_owned(), guidelines)
}

pub fn create_powershell_tool_definition(
    cwd: impl Into<String>,
    options: Option<PowerShellToolOptions>,
) -> ToolDefinition {
    let opts = options.unwrap_or_default();
    let bash_opts = BashToolOptions {
        operations: Some(
            opts.operations
                .unwrap_or_else(create_local_powershell_operations),
        ),
        command_prefix: opts.command_prefix,
        shell_path: None,
        expose_session_environment: opts.expose_session_environment,
        spawn_hook: opts.spawn_hook,
        temp_dir: opts.temp_dir,
    };
    create_shell_tool_definition(
        cwd,
        ShellToolConfig {
            name: "powershell",
            label: "powershell",
            shell_name: "PowerShell",
            prompt_snippet: "Execute PowerShell commands",
            temp_file_prefix: "pi-powershell",
        },
        Some(bash_opts),
    )
}

pub fn create_powershell_tool(
    cwd: impl Into<String>,
    options: Option<PowerShellToolOptions>,
) -> AgentTool {
    let cwd_str = cwd.into();
    let def = create_powershell_tool_definition(cwd_str.clone(), options);
    let runner = pi_session::ExtensionRunner::empty(cwd_str);
    runner.set_ui_context(
        Some(Arc::new(NoOpUiContext::new())),
        pi_session::ExtensionMode::Print,
    );
    def.to_agent_tool(move |tc_id, cancel| runner.create_tool_context(tc_id, cancel))
}

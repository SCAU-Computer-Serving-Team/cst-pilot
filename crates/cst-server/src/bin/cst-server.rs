#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use cst_kit::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_kit::telemetry::TelemetryOptions;
use cst_kit::update::service::UpdateOptions;
use cst_kit::{Kit, KitHost};
use cst_server::{RunningServer, ServerConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OfflineCliMode {
    Inherit,
    ForceOffline,
    ForceOnline,
}

#[derive(Debug, PartialEq, Eq)]
struct CliArgs {
    health_check: bool,
    key_fingerprint: bool,
    standalone_web: bool,
    open_browser: bool,
    sanitize_env: bool,
    offline_mode: OfflineCliMode,
    port: Option<u16>,
    kit_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
    static_dir: Option<PathBuf>,
}

fn parse_args() -> Result<CliArgs, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(args: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut health_check = false;
    let mut key_fingerprint = false;
    let mut standalone_web = false;
    let mut open_browser = false;
    let mut sanitize_env = false;
    let mut force_offline = false;
    let mut force_online = false;
    let mut port = None;
    let mut kit_root = None;
    let mut cwd = None;
    let mut static_dir = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--health-check" => health_check = true,
            "--key-fingerprint" => key_fingerprint = true,
            "--standalone-web" => standalone_web = true,
            "--open-browser" => open_browser = true,
            "--sanitize-env" => sanitize_env = true,
            "--offline" => force_offline = true,
            "--online" => force_online = true,
            "--port" => {
                let val = iter.next().ok_or("`--port` 缺少端口号参数")?;
                port = Some(val.parse::<u16>().map_err(|_| "`--port` 端口号格式无效")?);
            }
            "--kit-root" => {
                let val = iter.next().ok_or("`--kit-root` 缺少路径参数")?;
                kit_root = Some(PathBuf::from(val));
            }
            "--cwd" => {
                let val = iter.next().ok_or("`--cwd` 缺少路径参数")?;
                cwd = Some(PathBuf::from(val));
            }
            "--static-dir" => {
                let val = iter.next().ok_or("`--static-dir` 缺少路径参数")?;
                static_dir = Some(PathBuf::from(val));
            }
            other => return Err(format!("未知的命令行参数：{other}")),
        }
    }

    if force_offline && force_online {
        return Err("`--offline` 与 `--online` 不能同时指定".to_owned());
    }
    if open_browser && !standalone_web {
        return Err("`--open-browser` 需配合 `--standalone-web` 使用".to_owned());
    }
    let offline_mode = match (force_offline, force_online) {
        (true, false) => OfflineCliMode::ForceOffline,
        (false, true) => OfflineCliMode::ForceOnline,
        _ => OfflineCliMode::Inherit,
    };

    Ok(CliArgs {
        health_check,
        key_fingerprint,
        standalone_web,
        open_browser,
        sanitize_env,
        offline_mode,
        port,
        kit_root,
        cwd,
        static_dir,
    })
}

fn open_default_browser(url: &str) {
    #[cfg(windows)]
    {
        if let Ok(win_dir) = cst_os::windows_directory() {
            let explorer = win_dir.join("explorer.exe");
            let _ = std::process::Command::new(explorer).arg(url).spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

fn resolve_default_kit_root() -> Result<PathBuf, String> {
    cst_os::resolve_kit_root()
}

fn resolve_effective_offline_state(
    offline_mode: OfflineCliMode,
    cst_offline: Option<&str>,
    pi_offline: Option<&str>,
) -> bool {
    match offline_mode {
        OfflineCliMode::ForceOffline => true,
        OfflineCliMode::ForceOnline => false,
        OfflineCliMode::Inherit => cst_os::is_offline_env_values(cst_offline, pi_offline),
    }
}

/// Sanitizes the current single-threaded process environment when launched via `ShellExecuteExW("runas")`
/// (before spawning any Tokio worker threads), matching `cst_client::apply_portable_sanitized_env`.
fn sanitize_current_process_env(
    kit_root: &std::path::Path,
    offline_mode: OfflineCliMode,
) -> Result<(), String> {
    let canonical = kit_root
        .canonicalize()
        .map_err(|e| format!("无法读取工具包根目录 {}：{e}", kit_root.display()))?;
    let clean_root = cst_os::strip_windows_verbatim_prefix(&canonical);
    let tmp_dir = clean_root.join(".state").join("tmp");
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("无法创建便携运行临时目录 {}：{e}", tmp_dir.display()))?;

    let win_dir = cst_os::windows_directory()?;
    let sys_dir = cst_os::system_directory()?;

    let mut path_entries = Vec::new();
    let kit_pwsh = clean_root.join("pwsh");
    if kit_pwsh.is_dir() {
        path_entries.push(kit_pwsh);
    }
    let kit_bin = clean_root.join("bin");
    if kit_bin.is_dir() {
        path_entries.push(kit_bin);
    }
    path_entries.push(sys_dir.clone());
    path_entries.push(win_dir.clone());
    path_entries.push(sys_dir.join("Wbem"));
    path_entries.push(sys_dir.join("WindowsPowerShell").join("v1.0"));
    let joined_path =
        std::env::join_paths(path_entries).map_err(|e| format!("组装安全 PATH 失败：{e}"))?;

    const PASSTHROUGH_KEYS: &[&str] = &[
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
    ];

    let cur_cst = std::env::var("CST_OFFLINE").ok();
    let cur_pi = std::env::var("PI_OFFLINE").ok();
    let caller_offline =
        resolve_effective_offline_state(offline_mode, cur_cst.as_deref(), cur_pi.as_deref());
    let existing_keys: Vec<std::ffi::OsString> = std::env::vars_os().map(|(k, _)| k).collect();
    // SAFETY: Called strictly at single-threaded `fn main()` entry before building the Tokio runtime.
    unsafe {
        for key in existing_keys {
            let keep = key.to_str().is_some_and(|s| {
                PASSTHROUGH_KEYS
                    .iter()
                    .any(|allowed| s.eq_ignore_ascii_case(allowed))
            });
            if !keep {
                std::env::remove_var(&key);
            }
        }
        std::env::set_var("SystemRoot", &win_dir);
        std::env::set_var("WINDIR", &win_dir);
        std::env::set_var("ComSpec", sys_dir.join("cmd.exe"));
        std::env::set_var("PATH", joined_path);
        std::env::set_var("PATHEXT", ".COM;.EXE;.BAT;.CMD");
        std::env::set_var("TEMP", &tmp_dir);
        std::env::set_var("TMP", &tmp_dir);
        std::env::set_var("CST_PILOT_ROOT", &clean_root);
        if caller_offline {
            std::env::set_var("CST_OFFLINE", "1");
        }
    }
    let _ = std::env::set_current_dir(&clean_root);
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            cst_os::attach_parent_console_if_stdio_missing();
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    if args.key_fingerprint {
        cst_os::attach_parent_console_if_stdio_missing();
        return match cst_kit::update::service::compiled_update_key_fingerprint() {
            Ok(fp) => {
                let _ = writeln!(std::io::stdout(), "{fp}");
                let _ = std::io::stdout().flush();
                ExitCode::SUCCESS
            }
            Err(msg) => {
                let _ = writeln!(std::io::stderr(), "{msg}");
                let _ = std::io::stderr().flush();
                ExitCode::FAILURE
            }
        };
    }

    if args.standalone_web || args.health_check {
        cst_os::attach_parent_console_if_stdio_missing();
    }

    let kit_root = match args
        .kit_root
        .clone()
        .map(Ok)
        .unwrap_or_else(resolve_default_kit_root)
    {
        Ok(r) => r,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    if args.sanitize_env {
        if let Err(msg) = sanitize_current_process_env(&kit_root, args.offline_mode) {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    } else {
        match args.offline_mode {
            OfflineCliMode::ForceOffline => {
                // SAFETY: Single-threaded `fn main()` entry before building the Tokio runtime.
                unsafe {
                    std::env::set_var("CST_OFFLINE", "1");
                }
            }
            OfflineCliMode::ForceOnline => {
                // SAFETY: Single-threaded `fn main()` entry before building the Tokio runtime.
                unsafe {
                    std::env::remove_var("CST_OFFLINE");
                    std::env::remove_var("PI_OFFLINE");
                }
            }
            OfflineCliMode::Inherit => {}
        }
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "无法初始化异步运行时：{err}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(async_main(args, kit_root))
}

async fn async_main(args: CliArgs, kit_root: PathBuf) -> ExitCode {
    if args.health_check {
        return match cst_kit::update::handoff::run_cli_health_check_with_version(
            &kit_root,
            option_env!("CST_BUILD_VERSION"),
        ) {
            Ok(version) => {
                let _ = writeln!(std::io::stdout(), "cst-pilot {version}");
                let _ = std::io::stdout().flush();
                ExitCode::SUCCESS
            }
            Err(msg) => {
                let _ = writeln!(std::io::stderr(), "{msg}");
                let _ = std::io::stderr().flush();
                ExitCode::FAILURE
            }
        };
    }

    let kit = match Kit::new(&kit_root) {
        Ok(k) => k,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };
    if let Err(msg) = kit.ensure_default_configs() {
        let _ = writeln!(std::io::stderr(), "{msg}");
        let _ = std::io::stderr().flush();
        return ExitCode::FAILURE;
    }

    let oa_client = match OaClient::official() {
        Ok(c) => c,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    let auth = match AuthRuntime::create(kit.root(), Channel::Gui, oa_client).await {
        Ok(a) => a,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    let update_options = if args.standalone_web {
        UpdateOptions::Disabled
    } else {
        UpdateOptions::Compiled
    };
    let host = match KitHost::new(kit, auth, TelemetryOptions::default(), update_options).await {
        Ok(h) => h,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    let mut config = ServerConfig::new(&kit_root);
    config.mode_config.standalone_web = args.standalone_web;
    config.port = args.port;
    config.cwd = args.cwd.or_else(|| std::env::current_dir().ok());
    config.static_dir = args.static_dir;

    let server = match RunningServer::start(host, config).await {
        Ok(s) => s,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    let _ = writeln!(
        std::io::stdout(),
        "cst-server listening on {}",
        server.base_url()
    );
    let _ = std::io::stdout().flush();
    if args.open_browser {
        open_default_browser(&format!("{}/", server.base_url()));
    }

    let stop_token = server.state.server_stop.clone();
    tokio::select! {
        _ = stop_token.cancelled() => {},
        _ = tokio::signal::ctrl_c() => {
            let _ = server.state.execute_shutdown().await;
        }
    }

    server.shutdown().await;
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::parse_args_from;

    #[test]
    fn parse_standalone_web_and_open_browser() {
        let parsed = parse_args_from(
            ["--standalone-web", "--open-browser", "--port", "52831"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("valid cli args");
        assert!(parsed.standalone_web);
        assert!(parsed.open_browser);
        assert_eq!(parsed.port, Some(52831));
    }

    #[test]
    fn open_browser_requires_standalone_web() {
        let err = parse_args_from(["--open-browser"].into_iter().map(str::to_owned))
            .expect_err("open-browser without standalone-web should fail");
        assert!(err.contains("--standalone-web"));
    }

    #[test]
    fn parse_sanitize_env_and_kit_root() {
        let parsed = parse_args_from(
            ["--sanitize-env", "--kit-root", r"E:\kit"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("valid sanitize-env cli args");
        assert!(parsed.sanitize_env);
        assert_eq!(
            parsed.kit_root.as_deref(),
            Some(std::path::Path::new(r"E:\kit"))
        );
    }

    #[test]
    fn parse_key_fingerprint_flag() {
        let parsed =
            parse_args_from(["--key-fingerprint"].into_iter().map(str::to_owned)).expect("valid");
        assert!(parsed.key_fingerprint);
    }

    #[test]
    fn parse_offline_and_online_flags_reject_conflict() {
        let off = parse_args_from(["--offline"].into_iter().map(str::to_owned)).expect("offline");
        assert_eq!(off.offline_mode, super::OfflineCliMode::ForceOffline);

        let on = parse_args_from(["--online"].into_iter().map(str::to_owned)).expect("online");
        assert_eq!(on.offline_mode, super::OfflineCliMode::ForceOnline);

        assert!(parse_args_from(["--offline", "--online"].into_iter().map(str::to_owned)).is_err());

        assert!(super::resolve_effective_offline_state(
            super::OfflineCliMode::ForceOffline,
            Some("0"),
            Some("0"),
        ));
        assert!(!super::resolve_effective_offline_state(
            super::OfflineCliMode::ForceOnline,
            Some("1"),
            Some("1"),
        ));
        assert!(super::resolve_effective_offline_state(
            super::OfflineCliMode::Inherit,
            None,
            Some("1"),
        ));
        assert!(!super::resolve_effective_offline_state(
            super::OfflineCliMode::Inherit,
            Some("0"),
            Some("0"),
        ));
    }

    #[cfg(windows)]
    #[test]
    fn sanitize_env_child_process_honors_online_and_offline_overrides() {
        const CHILD_MODE_ENV: &str = "CST_SERVER_TEST_SANITIZE_PROBE_MODE";
        const CHILD_DIR_ENV: &str = "CST_SERVER_TEST_SANITIZE_PROBE_DIR";
        if let Ok(mode_str) = std::env::var(CHILD_MODE_ENV) {
            let dir = std::path::PathBuf::from(
                std::env::var_os(CHILD_DIR_ENV).expect("missing probe dir env"),
            );
            let mode = match mode_str.as_str() {
                "online" => super::OfflineCliMode::ForceOnline,
                "offline" => super::OfflineCliMode::ForceOffline,
                _ => super::OfflineCliMode::Inherit,
            };
            super::sanitize_current_process_env(&dir, mode).unwrap();
            let _ = std::env::set_current_dir(std::env::temp_dir());
            assert!(std::env::var_os("PI_OFFLINE").is_none());
            match mode_str.as_str() {
                "online" => {
                    assert!(std::env::var_os("CST_OFFLINE").is_none());
                    assert!(!cst_os::is_offline_env_enabled());
                }
                "offline" | "inherit" => {
                    assert_eq!(std::env::var("CST_OFFLINE").as_deref(), Ok("1"));
                    assert!(cst_os::is_offline_env_enabled());
                }
                other => panic!("unexpected probe mode: {other}"),
            }
            return;
        }

        let exe = std::env::current_exe().expect("test binary path");
        for (mode, cst_off, pi_off) in [
            ("online", "1", "1"),
            ("offline", "0", "0"),
            ("inherit", "0", "1"),
        ] {
            let probe_dir = std::env::temp_dir().join(format!(
                "cst-server-sanitize-probe-{}-{mode}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&probe_dir);
            std::fs::create_dir_all(&probe_dir).expect("create probe dir");
            let status = std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "tests::sanitize_env_child_process_honors_online_and_offline_overrides",
                    "--nocapture",
                ])
                .env(CHILD_MODE_ENV, mode)
                .env(CHILD_DIR_ENV, &probe_dir)
                .env("CST_OFFLINE", cst_off)
                .env("PI_OFFLINE", pi_off)
                .status()
                .expect("spawn child probe");
            let _ = std::fs::remove_dir_all(&probe_dir);
            assert!(
                status.success(),
                "child sanitize probe failed for mode={mode}"
            );
            assert!(
                !probe_dir.exists(),
                "probe_dir must be cleaned up after child exit"
            );
        }
    }
}

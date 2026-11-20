#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Write as _;
use std::process::ExitCode;

use cst_desktop::{
    forward_standalone_web, parse_cli_args, run_desktop_app, run_desktop_health_check,
};
use cst_os::RestartUi;

fn show_error_dialog(message: &str) {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        use windows::core::PCWSTR;

        let title: Vec<u16> = OsStr::new("CST Pilot").encode_wide().chain([0]).collect();
        let body: Vec<u16> = OsStr::new(message).encode_wide().chain([0]).collect();
        unsafe {
            let _ = MessageBoxW(
                Some(HWND::default()),
                PCWSTR(body.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("{message}");
    }
}

/// Ensures the desktop process runs elevated via UAC (`ShellExecuteExW("runas")`) on normal startup.
/// Returns `Ok(true)` if already elevated, `Ok(false)` if an elevated child was launched (caller should exit 0).
fn ensure_elevated_at_entry(
    kit_root: &std::path::Path,
    has_explicit_kit_root: bool,
) -> Result<bool, String> {
    if std::env::var("CST_ALLOW_UNELEVATED").as_deref() == Ok("1") {
        return Ok(true);
    }
    if cst_os::is_elevated()? {
        return Ok(true);
    }

    #[cfg(windows)]
    {
        use std::ffi::{OsStr, OsString};
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows::Win32::UI::Shell::{SEE_MASK_DEFAULT, SHELLEXECUTEINFOW, ShellExecuteExW};
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
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

        let exe = std::env::current_exe().map_err(|e| format!("无法获取当前程序路径：{e}"))?;
        let mut joined_params = OsString::new();
        for (idx, arg) in std::env::args_os().skip(1).enumerate() {
            if idx > 0 {
                joined_params.push(" ");
            }
            joined_params.push(quote_win32_arg(&arg));
        }
        if !has_explicit_kit_root {
            if !joined_params.is_empty() {
                joined_params.push(" ");
            }
            joined_params.push(quote_win32_arg(OsStr::new("--kit-root")));
            joined_params.push(" ");
            joined_params.push(quote_win32_arg(kit_root.as_os_str()));
        }

        let verb_w: Vec<u16> = OsStr::new("runas").encode_wide().chain([0]).collect();
        let file_w: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
        let params_w: Vec<u16> = joined_params.as_os_str().encode_wide().chain([0]).collect();
        let dir_w: Vec<u16> = kit_root.as_os_str().encode_wide().chain([0]).collect();

        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_DEFAULT,
            lpVerb: PCWSTR(verb_w.as_ptr()),
            lpFile: PCWSTR(file_w.as_ptr()),
            lpParameters: PCWSTR(params_w.as_ptr()),
            lpDirectory: PCWSTR(dir_w.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };

        unsafe { ShellExecuteExW(&mut sei) }
            .map_err(|_| "需要管理员权限以运行诊断工具，UAC 授权已取消。".to_owned())?;
        Ok(false)
    }

    #[cfg(not(windows))]
    {
        let _ = (kit_root, has_explicit_kit_root);
        Ok(true)
    }
}

fn main() -> ExitCode {
    let cli = match parse_cli_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            return ExitCode::FAILURE;
        }
    };

    let has_explicit_kit_root = cli.kit_root.is_some();
    let kit_root = match cli
        .kit_root
        .map(Ok)
        .unwrap_or_else(cst_os::resolve_kit_root)
    {
        Ok(root) => root,
        Err(msg) => {
            let _ = writeln!(std::io::stderr(), "{msg}");
            let _ = std::io::stderr().flush();
            if !cli.health_check {
                show_error_dialog(&msg);
            }
            return ExitCode::FAILURE;
        }
    };

    // 1. Pre-GUI, pre-UAC, lock-free `--health-check` fast path
    if cli.health_check {
        return match run_desktop_health_check(&kit_root, option_env!("CST_BUILD_VERSION")) {
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

    // 2. `--ui web` standalone browser forwarding
    if cli.ui == RestartUi::Web {
        return match forward_standalone_web(&kit_root) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                show_error_dialog(&msg);
                ExitCode::FAILURE
            }
        };
    }

    // 3. UAC administrator elevation at entry boundary
    match ensure_elevated_at_entry(&kit_root, has_explicit_kit_root) {
        Ok(true) => {}
        Ok(false) => return ExitCode::SUCCESS,
        Err(msg) => {
            show_error_dialog(&msg);
            return ExitCode::FAILURE;
        }
    }

    // 4. Desktop GUI lifecycle
    match run_desktop_app(kit_root, cli.ui) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            show_error_dialog(&msg);
            ExitCode::FAILURE
        }
    }
}

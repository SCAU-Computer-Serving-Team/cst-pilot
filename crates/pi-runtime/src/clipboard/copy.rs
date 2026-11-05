use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{
    DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES, DEFAULT_POWERSHELL_CLIPBOARD_TIMEOUT_MS, should_emit_osc52,
    try_encode_osc52_clipboard_sequence,
};

pub trait ClipboardTextWriter: Send + Sync {
    fn set_text(&self, text: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClipboard;

impl ClipboardTextWriter for SystemClipboard {
    fn set_text(&self, text: &str) -> Result<(), String> {
        #[cfg(windows)]
        {
            if crate::clipboard_native::set_text(text).is_ok() {
                return Ok(());
            }
            let script = "[Console]::InputEncoding=[System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Clipboard]::SetText([Console]::In.ReadToEnd())";
            copy_command(
                "powershell.exe",
                &["-NoProfile", "-NonInteractive", "-STA", "-Command", script],
                text,
            )
        }
        #[cfg(target_os = "macos")]
        {
            copy_command("pbcopy", &[], text)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let env: BTreeMap<String, String> = std::env::vars().collect();
            if super::is_wayland_session(&env) && copy_command("wl-copy", &[], text).is_ok() {
                return Ok(());
            }
            if env.get("DISPLAY").is_some_and(|value| !value.is_empty()) {
                for (program, args) in [
                    ("xclip", &["-selection", "clipboard", "-in"][..]),
                    ("xsel", &["--clipboard", "--input"][..]),
                ] {
                    if copy_command(program, args, text).is_ok() {
                        return Ok(());
                    }
                }
            }
            Err(
                "Clipboard unavailable: install wl-clipboard for Wayland or xclip/xsel for X11"
                    .to_owned(),
            )
        }
    }
}

/// Native writes precede terminal writes. The injected output keeps OSC 52 out of
/// a host application's JSON/RPC channel unless it explicitly chooses stdout.
pub fn copy_to_clipboard_with(
    text: &str,
    clipboard: &dyn ClipboardTextWriter,
    terminal: &mut dyn Write,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let local = clipboard.set_text(text);
    let headless = cfg!(target_os = "linux")
        && !env.contains_key("DISPLAY")
        && !env.contains_key("WAYLAND_DISPLAY");
    if should_emit_osc52(env) || (local.is_err() && headless) {
        match try_encode_osc52_clipboard_sequence(text) {
            Ok(sequence) => {
                terminal
                    .write_all(sequence.as_bytes())
                    .and_then(|()| terminal.flush())
                    .map_err(|_| "Failed to write clipboard OSC 52 sequence".to_owned())?;
                return Ok(());
            }
            Err(error) if local.is_err() => return Err(error),
            Err(_) => {}
        }
    }
    local
}

pub fn copy_to_clipboard_sync(text: &str) -> Result<(), String> {
    copy_to_clipboard_with(
        text,
        &SystemClipboard,
        &mut std::io::stdout().lock(),
        &std::env::vars().collect(),
    )
}

pub async fn copy_to_clipboard(text: impl Into<String>) -> Result<(), String> {
    let text = text.into();
    tokio::task::spawn_blocking(move || copy_to_clipboard_sync(&text))
        .await
        .map_err(|_| "Clipboard task failed".to_owned())?
}

fn copy_command(program: &str, args: &[&str], text: &str) -> Result<(), String> {
    if text.len() > DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES {
        return Err("Clipboard text is too large".to_owned());
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    let job = {
        use crate::config_value::win_job;
        use std::os::windows::process::CommandExt;
        command.creation_flags(win_job::CREATE_NO_WINDOW | win_job::CREATE_SUSPENDED);
        win_job::WinJobGuard::create().ok_or("Failed to create clipboard process containment")?
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Clipboard command unavailable".to_owned())?;
    #[cfg(windows)]
    if !job.assign_and_resume(&mut child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Failed to contain clipboard command".to_owned());
    }
    let pid = child.id();
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Missing clipboard command stdin")?;
    let bytes = text.as_bytes().to_vec();
    let (done, result) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = done.send(stdin.write_all(&bytes));
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success()
                    && result
                        .recv_timeout(Duration::from_millis(200))
                        .is_ok_and(|write| write.is_ok())
                {
                    // Linux clipboard utilities intentionally retain a selection-owner daemon.
                    // A successful write must leave that owner alive.
                    return Ok(());
                }
                break;
            }
            Ok(None)
                if started.elapsed()
                    < Duration::from_millis(DEFAULT_POWERSHELL_CLIPBOARD_TIMEOUT_MS) =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => break,
        }
    }
    #[cfg(windows)]
    job.terminate();
    crate::config_value::kill_process_tree_sync(pid);
    let _ = child.kill();
    let _ = child.wait();
    Err("Clipboard command failed or timed out".to_owned())
}

mod copy;
pub use copy::{
    ClipboardTextWriter, SystemClipboard, copy_to_clipboard, copy_to_clipboard_sync,
    copy_to_clipboard_with,
};

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use crate::config_value::kill_process_tree_sync;
#[cfg(windows)]
use crate::config_value::win_job;
use crate::image_process::{SUPPORTED_IMAGE_MIME_TYPES, convert_to_png_sync};

pub const DEFAULT_CLIPBOARD_TIMEOUT_MS: u64 = 3_000;
pub const DEFAULT_POWERSHELL_CLIPBOARD_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_OSC52_BYTES: usize = 100_000;

/// Order matches ClipboardImage preference (`png`, `jpeg`, `webp`, `gif`).
pub const PREFERRED_CLIPBOARD_IMAGE_MIMES: &[&str] =
    &["image/png", "image/jpeg", "image/webp", "image/gif"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

pub fn is_wayland_session(env: &BTreeMap<String, String>) -> bool {
    env.get("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || env
            .get("XDG_SESSION_TYPE")
            .is_some_and(|v| v.eq_ignore_ascii_case("wayland"))
}

pub fn select_preferred_image_mime_type(mime_types: &[String]) -> Option<String> {
    let normalized: Vec<String> = mime_types
        .iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect();

    for &preferred in PREFERRED_CLIPBOARD_IMAGE_MIMES {
        if let Some(found) = normalized
            .iter()
            .find(|t| t.eq_ignore_ascii_case(preferred))
        {
            return Some(found.to_ascii_lowercase());
        }
    }

    normalized
        .into_iter()
        .find(|t| t.to_ascii_lowercase().starts_with("image/"))
}

pub fn extension_for_image_mime_type(mime_type: &str) -> &'static str {
    let base = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

/// Converts unsupported image formats (such as `image/bmp` or `image/tiff`) to `image/png`.
pub fn normalize_clipboard_image(image: ClipboardImage) -> Option<ClipboardImage> {
    let base_mime = image
        .mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if SUPPORTED_IMAGE_MIME_TYPES.contains(&base_mime.as_str()) {
        return Some(ClipboardImage {
            bytes: image.bytes,
            mime_type: base_mime,
        });
    }

    let b64 = BASE64_STANDARD.encode(&image.bytes);
    let converted = convert_to_png_sync(&b64, &base_mime)?;
    let png_bytes = BASE64_STANDARD.decode(&converted.data).ok()?;
    Some(ClipboardImage {
        bytes: png_bytes,
        mime_type: converted.mime_type,
    })
}

pub fn should_emit_osc52(env: &BTreeMap<String, String>) -> bool {
    if env.get("PI_CLIPBOARD_OSC52").is_some_and(|v| v == "1") {
        return true;
    }
    ["SSH_CONNECTION", "SSH_CLIENT", "MOSH_CONNECTION"]
        .iter()
        .any(|k| env.get(*k).is_some_and(|v| !v.is_empty()))
}

pub fn encode_osc52_clipboard_sequence(text: &str) -> String {
    let encoded = BASE64_STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

pub fn try_encode_osc52_clipboard_sequence(text: &str) -> Result<String, String> {
    if text.len().div_ceil(3).saturating_mul(4) > MAX_OSC52_BYTES {
        return Err("Clipboard text exceeds the OSC 52 encoded size limit".to_owned());
    }
    Ok(encode_osc52_clipboard_sequence(text))
}

fn run_clipboard_command_bounded(
    program: &str,
    args: &[&str],
    timeout_ms: u64,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    let job = win_job::WinJobGuard::create();

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

    let mut child = cmd.spawn().ok()?;
    let pid = child.id();

    #[cfg(windows)]
    if let Some(ref j) = job
        && !j.assign_and_resume(&mut child)
    {
        let _ = child.kill();
        j.terminate();
        let _ = child.wait();
        return None;
    }

    let stdout_pipe = child.stdout.take()?;
    let out_handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = stdout_pipe;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len().saturating_add(n) > max_bytes {
                        return None;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
        Some(buf)
    });

    let start = Instant::now();
    let status = loop {
        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            #[cfg(windows)]
            if let Some(ref j) = job {
                j.terminate();
            }
            kill_process_tree_sync(pid);
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                #[cfg(windows)]
                if let Some(ref j) = job {
                    j.terminate();
                }
                kill_process_tree_sync(pid);
                let _ = child.wait();
                return None;
            }
        }
    };

    #[cfg(windows)]
    if let Some(ref j) = job {
        j.terminate();
    }
    #[cfg(unix)]
    kill_process_tree_sync(pid);

    if !status.success() {
        return None;
    }
    out_handle.join().ok().flatten()
}

pub fn read_clipboard_image_sync() -> Option<ClipboardImage> {
    #[cfg(windows)]
    {
        if let Some(image) = crate::clipboard_native::read_image() {
            return normalize_clipboard_image(image);
        }
        let ps_script = "\
            Add-Type -AssemblyName System.Windows.Forms, System.Drawing; \
            if ([System.Windows.Forms.Clipboard]::ContainsImage()) { \
                $img = [System.Windows.Forms.Clipboard]::GetImage(); \
                if ($img -ne $null) { \
                    $ms = New-Object System.IO.MemoryStream; \
                    $img.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); \
                    [Console]::Out.Write([Convert]::ToBase64String($ms.ToArray())); \
                    $ms.Dispose(); \
                    $img.Dispose(); \
                } \
            }";
        let out = run_clipboard_command_bounded(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", ps_script],
            DEFAULT_POWERSHELL_CLIPBOARD_TIMEOUT_MS,
            DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES,
        )?;
        let b64 = String::from_utf8_lossy(&out);
        let trimmed = b64.trim();
        if trimmed.is_empty() {
            return None;
        }
        let bytes = BASE64_STANDARD.decode(trimmed).ok()?;
        if bytes.is_empty() {
            return None;
        }
        normalize_clipboard_image(ClipboardImage {
            bytes,
            mime_type: "image/png".to_owned(),
        })
    }

    #[cfg(not(windows))]
    {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        if is_wayland_session(&env) {
            let types_raw = run_clipboard_command_bounded(
                "wl-paste",
                &["--list-types"],
                DEFAULT_CLIPBOARD_TIMEOUT_MS,
                64 * 1024,
            )?;
            let types: Vec<String> = String::from_utf8_lossy(&types_raw)
                .lines()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
            let selected = select_preferred_image_mime_type(&types)?;
            let data = run_clipboard_command_bounded(
                "wl-paste",
                &["--type", &selected, "--no-newline"],
                DEFAULT_CLIPBOARD_TIMEOUT_MS,
                DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES,
            )?;
            if data.is_empty() {
                return None;
            }
            return normalize_clipboard_image(ClipboardImage {
                bytes: data,
                mime_type: selected,
            });
        }

        let targets_raw = run_clipboard_command_bounded(
            "xclip",
            &["-selection", "clipboard", "-t", "TARGETS", "-o"],
            DEFAULT_CLIPBOARD_TIMEOUT_MS,
            64 * 1024,
        )?;
        let types: Vec<String> = String::from_utf8_lossy(&targets_raw)
            .lines()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let selected = select_preferred_image_mime_type(&types)?;
        let data = run_clipboard_command_bounded(
            "xclip",
            &["-selection", "clipboard", "-t", &selected, "-o"],
            DEFAULT_CLIPBOARD_TIMEOUT_MS,
            DEFAULT_CLIPBOARD_MAX_BUFFER_BYTES,
        )?;
        if data.is_empty() {
            return None;
        }
        normalize_clipboard_image(ClipboardImage {
            bytes: data,
            mime_type: selected,
        })
    }
}

pub async fn read_clipboard_image() -> Option<ClipboardImage> {
    tokio::task::spawn_blocking(read_clipboard_image_sync)
        .await
        .ok()
        .flatten()
}

pub fn parse_clipboard_file_paths(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let uri = url::Url::parse(line).ok()?;
            if uri.scheme() != "file" || uri.host_str().is_some_and(|host| host != "localhost") {
                return None;
            }
            uri.to_file_path().ok()
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

pub fn read_clipboard_file_paths_sync() -> Vec<String> {
    #[cfg(windows)]
    {
        if let Some(paths) = crate::clipboard_native::read_paths() {
            return paths;
        }
        let ps_script = "\
            Add-Type -AssemblyName System.Windows.Forms; \
            if ([System.Windows.Forms.Clipboard]::ContainsFileDropList()) { \
                [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
                [System.Windows.Forms.Clipboard]::GetFileDropList() | ForEach-Object { [Console]::WriteLine($_) } \
            }";
        let Some(out) = run_clipboard_command_bounded(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", ps_script],
            DEFAULT_POWERSHELL_CLIPBOARD_TIMEOUT_MS,
            1024 * 1024,
        ) else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out)
            .lines()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[cfg(not(windows))]
    {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let candidates = ["text/uri-list", "x-special/gnome-copied-files"];
        for mime in candidates {
            let data = if is_wayland_session(&env) {
                run_clipboard_command_bounded(
                    "wl-paste",
                    &["--type", mime, "--no-newline"],
                    DEFAULT_CLIPBOARD_TIMEOUT_MS,
                    1024 * 1024,
                )
            } else {
                run_clipboard_command_bounded(
                    "xclip",
                    &["-selection", "clipboard", "-target", mime, "-out"],
                    DEFAULT_CLIPBOARD_TIMEOUT_MS,
                    1024 * 1024,
                )
            };
            if let Some(data) = data {
                let paths = parse_clipboard_file_paths(&String::from_utf8_lossy(&data));
                if !paths.is_empty() {
                    return paths;
                }
            }
        }
        Vec::new()
    }
}

pub async fn read_clipboard_file_paths() -> Vec<String> {
    tokio::task::spawn_blocking(read_clipboard_file_paths_sync)
        .await
        .unwrap_or_default()
}

pub fn read_clipboard_text_sync() -> Option<String> {
    #[cfg(windows)]
    {
        if let Some(text) = crate::clipboard_native::read_text() {
            return Some(text);
        }
        let ps_script = "\
            Add-Type -AssemblyName System.Windows.Forms; \
            [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
            if ([System.Windows.Forms.Clipboard]::ContainsText()) { \
                [Console]::Out.Write([System.Windows.Forms.Clipboard]::GetText()) \
            }";
        let out = run_clipboard_command_bounded(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", ps_script],
            DEFAULT_POWERSHELL_CLIPBOARD_TIMEOUT_MS,
            10 * 1024 * 1024,
        )?;
        let text = String::from_utf8_lossy(&out).into_owned();
        (!text.is_empty()).then_some(text)
    }

    #[cfg(not(windows))]
    {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let out = if is_wayland_session(&env) {
            run_clipboard_command_bounded(
                "wl-paste",
                &["--no-newline"],
                DEFAULT_CLIPBOARD_TIMEOUT_MS,
                10 * 1024 * 1024,
            )?
        } else {
            run_clipboard_command_bounded(
                "xclip",
                &["-selection", "clipboard", "-o"],
                DEFAULT_CLIPBOARD_TIMEOUT_MS,
                10 * 1024 * 1024,
            )?
        };
        let text = String::from_utf8_lossy(&out).into_owned();
        (!text.is_empty()).then_some(text)
    }
}

pub async fn read_clipboard_text() -> Option<String> {
    tokio::task::spawn_blocking(read_clipboard_text_sync)
        .await
        .ok()
        .flatten()
}

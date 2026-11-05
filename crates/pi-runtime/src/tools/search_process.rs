//! Optional fd/rg backend with process-tree containment and bounded record framing.
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::process_io::read_pipe;
#[cfg(unix)]
use crate::config_value::kill_process_tree_sync;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchBackend {
    #[default]
    Auto,
    Native,
    External,
}

/// None means unavailable or an oversized external record; the caller can use native fallback.
pub(crate) fn run_search(
    executable: &str,
    args: &[String],
    cwd: &Path,
    delimiter: u8,
    max_record_bytes: usize,
    cancel: &CancellationToken,
    mut record: impl FnMut(&[u8]) -> bool + Send,
) -> Result<Option<()>, String> {
    if cancel.is_cancelled() {
        return Err("Operation aborted".to_owned());
    }
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Disable user config/color so both backends expose the same output contract.
    command.env_remove("RIPGREP_CONFIG_PATH");
    #[cfg(windows)]
    let job = {
        use std::os::windows::process::CommandExt;
        command.creation_flags(
            crate::config_value::win_job::CREATE_NO_WINDOW
                | crate::config_value::win_job::CREATE_SUSPENDED,
        );
        crate::config_value::win_job::WinJobGuard::create()
            .ok_or("Failed to contain external search")?
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Failed to start external search".to_owned()),
    };
    #[cfg(unix)]
    let pid = child.id();
    #[cfg(windows)]
    if !job.assign_and_resume(&mut child) {
        job.terminate();
        let _ = child.kill();
        let _ = child.wait();
        return Err("Failed to contain external search".to_owned());
    }
    let pipe = child.stdout.take().expect("piped stdout");
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_read = Arc::new(AtomicBool::new(false));
    let oversized = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    std::thread::scope(|scope| {
        let stopped_cb = stopped.clone();
        let stop_cb = stop_read.clone();
        let oversized_cb = oversized.clone();
        let reader = scope.spawn(move || {
            let mut pending = Vec::new();
            read_pipe(pipe, stop_cb, |chunk| {
                for &byte in chunk {
                    if stopped_cb.load(Ordering::Acquire) {
                        break;
                    }
                    if byte == delimiter {
                        if !record(&pending) {
                            stopped_cb.store(true, Ordering::Release);
                        }
                        pending.clear();
                    } else if pending.len() < max_record_bytes {
                        pending.push(byte);
                    } else {
                        oversized_cb.store(true, Ordering::Release);
                        stopped_cb.store(true, Ordering::Release);
                    }
                }
            });
        });
        let status = loop {
            if cancel.is_cancelled()
                || stopped.load(Ordering::Acquire)
                || Instant::now() >= deadline
            {
                break None;
            }
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => break None,
            }
        };
        // Every completion path kills remaining descendants before draining pipes.
        #[cfg(windows)]
        job.terminate();
        #[cfg(unix)]
        kill_process_tree_sync(pid);
        if status.is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
        let drain_until = Instant::now() + Duration::from_millis(100);
        while !reader.is_finished() && Instant::now() < drain_until {
            std::thread::sleep(Duration::from_millis(2));
        }
        stop_read.store(true, Ordering::Release);
        reader
            .join()
            .map_err(|_| "External search reader failed".to_owned())?;
        if cancel.is_cancelled() {
            return Err("Operation aborted".to_owned());
        }
        if Instant::now() >= deadline {
            return Err("External search timed out".to_owned());
        }
        if oversized.load(Ordering::Acquire) {
            return Ok(None);
        }
        if stopped.load(Ordering::Acquire)
            || status.is_some_and(|s| s.success() || s.code() == Some(1))
        {
            Ok(Some(()))
        } else {
            Err("External search failed".to_owned())
        }
    })
}

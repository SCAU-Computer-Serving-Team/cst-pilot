use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::SkippedRoot;

#[derive(Clone, Debug)]
pub(crate) struct ReadyVolume {
    /// Normalized display root such as `"C:/"`.
    pub display_root: String,
    /// Native root path such as `"C:\\"`.
    pub native_root: PathBuf,
    /// Uppercase drive prefix such as `"C:"`.
    pub drive: String,
    /// Filesystem name such as `"NTFS"`, `"exFAT"`, `"FAT32"`.
    pub filesystem: String,
    /// Volume GUID path if available (`\\?\Volume{...}\`).
    #[allow(dead_code)]
    pub volume_guid: Option<String>,
    /// True if this drive letter is a `SUBST` virtual mapping (must not expand to whole volume MFT).
    pub is_subst: bool,
    /// Win32 volume serial number.
    #[allow(dead_code)]
    pub serial: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct VolumeEnumeration {
    pub ready: Vec<ReadyVolume>,
    pub skipped: Vec<SkippedRoot>,
    /// True if any eligible local volume failed readiness/access (not policy-skipped CD-ROM/network).
    pub had_non_policy_failure: bool,
}

#[derive(Default)]
struct EnumProgress {
    done: bool,
    fatal_err: Option<String>,
    pending_letters: Vec<char>,
    ready: Vec<ReadyVolume>,
    skipped: Vec<SkippedRoot>,
    had_non_policy_failure: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum ProbeDecision {
    Ready(ReadyVolume),
    Skipped {
        root: String,
        reason: String,
        non_policy_failure: bool,
    },
}

struct EnumDoneGuard {
    shared: Arc<(Mutex<EnumProgress>, Condvar)>,
}

impl Drop for EnumDoneGuard {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.done = true;
        cv.notify_all();
    }
}

/// Runs volume probing on a dedicated thread while streaming each completed volume into shared
/// progress immediately. If `deadline` expires mid-probe, already-ready volumes are preserved and
/// unprobed volumes are recorded in `skipped` with reason `"probe_timeout"`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn enumerate_volumes_with_probe<F>(
    letters: Vec<char>,
    deadline: Instant,
    cancel: &CancellationToken,
    probe: F,
) -> Result<VolumeEnumeration, String>
where
    F: Fn(char, Instant, &CancellationToken) -> Result<ProbeDecision, String> + Send + 'static,
{
    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    let shared = Arc::new((
        Mutex::new(EnumProgress {
            pending_letters: letters.clone(),
            ..Default::default()
        }),
        Condvar::new(),
    ));
    let bg_shared = Arc::clone(&shared);
    let cancel_bg = cancel.clone();
    std::thread::Builder::new()
        .name("cst-fff-enum-vols".into())
        .spawn(move || {
            let _done = EnumDoneGuard {
                shared: Arc::clone(&bg_shared),
            };
            let (lock, cv) = &*bg_shared;
            for letter in letters {
                if cancel_bg.is_cancelled() {
                    return;
                }
                if Instant::now() >= deadline {
                    return;
                }
                match probe(letter, deadline, &cancel_bg) {
                    Ok(decision) => {
                        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        guard.pending_letters.retain(|&l| l != letter);
                        match decision {
                            ProbeDecision::Ready(vol) => guard.ready.push(vol),
                            ProbeDecision::Skipped {
                                root,
                                reason,
                                non_policy_failure,
                            } => {
                                guard.had_non_policy_failure |= non_policy_failure;
                                guard.skipped.push(SkippedRoot { root, reason });
                            }
                        }
                        cv.notify_all();
                    }
                    Err(err) => {
                        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        guard.fatal_err = Some(err);
                        cv.notify_all();
                        return;
                    }
                }
            }
        })
        .map_err(|_| "枚举本机逻辑驱动器线程启动失败。".to_owned())?;

    let (lock, cv) = &*shared;
    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if cancel.is_cancelled() {
            return Err("文件检索已取消。".into());
        }
        if guard.done {
            if let Some(err) = guard.fatal_err.clone() {
                return Err(err);
            }
            let mut skipped = guard.skipped.clone();
            let mut had_non_policy_failure = guard.had_non_policy_failure;
            for letter in &guard.pending_letters {
                had_non_policy_failure = true;
                skipped.push(SkippedRoot {
                    root: format!("{letter}:/"),
                    reason: "probe_timeout".into(),
                });
            }
            if guard.ready.is_empty() && !guard.pending_letters.is_empty() {
                return Err("枚举本机逻辑驱动器超时。".into());
            }
            return Ok(VolumeEnumeration {
                ready: guard.ready.clone(),
                skipped,
                had_non_policy_failure,
            });
        }
        let now = Instant::now();
        if now >= deadline {
            if let Some(err) = guard.fatal_err.clone() {
                return Err(err);
            }
            let ready = guard.ready.clone();
            let mut skipped = guard.skipped.clone();
            let mut had_non_policy_failure = guard.had_non_policy_failure;
            for letter in &guard.pending_letters {
                had_non_policy_failure = true;
                skipped.push(SkippedRoot {
                    root: format!("{letter}:/"),
                    reason: "probe_timeout".into(),
                });
            }
            if ready.is_empty() {
                return Err("枚举本机逻辑驱动器超时。".into());
            }
            return Ok(VolumeEnumeration {
                ready,
                skipped,
                had_non_policy_failure,
            });
        }
        let wait = (deadline - now).min(Duration::from_millis(25));
        let (next, _) = cv
            .wait_timeout(guard, wait)
            .unwrap_or_else(|e| e.into_inner());
        guard = next;
    }
}

#[cfg(windows)]
pub(crate) fn enumerate_whole_pc_volumes(
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<VolumeEnumeration, String> {
    use std::collections::HashSet;
    use windows::Win32::Storage::FileSystem::GetLogicalDrives;

    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err("枚举本机逻辑驱动器失败。".into());
    }
    let letters: Vec<char> = (0..26u32)
        .filter(|bit| (mask & (1u32 << bit)) != 0)
        .map(|bit| (b'A' + bit as u8) as char)
        .collect();
    let seen_root_keys: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    enumerate_volumes_with_probe(letters, deadline, cancel, move |letter, dl, c| {
        probe_single_windows_volume(letter, dl, c, &seen_root_keys)
    })
}

#[cfg(windows)]
fn probe_single_windows_volume(
    letter: char,
    deadline: Instant,
    cancel: &CancellationToken,
    seen_root_keys: &Mutex<std::collections::HashSet<String>>,
) -> Result<ProbeDecision, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_NOT_READY, ERROR_UNRECOGNIZED_MEDIA, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, GetDriveTypeW, GetFileInformationByHandle, GetVolumeInformationW,
        GetVolumeNameForVolumeMountPointW, QueryDosDeviceW,
    };
    use windows::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
    };
    use windows::core::PCWSTR;

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const ERROR_FVE_LOCKED_VOLUME: u32 = 0x8031_0000;

    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    if Instant::now() >= deadline {
        return Ok(ProbeDecision::Skipped {
            root: format!("{letter}:/"),
            reason: "probe_timeout".into(),
            non_policy_failure: true,
        });
    }

    struct ErrorModeGuard(THREAD_ERROR_MODE);
    impl Drop for ErrorModeGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = SetThreadErrorMode(self.0, None);
            }
        }
    }
    let mut prev_mode = THREAD_ERROR_MODE(0);
    unsafe {
        let _ = SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut prev_mode));
    }
    let _mode_guard = ErrorModeGuard(prev_mode);

    let drive = format!("{letter}:");
    let native_str = format!("{letter}:\\");
    let display_root = format!("{letter}:/");
    let wide_root: Vec<u16> = OsStr::new(&native_str)
        .encode_wide()
        .chain(Some(0))
        .collect();

    let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_root.as_ptr())) };
    match drive_type {
        DRIVE_FIXED | DRIVE_REMOVABLE => {}
        DRIVE_REMOTE => {
            return Ok(ProbeDecision::Skipped {
                root: display_root,
                reason: "network_drive_skipped".into(),
                non_policy_failure: false,
            });
        }
        DRIVE_CDROM => {
            return Ok(ProbeDecision::Skipped {
                root: display_root,
                reason: "cdrom_skipped".into(),
                non_policy_failure: false,
            });
        }
        _ => {
            return Ok(ProbeDecision::Skipped {
                root: display_root,
                reason: "unsupported_drive_type".into(),
                non_policy_failure: false,
            });
        }
    }

    // Detect SUBST virtual drive letters via QueryDosDeviceW (`\??\...`).
    // Fail-safe: if QueryDosDeviceW fails, treat as `is_subst = true` so raw $MFT is disabled.
    let wide_dev: Vec<u16> = OsStr::new(&drive).encode_wide().chain(Some(0)).collect();
    let mut dev_buf = [0u16; 512];
    let dev_len = unsafe { QueryDosDeviceW(PCWSTR(wide_dev.as_ptr()), Some(&mut dev_buf)) };
    let is_subst = if dev_len > 0 {
        let end = dev_buf[..dev_len as usize]
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(dev_len as usize);
        let target = String::from_utf16_lossy(&dev_buf[..end]);
        target.starts_with(r"\??\")
    } else {
        true
    };

    let mut serial = 0u32;
    let mut fs_buf = [0u16; 64];
    let vol_info = unsafe {
        GetVolumeInformationW(
            PCWSTR(wide_root.as_ptr()),
            None,
            Some(&mut serial),
            None,
            None,
            Some(&mut fs_buf),
        )
    };
    if let Err(err) = vol_info {
        let hr = err.code().0 as u32;
        let win32 = hr & 0xFFFF;
        let reason = if hr == ERROR_FVE_LOCKED_VOLUME {
            "locked"
        } else if win32 == ERROR_ACCESS_DENIED.0 {
            "access_denied"
        } else if win32 == ERROR_NOT_READY.0 || win32 == ERROR_UNRECOGNIZED_MEDIA.0 {
            "not_ready"
        } else {
            "unavailable"
        };
        let non_policy_failure = drive_type == DRIVE_FIXED || reason != "not_ready";
        return Ok(ProbeDecision::Skipped {
            root: display_root,
            reason: reason.into(),
            non_policy_failure,
        });
    }

    let mut guid_buf = [0u16; 128];
    let volume_guid = if is_subst {
        None
    } else {
        unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(wide_root.as_ptr()), &mut guid_buf).ok() }
            .map(|()| {
                let len = guid_buf
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(guid_buf.len());
                String::from_utf16_lossy(&guid_buf[..len])
            })
            .filter(|g| !g.is_empty())
    };

    // Deduplicate whole-volume aliases (including SUBST to a volume root) by (volume, root_id)
    // while keeping SUBST mappings to distinct subdirectories separate.
    let root_obj_id = std::fs::OpenOptions::new()
        .read(false)
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(&native_str)
        .ok()
        .and_then(|f| {
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            unsafe { GetFileInformationByHandle(HANDLE(f.as_raw_handle()), &mut info) }
                .ok()
                .map(|()| {
                    let idx = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
                    (info.dwVolumeSerialNumber, idx)
                })
        });

    let dedup_key = match (&volume_guid, root_obj_id) {
        (Some(guid), Some((_, idx))) if idx != 0 => {
            Some(format!("guid:{}:{idx}", guid.to_ascii_lowercase()))
        }
        (Some(guid), _) => Some(format!("guid:{}", guid.to_ascii_lowercase())),
        (None, Some((vser, idx))) if vser != 0 && idx != 0 => Some(format!("ser:{vser}:{idx}")),
        _ => None,
    };
    if let Some(key) = dedup_key {
        let mut seen = seen_root_keys.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.insert(key) {
            return Ok(ProbeDecision::Skipped {
                root: display_root,
                reason: "duplicate_volume_alias".into(),
                non_policy_failure: false,
            });
        }
    }

    let fs_len = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
    let filesystem = String::from_utf16_lossy(&fs_buf[..fs_len]);

    Ok(ProbeDecision::Ready(ReadyVolume {
        display_root,
        native_root: PathBuf::from(&native_str),
        drive,
        filesystem,
        volume_guid,
        is_subst,
        serial,
    }))
}

#[cfg(not(windows))]
pub(crate) fn enumerate_whole_pc_volumes(
    _deadline: Instant,
    _cancel: &CancellationToken,
) -> Result<VolumeEnumeration, String> {
    Err("全电脑扫描（path=\"*\"）仅支持 Windows。".into())
}

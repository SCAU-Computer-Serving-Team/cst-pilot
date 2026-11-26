use std::path::PathBuf;
#[cfg(windows)]
use std::time::Duration;
use std::time::Instant;

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

#[cfg(windows)]
pub(crate) fn enumerate_whole_pc_volumes(
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<VolumeEnumeration, String> {
    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    let cancel_bg = cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("cst-fff-enum-vols".into())
        .spawn(move || {
            let res = enumerate_whole_pc_volumes_blocking(deadline, &cancel_bg);
            let _ = tx.send(res);
        })
        .map_err(|_| "枚举本机逻辑驱动器线程启动失败。".to_owned())?;

    loop {
        if cancel.is_cancelled() {
            return Err("文件检索已取消。".into());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("枚举本机逻辑驱动器超时。".into());
        }
        let wait = (deadline - now).min(Duration::from_millis(50));
        match rx.recv_timeout(wait) {
            Ok(res) => return res,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("枚举本机逻辑驱动器异常中止。".into());
            }
        }
    }
}

#[cfg(windows)]
fn enumerate_whole_pc_volumes_blocking(
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<VolumeEnumeration, String> {
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_NOT_READY, ERROR_UNRECOGNIZED_MEDIA, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, GetDriveTypeW, GetFileInformationByHandle, GetLogicalDrives,
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, QueryDosDeviceW,
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

    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err("枚举本机逻辑驱动器失败。".into());
    }

    let mut ready = Vec::new();
    let mut skipped = Vec::new();
    let mut had_non_policy_failure = false;
    let mut seen_root_keys: HashSet<String> = HashSet::new();

    for bit in 0..26u32 {
        if cancel.is_cancelled() {
            return Err("文件检索已取消。".into());
        }
        if (mask & (1u32 << bit)) == 0 {
            continue;
        }
        let letter = (b'A' + bit as u8) as char;
        let drive = format!("{letter}:");
        let native_str = format!("{letter}:\\");
        let display_root = format!("{letter}:/");
        if Instant::now() >= deadline {
            had_non_policy_failure = true;
            skipped.push(SkippedRoot {
                root: display_root,
                reason: "timeout".into(),
            });
            continue;
        }
        let wide_root: Vec<u16> = OsStr::new(&native_str)
            .encode_wide()
            .chain(Some(0))
            .collect();

        let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_root.as_ptr())) };
        match drive_type {
            DRIVE_FIXED | DRIVE_REMOVABLE => {}
            DRIVE_REMOTE => {
                skipped.push(SkippedRoot {
                    root: display_root,
                    reason: "network_drive_skipped".into(),
                });
                continue;
            }
            DRIVE_CDROM => {
                skipped.push(SkippedRoot {
                    root: display_root,
                    reason: "cdrom_skipped".into(),
                });
                continue;
            }
            _ => {
                skipped.push(SkippedRoot {
                    root: display_root,
                    reason: "unsupported_drive_type".into(),
                });
                continue;
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
            if drive_type == DRIVE_FIXED || reason != "not_ready" {
                had_non_policy_failure = true;
            }
            skipped.push(SkippedRoot {
                root: display_root,
                reason: reason.into(),
            });
            continue;
        }

        let mut guid_buf = [0u16; 128];
        let volume_guid = if is_subst {
            None
        } else {
            unsafe {
                GetVolumeNameForVolumeMountPointW(PCWSTR(wide_root.as_ptr()), &mut guid_buf).ok()
            }
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
                        let idx =
                            ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
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
        if let Some(key) = dedup_key
            && !seen_root_keys.insert(key)
        {
            skipped.push(SkippedRoot {
                root: display_root,
                reason: "duplicate_volume_alias".into(),
            });
            continue;
        }

        let fs_len = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
        let filesystem = String::from_utf16_lossy(&fs_buf[..fs_len]);

        ready.push(ReadyVolume {
            display_root,
            native_root: PathBuf::from(&native_str),
            drive,
            filesystem,
            volume_guid,
            is_subst,
            serial,
        });
    }

    Ok(VolumeEnumeration {
        ready,
        skipped,
        had_non_policy_failure,
    })
}

#[cfg(not(windows))]
pub(crate) fn enumerate_whole_pc_volumes(
    _deadline: Instant,
    _cancel: &CancellationToken,
) -> Result<VolumeEnumeration, String> {
    Err("全电脑扫描（path=\"*\"）仅支持 Windows。".into())
}

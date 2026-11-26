use std::path::PathBuf;
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
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_NOT_READY, ERROR_UNRECOGNIZED_MEDIA,
    };
    use windows::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
        QueryDosDeviceW,
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
    let mut seen_volume_keys: HashSet<String> = HashSet::new();

    for bit in 0..26u32 {
        if cancel.is_cancelled() {
            return Err("文件检索已取消。".into());
        }
        if Instant::now() >= deadline {
            break;
        }
        if (mask & (1u32 << bit)) == 0 {
            continue;
        }
        let letter = (b'A' + bit as u8) as char;
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
            false
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
            // Empty removable card readers reporting not_ready are expected hardware state,
            // while fixed-drive failures or access_denied/locked mark degraded coverage.
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

        // Deduplicate multiple drive letters mapped to the exact same physical volume
        // (keeping SUBST mappings distinct if pointing to a subdirectory, or skipping
        // whole-volume alias duplicates).
        if let Some(ref guid) = volume_guid {
            let key = guid.to_ascii_lowercase();
            if !seen_volume_keys.insert(key) {
                skipped.push(SkippedRoot {
                    root: display_root,
                    reason: "duplicate_volume_alias".into(),
                });
                continue;
            }
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

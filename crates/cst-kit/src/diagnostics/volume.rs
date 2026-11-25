use super::scan::{ScanSnapshot, SensitivePathFilter};
use super::volume_index::{ScanSource, VolumeIdentity};
use super::win_native::Control;

#[derive(Default)]
pub struct NativeScanSource {
    filter: SensitivePathFilter,
}

impl NativeScanSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_filter(filter: SensitivePathFilter) -> Self {
        Self { filter }
    }
}

impl ScanSource for NativeScanSource {
    fn identity(&self, drive: &str) -> Result<VolumeIdentity, String> {
        volume_identity(drive)
    }

    fn scan_volume(&self, drive: &str, control: &Control) -> Result<ScanSnapshot, String> {
        control.check()?;
        if !super::mft::mft_enabled() {
            return Err("原生 $MFT 直读默认关闭，当前使用目录遍历。".into());
        }
        let identity = volume_identity(drive)?;
        if !identity.filesystem.eq_ignore_ascii_case("NTFS") {
            return Err(format!(
                "卷 {drive} 文件系统为 {}，仅本地 NTFS 支持原生 $MFT 索引。",
                identity.filesystem
            ));
        }
        if !cst_os::is_elevated().unwrap_or(false) {
            return Err("当前非管理员权限，无法直读 NTFS 主文件表（$MFT）。".into());
        }
        super::mft::scan_ntfs_volume_with_filter(
            drive,
            identity.volume_guid.as_deref(),
            control,
            &self.filter,
        )
    }
}

#[cfg(windows)]
fn volume_identity(drive: &str) -> Result<VolumeIdentity, String> {
    use std::fs;
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetDriveTypeW, GetFileInformationByHandle,
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    };
    use windows::core::PCWSTR;

    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOVABLE: u32 = 2;

    if drive.len() != 2 || !drive.as_bytes()[0].is_ascii_alphabetic() || drive.as_bytes()[1] != b':'
    {
        return Err("卷身份查询需要有效盘符。".into());
    }
    let path = format!("{drive}\\");
    let wide: Vec<u16> = std::ffi::OsStr::new(&path)
        .encode_wide()
        .chain(Some(0))
        .collect();
    // Reject mapped network drives, CD-ROMs, and unknown mounts before touching volume metadata.
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
    if drive_type != DRIVE_FIXED && drive_type != DRIVE_REMOVABLE {
        return Err("目标卷不是本地固定或可移动磁盘。".into());
    }
    let mut guid_buf = [0u16; 128];
    let volume_guid =
        unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(wide.as_ptr()), &mut guid_buf).ok() }
            .map(|()| {
                let len = guid_buf
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(guid_buf.len());
                String::from_utf16_lossy(&guid_buf[..len])
            })
            .filter(|g| !g.is_empty());

    let mut serial = 0u32;
    let mut filesystem = [0u16; 64];
    // SAFETY: drive root is NUL-terminated and all output buffers remain alive.
    unsafe {
        GetVolumeInformationW(
            PCWSTR(wide.as_ptr()),
            None,
            Some(&mut serial),
            None,
            None,
            Some(&mut filesystem),
        )
    }
    .map_err(|_| "卷信息无法读取。")?;
    let file = fs::OpenOptions::new()
        .read(true)
        .access_mode(0)
        .share_mode(7)
        .custom_flags(0x02000000)
        .open(&path)
        .map_err(|_| "卷根目录无法打开。")?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the file handle and output POD struct are valid for the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| "卷根目录身份无法读取。")?;
    if serial == 0 || serial != info.dwVolumeSerialNumber {
        return Err("卷身份不稳定。".into());
    }
    let len = filesystem
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(filesystem.len());
    Ok(VolumeIdentity {
        serial,
        filesystem: String::from_utf16_lossy(&filesystem[..len]),
        root_created: ((info.ftCreationTime.dwHighDateTime as u64) << 32)
            | info.ftCreationTime.dwLowDateTime as u64,
        volume_guid,
    })
}

#[cfg(not(windows))]
fn volume_identity(_: &str) -> Result<VolumeIdentity, String> {
    Err("卷身份查询仅支持 Windows。".into())
}

use super::win_native::Control;
use super::wz_index::{ScanSource, VolumeIdentity, display_path, drive_key, path_key};
use std::fs::{self, File};
use std::io::{Read, Result as IoResult};
use std::path::{Path, PathBuf};

pub struct NativeScanSource {
    root: PathBuf,
}
impl NativeScanSource {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().into(),
        }
    }
}
struct ExportFile {
    file: Option<File>,
    path: PathBuf,
    _directories: Vec<File>,
}
impl Read for ExportFile {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        self.file.as_mut().expect("export file is open").read(buf)
    }
}
impl Drop for ExportFile {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}
impl ScanSource for NativeScanSource {
    fn identity(&self, drive: &str) -> Result<VolumeIdentity, String> {
        volume_identity(drive)
    }
    fn export(&self, root: &Path, control: &Control) -> Result<Box<dyn Read + Send>, String> {
        control.check()?;
        let layout = cst_os::KitLayout::new(&self.root)?;
        let executable = layout.wiztree_dir().join("WizTree64.exe");
        if !executable.is_file() {
            return Err("随包 WizTree 不可用。".into());
        }
        #[cfg(windows)]
        if let Some(drive) = drive_key(root)
            && path_key(root) == drive
            && volume_identity(&drive).is_ok_and(|id| id.filesystem.eq_ignore_ascii_case("NTFS"))
            && !cst_os::is_elevated().unwrap_or(false)
        {
            return Err("当前非管理员权限，无法通过 WizTree 读取 NTFS 主文件表（MFT）。".into());
        }
        let directories = pin_temporary(layout.root())?;
        let path = layout
            .wiztree_tmp_dir()
            .join(format!("wz-{}.csv", uuid::Uuid::new_v4()));
        let mut exported = ExportFile {
            file: None,
            path: path.clone(),
            _directories: directories,
        };
        let cli_root = wiztree_target_arg(root);
        let cli_export = display_path(&path);
        super::native_job::run(
            &executable,
            &[
                cli_root.into(),
                "/admin=0".into(),
                format!("/export={cli_export}").into(),
                "/exportfolders=1".into(),
                "/exportfiles=1".into(),
            ],
            control,
            1024 * 1024,
        )?;
        control.check()?;
        let meta = fs::symlink_metadata(&path).map_err(|_| "WizTree 未生成导出文件。")?;
        if super::walk::linked(&meta) || !meta.is_file() {
            return Err("WizTree 导出路径无效。".into());
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let file = options
            .open(&path)
            .map_err(|_| "WizTree 导出文件无法打开。")?;
        let handle =
            same_file::Handle::from_file(file.try_clone().map_err(|_| "无法验证 CSV 文件身份。")?)
                .map_err(|_| "无法验证 CSV 文件身份。")?;
        if handle != same_file::Handle::from_path(&path).map_err(|_| "无法验证 CSV 文件身份。")?
        {
            return Err("WizTree 导出文件身份发生变化。".into());
        }
        exported.file = Some(file);
        Ok(Box::new(exported))
    }
}
fn wiztree_target_arg(root: &Path) -> String {
    let clean = display_path(root);
    let trimmed = clean.trim_end_matches(['\\', '/']);
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        format!("{trimmed}\\")
    } else if trimmed.is_empty() {
        clean
    } else {
        trimmed.to_owned()
    }
}

fn pin_temporary(root: &Path) -> Result<Vec<File>, String> {
    let mut files = Vec::new();
    let mut path = root.to_path_buf();
    for part in ["", ".state", "tmp", "wiztree"] {
        if !part.is_empty() {
            path.push(part);
        }
        match fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err("WizTree 临时目录无法创建。".into()),
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options
                .access_mode(0)
                .share_mode(3)
                .custom_flags(0x02000000 | 0x00200000);
        }
        let file = options
            .open(&path)
            .map_err(|_| "WizTree 临时目录无法锁定。")?;
        let meta = file.metadata().map_err(|_| "WizTree 临时目录无法验证。")?;
        if super::walk::linked(&meta) || !meta.is_dir() {
            return Err("WizTree 临时目录包含链接或重解析点。".into());
        }
        files.push(file);
    }
    Ok(files)
}
#[cfg(windows)]
fn volume_identity(drive: &str) -> Result<VolumeIdentity, String> {
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, GetVolumeInformationW,
    };
    use windows::core::PCWSTR;
    if drive.len() != 2 || !drive.as_bytes()[0].is_ascii_alphabetic() || drive.as_bytes()[1] != b':'
    {
        return Err("卷身份查询需要有效盘符。".into());
    }
    let path = format!("{drive}\\");
    let wide: Vec<u16> = std::ffi::OsStr::new(&path)
        .encode_wide()
        .chain(Some(0))
        .collect();
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
    })
}
#[cfg(not(windows))]
fn volume_identity(_: &str) -> Result<VolumeIdentity, String> {
    Err("卷身份查询仅支持 Windows。".into())
}

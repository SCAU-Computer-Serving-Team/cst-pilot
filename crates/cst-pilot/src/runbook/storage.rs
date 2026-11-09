use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct Storage {
    pub root: PathBuf,
}
impl Storage {
    pub fn new(root: &Path) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|_| "工具包目录无法读取。")?;
        if !root.is_dir() {
            return Err("工具包根路径必须是目录。".into());
        }
        Ok(Self { root })
    }
    /// Inspect each component, including existing in-kit links; never create through a link.
    pub fn directory(&self, relative: &Path, create: bool) -> Result<PathBuf, String> {
        let mut path = self.root.clone();
        for part in relative.components() {
            let Component::Normal(part) = part else {
                return Err("交付目录路径无效。".into());
            };
            path.push(part);
            if create {
                match fs::create_dir(&path) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(io_error("创建交付目录", e)),
                }
            }
            let meta = fs::symlink_metadata(&path).map_err(|e| io_error("检查交付目录", e))?;
            if linked(&meta) || !meta.is_dir() {
                return Err("交付目录包含链接、重解析点或非目录路径。".into());
            }
            if path.canonicalize().map_err(|_| "交付目录无法解析。")? != path {
                return Err("交付目录真实路径发生变化。".into());
            }
        }
        Ok(path)
    }
    /// Keep every ancestor open without delete sharing on Windows. This prevents
    /// junction/ancestor renames between validation and the atomic file commit.
    pub fn pin_directory(&self, relative: &Path, create: bool) -> Result<DirectoryLease, String> {
        let mut handles = Vec::new();
        handles.push(pin(&self.root)?);
        let mut partial = PathBuf::new();
        for component in relative.components() {
            let Component::Normal(part) = component else {
                return Err("交付目录路径无效。".into());
            };
            partial.push(part);
            let path = self.directory(&partial, create)?;
            handles.push(pin(&path)?);
        }
        Ok(DirectoryLease { _handles: handles })
    }
    pub fn check_parent(&self, file: &Path) -> Result<(), String> {
        let relative = file
            .parent()
            .and_then(|p| p.strip_prefix(&self.root).ok())
            .ok_or("交付路径超出工具包。")?;
        self.directory(relative, false).map(|_| ())
    }
    pub fn read(&self, file: &Path) -> Result<Option<Vec<u8>>, String> {
        self.read_limited(file, 256 * 1024)
    }
    pub fn open_read(&self, file: &Path) -> Result<Option<File>, String> {
        self.check_parent(file)?;
        if !check_file(file)? {
            return Ok(None);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let opened = options
            .open(file)
            .map_err(|e| io_error("读取交付文件", e))?;
        validate_open(&opened, file)?;
        Ok(Some(opened))
    }
    pub fn read_limited(&self, file: &Path, limit: usize) -> Result<Option<Vec<u8>>, String> {
        let Some(opened) = self.open_read(file)? else {
            return Ok(None);
        };
        if opened
            .metadata()
            .map_err(|e| io_error("读取文件信息", e))?
            .len()
            > limit as u64
        {
            return Err("方案或清单文件超过 256 KiB，请检查文件。".into());
        }
        let mut bytes = Vec::new();
        opened
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("读取交付文件", e))?;
        if bytes.len() > limit {
            return Err("方案或清单文件超过 256 KiB，请检查文件。".into());
        }
        self.check_parent(file)?;
        Ok(Some(bytes))
    }
    pub fn atomic(
        &self,
        file: &Path,
        bytes: &[u8],
        replace: bool,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        cancelled(cancel)?;
        self.check_parent(file)?;
        if check_file(file)? && !replace {
            return Err("目标文件已存在，请核对后重试。".into());
        }
        let temporary = file.with_file_name(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let cleanup = Temporary(temporary.clone());
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| io_error("创建交付临时文件", e))?;
        output
            .write_all(bytes)
            .and_then(|_| output.sync_all())
            .map_err(|e| io_error("同步交付临时文件", e))?;
        validate_open(&output, &temporary)?;
        drop(output);
        cancelled(cancel)?;
        self.check_parent(file)?;
        check_file(file)?;
        rename(&temporary, file, replace).map_err(|e| io_error("原子保存交付文件", e))?;
        sync_directory(file.parent().ok_or("文件缺少目录。")?)?;
        drop(cleanup);
        Ok(())
    }
}
pub(crate) struct DirectoryLease {
    _handles: Vec<File>,
}
fn pin(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(0)
            .share_mode(3)
            .custom_flags(0x0200_0000 | 0x0020_0000);
    }
    let file = options
        .open(path)
        .map_err(|e| io_error("锁定交付目录路径", e))?;
    let meta = file
        .metadata()
        .map_err(|e| io_error("读取交付目录身份", e))?;
    if linked(&meta) || !meta.is_dir() {
        return Err("交付目录包含链接或重解析点。".into());
    }
    Ok(file)
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub(super) fn cancelled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("命令交付已取消。".into())
    } else {
        Ok(())
    }
}
pub(super) fn io_error(action: &str, e: std::io::Error) -> String {
    format!(
        "{action}失败（系统错误 {}）：{e}",
        e.raw_os_error().unwrap_or_default()
    )
}
fn linked(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
fn check_file(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !linked(&meta) && meta.is_file() => Ok(true),
        Ok(_) => Err("方案或清单路径包含链接、重解析点或非普通文件。".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error("检查交付文件", e)),
    }
}
fn validate_open(file: &File, path: &Path) -> Result<(), String> {
    let handle =
        same_file::Handle::from_file(file.try_clone().map_err(|e| io_error("复制文件句柄", e))?)
            .map_err(|e| io_error("读取文件身份", e))?;
    if !check_file(path)?
        || handle != same_file::Handle::from_path(path).map_err(|e| io_error("核对文件身份", e))?
    {
        return Err("方案或清单文件身份已变化。".into());
    }
    #[cfg(windows)]
    let links = {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| "文件链接数量无法读取。")?;
        u64::from(info.nNumberOfLinks)
    };
    #[cfg(unix)]
    let links = {
        use std::os::unix::fs::MetadataExt;
        file.metadata()
            .map_err(|e| io_error("读取文件身份", e))?
            .nlink()
    };
    #[cfg(not(any(unix, windows)))]
    let links = 1;
    if links != 1 {
        return Err("方案或清单不能是硬链接文件。".into());
    }
    Ok(())
}
fn rename(from: &Path, to: &Path, replace: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            },
            core::PCWSTR,
        };
        let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut flags = MOVEFILE_WRITE_THROUGH;
        if replace {
            flags |= MOVEFILE_REPLACE_EXISTING;
        }
        unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), flags) }
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xffff))
    }
    #[cfg(not(windows))]
    {
        if replace {
            fs::rename(from, to)
        } else {
            fs::hard_link(from, to)?;
            fs::remove_file(from)
        }
    }
}
fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| io_error("同步交付目录", e))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

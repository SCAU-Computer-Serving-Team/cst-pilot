use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use uuid::Uuid;

const RETRY_DELAYS_MS: [u64; 5] = [10, 20, 45, 100, 200];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageErrorKind {
    StorageFull,
    StorageUnavailable,
}

#[derive(Debug, Clone)]
pub struct StorageError {
    pub kind: StorageErrorKind,
    pub message: String,
}

impl StorageError {
    pub fn code(&self) -> &'static str {
        match self.kind {
            StorageErrorKind::StorageFull => "storage_full",
            StorageErrorKind::StorageUnavailable => "storage_unavailable",
        }
    }

    pub fn status_code(&self) -> u16 {
        match self.kind {
            StorageErrorKind::StorageFull => 507,
            StorageErrorKind::StorageUnavailable => 500,
        }
    }

    pub fn from_io(err: &io::Error, context: &str) -> Self {
        if is_storage_full_io(err) {
            Self {
                kind: StorageErrorKind::StorageFull,
                message: format!("{context}：存储空间不足，输入或配置未被接受。"),
            }
        } else {
            Self {
                kind: StorageErrorKind::StorageUnavailable,
                message: format!("{context}：存储介质不可写或访问受拒。"),
            }
        }
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

pub fn is_storage_full_io(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::StorageFull {
        return true;
    }
    matches!(err.raw_os_error(), Some(28 | 39 | 112))
}

pub fn is_transient_replace_error(err: &io::Error) -> bool {
    if is_storage_full_io(err) {
        return false;
    }
    if matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    matches!(err.raw_os_error(), Some(5 | 32 | 33))
}

/// Synchronously write `bytes` to a sibling temp file, `sync_all()`, and atomically rename
/// over `target`, retrying transient Windows sharing/permission locks up to 5 times (<= 375ms).
pub fn atomic_write_bytes_sync(
    target: &Path,
    bytes: &[u8],
    context: &str,
) -> Result<(), StorageError> {
    let parent = target.parent().ok_or_else(|| StorageError {
        kind: StorageErrorKind::StorageUnavailable,
        message: format!("{context}：目标路径缺少父目录。"),
    })?;
    fs::create_dir_all(parent).map_err(|e| StorageError::from_io(&e, context))?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("state");
    let tmp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let write_res = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(StorageError::from_io(&err, context));
    }

    let mut last_err = None;
    for (attempt, delay_ms) in RETRY_DELAYS_MS.iter().copied().enumerate() {
        match fs::rename(&tmp_path, target) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let retryable = is_transient_replace_error(&err);
                last_err = Some(err);
                if !retryable {
                    break;
                }
                std::thread::sleep(Duration::from_millis(delay_ms));
                if attempt + 1 == RETRY_DELAYS_MS.len() {
                    if let Err(final_err) = fs::rename(&tmp_path, target) {
                        last_err = Some(final_err);
                    } else {
                        return Ok(());
                    }
                }
            }
        }
    }

    let _ = fs::remove_file(&tmp_path);
    let err = last_err.unwrap_or_else(|| io::Error::other("atomic rename failed"));
    Err(StorageError::from_io(&err, context))
}

pub fn atomic_write_json_sync<T: Serialize>(
    target: &Path,
    value: &T,
    context: &str,
) -> Result<(), StorageError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| StorageError {
        kind: StorageErrorKind::StorageUnavailable,
        message: format!("{context}：JSON 序列化失败。"),
    })?;
    bytes.push(b'\n');
    atomic_write_bytes_sync(target, &bytes, context)
}

/// Anchored, reparse-point-safe, hardlink-rejecting storage within a canonical kit root.
#[derive(Clone)]
pub struct Storage {
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
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(io_error("创建交付目录", e)),
                }
            }
            let meta = fs::symlink_metadata(&path).map_err(|e| io_error("检查交付目录", e))?;
            if is_reparse_or_symlink(&meta) || !meta.is_dir() {
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
        let too_large = || {
            if limit == 256 * 1024 {
                "方案或清单文件超过 256 KiB，请检查文件。".to_owned()
            } else {
                format!("文件超过大小上限（{limit} 字节），请检查文件。")
            }
        };
        if opened
            .metadata()
            .map_err(|e| io_error("读取文件信息", e))?
            .len()
            > limit as u64
        {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        opened
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("读取交付文件", e))?;
        if bytes.len() > limit {
            return Err(too_large());
        }
        self.check_parent(file)?;
        Ok(Some(bytes))
    }

    pub fn atomic_checked(
        &self,
        file: &Path,
        bytes: &[u8],
        replace: bool,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        if is_cancelled() {
            return Err("命令交付已取消。".into());
        }
        self.check_parent(file)?;
        if check_file(file)? && !replace {
            return Err("目标文件已存在，请核对后重试。".into());
        }
        let temporary = file.with_file_name(format!(".{}.tmp", Uuid::new_v4()));
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
        if is_cancelled() {
            return Err("命令交付已取消。".into());
        }
        self.check_parent(file)?;
        check_file(file)?;
        rename_file(&temporary, file, replace).map_err(|e| io_error("原子保存交付文件", e))?;
        sync_directory(file.parent().ok_or("文件缺少目录。")?)?;
        drop(cleanup);
        Ok(())
    }
}

pub struct DirectoryLease {
    _handles: Vec<File>,
}

impl DirectoryLease {
    pub fn leaf_same_handle(&self, other: &Path) -> Result<bool, String> {
        let Some(leaf) = self._handles.last() else {
            return Ok(false);
        };
        let leaf_h = same_file::Handle::from_file(
            leaf.try_clone()
                .map_err(|e| io_error("复制目录租约句柄", e))?,
        )
        .map_err(|e| io_error("读取目录租约身份", e))?;
        let other_h =
            same_file::Handle::from_path(other).map_err(|e| io_error("核对目标目录身份", e))?;
        Ok(leaf_h == other_h)
    }
}

fn pin(path: &Path) -> Result<File, String> {
    let lmeta = fs::symlink_metadata(path).map_err(|e| io_error("检查交付目录链接状态", e))?;
    if is_reparse_or_symlink(&lmeta) || !lmeta.is_dir() {
        return Err("交付目录包含链接或重解析点。".into());
    }
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
    if is_reparse_or_symlink(&meta) || !meta.is_dir() {
        return Err("交付目录包含链接或重解析点。".into());
    }
    let opened_h =
        same_file::Handle::from_file(file.try_clone().map_err(|e| io_error("复制目录句柄", e))?)
            .map_err(|e| io_error("读取打开目录身份", e))?;
    let path_h = same_file::Handle::from_path(path).map_err(|e| io_error("复核目录身份", e))?;
    if opened_h != path_h {
        return Err("交付目录真实身份发生变化。".into());
    }
    Ok(file)
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn io_error(action: &str, e: io::Error) -> String {
    format!(
        "{action}失败（系统错误 {}）：{e}",
        e.raw_os_error().unwrap_or_default()
    )
}

pub fn is_reparse_or_symlink(meta: &Metadata) -> bool {
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
        Ok(meta) if !is_reparse_or_symlink(&meta) && meta.is_file() => Ok(true),
        Ok(_) => Err("方案或清单路径包含链接、重解析点或非普通文件。".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
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
    if link_count(file)? != 1 {
        return Err("方案或清单不能是硬链接文件。".into());
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileIdentity(same_file::Handle);

impl FileIdentity {
    pub fn from_file(file: &File) -> Result<Self, String> {
        let cloned = file.try_clone().map_err(|e| io_error("复制文件句柄", e))?;
        same_file::Handle::from_file(cloned)
            .map(Self)
            .map_err(|e| io_error("读取已打开句柄身份", e))
    }

    pub fn from_path(path: &Path) -> Result<Self, String> {
        same_file::Handle::from_path(path)
            .map(Self)
            .map_err(|e| io_error("读取路径文件身份", e))
    }
}

pub fn pin_directory_handle(path: &Path) -> Result<(File, FileIdentity), String> {
    let f = pin(path)?;
    let id = FileIdentity::from_file(&f)?;
    Ok((f, id))
}

pub fn commit_new_file_no_overwrite(temp_path: &Path, target_path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        rename_file(temp_path, target_path, false).map_err(|e| io_error("原子创建目标文件", e))?;
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(temp_path, target_path).map_err(|e| io_error("原子链接新目标文件", e))?;
        let _ = fs::remove_file(temp_path);
    }
    if let Some(parent) = target_path.parent()
        && !parent.as_os_str().is_empty()
    {
        sync_directory(parent)?;
    }
    Ok(())
}

pub fn is_same_existing_file(a: &Path, b: &Path) -> bool {
    same_file::is_same_file(a, b).unwrap_or(false)
}

pub fn link_count(file: &File) -> Result<u64, String> {
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
    Ok(links)
}

fn rename_file(from: &Path, to: &Path, replace: bool) -> io::Result<()> {
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
            .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff))
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

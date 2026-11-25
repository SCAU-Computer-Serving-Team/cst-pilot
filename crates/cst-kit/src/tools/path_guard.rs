use std::fs::{File, Metadata};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cst_os::KitLayout;
use pi_runtime::ReadOperations;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FileIdentity {
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index: u64,
    #[cfg(not(windows))]
    dev: u64,
    #[cfg(not(windows))]
    ino: u64,
}

#[derive(Clone, Copy, Debug)]
struct HandleMeta {
    identity: FileIdentity,
    link_count: u64,
    is_dir: bool,
    is_file: bool,
    file_size: u64,
}

#[derive(Clone)]
pub struct PathGuard {
    layout: KitLayout,
}

/// Single-operation security baseline created per call and dropped immediately after.
/// Holds no open OS handles and never persists across calls.
#[derive(Clone)]
pub struct PathGuardSession {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    state_dir: PathBuf,
    state_keys: Vec<String>,
    state_identity: Option<FileIdentity>,
    secret_paths: [PathBuf; 5],
    secret_keys: Vec<String>,
    secret_identities: Vec<FileIdentity>,
    #[cfg(test)]
    checks: AtomicUsize,
}

impl PathGuard {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let layout = KitLayout::new(root)?;
        Ok(Self { layout })
    }

    pub fn from_layout(layout: KitLayout) -> Self {
        Self { layout }
    }

    fn secret_paths(&self) -> [PathBuf; 5] {
        let cfg = self.layout.config_dir();
        [
            cfg.join("auth.json"),
            cfg.join("models.json"),
            cfg.join("mcp-auth.json"),
            cfg.join("web-search.json"),
            self.layout.telemetry_data_dir().join("identity.json"),
        ]
    }

    /// Build a fresh per-operation baseline; never cached across tool calls.
    pub fn session(&self) -> PathGuardSession {
        let state_dir = self.layout.state_dir();
        let mut state_keys = Vec::with_capacity(2);
        push_unique_key(&mut state_keys, &state_dir);
        let state_identity = inspect_path(&state_dir).ok().map(|(resolved, meta)| {
            push_unique_key(&mut state_keys, &resolved);
            meta.identity
        });

        let secret_paths = self.secret_paths();
        let mut secret_keys = Vec::with_capacity(10);
        let mut secret_identities = Vec::with_capacity(5);
        for secret in &secret_paths {
            push_unique_key(&mut secret_keys, secret);
            if let Ok((resolved, meta)) = inspect_path(secret) {
                push_unique_key(&mut secret_keys, &resolved);
                secret_identities.push(meta.identity);
            }
        }

        PathGuardSession {
            inner: Arc::new(SessionInner {
                state_dir,
                state_keys,
                state_identity,
                secret_paths,
                secret_keys,
                secret_identities,
                #[cfg(test)]
                checks: AtomicUsize::new(0),
            }),
        }
    }

    /// Resolve aliases before comparing; creates a fresh baseline on every access.
    pub fn check(&self, path: impl AsRef<Path>) -> Result<PathBuf, String> {
        self.session().check_path(path)
    }

    /// Open and verify using a single handle to prevent TOCTOU and hard-link bypasses.
    pub fn open(&self, path: impl AsRef<Path>) -> Result<File, String> {
        self.session().open_file(path).map(|(file, _)| file)
    }
}

impl PathGuardSession {
    #[cfg(test)]
    pub(crate) fn check_count(&self) -> usize {
        self.inner.checks.load(Ordering::Relaxed)
    }

    fn record_check(&self) {
        #[cfg(test)]
        self.inner.checks.fetch_add(1, Ordering::Relaxed);
    }

    /// Prune protected paths and refuse recursion into discovered directory links.
    pub fn allow_walk_dir(&self, path: &Path) -> bool {
        if self.verify_requested(path).is_err()
            || !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir() && !is_link(&meta))
        {
            return false;
        }
        let Some(state_id) = self.inner.state_identity else {
            return true;
        };
        path_meta(path).is_ok_and(|meta| meta.identity != state_id)
    }

    /// Check an arbitrary path (file or directory) against all protected targets.
    pub fn check_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, String> {
        self.record_check();
        let path = path.as_ref();
        self.verify_requested(path)?;
        let (resolved, meta) = inspect_path(path).map_err(|_| "目标路径无法读取。")?;
        self.verify_resolved(&resolved, meta)?;
        Ok(resolved)
    }

    /// Open a regular file once and verify identity, canonical path, and hard links on that handle.
    pub fn open_file(&self, path: impl AsRef<Path>) -> Result<(File, u64), String> {
        self.record_check();
        let path = path.as_ref();
        self.verify_requested(path)?;
        let file = open_read_handle(path).map_err(|_| "目标文件无法打开。")?;
        let (resolved, meta) = inspect_opened_file(&file, path).map_err(|_| denied())?;
        if !meta.is_file {
            return Err("目标路径不是普通文件。".into());
        }
        self.verify_resolved(&resolved, meta)?;
        Ok((file, meta.file_size))
    }

    pub(crate) fn verify_requested(&self, path: &Path) -> Result<(), String> {
        reject_unsafe_windows_path(path)?;
        if path.is_absolute() {
            self.verify_key(path)
        } else {
            self.verify_key(&std::path::absolute(path).map_err(|_| denied())?)
        }
    }

    fn verify_key(&self, path: &Path) -> Result<(), String> {
        let key = path_key(path);
        if self.is_state_key(&key) || self.inner.secret_keys.contains(&key) {
            return Err(denied());
        }
        Ok(())
    }

    fn verify_resolved(&self, resolved: &Path, meta: HandleMeta) -> Result<(), String> {
        self.verify_key(resolved)?;
        if self.inner.secret_identities.contains(&meta.identity) {
            return Err(denied());
        }
        if let Some(state_id) = self.inner.state_identity {
            if meta.identity == state_id {
                return Err(denied());
            }
            // A canonical name can still use an alternate volume or mount name.
            for ancestor in resolved.ancestors().skip(1) {
                if path_meta(ancestor).is_ok_and(|meta| meta.identity == state_id) {
                    return Err(denied());
                }
            }
        }

        if meta.is_file {
            match meta.link_count {
                0 => return Err(denied()),
                1 => (),
                _ => self.verify_hard_link(meta.identity)?,
            }
        }
        Ok(())
    }

    fn is_state_key(&self, key: &str) -> bool {
        self.inner.state_keys.iter().any(|state| {
            key == state
                || (key.len() > state.len()
                    && key.starts_with(state)
                    && key.as_bytes()[state.len()] == b'/')
        })
    }

    // The per-call baseline remains useful for ordinary files. Hard links need live
    // membership checks because protected files can be created or replaced mid-call.
    fn verify_hard_link(&self, identity: FileIdentity) -> Result<(), String> {
        for secret in &self.inner.secret_paths {
            match path_meta(secret) {
                Ok(meta) if meta.identity == identity => return Err(denied()),
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(_) => return Err(denied()),
            }
        }

        let state_dir = &self.inner.state_dir;
        let (state, state_meta) = match inspect_path(state_dir) {
            Ok((state, meta)) if meta.is_dir => (state, meta),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            _ => return Err(denied()),
        };
        let mut walker = ignore::WalkBuilder::new(&state);
        walker.standard_filters(false).follow_links(false);
        for (visited, entry) in walker.build().enumerate() {
            if visited >= 100_000 {
                return Err(denied());
            }
            let entry = entry.map_err(|_| denied())?;
            let meta = std::fs::symlink_metadata(entry.path()).map_err(|_| denied())?;
            if is_link(&meta) {
                return Err(denied());
            }
            if meta.is_file()
                && (path_meta(entry.path()).map_err(|_| denied())?.identity == identity)
            {
                return Err(denied());
            }
        }
        if path_meta(state_dir).map_err(|_| denied())?.identity != state_meta.identity {
            return Err(denied());
        }
        Ok(())
    }
}

fn is_link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
    #[cfg(not(windows))]
    meta.file_type().is_symlink()
}

#[cfg(windows)]
fn open_read_handle(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(not(windows))]
fn open_read_handle(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // A non-regular entry can appear between enumeration and opening. In
    // particular, don't block waiting for a writer if the name is now a FIFO.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

fn push_unique_key(target: &mut Vec<String>, path: &Path) {
    let key = path_key(path);
    if !target.contains(&key) {
        target.push(key);
    }
}

#[cfg(windows)]
fn open_metadata_handle(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    std::fs::OpenOptions::new()
        .read(false)
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)
}

#[cfg(windows)]
fn handle_meta(file: &File) -> std::io::Result<HandleMeta> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_TYPE_DISK,
        GetFileInformationByHandle, GetFileType,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let handle = HANDLE(file.as_raw_handle());
    // SAFETY: `handle` is borrowed from a live `File` for both Win32 calls.
    if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
        return Err(io::Error::other("not a disk file"));
    }
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(|error| {
        let code = error.code().0 as u32;
        if code >> 16 == 0x8007 {
            io::Error::from_raw_os_error((code & 0xffff) as i32)
        } else {
            io::Error::other(error)
        }
    })?;
    let is_dir = (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0;
    Ok(HandleMeta {
        identity: FileIdentity {
            volume: info.dwVolumeSerialNumber,
            index: ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64),
        },
        link_count: u64::from(info.nNumberOfLinks),
        is_dir,
        is_file: !is_dir,
        file_size: ((info.nFileSizeHigh as u64) << 32) | (info.nFileSizeLow as u64),
    })
}

#[cfg(windows)]
fn handle_final_path(file: &File) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW};

    let handle = HANDLE(file.as_raw_handle());
    let mut stack = [0u16; 512];
    // SAFETY: `handle` is live and `stack` is a valid writable UTF-16 buffer.
    let len =
        unsafe { GetFinalPathNameByHandleW(handle, &mut stack, FILE_NAME_NORMALIZED) } as usize;
    if len == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if len < stack.len() {
        return Ok(PathBuf::from(OsString::from_wide(&stack[..len])));
    }
    let mut heap = vec![0u16; len + 1];
    // SAFETY: `handle` is live and `heap` has enough capacity for the full normalized path.
    let len2 =
        unsafe { GetFinalPathNameByHandleW(handle, &mut heap, FILE_NAME_NORMALIZED) } as usize;
    if len2 == 0 || len2 >= heap.len() {
        return Err(std::io::Error::last_os_error());
    }
    Ok(PathBuf::from(OsString::from_wide(&heap[..len2])))
}

#[cfg(windows)]
fn path_meta(path: &Path) -> std::io::Result<HandleMeta> {
    let file = open_metadata_handle(path)?;
    handle_meta(&file)
}

#[cfg(windows)]
fn inspect_path(path: &Path) -> io::Result<(PathBuf, HandleMeta)> {
    let file = open_metadata_handle(path)?;
    inspect_opened_file(&file, path)
}

#[cfg(windows)]
fn inspect_opened_file(file: &File, _path: &Path) -> io::Result<(PathBuf, HandleMeta)> {
    Ok((handle_final_path(file)?, handle_meta(file)?))
}

#[cfg(not(windows))]
fn metadata_to_handle_meta(meta: &std::fs::Metadata) -> HandleMeta {
    use std::os::unix::fs::MetadataExt;
    HandleMeta {
        identity: FileIdentity {
            dev: meta.dev(),
            ino: meta.ino(),
        },
        link_count: meta.nlink(),
        is_dir: meta.is_dir(),
        is_file: meta.is_file(),
        file_size: meta.len(),
    }
}

#[cfg(not(windows))]
fn path_meta(path: &Path) -> std::io::Result<HandleMeta> {
    std::fs::metadata(path).map(|meta| metadata_to_handle_meta(&meta))
}

#[cfg(not(windows))]
fn inspect_path(path: &Path) -> io::Result<(PathBuf, HandleMeta)> {
    let resolved = path.canonicalize()?;
    let meta = path_meta(&resolved)?;
    Ok((resolved, meta))
}

#[cfg(not(windows))]
fn inspect_opened_file(file: &File, path: &Path) -> io::Result<(PathBuf, HandleMeta)> {
    let opened = metadata_to_handle_meta(&file.metadata()?);
    let (resolved, current) = inspect_path(path)?;
    if opened.identity != current.identity {
        return Err(io::Error::other("path no longer identifies the open file"));
    }
    Ok((resolved, opened))
}

fn denied() -> String {
    "权限拒绝：禁止读取或枚举凭据文件与运行态目录。".into()
}

pub(crate) fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    let value = value.replace('\\', "/").to_lowercase();
    let trimmed = value.trim_end_matches('/');
    #[cfg(windows)]
    if let Some(rest) = trimmed.strip_prefix("//?/unc/") {
        return format!("//{rest}");
    }
    #[cfg(windows)]
    if let Some(rest) = trimmed.strip_prefix("//?/") {
        return rest.to_owned();
    }
    trimmed.to_owned()
}

fn reject_unsafe_windows_path(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};

        // Colons are only allowed in an absolute DOS drive prefix. Limit
        // extended paths to DOS/UNC and reject other Win32 namespaces.
        let raw = path.to_string_lossy();
        let colon = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(_) if path.is_absolute() => Some(1),
                Prefix::VerbatimDisk(_) if path.is_absolute() => Some(5),
                Prefix::UNC(server, _) if server != "." && server != "?" => None,
                Prefix::VerbatimUNC(..) => None,
                _ => return Err(denied()),
            },
            _ => None,
        };
        if raw
            .match_indices(':')
            .any(|(index, _)| Some(index) != colon)
        {
            return Err(denied());
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

#[derive(Clone)]
pub struct GuardedReadOperations(pub PathGuard);

impl ReadOperations for GuardedReadOperations {
    fn access(&self, path: &Path) -> Result<(), String> {
        self.0.open(path).map(|_| ())
    }

    fn detect_image_mime_type(&self, path: &Path) -> Result<Option<String>, String> {
        let mut head = [0u8; 16];
        let n = self
            .0
            .open(path)?
            .read(&mut head)
            .map_err(|_| "文件类型无法读取。")?;
        let bytes = &head[..n];
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some("image/jpeg")
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some("image/gif")
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some("image/webp")
        } else if bytes.starts_with(b"BM") {
            Some("image/bmp")
        } else {
            None
        };
        Ok(mime.map(str::to_owned))
    }

    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.0
            .open(path)?
            .read_to_end(&mut bytes)
            .map_err(|_| "文件无法读取。")?;
        Ok(bytes)
    }

    fn open_reader(&self, path: &Path) -> Result<Box<dyn BufRead + Send>, String> {
        Ok(Box::new(BufReader::new(self.0.open(path)?)))
    }
}

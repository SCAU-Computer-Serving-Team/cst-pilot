use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

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
    secret_keys: Vec<String>,
    secret_identities: Vec<FileIdentity>,
    state_file_identities: OnceLock<Result<HashSet<FileIdentity>, String>>,
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
        let mut state_identity = None;
        if let Ok(resolved) = state_dir.canonicalize() {
            push_unique_key(&mut state_keys, &resolved);
            if let Ok(meta) = path_meta(&resolved) {
                state_identity = Some(meta.identity);
            }
        } else if let Ok(meta) = path_meta(&state_dir) {
            state_identity = Some(meta.identity);
        }

        let mut secret_keys = Vec::with_capacity(10);
        let mut secret_identities = Vec::with_capacity(5);
        for secret in self.secret_paths() {
            push_unique_key(&mut secret_keys, &secret);
            if let Ok(resolved) = secret.canonicalize() {
                push_unique_key(&mut secret_keys, &resolved);
                if let Ok(meta) = path_meta(&resolved)
                    && !secret_identities.contains(&meta.identity)
                {
                    secret_identities.push(meta.identity);
                }
            } else if let Ok(meta) = path_meta(&secret)
                && !secret_identities.contains(&meta.identity)
            {
                secret_identities.push(meta.identity);
            }
        }

        PathGuardSession {
            inner: Arc::new(SessionInner {
                state_dir,
                state_keys,
                state_identity,
                secret_keys,
                secret_identities,
                state_file_identities: OnceLock::new(),
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

    /// Fast directory check for non-symlink entries during `follow_links(false)` traversal.
    /// Does not open handles when the canonical path key cleanly misses `.state/`.
    pub fn allow_walk_dir(&self, path: &Path) -> bool {
        if reject_alternate_stream(path).is_err() {
            return false;
        }
        let key = path_key(path);
        !self.is_state_key(&key)
    }

    /// Check an arbitrary path (file or directory) against all protected targets.
    pub fn check_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, String> {
        self.record_check();
        let path = path.as_ref();
        reject_alternate_stream(path)?;
        let (resolved, meta) = inspect_path(path)?;
        self.verify_resolved(&resolved, meta)?;
        Ok(resolved)
    }

    /// Open a regular file once and verify identity, canonical path, and hard links on that handle.
    pub fn open_file(&self, path: impl AsRef<Path>) -> Result<(File, u64), String> {
        self.record_check();
        let path = path.as_ref();
        reject_alternate_stream(path)?;
        let file = File::open(path).map_err(|_| "目标文件无法打开。")?;
        let (resolved, meta) = inspect_opened_file(&file, path)?;
        if meta.is_dir {
            return Err("目标路径不是普通文件。".into());
        }
        self.verify_resolved(&resolved, meta)?;
        Ok((file, meta.file_size))
    }

    fn verify_resolved(&self, resolved: &Path, meta: HandleMeta) -> Result<(), String> {
        let key = path_key(resolved);
        if self.is_state_key(&key) || self.is_secret_key(&key) {
            return Err(denied());
        }
        if let Some(state_id) = self.inner.state_identity {
            if meta.identity == state_id {
                return Err(denied());
            }
            if !meta.is_dir && self.inner.secret_identities.contains(&meta.identity) {
                return Err(denied());
            }
            // Check ancestors for directory aliases pointing at `.state/`.
            if meta.is_dir {
                for ancestor in resolved.ancestors().skip(1) {
                    if let Ok(anc_meta) = path_meta(ancestor)
                        && anc_meta.identity == state_id
                    {
                        return Err(denied());
                    }
                }
            }
        } else if !meta.is_dir && self.inner.secret_identities.contains(&meta.identity) {
            return Err(denied());
        }

        // Only multiply-linked regular files need `.state/` identity membership checks.
        if !meta.is_dir && meta.link_count > 1 && self.inner.state_identity.is_some() {
            let protected = self.state_file_identities()?;
            if protected.contains(&meta.identity) {
                return Err(denied());
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

    fn is_secret_key(&self, key: &str) -> bool {
        self.inner.secret_keys.iter().any(|secret| key == secret)
    }

    fn state_file_identities(&self) -> Result<&HashSet<FileIdentity>, String> {
        let result = self.inner.state_file_identities.get_or_init(|| {
            let state = match self.inner.state_dir.canonicalize() {
                Ok(state) => state,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(HashSet::new());
                }
                Err(_) => return Err(denied()),
            };
            let mut ids = HashSet::new();
            let mut walker = ignore::WalkBuilder::new(state);
            walker
                .hidden(false)
                .ignore(false)
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .parents(false)
                .follow_links(false);
            for (visited, entry) in walker.build().enumerate() {
                if visited >= 100_000 {
                    return Err(denied());
                }
                let entry = entry.map_err(|_| denied())?;
                if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
                    return Err(denied());
                }
                if entry.file_type().is_some_and(|kind| kind.is_file()) {
                    match path_meta(entry.path()) {
                        Ok(meta) => {
                            ids.insert(meta.identity);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(_) => return Err(denied()),
                    }
                }
            }
            Ok(ids)
        });
        result.as_ref().map_err(Clone::clone)
    }
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
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let handle = HANDLE(file.as_raw_handle());
    // SAFETY: `handle` is borrowed from a live `File` for the duration of the Win32 call.
    unsafe { GetFileInformationByHandle(handle, &mut info) }
        .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))?;
    Ok(HandleMeta {
        identity: FileIdentity {
            volume: info.dwVolumeSerialNumber,
            index: ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64),
        },
        link_count: u64::from(info.nNumberOfLinks),
        is_dir: (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0,
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
fn inspect_path(path: &Path) -> Result<(PathBuf, HandleMeta), String> {
    let file = open_metadata_handle(path).map_err(|_| "目标路径无法读取。")?;
    let meta = handle_meta(&file).map_err(|_| "目标路径无法读取。")?;
    let resolved = handle_final_path(&file).map_err(|_| "目标路径无法读取。")?;
    Ok((resolved, meta))
}

#[cfg(windows)]
fn inspect_opened_file(file: &File, _path: &Path) -> Result<(PathBuf, HandleMeta), String> {
    let meta = handle_meta(file).map_err(|_| denied())?;
    let resolved = handle_final_path(file).map_err(|_| denied())?;
    Ok((resolved, meta))
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
        file_size: meta.len(),
    }
}

#[cfg(not(windows))]
fn path_meta(path: &Path) -> std::io::Result<HandleMeta> {
    std::fs::metadata(path).map(|meta| metadata_to_handle_meta(&meta))
}

#[cfg(not(windows))]
fn inspect_path(path: &Path) -> Result<(PathBuf, HandleMeta), String> {
    let resolved = path.canonicalize().map_err(|_| "目标路径无法读取。")?;
    let meta = path_meta(&resolved).map_err(|_| "目标路径无法读取。")?;
    Ok((resolved, meta))
}

#[cfg(not(windows))]
fn inspect_opened_file(file: &File, path: &Path) -> Result<(PathBuf, HandleMeta), String> {
    let meta = file.metadata().map_err(|_| denied())?;
    let opened = metadata_to_handle_meta(&meta);
    let resolved = path.canonicalize().map_err(|_| denied())?;
    let current = path_meta(&resolved).map_err(|_| denied())?;
    if opened.identity != current.identity {
        return Err(denied());
    }
    Ok((resolved, opened))
}

fn denied() -> String {
    "权限拒绝：禁止读取或枚举凭据文件与运行态目录。".into()
}

pub(crate) fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let value = value.to_lowercase();
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

fn reject_alternate_stream(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        // A colon is legal only in the drive prefix. Deny NT device paths too.
        let raw = path.to_string_lossy();
        let raw = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
        if raw.starts_with(r"\\.\") || raw.char_indices().any(|(i, c)| c == ':' && i != 1) {
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

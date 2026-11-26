use std::collections::HashSet;
use std::fs::{File, Metadata};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use cst_os::KitLayout;
use pi_runtime::ReadOperations;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FileIdentity {
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    id: [u8; 16],
    #[cfg(not(windows))]
    dev: u64,
    #[cfg(not(windows))]
    ino: u64,
}

#[derive(Clone, Copy, Debug)]
struct HandleMeta {
    identity: Option<FileIdentity>,
    #[cfg(windows)]
    legacy_id: Option<(u32, u64)>,
    #[cfg(windows)]
    attrs: u32,
    link_count: u64,
    is_dir: bool,
    is_file: bool,
    unhydrated: bool,
    file_size: u64,
    modified_secs: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedSearchEntry {
    pub canonical_path: PathBuf,
    pub is_dir: bool,
    pub size_bytes: Option<u64>,
    pub modified_secs: Option<i64>,
    #[allow(dead_code)]
    pub unhydrated: bool,
    #[cfg(windows)]
    pub vol_serial: u32,
    #[cfg(windows)]
    pub index64: u64,
    /// Pinned root directory handle kept open for the lifetime of the search to prevent
    /// concurrent rename/replacement of the verified search root on Windows.
    #[cfg(windows)]
    #[allow(dead_code)]
    pub pinned_handle: Option<Arc<File>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchGuardError {
    /// Protected secret, `.state/`, `.git`, or reparse/symlink escape.
    /// Callers must never count this in `skipped` or `truncated`.
    PolicyDenied,
    /// Non-policy I/O, access denied, or stale/missing entry failure.
    IoOrAccess,
}

#[cfg(any(windows, test))]
const FILE_ATTRIBUTE_DIRECTORY_BITS: u32 = 0x0000_0010;
#[cfg(any(windows, test))]
const FILE_ATTRIBUTE_REPARSE_POINT_BITS: u32 = 0x0000_0400;
#[cfg(any(windows, test))]
const FILE_ATTRIBUTE_OFFLINE_BITS: u32 = 0x0000_1000;
#[cfg(any(windows, test))]
const FILE_ATTRIBUTE_RECALL_ON_OPEN_BITS: u32 = 0x0004_0000;
#[cfg(any(windows, test))]
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS_BITS: u32 = 0x0040_0000;
#[cfg(any(windows, test))]
pub(crate) const FILE_ATTRIBUTE_UNHYDRATED_MASK: u32 = FILE_ATTRIBUTE_OFFLINE_BITS
    | FILE_ATTRIBUTE_RECALL_ON_OPEN_BITS
    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS_BITS;

#[cfg(any(windows, test))]
fn build_windows_file_identity(
    ex_id: Option<(u64, [u8; 16])>,
    vol32: u32,
    index64: u64,
) -> Option<(u64, [u8; 16])> {
    if let Some((vol, id)) = ex_id {
        if vol != 0 && id != [0u8; 16] && id != [0xFFu8; 16] {
            return Some((vol, id));
        }
        return None;
    }
    if vol32 != 0 && index64 != 0 && index64 != u64::MAX {
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&index64.to_le_bytes());
        return Some((u64::from(vol32), id));
    }
    None
}

#[cfg(any(windows, test))]
pub(crate) fn should_reject_windows_open_attrs(attrs: u32, reparse_tag: Option<u32>) -> bool {
    if attrs & (FILE_ATTRIBUTE_DIRECTORY_BITS | FILE_ATTRIBUTE_UNHYDRATED_MASK) != 0 {
        return true;
    }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT_BITS != 0 {
        return !reparse_tag.is_some_and(crate::diagnostics::walk::is_cloud_reparse_tag);
    }
    false
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
    #[cfg(windows)]
    state_legacy_id: Option<(u32, u64)>,
    secret_paths: [PathBuf; 5],
    secret_keys: Vec<String>,
    secret_identities: Vec<FileIdentity>,
    /// Canonical ancestor directories already verified not to match `state_identity`
    /// during this single operation. Dropped with the session; never persists across calls.
    verified_non_state_dirs: RwLock<HashSet<String>>,
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
        let state_inspected = inspect_path(&state_dir).ok().map(|(resolved, meta)| {
            push_unique_key(&mut state_keys, &resolved);
            meta
        });
        let state_identity = state_inspected.and_then(|meta| meta.identity);
        #[cfg(windows)]
        let state_legacy_id = state_inspected.and_then(|meta| meta.legacy_id);

        let secret_paths = self.secret_paths();
        let mut secret_keys = Vec::with_capacity(10);
        let mut secret_identities = Vec::with_capacity(5);
        for secret in &secret_paths {
            push_unique_key(&mut secret_keys, secret);
            if let Ok((resolved, meta)) = inspect_path(secret) {
                push_unique_key(&mut secret_keys, &resolved);
                if let Some(id) = meta.identity {
                    secret_identities.push(id);
                }
            }
        }

        PathGuardSession {
            inner: Arc::new(SessionInner {
                state_dir,
                state_keys,
                state_identity,
                #[cfg(windows)]
                state_legacy_id,
                secret_paths,
                secret_keys,
                secret_identities,
                verified_non_state_dirs: RwLock::new(HashSet::new()),
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
    #[allow(dead_code)]
    pub(crate) fn check_count(&self) -> usize {
        self.inner.checks.load(Ordering::Relaxed)
    }

    fn record_check(&self) {
        #[cfg(test)]
        self.inner.checks.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(windows)]
    pub(crate) fn state_legacy_identity(&self) -> Option<(u32, u64)> {
        self.inner.state_legacy_id
    }

    #[cfg(windows)]
    pub(crate) fn has_state_identity(&self) -> bool {
        self.inner.state_identity.is_some()
    }

    /// Returns true if any protected state or secret path lies at or beneath `dir`.
    #[cfg(windows)]
    pub(crate) fn dir_may_contain_protected_key(&self, dir: &Path) -> bool {
        let dir_key = if dir.is_absolute() {
            path_key(dir)
        } else if let Ok(abs) = std::path::absolute(dir) {
            path_key(&abs)
        } else {
            return true;
        };
        let matches_prefix = |target: &str| {
            target == dir_key
                || target
                    .strip_prefix(&dir_key)
                    .is_some_and(|rest| dir_key.ends_with('/') || rest.starts_with('/'))
        };
        self.inner.state_keys.iter().any(|k| matches_prefix(k))
            || self.inner.secret_keys.iter().any(|k| matches_prefix(k))
    }

    /// Search-dedicated root verification: refuses `.state/`, `.git`, credentials, UNC/network
    /// paths, and reparse/symlink links at the root or any ancestor before traversal starts.
    pub(crate) fn verify_search_root(&self, path: &Path) -> Result<VerifiedSearchEntry, String> {
        self.record_check();
        if is_unc_or_network_path(path) {
            return Err("不支持检索 UNC 网络共享或网络映射盘。".into());
        }
        if has_git_component(path) {
            return Err(denied());
        }
        self.verify_requested(path)?;
        reject_symlink_ancestors(path)?;

        #[cfg(windows)]
        {
            let file = open_search_handle_no_follow(path, true).map_err(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    "检索路径不存在。".to_owned()
                } else if err.kind() == io::ErrorKind::PermissionDenied {
                    "目标路径无权访问。".to_owned()
                } else {
                    "目标路径无法读取。".to_owned()
                }
            })?;
            let (resolved, meta) = inspect_search_handle(&file, true).map_err(|err| match err {
                SearchGuardError::PolicyDenied => denied(),
                SearchGuardError::IoOrAccess => "目标路径无法读取。".to_owned(),
            })?;
            if is_unc_or_network_path(&resolved) {
                return Err("不支持检索 UNC 网络共享或网络映射盘。".into());
            }
            if has_git_component(&resolved) {
                return Err(denied());
            }
            self.verify_resolved_strict(&resolved, meta)?;
            let (vol_serial, index64) = meta.legacy_id.unwrap_or((0, 0));
            // If root is a directory, hold a pinned FILE_LIST_DIRECTORY no-follow handle
            // and confirm it resolves to the exact same volume & object ID.
            let pinned_handle = if meta.is_dir {
                let dir_handle = open_dir_list_handle_no_follow(path)
                    .map_err(|_| "目标目录无权枚举。".to_owned())?;
                let (dir_resolved, dir_meta) =
                    inspect_search_handle(&dir_handle, true).map_err(|err| match err {
                        SearchGuardError::PolicyDenied => denied(),
                        SearchGuardError::IoOrAccess => "目标目录无权枚举。".to_owned(),
                    })?;
                if !dir_meta.is_dir
                    || dir_meta.legacy_id != meta.legacy_id
                    || path_key(&dir_resolved) != path_key(&resolved)
                {
                    return Err(denied());
                }
                Some(Arc::new(dir_handle))
            } else {
                None
            };
            Ok(VerifiedSearchEntry {
                canonical_path: resolved,
                is_dir: meta.is_dir,
                size_bytes: meta.is_file.then_some(meta.file_size),
                modified_secs: meta.modified_secs,
                unhydrated: meta.unhydrated,
                vol_serial,
                index64,
                pinned_handle,
            })
        }
        #[cfg(not(windows))]
        {
            let link_meta = std::fs::symlink_metadata(path).map_err(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    "检索路径不存在。".to_owned()
                } else {
                    "目标路径无法读取。".to_owned()
                }
            })?;
            if is_link(&link_meta) {
                return Err(denied());
            }
            let (resolved, meta) = inspect_path(path).map_err(|_| "目标路径无法读取。")?;
            if has_git_component(&resolved) {
                return Err(denied());
            }
            self.verify_resolved_strict(&resolved, meta)?;
            Ok(VerifiedSearchEntry {
                canonical_path: resolved,
                is_dir: meta.is_dir,
                size_bytes: meta.is_file.then_some(meta.file_size),
                modified_secs: meta.modified_secs,
                unhydrated: meta.unhydrated,
            })
        }
    }

    /// Search-dedicated candidate verification on a single no-follow handle.
    /// Confirms volume/object identity, root containment, `.git`/`.state`/secret policy,
    /// cloud/reparse status, and extracts verified path, logical size, and modified time.
    pub(crate) fn verify_search_entry(
        &self,
        path: &Path,
        expected_root: &VerifiedSearchEntry,
        expected_vol: Option<u32>,
        expected_index64: Option<u64>,
        allow_dir: bool,
    ) -> Result<VerifiedSearchEntry, SearchGuardError> {
        self.record_check();
        if has_git_component(path) || self.verify_requested(path).is_err() {
            return Err(SearchGuardError::PolicyDenied);
        }
        if is_unc_or_network_path(path) {
            return Err(SearchGuardError::PolicyDenied);
        }

        let root_key = path_key(&expected_root.canonical_path);

        #[cfg(windows)]
        {
            let file = open_search_handle_no_follow(path, false)
                .map_err(|_| SearchGuardError::IoOrAccess)?;
            let (resolved, meta) = inspect_search_handle(&file, allow_dir)?;
            if meta.is_dir && !allow_dir {
                return Err(SearchGuardError::IoOrAccess);
            }
            if is_unc_or_network_path(&resolved) || has_git_component(&resolved) {
                return Err(SearchGuardError::PolicyDenied);
            }
            let resolved_key = path_key(&resolved);
            if !is_within_root_key(&resolved_key, &root_key) {
                return Err(SearchGuardError::PolicyDenied);
            }
            let (vol_serial, index64) = meta.legacy_id.unwrap_or((0, 0));
            if expected_root.vol_serial != 0
                && (vol_serial == 0 || vol_serial != expected_root.vol_serial)
            {
                return Err(SearchGuardError::PolicyDenied);
            }
            if let Some(ev) = expected_vol
                && ev != 0
                && (vol_serial == 0 || vol_serial != ev)
            {
                return Err(SearchGuardError::IoOrAccess);
            }
            if let Some(eid) = expected_index64
                && eid != 0
                && (index64 == 0 || index64 != eid)
            {
                return Err(SearchGuardError::IoOrAccess);
            }
            self.verify_resolved_strict(&resolved, meta)
                .map_err(|_| SearchGuardError::PolicyDenied)?;
            Ok(VerifiedSearchEntry {
                canonical_path: resolved,
                is_dir: meta.is_dir,
                size_bytes: meta.is_file.then_some(meta.file_size),
                modified_secs: meta.modified_secs,
                unhydrated: meta.unhydrated,
                vol_serial,
                index64,
                pinned_handle: None,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (expected_vol, expected_index64);
            let link_meta =
                std::fs::symlink_metadata(path).map_err(|_| SearchGuardError::IoOrAccess)?;
            if is_link(&link_meta) {
                return Err(SearchGuardError::PolicyDenied);
            }
            let (resolved, meta) = inspect_path(path).map_err(|_| SearchGuardError::IoOrAccess)?;
            if meta.is_dir && !allow_dir {
                return Err(SearchGuardError::IoOrAccess);
            }
            if has_git_component(&resolved) {
                return Err(SearchGuardError::PolicyDenied);
            }
            let resolved_key = path_key(&resolved);
            if !is_within_root_key(&resolved_key, &root_key) {
                return Err(SearchGuardError::PolicyDenied);
            }
            self.verify_resolved_strict(&resolved, meta)
                .map_err(|_| SearchGuardError::PolicyDenied)?;
            Ok(VerifiedSearchEntry {
                canonical_path: resolved,
                is_dir: meta.is_dir,
                size_bytes: meta.is_file.then_some(meta.file_size),
                modified_secs: meta.modified_secs,
                unhydrated: meta.unhydrated,
            })
        }
    }

    /// Verifies caller holds `FILE_LIST_DIRECTORY` access on a directory path and confirms its
    /// volume, 64-bit file index, non-reparse status, and `.git`/`.state` safety on the same handle.
    #[cfg(windows)]
    pub(crate) fn verify_dir_list_identity(
        &self,
        path: &Path,
        expected_root: &VerifiedSearchEntry,
        expected_index64: u64,
    ) -> Result<(), SearchGuardError> {
        if has_git_component(path)
            || is_unc_or_network_path(path)
            || self.verify_requested(path).is_err()
        {
            return Err(SearchGuardError::PolicyDenied);
        }
        let dir_handle =
            open_dir_list_handle_no_follow(path).map_err(|_| SearchGuardError::IoOrAccess)?;
        let (resolved, meta) = inspect_search_handle(&dir_handle, true)?;
        if !meta.is_dir {
            return Err(SearchGuardError::IoOrAccess);
        }
        if is_unc_or_network_path(&resolved) || has_git_component(&resolved) {
            return Err(SearchGuardError::PolicyDenied);
        }
        let root_key = path_key(&expected_root.canonical_path);
        let resolved_key = path_key(&resolved);
        if !is_within_root_key(&resolved_key, &root_key) {
            return Err(SearchGuardError::PolicyDenied);
        }
        let (vol_serial, index64) = meta.legacy_id.unwrap_or((0, 0));
        if expected_root.vol_serial != 0
            && (vol_serial == 0 || vol_serial != expected_root.vol_serial)
        {
            return Err(SearchGuardError::PolicyDenied);
        }
        if expected_index64 != 0 && (index64 == 0 || index64 != expected_index64) {
            return Err(SearchGuardError::IoOrAccess);
        }
        self.verify_resolved_strict(&resolved, meta)
            .map_err(|_| SearchGuardError::PolicyDenied)?;
        Ok(())
    }

    /// Open a regular file for `ffgrep` content reading with single-handle no-follow open,
    /// root containment, and policy checks (zero TOCTOU between reparse check and read).
    pub(crate) fn open_grep_file(
        &self,
        path: &Path,
        expected_root: &VerifiedSearchEntry,
    ) -> Result<(File, VerifiedSearchEntry), SearchGuardError> {
        self.record_check();
        if has_git_component(path)
            || is_unc_or_network_path(path)
            || self.verify_requested(path).is_err()
        {
            return Err(SearchGuardError::PolicyDenied);
        }
        let file = open_grep_handle_no_follow(path).map_err(|err| {
            if err.kind() == io::ErrorKind::InvalidData {
                SearchGuardError::PolicyDenied
            } else {
                SearchGuardError::IoOrAccess
            }
        })?;
        let (resolved, meta) =
            inspect_opened_file(&file, path).map_err(|_| SearchGuardError::PolicyDenied)?;
        if meta.unhydrated {
            return Err(SearchGuardError::PolicyDenied);
        }
        if !meta.is_file {
            return Err(SearchGuardError::IoOrAccess);
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::Storage::FileSystem::{
                FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo, GetFileInformationByHandleEx,
            };
            if meta.attrs & FILE_ATTRIBUTE_REPARSE_POINT_BITS != 0 {
                let mut tag_info = FILE_ATTRIBUTE_TAG_INFO::default();
                unsafe {
                    GetFileInformationByHandleEx(
                        HANDLE(file.as_raw_handle()),
                        FileAttributeTagInfo,
                        (&raw mut tag_info).cast(),
                        std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
                    )
                }
                .map_err(|_| SearchGuardError::PolicyDenied)?;
                if should_reject_windows_open_attrs(
                    tag_info.FileAttributes,
                    Some(tag_info.ReparseTag),
                ) {
                    return Err(SearchGuardError::PolicyDenied);
                }
            }
        }
        if is_unc_or_network_path(&resolved) || has_git_component(&resolved) {
            return Err(SearchGuardError::PolicyDenied);
        }
        let root_key = path_key(&expected_root.canonical_path);
        let resolved_key = path_key(&resolved);
        if !is_within_root_key(&resolved_key, &root_key) {
            return Err(SearchGuardError::PolicyDenied);
        }
        #[cfg(windows)]
        let (vol_serial, index64) = meta.legacy_id.unwrap_or((0, 0));
        #[cfg(windows)]
        if expected_root.vol_serial != 0
            && (vol_serial == 0 || vol_serial != expected_root.vol_serial)
        {
            return Err(SearchGuardError::PolicyDenied);
        }
        self.verify_resolved_strict(&resolved, meta)
            .map_err(|_| SearchGuardError::PolicyDenied)?;
        Ok((
            file,
            VerifiedSearchEntry {
                canonical_path: resolved,
                is_dir: false,
                size_bytes: Some(meta.file_size),
                modified_secs: meta.modified_secs,
                unhydrated: false,
                #[cfg(windows)]
                vol_serial,
                #[cfg(windows)]
                index64,
                #[cfg(windows)]
                pinned_handle: None,
            },
        ))
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
        path_meta(path).is_ok_and(|meta| meta.identity != Some(state_id))
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
        if meta.unhydrated {
            return Err("目标文件无法打开。".into());
        }
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
        if let Some(id) = meta.identity
            && self.inner.secret_identities.contains(&id)
        {
            return Err(denied());
        }
        if let Some(state_id) = self.inner.state_identity {
            if meta.identity == Some(state_id) {
                return Err(denied());
            }
            // A canonical name can still use an alternate volume or mount name.
            // Deduplicate verified canonical ancestor directories within this single session.
            self.verify_ancestors_not_state(resolved, state_id)?;
        }

        if meta.is_file {
            match meta.link_count {
                0 => return Err(denied()),
                1 => (),
                _ => {
                    let Some(id) = meta.identity else {
                        return Err(denied());
                    };
                    self.verify_hard_link(id)?;
                }
            }
        }
        Ok(())
    }

    fn verify_resolved_strict(&self, resolved: &Path, meta: HandleMeta) -> Result<(), String> {
        self.verify_key(resolved)?;
        if let Some(id) = meta.identity
            && self.inner.secret_identities.contains(&id)
        {
            return Err(denied());
        }
        if let Some(state_id) = self.inner.state_identity {
            if meta.identity == Some(state_id) {
                return Err(denied());
            }
            // Strict ancestor verification: refuse candidates if any ancestor identity
            // cannot be read or matches `.state`.
            self.verify_ancestors_not_state_strict(resolved, state_id)?;
        }
        if meta.is_file {
            let need_live_secret_check = meta.link_count != 1
                || self.inner.secret_identities.len() < self.inner.secret_paths.len();
            match meta.link_count {
                0 => return Err(denied()),
                _ if need_live_secret_check => {
                    let Some(id) = meta.identity else {
                        return Err(denied());
                    };
                    self.verify_hard_link(id)?;
                }
                _ => (),
            }
        }
        Ok(())
    }

    /// Strict live ancestor verification without string-key caching: immune to mid-session
    /// directory rebinding and partial-chain cache poisoning.
    fn verify_ancestors_not_state_strict(
        &self,
        resolved: &Path,
        state_id: FileIdentity,
    ) -> Result<(), String> {
        for ancestor in resolved.ancestors().skip(1) {
            match path_meta_no_follow(ancestor) {
                Ok(meta) => match meta.identity {
                    Some(id) if id == state_id => return Err(denied()),
                    Some(_) => {}
                    None => {
                        if ancestor.parent().is_some() {
                            return Err(denied());
                        }
                    }
                },
                Err(_) => {
                    if ancestor.parent().is_some() {
                        return Err(denied());
                    }
                }
            }
        }
        Ok(())
    }

    fn verify_ancestors_not_state(
        &self,
        resolved: &Path,
        state_id: FileIdentity,
    ) -> Result<(), String> {
        verify_ancestors_with(
            &self.inner.verified_non_state_dirs,
            resolved,
            state_id,
            |ancestor| path_meta(ancestor).map(|meta| meta.identity),
        )
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
                Ok(meta) if meta.identity == Some(identity) || meta.identity.is_none() => {
                    return Err(denied());
                }
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
            if meta.is_file() {
                let entry_id = path_meta(entry.path())
                    .map_err(|_| denied())?
                    .identity
                    .ok_or_else(denied)?;
                if entry_id == identity {
                    return Err(denied());
                }
            }
        }
        if state_meta.identity.is_none()
            || path_meta(state_dir).map_err(|_| denied())?.identity != state_meta.identity
        {
            return Err(denied());
        }
        Ok(())
    }
}

fn verify_ancestors_with(
    cache: &RwLock<HashSet<String>>,
    resolved: &Path,
    state_id: FileIdentity,
    mut lookup: impl FnMut(&Path) -> io::Result<Option<FileIdentity>>,
) -> Result<(), String> {
    let Some(parent) = resolved.parent() else {
        return Ok(());
    };
    let parent_key = path_key(parent);
    if cache
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&parent_key)
    {
        return Ok(());
    }

    let mut newly_verified = Vec::new();
    for ancestor in resolved.ancestors().skip(1) {
        let key = path_key(ancestor);
        if cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&key)
        {
            break;
        }
        match lookup(ancestor) {
            Ok(Some(id)) if id == state_id => return Err(denied()),
            Ok(Some(_)) => newly_verified.push(key),
            // Never cache unverified (`Err`) or identity-less ancestors as safe.
            Ok(None) | Err(_) => {}
        }
    }
    if !newly_verified.is_empty() {
        let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
        if guard.len() < 16_384 {
            guard.extend(newly_verified);
        }
    }
    Ok(())
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
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_TYPE_DISK, FileAttributeTagInfo, GetFileInformationByHandleEx, GetFileType,
    };

    // Inspect target attributes without following reparse points or requesting FILE_READ_DATA,
    // preventing cloud-placeholder hydration, outbound SMB symlink traversal, and pipe blocking.
    let pre = std::fs::symlink_metadata(path)?;
    let attrs = pre.file_attributes();
    if attrs & (FILE_ATTRIBUTE_DIRECTORY_BITS | FILE_ATTRIBUTE_UNHYDRATED_MASK) != 0 {
        return Err(io::Error::other("non-regular or unhydrated cloud file"));
    }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT_BITS != 0 {
        let raw_reparse = std::fs::OpenOptions::new()
            .read(false)
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)?;
        let handle = HANDLE(raw_reparse.as_raw_handle());
        if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
            return Err(io::Error::other("not a disk file"));
        }
        let mut tag_info = FILE_ATTRIBUTE_TAG_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileAttributeTagInfo,
                (&raw mut tag_info).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        }
        .map_err(io::Error::other)?;
        if should_reject_windows_open_attrs(tag_info.FileAttributes, Some(tag_info.ReparseTag)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "reparse point rejected",
            ));
        }
    }
    File::open(path)
}

#[cfg(not(windows))]
fn open_read_handle(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // Refuse symlinks atomically and don't block waiting for a writer if the name is a FIFO.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
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
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ID_INFO, FILE_TYPE_DISK,
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx, GetFileType,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let handle = HANDLE(file.as_raw_handle());
    // SAFETY: `handle` is borrowed from a live `File` for all Win32 calls.
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
    let mut id_info = FILE_ID_INFO::default();
    let ex_id = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&raw mut id_info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .ok()
    .map(|()| (id_info.VolumeSerialNumber, id_info.FileId.Identifier));

    let index64 = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    let legacy_id = (info.dwVolumeSerialNumber != 0 && index64 != 0 && index64 != u64::MAX)
        .then_some((info.dwVolumeSerialNumber, index64));
    let identity = build_windows_file_identity(ex_id, info.dwVolumeSerialNumber, index64)
        .map(|(volume, id)| FileIdentity { volume, id });
    let is_dir = (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0;
    let ft = ((info.ftLastWriteTime.dwHighDateTime as u64) << 32)
        | (info.ftLastWriteTime.dwLowDateTime as u64);
    Ok(HandleMeta {
        identity,
        legacy_id,
        attrs: info.dwFileAttributes,
        link_count: u64::from(info.nNumberOfLinks),
        is_dir,
        is_file: !is_dir,
        unhydrated: (info.dwFileAttributes & FILE_ATTRIBUTE_UNHYDRATED_MASK) != 0,
        file_size: ((info.nFileSizeHigh as u64) << 32) | (info.nFileSizeLow as u64),
        modified_secs: filetime_to_unix_secs(ft),
    })
}

#[cfg(windows)]
pub(crate) fn filetime_to_unix_secs(ft: u64) -> Option<i64> {
    const WINDOWS_EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    if ft == 0 {
        return None;
    }
    let delta = ft as i128 - WINDOWS_EPOCH_DIFF_100NS as i128;
    let secs = delta.div_euclid(10_000_000);
    i64::try_from(secs).ok()
}

#[cfg(windows)]
fn open_search_handle_no_follow(path: &Path, is_root: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    };
    let _ = is_root;
    std::fs::OpenOptions::new()
        .read(false)
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode(7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
}

#[cfg(windows)]
fn open_dir_list_handle_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
        FILE_READ_ATTRIBUTES,
    };
    std::fs::OpenOptions::new()
        .read(false)
        .access_mode(FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0)
        .share_mode(7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
}

#[cfg(windows)]
fn open_grep_handle_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_SEQUENTIAL_SCAN, FILE_TYPE_DISK, GetFileType,
    };
    // Single-handle no-follow open: refuses symlink/reparse traversal atomically without TOCTOU.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(GENERIC_READ.0)
        .share_mode(7)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_SEQUENTIAL_SCAN.0)
        .open(path)?;
    if unsafe { GetFileType(HANDLE(file.as_raw_handle())) } != FILE_TYPE_DISK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a disk file",
        ));
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_grep_handle_no_follow(path: &Path) -> io::Result<File> {
    open_read_handle(path)
}

#[cfg(windows)]
fn path_meta_no_follow(path: &Path) -> io::Result<HandleMeta> {
    let file = open_search_handle_no_follow(path, false)?;
    handle_meta(&file)
}

#[cfg(not(windows))]
fn path_meta_no_follow(path: &Path) -> io::Result<HandleMeta> {
    let meta = std::fs::symlink_metadata(path)?;
    if is_link(&meta) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "symlink"));
    }
    path_meta(path)
}

#[cfg(windows)]
fn inspect_search_handle(
    file: &File,
    allow_dir: bool,
) -> Result<(PathBuf, HandleMeta), SearchGuardError> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo, GetFileInformationByHandleEx,
    };

    let meta = handle_meta(file).map_err(|_| SearchGuardError::IoOrAccess)?;
    if meta.is_dir {
        if !allow_dir {
            return Err(SearchGuardError::IoOrAccess);
        }
        if meta.attrs & (FILE_ATTRIBUTE_REPARSE_POINT_BITS | FILE_ATTRIBUTE_UNHYDRATED_MASK) != 0 {
            return Err(SearchGuardError::PolicyDenied);
        }
    } else if meta.attrs & FILE_ATTRIBUTE_REPARSE_POINT_BITS != 0 {
        let mut tag_info = FILE_ATTRIBUTE_TAG_INFO::default();
        let handle = HANDLE(file.as_raw_handle());
        unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileAttributeTagInfo,
                (&raw mut tag_info).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        }
        .map_err(|_| SearchGuardError::PolicyDenied)?;
        if !crate::diagnostics::walk::is_cloud_reparse_tag(tag_info.ReparseTag) {
            return Err(SearchGuardError::PolicyDenied);
        }
    }
    let resolved = handle_final_path(file).map_err(|_| SearchGuardError::IoOrAccess)?;
    Ok((resolved, meta))
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
    let identity = (meta.dev() != 0 && meta.ino() != 0).then_some(FileIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    });
    HandleMeta {
        identity,
        link_count: meta.nlink(),
        is_dir: meta.is_dir(),
        is_file: meta.is_file(),
        unhydrated: false,
        file_size: meta.len(),
        modified_secs: Some(meta.mtime()),
    }
}

pub(crate) fn has_git_component(path: &Path) -> bool {
    path.components().any(|c| match c {
        std::path::Component::Normal(name) => {
            #[cfg(windows)]
            {
                name.to_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(".git"))
            }
            #[cfg(not(windows))]
            {
                name == ".git"
            }
        }
        _ => false,
    })
}

pub(crate) fn is_within_root_key(child_key: &str, root_key: &str) -> bool {
    if child_key == root_key {
        return true;
    }
    child_key
        .strip_prefix(root_key)
        .is_some_and(|rest| root_key.ends_with('/') || rest.starts_with('/'))
}

pub(crate) fn is_unc_or_network_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let raw = path.to_string_lossy();
        if raw.starts_with(r"\\") || raw.starts_with("//") {
            let upper = raw.to_ascii_uppercase();
            if upper.starts_with(r"\\?\UNC\") || upper.starts_with("//?/UNC/") {
                return true;
            }
            if !upper.starts_with(r"\\?\") && !upper.starts_with("//?/") {
                return true;
            }
        }
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            match prefix.kind() {
                Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => return true,
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    use std::os::windows::ffi::OsStrExt;
                    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
                    use windows::core::PCWSTR;
                    const DRIVE_REMOTE: u32 = 4;
                    let drive_root = format!("{}:\\", letter as char);
                    let wide: Vec<u16> = std::ffi::OsStr::new(&drive_root)
                        .encode_wide()
                        .chain(Some(0))
                        .collect();
                    if unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) } == DRIVE_REMOTE {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::absolute(path).map_err(|_| denied())?
    };
    for ancestor in abs.ancestors().skip(1) {
        if ancestor.parent().is_none() {
            // Stop at volume root (`C:\` or `/`).
            break;
        }
        if let Ok(meta) = std::fs::symlink_metadata(ancestor)
            && is_link(&meta)
        {
            return Err(denied());
        }
    }
    Ok(())
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
    if opened.identity.is_none() || opened.identity != current.identity {
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
                Prefix::VerbatimUNC(server, _) if server != "." && server != "?" => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_128bit_file_identity_distinguishes_high_bits_and_rejects_sentinels() {
        let mut id_a = [0u8; 16];
        id_a[0] = 7;
        id_a[12] = 1;
        let mut id_b = id_a;
        id_b[12] = 2;

        let a = build_windows_file_identity(Some((0x1234_5678, id_a)), 0x5678, u64::MAX).unwrap();
        let b = build_windows_file_identity(Some((0x1234_5678, id_b)), 0x5678, u64::MAX).unwrap();
        assert_ne!(a, b);

        // Fallback to 64-bit when FileIdInfo is unavailable, while rejecting 0 and u64::MAX.
        let fallback = build_windows_file_identity(None, 42, 99).unwrap();
        assert_eq!(fallback.0, 42);
        assert_eq!(&fallback.1[..8], &99u64.to_le_bytes());

        assert!(build_windows_file_identity(Some((1, [0u8; 16])), 1, 99).is_none());
        assert!(build_windows_file_identity(Some((1, [0xFFu8; 16])), 1, 99).is_none());
        assert!(build_windows_file_identity(Some((0, id_a)), 1, 99).is_none());
        assert!(build_windows_file_identity(None, 42, 0).is_none());
        assert!(build_windows_file_identity(None, 42, u64::MAX).is_none());
        assert!(build_windows_file_identity(None, 0, 99).is_none());
    }

    #[test]
    fn windows_pre_open_classifier_blocks_unhydrated_cloud_and_symlinks() {
        const CLOUD_TAG: u32 = 0x9000_101A;
        const ONEDRIVE_TAG: u32 = 0x8000_0021;
        const SYMLINK_TAG: u32 = 0xA000_000C;

        // Normal local file -> allowed
        assert!(!should_reject_windows_open_attrs(0x20, None));
        // Hydrated local OneDrive files -> allowed
        assert!(!should_reject_windows_open_attrs(
            FILE_ATTRIBUTE_REPARSE_POINT_BITS,
            Some(CLOUD_TAG)
        ));
        assert!(!should_reject_windows_open_attrs(
            FILE_ATTRIBUTE_REPARSE_POINT_BITS,
            Some(ONEDRIVE_TAG)
        ));
        // Online-only / recall-on-open / offline placeholders -> blocked before File::open
        for bit in [
            FILE_ATTRIBUTE_OFFLINE_BITS,
            FILE_ATTRIBUTE_RECALL_ON_OPEN_BITS,
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS_BITS,
        ] {
            assert!(should_reject_windows_open_attrs(bit, None));
            assert!(should_reject_windows_open_attrs(
                FILE_ATTRIBUTE_REPARSE_POINT_BITS | bit,
                Some(CLOUD_TAG)
            ));
        }
        // Symlinks, junctions, directories, and unknown reparse tags -> blocked
        assert!(should_reject_windows_open_attrs(
            FILE_ATTRIBUTE_REPARSE_POINT_BITS,
            Some(SYMLINK_TAG)
        ));
        assert!(should_reject_windows_open_attrs(
            FILE_ATTRIBUTE_REPARSE_POINT_BITS,
            None
        ));
        assert!(should_reject_windows_open_attrs(
            FILE_ATTRIBUTE_DIRECTORY_BITS,
            None
        ));
    }

    #[test]
    fn ancestor_verification_never_caches_error_or_sentinel_ancestors_as_safe() {
        #[cfg(windows)]
        let make_id = |b: u8| FileIdentity {
            volume: 1,
            id: [b; 16],
        };
        #[cfg(not(windows))]
        let make_id = |b: u8| FileIdentity {
            dev: 1,
            ino: u64::from(b),
        };

        let state_id = make_id(9);
        let safe_id = make_id(2);
        let cache = RwLock::new(HashSet::new());
        let resolved = Path::new("/kit/unreadable/sentinel/safe/doc.txt");

        verify_ancestors_with(&cache, resolved, state_id, |ancestor| {
            let key = path_key(ancestor);
            if key.ends_with("/safe") {
                Ok(Some(safe_id))
            } else if key.ends_with("/sentinel") {
                Ok(None)
            } else {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        })
        .unwrap();

        let cached = cache.read().unwrap();
        assert!(cached.iter().any(|k| k.ends_with("/safe")));
        assert!(!cached.iter().any(|k| k.ends_with("/sentinel")));
        assert!(!cached.iter().any(|k| k.ends_with("/unreadable")));
        drop(cached);

        let err = verify_ancestors_with(
            &cache,
            Path::new("/kit/alias/sub/doc.txt"),
            state_id,
            |ancestor| {
                if path_key(ancestor).ends_with("/alias") {
                    Ok(Some(state_id))
                } else {
                    Ok(Some(safe_id))
                }
            },
        );
        assert!(err.is_err());
    }

    #[cfg(windows)]
    #[test]
    fn reject_unsafe_windows_path_blocks_verbatim_unc_device_servers() {
        assert!(reject_unsafe_windows_path(Path::new(r"\\.\pipe\demo")).is_err());
        assert!(reject_unsafe_windows_path(Path::new(r"\\?\UNC\.\pipe\demo")).is_err());
        assert!(reject_unsafe_windows_path(Path::new(r"\\?\UNC\?\GLOBALROOT\demo")).is_err());
        assert!(reject_unsafe_windows_path(Path::new(r"\\?\UNC\fileserver\share\a.log")).is_ok());
    }
}

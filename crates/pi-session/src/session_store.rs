use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(feature = "test-support")]
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use thiserror::Error;
use uuid::Uuid;

use crate::session_manager::{
    FileEntry, MAX_SESSION_HEADER_SCAN_BYTES, SessionEntry, SessionHeader, parse_session_entry_line,
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionStoreError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Session file is not a valid pi session: {0}")]
    InvalidSessionFile(String),
    #[error("Session file already exists (exclusive create refused): {0}")]
    ExclusiveCreateExists(PathBuf),
    #[error("Session file is already locked by another writer instance: {0}")]
    SingleWriterLocked(PathBuf),
    #[error("Storage state is uncertain after recovery failure (store poisoned): {0}")]
    StorageUncertain(String),
    #[error("Session store is poisoned and refuses further writes: {0}")]
    Poisoned(String),
    #[error("Session header exceeds {MAX_SESSION_HEADER_SCAN_BYTES}-byte scan limit: {0}")]
    HeaderScanLimitExceeded(PathBuf),
    #[error("Invalid operation: {0}")]
    InvalidOperation(String),
}

/// 会话文件打开时的尾部核对与修复结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailRepairOutcome {
    Clean,
    AppendedMissingNewline,
    TruncatedIncompleteTail { truncated_bytes: u64 },
}

/// 仅供测试注入磁盘部分写入、同步失败或恢复失败的故障配置。
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Default)]
pub struct JsonlFaultInjection {
    /// 在 `append_entry` 写入指定字节数后模拟 I/O 失败（触发截断回原长度恢复）。
    pub fail_append_after_bytes: Option<usize>,
    /// 当 `fail_append_after_bytes` 触发后，模拟 `set_len` 恢复也失败（使存储进入 `poisoned` 状态）。
    pub fail_append_recovery: bool,
    /// 在 `rewrite_all(exclusive_create=true)` 创建文件并写入部分字节后模拟失败（触发删除新创建文件）。
    pub fail_exclusive_create_after_open: bool,
    /// 当 `fail_exclusive_create_after_open` 触发后，模拟清理新文件失败（使存储进入 `poisoned` 状态）。
    pub fail_exclusive_create_cleanup: bool,
    /// 在 `rewrite_all(exclusive_create=false)` 写入临时文件后、重命名前模拟失败（保留旧文件）。
    pub fail_rewrite_before_rename: bool,
    /// 在 `rewrite_all(exclusive_create=false)` 完成重命名替换后，模拟同步确认失败（使存储进入 `poisoned` 状态）。
    pub fail_rewrite_after_rename_sync: bool,
    /// 在打开文件执行尾部修复时模拟同步失败。
    pub fail_tail_repair_sync: bool,
}

/// 会话存储抽象接口（解耦内存、JSONL 与 SQLite 存储介质）。
pub trait SessionStore: Send + Sync {
    /// 返回底层存储路径标识（内存模式返回 `None`）。
    fn location(&self) -> Option<&Path>;
    /// 是否支持持久化。
    fn is_persisted(&self) -> bool;
    /// 存储是否因不可恢复的I/O或同步未知状态被标记为 `poisoned`。
    fn is_poisoned(&self) -> bool {
        false
    }
    /// 最近一次打开时的尾部修复记录。
    fn last_tail_repair(&self) -> Option<&TailRepairOutcome> {
        None
    }
    /// 读取全部原始文件条目（含 `SessionHeader`）。
    fn load_entries(&mut self) -> Result<Vec<FileEntry>, SessionStoreError>;
    /// 首次创建、迁移或批量提交完整快照；成功返回前完成同步。
    /// `exclusive_create=false` 时原子替换：重新打开仅可见旧快照或完整新快照。
    /// `exclusive_create=true` 时禁止覆盖已有文件。
    fn rewrite_all(
        &mut self,
        entries: &[FileEntry],
        exclusive_create: bool,
    ) -> Result<(), SessionStoreError>;
    /// 追加单条并同步；失败时恢复原尾部，恢复失败后标记 `poisoned` 并禁止继续写入。
    fn append_entry(&mut self, entry: &SessionEntry) -> Result<(), SessionStoreError>;
}

// =============================================================================
// 1. InMemorySessionStore
// =============================================================================

#[derive(Debug, Clone, Default)]
pub struct InMemorySessionStore {
    entries: Vec<FileEntry>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<FileEntry>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }
}

impl SessionStore for InMemorySessionStore {
    fn location(&self) -> Option<&Path> {
        None
    }

    fn is_persisted(&self) -> bool {
        false
    }

    fn load_entries(&mut self) -> Result<Vec<FileEntry>, SessionStoreError> {
        Ok(self.entries.clone())
    }

    fn rewrite_all(
        &mut self,
        entries: &[FileEntry],
        _exclusive_create: bool,
    ) -> Result<(), SessionStoreError> {
        self.entries = entries.to_vec();
        Ok(())
    }

    fn append_entry(&mut self, entry: &SessionEntry) -> Result<(), SessionStoreError> {
        self.entries.push(FileEntry::Entry(entry.clone()));
        Ok(())
    }
}

// =============================================================================
// 2. Single-Writer Lock Guard (Process + OS Exclusive Lock File)
// =============================================================================

fn active_session_locks() -> &'static Mutex<BTreeSet<PathBuf>> {
    static LOCKS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

pub fn normalize_lock_key(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let normalized = normalized.canonicalize().unwrap_or_else(|_| {
        match (normalized.parent(), normalized.file_name()) {
            (Some(parent), Some(name)) => parent
                .canonicalize()
                .map(|p| p.join(name))
                .unwrap_or(normalized),
            _ => normalized,
        }
    });
    #[cfg(windows)]
    {
        PathBuf::from(
            normalized
                .to_string_lossy()
                .replace('/', "\\")
                .to_lowercase(),
        )
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

#[derive(Debug)]
pub struct SingleWriterLockGuard {
    session_path: PathBuf,
    lock_key: PathBuf,
    _file: File,
}

impl SingleWriterLockGuard {
    pub fn acquire(session_path: &Path) -> Result<Self, SessionStoreError> {
        let lock_key = normalize_lock_key(session_path);
        let mut set = active_session_locks()
            .lock()
            .expect("active_session_locks poisoned");
        if set.contains(&lock_key) {
            return Err(SessionStoreError::SingleWriterLocked(
                session_path.to_path_buf(),
            ));
        }

        let mut lock_name = lock_key.as_os_str().to_owned();
        lock_name.push(".lock");
        let lock_file_path = PathBuf::from(lock_name);
        if let Some(parent) = lock_file_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::create_dir_all(parent)
                .map_err(|e| SessionStoreError::Io(format!("Failed to create session dir: {e}")))?;
        }

        let mut open_opts = OpenOptions::new();
        open_opts
            .read(true)
            .write(true)
            .create(true)
            .truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // share_mode(0) 禁止其他句柄并发打开该 .lock 文件
            open_opts.share_mode(0);
        }

        let mut file = open_opts
            .open(&lock_file_path)
            .map_err(|_| SessionStoreError::SingleWriterLocked(session_path.to_path_buf()))?;
        file.try_lock()
            .map_err(|_| SessionStoreError::SingleWriterLocked(session_path.to_path_buf()))?;
        let _ = writeln!(file, "{}", std::process::id());
        set.insert(lock_key.clone());

        Ok(Self {
            session_path: session_path.to_path_buf(),
            lock_key,
            _file: file,
        })
    }

    pub fn session_path(&self) -> &Path {
        &self.session_path
    }
}

impl Drop for SingleWriterLockGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = active_session_locks().lock() {
            set.remove(&self.lock_key);
        }
        // Keep the lock inode stable: unlinking it would let another process lock a
        // new inode while an existing opener still owns the old one.
    }
}

// =============================================================================
// 3. Atomic File Replacement with Windows Retry Backoff
// =============================================================================

const WINDOWS_REPLACE_RETRY_DELAYS_MS: [u64; 5] = [15, 30, 60, 120, 150];

pub fn is_transient_replace_error(err: &std::io::Error) -> bool {
    if matches!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    matches!(err.raw_os_error(), Some(5 | 32 | 13 | 16))
}

/// 将同目录下的临时文件原子重命名替换到目标路径（在 Windows 下对 `EPERM`/`EACCES`/`EBUSY` 退避重试最多 5 次）。
pub fn atomic_replace_file_sync(tmp_path: &Path, target_path: &Path) -> Result<(), std::io::Error> {
    let mut last_err = None;
    for (idx, delay_ms) in std::iter::once(0u64)
        .chain(WINDOWS_REPLACE_RETRY_DELAYS_MS)
        .enumerate()
    {
        if idx > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        match fs::rename(tmp_path, target_path) {
            Ok(()) => {
                return sync_parent_directory(target_path);
            }
            Err(err) => {
                let retryable = is_transient_replace_error(&err);
                last_err = Some(err);
                if !retryable {
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("atomic_replace_file_sync failed")))
}

/// Sync directory entries on platforms supporting directory handles.
fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

// =============================================================================
// 4. JsonlSessionStore
// =============================================================================

pub struct JsonlSessionStore {
    path: PathBuf,
    lock_guard: SingleWriterLockGuard,
    poisoned: bool,
    last_repair: Option<TailRepairOutcome>,
    skipped_intermediate_corrupt_lines: usize,
    #[cfg(feature = "test-support")]
    fault_injection: Arc<Mutex<JsonlFaultInjection>>,
}

impl std::fmt::Debug for JsonlSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlSessionStore")
            .field("path", &self.path)
            .field("poisoned", &self.poisoned)
            .field("last_repair", &self.last_repair)
            .finish()
    }
}

impl JsonlSessionStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SessionStoreError> {
        let path = path.into();
        let lock_guard = SingleWriterLockGuard::acquire(&path)?;
        Ok(Self {
            path,
            lock_guard,
            poisoned: false,
            last_repair: None,
            skipped_intermediate_corrupt_lines: 0,
            #[cfg(feature = "test-support")]
            fault_injection: Arc::new(Mutex::new(JsonlFaultInjection::default())),
        })
    }

    #[cfg(feature = "test-support")]
    pub fn fault_injection(&self) -> Arc<Mutex<JsonlFaultInjection>> {
        self.fault_injection.clone()
    }

    pub fn skipped_intermediate_corrupt_lines(&self) -> usize {
        self.skipped_intermediate_corrupt_lines
    }

    pub fn mark_poisoned(&mut self) {
        self.poisoned = true;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lock_guard(&self) -> &SingleWriterLockGuard {
        &self.lock_guard
    }

    fn serialize_entries_jsonl(entries: &[FileEntry]) -> Result<Vec<u8>, SessionStoreError> {
        let mut buf = Vec::with_capacity(entries.len() * 256);
        for entry in entries {
            let line = serde_json::to_string(entry)
                .map_err(|e| SessionStoreError::Serialization(e.to_string()))?;
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
        Ok(buf)
    }
}

impl SessionStore for JsonlSessionStore {
    fn location(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn is_persisted(&self) -> bool {
        true
    }

    fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn last_tail_repair(&self) -> Option<&TailRepairOutcome> {
        self.last_repair.as_ref()
    }

    fn load_entries(&mut self) -> Result<Vec<FileEntry>, SessionStoreError> {
        if self.poisoned {
            return Err(SessionStoreError::Poisoned(format!(
                "Session store is poisoned: {}",
                self.path.display()
            )));
        }
        if !self.path.exists() {
            self.last_repair = Some(TailRepairOutcome::Clean);
            return Ok(Vec::new());
        }

        let raw_bytes = fs::read(&self.path).map_err(|e| {
            SessionStoreError::Io(format!("Failed to read {}: {e}", self.path.display()))
        })?;

        if raw_bytes.is_empty() {
            self.last_repair = Some(TailRepairOutcome::Clean);
            return Ok(Vec::new());
        }

        // 按物理行切分，记录每行的起始字节偏移、结束字节偏移以及是否带 `\n`
        let mut line_spans: Vec<(usize, usize, bool)> = Vec::new();
        let bytes = raw_bytes.as_slice();
        let mut start = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                line_spans.push((start, i + 1, true));
                start = i + 1;
            }
        }
        if start < bytes.len() {
            line_spans.push((start, bytes.len(), false));
        }

        let non_blank_indices: Vec<usize> = line_spans
            .iter()
            .enumerate()
            .filter_map(|(idx, &(s, e, _))| {
                (!raw_bytes[s..e].iter().all(u8::is_ascii_whitespace)).then_some(idx)
            })
            .collect();

        if non_blank_indices.is_empty() {
            return Err(SessionStoreError::InvalidSessionFile(
                self.path.display().to_string(),
            ));
        }

        // 先校验首条非空记录是否为合法的 SessionHeader；若不是，严禁修复或改动原文件！
        let first_idx = non_blank_indices[0];
        let (fs_start, fs_end, _) = line_spans[first_idx];
        let first_line = std::str::from_utf8(&raw_bytes[fs_start..fs_end]).ok();
        let Some(FileEntry::Session(header)) = first_line.and_then(parse_session_entry_line) else {
            return Err(SessionStoreError::InvalidSessionFile(
                self.path.display().to_string(),
            ));
        };
        if header.id.trim().is_empty() {
            return Err(SessionStoreError::InvalidSessionFile(
                self.path.display().to_string(),
            ));
        }

        let mut entries: Vec<FileEntry> = vec![FileEntry::Session(header)];
        let mut skipped_intermediate = 0usize;
        let last_non_blank_idx = *non_blank_indices.last().expect("non_blank non-empty");
        let last_physical_idx = line_spans.len() - 1;

        for &idx in &non_blank_indices[1..] {
            let (s, e, has_newline) = line_spans[idx];
            let parsed = std::str::from_utf8(&raw_bytes[s..e])
                .ok()
                .and_then(parse_session_entry_line);
            match parsed {
                Some(entry) => {
                    entries.push(entry);
                    if idx == last_physical_idx && !has_newline {
                        // 末行是有效 JSON 但缺少换行符：补写 `\n` 并同步
                        #[cfg(feature = "test-support")]
                        let fail_sync = self
                            .fault_injection
                            .lock()
                            .expect("fault lock")
                            .fail_tail_repair_sync;
                        #[cfg(feature = "test-support")]
                        if fail_sync {
                            self.poisoned = true;
                            return Err(SessionStoreError::Io(
                                "Fault-injected tail repair newline sync failure".to_owned(),
                            ));
                        }
                        let mut file =
                            OpenOptions::new()
                                .append(true)
                                .open(&self.path)
                                .map_err(|err| {
                                    self.poisoned = true;
                                    SessionStoreError::Io(format!(
                                        "Failed to open session file for newline repair: {err}"
                                    ))
                                })?;
                        file.write_all(b"\n")
                            .and_then(|()| file.sync_all())
                            .map_err(|err| {
                                self.poisoned = true;
                                SessionStoreError::Io(format!(
                                    "Failed to sync newline repair: {err}"
                                ))
                            })?;
                        self.last_repair = Some(TailRepairOutcome::AppendedMissingNewline);
                    }
                }
                None => {
                    if idx == last_non_blank_idx {
                        // Schema errors and complete unknown entries must never delete data.
                        let incomplete =
                            serde_json::from_slice::<serde_json::Value>(&raw_bytes[s..e])
                                .is_err_and(|error| error.is_eof());
                        if !incomplete || idx != last_physical_idx {
                            return Err(SessionStoreError::InvalidSessionFile(format!(
                                "{}: invalid or unsupported final entry",
                                self.path.display()
                            )));
                        }
                        // 末行因进程强杀/断电留下半截 JSON：截断到最近一个完整行边界 `s` 并同步
                        #[cfg(feature = "test-support")]
                        let fail_sync = self
                            .fault_injection
                            .lock()
                            .expect("fault lock")
                            .fail_tail_repair_sync;
                        #[cfg(feature = "test-support")]
                        if fail_sync {
                            self.poisoned = true;
                            return Err(SessionStoreError::Io(
                                "Fault-injected tail repair truncate sync failure".to_owned(),
                            ));
                        }
                        let truncated_bytes = (e - s) as u64;
                        let file =
                            OpenOptions::new()
                                .write(true)
                                .open(&self.path)
                                .map_err(|err| {
                                    self.poisoned = true;
                                    SessionStoreError::Io(format!(
                                        "Failed to open session file for tail truncation: {err}"
                                    ))
                                })?;
                        file.set_len(s as u64)
                            .and_then(|()| file.sync_all())
                            .map_err(|err| {
                                self.poisoned = true;
                                SessionStoreError::Io(format!(
                                    "Failed to truncate incomplete tail: {err}"
                                ))
                            })?;
                        self.last_repair =
                            Some(TailRepairOutcome::TruncatedIncompleteTail { truncated_bytes });
                    } else {
                        // 中间损坏行：跳过并计数，绝不与后续有效行拼接
                        skipped_intermediate += 1;
                    }
                }
            }
        }

        // 若首个非空行就是唯一的物理行且缺少换行符，同样补齐 `\n`
        if non_blank_indices.len() == 1 {
            let (_, _, has_newline) = line_spans[first_idx];
            if !has_newline {
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&self.path)
                    .map_err(|err| {
                        self.poisoned = true;
                        SessionStoreError::Io(format!(
                            "Failed to open session file for header newline repair: {err}"
                        ))
                    })?;
                file.write_all(b"\n")
                    .and_then(|()| file.sync_all())
                    .map_err(|err| {
                        self.poisoned = true;
                        SessionStoreError::Io(format!(
                            "Failed to sync header newline repair: {err}"
                        ))
                    })?;
                self.last_repair = Some(TailRepairOutcome::AppendedMissingNewline);
            }
        }

        self.skipped_intermediate_corrupt_lines = skipped_intermediate;
        if self.last_repair.is_none() {
            self.last_repair = Some(TailRepairOutcome::Clean);
        }
        Ok(entries)
    }

    fn rewrite_all(
        &mut self,
        entries: &[FileEntry],
        exclusive_create: bool,
    ) -> Result<(), SessionStoreError> {
        if self.poisoned {
            return Err(SessionStoreError::Poisoned(format!(
                "Session store is poisoned: {}",
                self.path.display()
            )));
        }

        let payload = Self::serialize_entries_jsonl(entries)?;
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::create_dir_all(parent).map_err(|e| {
                SessionStoreError::Io(format!(
                    "Failed to create session parent directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        if exclusive_create {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.path)
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        SessionStoreError::ExclusiveCreateExists(self.path.clone())
                    } else {
                        SessionStoreError::Io(format!(
                            "Failed to exclusively create {}: {e}",
                            self.path.display()
                        ))
                    }
                })?;

            #[cfg(feature = "test-support")]
            let (fail_after_open, fail_cleanup) = {
                let mut guard = self.fault_injection.lock().expect("fault lock");
                let fo = guard.fail_exclusive_create_after_open;
                let fc = guard.fail_exclusive_create_cleanup;
                guard.fail_exclusive_create_after_open = false;
                guard.fail_exclusive_create_cleanup = false;
                (fo, fc)
            };

            #[cfg(not(feature = "test-support"))]
            let write_res = file.write_all(&payload).and_then(|()| file.sync_all());
            #[cfg(feature = "test-support")]
            let write_res = if fail_after_open {
                let half = (payload.len() / 2).max(1).min(payload.len());
                let _ = file.write_all(&payload[..half]);
                Err(std::io::Error::other(
                    "Fault-injected exclusive create partial write failure",
                ))
            } else {
                file.write_all(&payload).and_then(|()| file.sync_all())
            };

            drop(file);

            if let Err(write_err) = write_res {
                // 首次创建失败时仅清理本次排他创建的文件
                let cleanup = || fs::remove_file(&self.path);
                #[cfg(feature = "test-support")]
                let cleanup = || {
                    if fail_cleanup {
                        Err(std::io::Error::other(
                            "Fault-injected exclusive create cleanup failure",
                        ))
                    } else {
                        cleanup()
                    }
                };
                if cleanup().is_err() {
                    self.poisoned = true;
                    return Err(SessionStoreError::StorageUncertain(format!(
                        "Exclusive create failed ({write_err}) and cleanup of {} failed",
                        self.path.display()
                    )));
                }
                if let Err(err) = sync_parent_directory(&self.path) {
                    self.poisoned = true;
                    return Err(SessionStoreError::StorageUncertain(format!(
                        "Failed to sync exclusive-create cleanup: {err}"
                    )));
                }
                return Err(SessionStoreError::Io(format!(
                    "Exclusive create failed (cleaned up partial file): {write_err}"
                )));
            }
            if let Err(err) = sync_parent_directory(&self.path) {
                self.poisoned = true;
                return Err(SessionStoreError::StorageUncertain(format!(
                    "Failed to sync newly created session: {err}"
                )));
            }
            return Ok(());
        }

        // exclusive_create == false：写入同目录唯一临时文件 -> sync_all -> 原子重命名替换
        let file_name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("session.jsonl");
        let tmp_path = self
            .path
            .with_file_name(format!("{file_name}.tmp.{}", Uuid::new_v4()));

        let write_tmp_res = (|| -> Result<(), std::io::Error> {
            let mut tmp_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            tmp_file.write_all(&payload)?;
            tmp_file.sync_all()?;
            Ok(())
        })();

        #[cfg(feature = "test-support")]
        let (fail_before_rename, fail_after_rename_sync) = {
            let mut guard = self.fault_injection.lock().expect("fault lock");
            let fbr = guard.fail_rewrite_before_rename;
            let far = guard.fail_rewrite_after_rename_sync;
            guard.fail_rewrite_before_rename = false;
            guard.fail_rewrite_after_rename_sync = false;
            (fbr, far)
        };

        if let Err(err) = write_tmp_res {
            let _ = fs::remove_file(&tmp_path);
            return Err(SessionStoreError::Io(format!(
                "Failed to write temporary snapshot {}: {err}",
                tmp_path.display()
            )));
        }

        #[cfg(feature = "test-support")]
        if fail_before_rename {
            let _ = fs::remove_file(&tmp_path);
            return Err(SessionStoreError::Io(
                "Fault-injected failure before atomic snapshot rename".to_owned(),
            ));
        }

        if let Err(err) = atomic_replace_file_sync(&tmp_path, &self.path) {
            if !tmp_path.exists() {
                self.poisoned = true;
                return Err(SessionStoreError::StorageUncertain(format!(
                    "Snapshot replaced but directory sync failed: {err}"
                )));
            }
            let _ = fs::remove_file(&tmp_path);
            return Err(SessionStoreError::Io(format!(
                "Failed to atomically replace {} with {}: {err}",
                self.path.display(),
                tmp_path.display()
            )));
        }

        #[cfg(feature = "test-support")]
        if fail_after_rename_sync {
            self.poisoned = true;
            return Err(SessionStoreError::StorageUncertain(
                "Fault-injected post-rename verification failure".to_owned(),
            ));
        }

        Ok(())
    }

    fn append_entry(&mut self, entry: &SessionEntry) -> Result<(), SessionStoreError> {
        if self.poisoned {
            return Err(SessionStoreError::Poisoned(format!(
                "Session store is poisoned: {}",
                self.path.display()
            )));
        }

        let mut line = serde_json::to_string(entry)
            .map_err(|e| SessionStoreError::Serialization(e.to_string()))?;
        line.push('\n');
        let line_bytes = line.as_bytes();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|e| {
                SessionStoreError::Io(format!(
                    "Failed to open {} for append: {e}",
                    self.path.display()
                ))
            })?;

        let orig_len = file
            .metadata()
            .map_err(|e| {
                SessionStoreError::Io(format!("Failed to stat {}: {e}", self.path.display()))
            })?
            .len();

        file.seek(SeekFrom::End(0)).map_err(|e| {
            SessionStoreError::Io(format!(
                "Failed to seek to end of {}: {e}",
                self.path.display()
            ))
        })?;

        #[cfg(feature = "test-support")]
        let (fail_after_bytes, fail_recovery) = {
            let mut guard = self.fault_injection.lock().expect("fault lock");
            let fab = guard.fail_append_after_bytes.take();
            let fr = guard.fail_append_recovery;
            guard.fail_append_recovery = false;
            (fab, fr)
        };

        #[cfg(not(feature = "test-support"))]
        let append_res = file.write_all(line_bytes).and_then(|()| file.sync_all());
        #[cfg(feature = "test-support")]
        let append_res = if let Some(partial_len) = fail_after_bytes {
            let n = partial_len.min(line_bytes.len());
            if n > 0 {
                let _ = file.write_all(&line_bytes[..n]);
                let _ = file.sync_all();
            }
            Err(std::io::Error::other(
                "Fault-injected partial append failure",
            ))
        } else {
            file.write_all(line_bytes).and_then(|()| file.sync_all())
        };

        if let Err(write_err) = append_res {
            // 部分写入或同步失败：截断回原长度 `orig_len` 并再次同步
            #[cfg(not(feature = "test-support"))]
            let recovery_res = file.set_len(orig_len).and_then(|()| file.sync_all());
            #[cfg(feature = "test-support")]
            let recovery_res = if fail_recovery {
                Err(std::io::Error::other(
                    "Fault-injected truncate recovery failure",
                ))
            } else {
                file.set_len(orig_len).and_then(|()| file.sync_all())
            };

            if let Err(rec_err) = recovery_res {
                self.poisoned = true;
                return Err(SessionStoreError::StorageUncertain(format!(
                    "Append failed ({write_err}) and tail recovery to {orig_len} bytes failed ({rec_err})"
                )));
            }

            return Err(SessionStoreError::Io(format!(
                "Append failed (restored tail to {orig_len} bytes): {write_err}"
            )));
        }

        Ok(())
    }
}

// =============================================================================
// 5. SQLite Pragmas & Volume Mode Specification (Reserved for SQLite Adapter)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteJournalMode {
    Wal,
    Truncate,
    Persist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSynchronousMode {
    Normal,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteLockingMode {
    Normal,
    Exclusive,
}

/// SQLite 针对 U 盘（FAT32/exFAT/可移动介质）与本机固定 NTFS 磁盘的自适应日志与同步配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlitePragmaConfig {
    pub journal_mode: SqliteJournalMode,
    pub synchronous: SqliteSynchronousMode,
    pub locking_mode: SqliteLockingMode,
}

impl SqlitePragmaConfig {
    /// 根据文件系统名称与是否为可移动驱动器生成对应的 SQLite PRAGMA 配置。
    pub fn for_volume(fs_name: &str, is_removable: bool) -> Self {
        let upper = fs_name.trim().to_ascii_uppercase();
        let is_fat_family =
            upper.contains("FAT") || upper.contains("EXFAT") || upper.contains("MSDOS");
        if is_removable || is_fat_family {
            Self {
                journal_mode: SqliteJournalMode::Truncate,
                synchronous: SqliteSynchronousMode::Full,
                locking_mode: SqliteLockingMode::Exclusive,
            }
        } else {
            Self {
                journal_mode: SqliteJournalMode::Wal,
                synchronous: SqliteSynchronousMode::Normal,
                locking_mode: SqliteLockingMode::Normal,
            }
        }
    }

    pub fn to_pragma_sql(&self) -> Vec<String> {
        let journal = match self.journal_mode {
            SqliteJournalMode::Wal => "WAL",
            SqliteJournalMode::Truncate => "TRUNCATE",
            SqliteJournalMode::Persist => "PERSIST",
        };
        let sync = match self.synchronous {
            SqliteSynchronousMode::Normal => "NORMAL",
            SqliteSynchronousMode::Full => "FULL",
        };
        let locking = match self.locking_mode {
            SqliteLockingMode::Normal => "NORMAL",
            SqliteLockingMode::Exclusive => "EXCLUSIVE",
        };
        vec![
            format!("PRAGMA locking_mode = {locking};"),
            format!("PRAGMA journal_mode = {journal};"),
            format!("PRAGMA synchronous = {sync};"),
        ]
    }
}

/// 读取会话文件头（以 4 KiB 块流式扫描，上界 `MAX_SESSION_HEADER_SCAN_BYTES = 1 MiB`）。
pub fn read_session_header(file_path: &Path) -> Result<Option<SessionHeader>, SessionStoreError> {
    let mut file = File::open(file_path).map_err(|e| {
        SessionStoreError::Io(format!("Failed to open {}: {e}", file_path.display()))
    })?;
    let mut buffer = [0u8; 4096];
    let mut pending_bytes: Vec<u8> = Vec::new();
    let mut scanned_bytes = 0usize;

    while scanned_bytes < MAX_SESSION_HEADER_SCAN_BYTES {
        let max_read = buffer
            .len()
            .min(MAX_SESSION_HEADER_SCAN_BYTES - scanned_bytes);
        let bytes_read = file.read(&mut buffer[..max_read]).map_err(|e| {
            SessionStoreError::Io(format!("Failed to read {}: {e}", file_path.display()))
        })?;
        if bytes_read == 0 {
            let line = String::from_utf8_lossy(&pending_bytes);
            return Ok(parse_header_candidate(&line).flatten());
        }
        scanned_bytes += bytes_read;
        pending_bytes.extend_from_slice(&buffer[..bytes_read]);

        let mut search_start = 0usize;
        while let Some(rel_pos) = pending_bytes[search_start..]
            .iter()
            .position(|&b| b == b'\n')
        {
            let newline_idx = search_start + rel_pos;
            let line = String::from_utf8_lossy(&pending_bytes[search_start..newline_idx]);
            if let Some(candidate) = parse_header_candidate(&line) {
                return Ok(candidate);
            }
            search_start = newline_idx + 1;
        }
        if search_start > 0 {
            pending_bytes.drain(..search_start);
        }
    }

    // 探测是否恰好在 1 MiB 边界处到达 EOF
    let mut probe = [0u8; 1];
    let extra = file.read(&mut probe).map_err(|e| {
        SessionStoreError::Io(format!("Failed to probe {}: {e}", file_path.display()))
    })?;
    if extra == 0 {
        let line = String::from_utf8_lossy(&pending_bytes);
        return Ok(parse_header_candidate(&line).flatten());
    }

    Err(SessionStoreError::HeaderScanLimitExceeded(
        file_path.to_path_buf(),
    ))
}

fn parse_header_candidate(line: &str) -> Option<Option<SessionHeader>> {
    if line.trim().is_empty() {
        return None;
    }
    let entry = parse_session_entry_line(line)?;
    match entry {
        FileEntry::Session(header) if !header.id.is_empty() => Some(Some(header)),
        _ => Some(None),
    }
}

/// 已关闭的只读存储占位实例：用于 `release_persistent_store()` 主动释放 `.lock` 文件锁后拒绝一切后续写入。
#[derive(Debug, Clone)]
pub struct ClosedSessionStore {
    location: Option<PathBuf>,
    was_persisted: bool,
    was_poisoned: bool,
}

impl ClosedSessionStore {
    pub fn new(location: Option<PathBuf>, was_persisted: bool, was_poisoned: bool) -> Self {
        Self {
            location,
            was_persisted,
            was_poisoned,
        }
    }
}

impl SessionStore for ClosedSessionStore {
    fn location(&self) -> Option<&Path> {
        self.location.as_deref()
    }

    fn is_persisted(&self) -> bool {
        self.was_persisted
    }

    fn is_poisoned(&self) -> bool {
        self.was_poisoned
    }

    fn load_entries(&mut self) -> Result<Vec<FileEntry>, SessionStoreError> {
        Err(SessionStoreError::InvalidOperation(
            "Session store has been closed".to_owned(),
        ))
    }

    fn rewrite_all(
        &mut self,
        _entries: &[FileEntry],
        _exclusive_create: bool,
    ) -> Result<(), SessionStoreError> {
        Err(SessionStoreError::InvalidOperation(
            "Session store has been closed".to_owned(),
        ))
    }

    fn append_entry(&mut self, _entry: &SessionEntry) -> Result<(), SessionStoreError> {
        Err(SessionStoreError::InvalidOperation(
            "Session store has been closed".to_owned(),
        ))
    }
}

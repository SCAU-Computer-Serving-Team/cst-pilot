use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use pi_ai::{
    ApiKeyCredential, BoxFuture, Credential, CredentialInfo, CredentialInspection,
    CredentialModifyFn, CredentialStore,
};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::config_value::{
    ConfigValueResolver, ConfigValueSource, is_command_config_value, is_config_value_configured,
};

const WINDOWS_REPLACE_BACKOFF_MS: [u64; 5] = [15, 30, 60, 120, 150];
const LOCK_ACQUIRE_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredAuthStatus {
    Missing,
    ConfiguredLiteralOrEnv,
    MissingEnv,
    StoredCommand,
    OAuth,
}

fn inspect_credential(mut credential: Credential) -> CredentialInspection {
    let configured = match &mut credential {
        Credential::OAuth(_) => true,
        Credential::ApiKey(api) => match api.key.take() {
            Some(key) if is_command_config_value(&key) => true,
            Some(key) => {
                let configured = is_config_value_configured(&key, api.env.as_ref());
                // Only metadata is needed by provider filters; resolve pure templates.
                api.key = if configured {
                    ConfigValueResolver::default().resolve(
                        &key,
                        api.env.as_ref(),
                        ConfigValueSource::Global,
                        None,
                    )
                } else {
                    None
                };
                configured
            }
            None => false,
        },
    };
    CredentialInspection {
        credential,
        configured,
    }
}

pub(crate) fn strip_utf8_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

pub(crate) fn lock_path_for(target: &Path) -> PathBuf {
    let mut os = target.as_os_str().to_os_string();
    os.push(".lock");
    PathBuf::from(os)
}

pub(crate) struct DedicatedFileLockGuard {
    _file: File,
}

fn ensure_private_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(parent)
            .map_err(|_| "Failed to create configuration directory".to_owned())?;
    }
    Ok(())
}

impl DedicatedFileLockGuard {
    pub fn acquire(target_path: &Path) -> Result<Self, String> {
        Self::acquire_cancellable(target_path, None)
    }

    pub async fn acquire_async(target_path: &Path) -> Result<Self, String> {
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let path = target_path.to_owned();
        tokio::task::spawn_blocking(move || Self::acquire_cancellable(&path, Some(&cancel)))
            .await
            .map_err(|_| "Configuration lock task failed".to_owned())?
    }

    fn acquire_cancellable(
        target_path: &Path,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<Self, String> {
        let lock_path = lock_path_for(target_path);
        ensure_private_parent(&lock_path)?;

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let start = Instant::now();
        let mut attempts = 0;
        let file = loop {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err("Configuration lock cancelled".to_owned());
            }
            match options.open(&lock_path) {
                Ok(file) => break file,
                Err(err)
                    if cfg!(windows)
                        && attempts < 5
                        && matches!(err.raw_os_error(), Some(5 | 32 | 33)) =>
                {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(10 * attempts));
                }
                Err(_) => return Err("Failed to open configuration lock".to_owned()),
            }
        };
        loop {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err("Configuration lock cancelled".to_owned());
            }
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if start.elapsed() >= Duration::from_millis(LOCK_ACQUIRE_TIMEOUT_MS) {
                        return Err("Timed out acquiring configuration lock".to_owned());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::Error(_)) => {
                    return Err("Failed to acquire configuration lock".to_owned());
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtomicWriteOutcome {
    CommittedAndSynced,
    CommittedWithParentSyncWarning(String),
}

/// 同目录临时文件 + `sync_all()` + 原子重命名替换（区分重命名前失败与重命名后父目录同步警告）。
pub(crate) fn atomic_write_file_sync(
    target: &Path,
    content: &[u8],
    unix_mode: u32,
) -> Result<AtomicWriteOutcome, String> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_parent(target)?;

    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let tmp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let write_res = (|| -> Result<(), String> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(unix_mode);
        }
        #[cfg(not(unix))]
        let _ = unix_mode;

        let mut tmp_file = opts
            .open(&tmp_path)
            .map_err(|e| format!("Failed to create temporary file: {e}"))?;
        tmp_file
            .write_all(content)
            .map_err(|e| format!("Failed to write temporary file: {e}"))?;
        tmp_file
            .flush()
            .map_err(|e| format!("Failed to flush temporary file: {e}"))?;
        tmp_file
            .sync_all()
            .map_err(|e| format!("Failed to sync temporary file: {e}"))?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    let mut last_err = None;
    let mut replaced = false;
    for (idx, delay_ms) in WINDOWS_REPLACE_BACKOFF_MS.iter().enumerate() {
        match fs::rename(&tmp_path, target) {
            Ok(()) => {
                replaced = true;
                break;
            }
            Err(err) => {
                let retryable = matches!(
                    err.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
                ) || matches!(err.raw_os_error(), Some(5 | 32));
                last_err = Some(err);
                if !retryable || idx + 1 == WINDOWS_REPLACE_BACKOFF_MS.len() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(*delay_ms));
            }
        }
    }

    if !replaced {
        let _ = fs::remove_file(&tmp_path);
        let err = last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown rename error".to_owned());
        return Err(format!("Failed to replace target file atomically: {err}"));
    }

    #[cfg(unix)]
    if File::open(parent).and_then(|dir| dir.sync_all()).is_err() {
        return Ok(AtomicWriteOutcome::CommittedWithParentSyncWarning(
            "Configuration committed but parent directory sync failed".to_owned(),
        ));
    }

    Ok(AtomicWriteOutcome::CommittedAndSynced)
}

type ParsedAuthMap = (Map<String, Value>, BTreeMap<String, Credential>);

fn validate_and_parse_auth_map(raw_content: &str) -> Result<ParsedAuthMap, String> {
    let stripped = strip_utf8_bom(raw_content);
    if stripped.trim().is_empty() {
        return Err("Invalid auth.json: file is empty".to_owned());
    }
    let parsed: Value = serde_json::from_str(stripped)
        .map_err(|_| "Failed to parse auth.json: invalid JSON syntax".to_owned())?;
    let Some(obj) = parsed.as_object() else {
        return Err("Invalid auth.json: expected a JSON object".to_owned());
    };

    let mut credentials = BTreeMap::new();
    for (provider_id, val) in obj {
        let cred = validate_credential_entry(provider_id, val)?;
        credentials.insert(provider_id.clone(), cred);
    }
    Ok((obj.clone(), credentials))
}

fn validate_credential_entry(_provider_id: &str, val: &Value) -> Result<Credential, String> {
    let invalid = || "Invalid auth.json credential structure".to_owned();
    let obj = val.as_object().ok_or_else(invalid)?;
    match obj.get("type").and_then(Value::as_str) {
        Some("api_key") => {
            if !obj.get("key").is_none_or(Value::is_string)
                || !obj.get("env").is_none_or(|e| {
                    e.as_object()
                        .is_some_and(|m| m.values().all(Value::is_string))
                })
            {
                return Err(invalid());
            }
        }
        Some("oauth") => {
            if !obj.get("access").is_some_and(Value::is_string)
                || !obj.get("refresh").is_some_and(Value::is_string)
                || !obj.get("expires").and_then(Value::as_u64).is_some()
            {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    serde_json::from_value::<Credential>(val.clone()).map_err(|_| invalid())
}

/// 基于 `auth.json` 的可读写凭据存储（带跨进程 `<auth_path>.lock` 互斥锁、锁内读改写与损坏保护）。
#[derive(Clone)]
pub struct FileAuthStorage {
    auth_path: PathBuf,
    resolver: ConfigValueResolver,
    in_process_lock: Arc<tokio::sync::Mutex<()>>,
    load_failed: Arc<AtomicBool>,
    write_uncertain: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for FileAuthStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileAuthStorage")
            .field("auth_path", &self.auth_path)
            .field("load_failed", &self.load_failed.load(Ordering::SeqCst))
            .finish()
    }
}

impl FileAuthStorage {
    pub fn new(auth_path: impl Into<PathBuf>, resolver: ConfigValueResolver) -> Self {
        let storage = Self {
            auth_path: auth_path.into(),
            resolver,
            in_process_lock: Arc::new(tokio::sync::Mutex::new(())),
            load_failed: Arc::new(AtomicBool::new(false)),
            write_uncertain: Arc::new(AtomicBool::new(false)),
            last_error: Arc::new(Mutex::new(None)),
        };
        let _ = storage.load_raw_unlocked();
        storage
    }

    pub fn auth_path(&self) -> &Path {
        &self.auth_path
    }

    pub fn resolver(&self) -> &ConfigValueResolver {
        &self.resolver
    }

    pub fn has_load_error(&self) -> bool {
        self.load_failed.load(Ordering::SeqCst) || self.write_uncertain.load(Ordering::SeqCst)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().expect("error lock").clone()
    }

    pub async fn reload(&self) -> Result<(), String> {
        let _guard = self.in_process_lock.lock().await;
        let _file_lock = DedicatedFileLockGuard::acquire_async(&self.auth_path).await?;
        self.load_raw_unlocked()?;
        self.write_uncertain.store(false, Ordering::SeqCst);
        *self.last_error.lock().expect("error lock") = None;
        Ok(())
    }

    fn check_write_outcome(&self, outcome: AtomicWriteOutcome) -> Result<(), String> {
        match outcome {
            AtomicWriteOutcome::CommittedAndSynced => Ok(()),
            AtomicWriteOutcome::CommittedWithParentSyncWarning(_) => {
                self.write_uncertain.store(true, Ordering::SeqCst);
                let error = "Auth configuration committed but directory sync failed; reload before writing again".to_owned();
                *self.last_error.lock().expect("error lock") = Some(error.clone());
                Err(error)
            }
        }
    }

    fn load_raw_unlocked(&self) -> Result<ParsedAuthMap, String> {
        if !self.auth_path.exists() {
            self.load_failed.store(false, Ordering::SeqCst);
            if !self.write_uncertain.load(Ordering::SeqCst) {
                *self.last_error.lock().expect("error lock") = None;
            }
            return Ok((Map::new(), BTreeMap::new()));
        }
        let mut raw = String::new();
        File::open(&self.auth_path)
            .and_then(|mut f| f.read_to_string(&mut raw))
            .map_err(|_| {
                let error = "Failed to read auth.json".to_owned();
                self.load_failed.store(true, Ordering::SeqCst);
                *self.last_error.lock().expect("error lock") = Some(error.clone());
                error
            })?;
        match validate_and_parse_auth_map(&raw) {
            Ok(res) => {
                self.load_failed.store(false, Ordering::SeqCst);
                if !self.write_uncertain.load(Ordering::SeqCst) {
                    *self.last_error.lock().expect("error lock") = None;
                }
                Ok(res)
            }
            Err(err) => {
                self.load_failed.store(true, Ordering::SeqCst);
                *self.last_error.lock().expect("error lock") = Some(err.clone());
                Err(err)
            }
        }
    }

    /// 只读检查某服务商在 `auth.json` 中的凭据配置状态（零 Shell 命令执行）。
    pub fn check_stored_status(&self, provider_id: &str) -> StoredAuthStatus {
        let Ok((_, creds)) = self.load_raw_unlocked() else {
            return StoredAuthStatus::Missing;
        };
        let Some(cred) = creds.get(provider_id) else {
            return StoredAuthStatus::Missing;
        };
        match cred {
            Credential::OAuth(_) => StoredAuthStatus::OAuth,
            Credential::ApiKey(api) => {
                let Some(key) = api.key.as_deref() else {
                    return StoredAuthStatus::Missing;
                };
                if is_command_config_value(key) {
                    StoredAuthStatus::StoredCommand
                } else if is_config_value_configured(key, api.env.as_ref()) {
                    StoredAuthStatus::ConfiguredLiteralOrEnv
                } else {
                    StoredAuthStatus::MissingEnv
                }
            }
        }
    }
}

impl CredentialStore for FileAuthStorage {
    fn inspect(&self, provider_id: &str) -> Option<CredentialInspection> {
        let (_, mut credentials) = self.load_raw_unlocked().ok()?;
        credentials.remove(provider_id).map(inspect_credential)
    }

    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let _proc_guard = self.in_process_lock.lock().await;
            let (_, mut creds) = self.load_raw_unlocked()?;
            let Some(cred) = creds.remove(provider_id) else {
                return Ok(None);
            };
            match cred {
                Credential::ApiKey(mut api) => {
                    if let Some(raw_key) = api.key.take() {
                        api.key = self
                            .resolver
                            .resolve_async(raw_key, api.env.clone(), ConfigValueSource::Global)
                            .await?;
                    }
                    Ok(Some(Credential::ApiKey(api)))
                }
                Credential::OAuth(oauth) => Ok(Some(Credential::OAuth(oauth))),
            }
        })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>> {
        Box::pin(async move {
            let _proc_guard = self.in_process_lock.lock().await;
            let (_, creds) = self.load_raw_unlocked()?;
            Ok(creds
                .into_iter()
                .map(|(provider_id, c)| CredentialInfo {
                    provider_id,
                    auth_type: c.auth_type(),
                })
                .collect())
        })
    }

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let _proc_guard = self.in_process_lock.lock().await;
            let _file_lock = DedicatedFileLockGuard::acquire_async(&self.auth_path).await?;
            if self.write_uncertain.load(Ordering::SeqCst) {
                return Err("Auth configuration requires reload after uncertain commit".to_owned());
            }
            let (mut raw_map, mut creds) = self.load_raw_unlocked()?;
            let current = creds.remove(provider_id);
            let next = f(current.clone()).await?;
            let Some(updated) = next else {
                return Ok(current);
            };

            let mut updated_val = serde_json::to_value(&updated)
                .map_err(|_| "Failed to serialize updated credential".to_owned())?;
            // Retain extension metadata, but remove stale fields of either credential type.
            if let Some(previous) = raw_map.get(provider_id).and_then(Value::as_object) {
                let fields = updated_val.as_object_mut().expect("credential object");
                for (name, value) in previous {
                    if !matches!(
                        name.as_str(),
                        "type" | "key" | "env" | "access" | "refresh" | "expires"
                    ) {
                        fields.entry(name.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
            raw_map.insert(provider_id.to_owned(), updated_val);
            let serialized = serde_json::to_string_pretty(&Value::Object(raw_map))
                .map_err(|_| "Failed to serialize auth.json".to_owned())?;
            let outcome = atomic_write_file_sync(&self.auth_path, serialized.as_bytes(), 0o600)?;
            self.resolver.clear_cache();
            self.check_write_outcome(outcome)?;
            Ok(Some(updated))
        })
    }

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let _proc_guard = self.in_process_lock.lock().await;
            let _file_lock = DedicatedFileLockGuard::acquire_async(&self.auth_path).await?;
            if self.write_uncertain.load(Ordering::SeqCst) {
                return Err("Auth configuration requires reload after uncertain commit".to_owned());
            }
            let (mut raw_map, _) = self.load_raw_unlocked()?;
            if raw_map.remove(provider_id).is_some() {
                let serialized = serde_json::to_string_pretty(&Value::Object(raw_map))
                    .map_err(|_| "Failed to serialize auth.json".to_owned())?;
                let outcome =
                    atomic_write_file_sync(&self.auth_path, serialized.as_bytes(), 0o600)?;
                self.resolver.clear_cache();
                self.check_write_outcome(outcome)?;
            }
            self.resolver.clear_cache();
            Ok(())
        })
    }
}

/// 只读凭据存储（对照 `ReadOnlyAuthStorage`）：
/// 1. 绝不执行 `!command`（遇到命令凭据时跳过命令求值并将 `key` 置为 `None`，防止将命令文本误当成 API Key）。
/// 2. 保留 `list()` 与 `check_stored_status()` 中的元信息。
/// 3. 任何 `modify` / `delete` 均返回只读错误。
#[derive(Clone)]
pub struct ReadOnlyAuthStorage {
    auth_path: PathBuf,
    resolver: ConfigValueResolver,
    cached: Arc<Mutex<Option<BTreeMap<String, Credential>>>>,
}

impl std::fmt::Debug for ReadOnlyAuthStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadOnlyAuthStorage")
            .field("auth_path", &self.auth_path)
            .finish()
    }
}

impl ReadOnlyAuthStorage {
    pub fn new(auth_path: impl Into<PathBuf>, resolver: ConfigValueResolver) -> Self {
        Self {
            auth_path: auth_path.into(),
            resolver,
            cached: Arc::new(Mutex::new(None)),
        }
    }

    fn load_map(&self) -> Result<BTreeMap<String, Credential>, String> {
        if let Some(existing) = self.cached.lock().expect("cache lock").as_ref() {
            return Ok(existing.clone());
        }
        if !self.auth_path.exists() {
            let empty = BTreeMap::new();
            *self.cached.lock().expect("cache lock") = Some(empty.clone());
            return Ok(empty);
        }
        let mut raw = String::new();
        File::open(&self.auth_path)
            .and_then(|mut f| f.read_to_string(&mut raw))
            .map_err(|_| "Failed to read auth.json".to_owned())?;
        let (_, creds) = validate_and_parse_auth_map(&raw)?;
        *self.cached.lock().expect("cache lock") = Some(creds.clone());
        Ok(creds)
    }

    pub fn check_stored_status(&self, provider_id: &str) -> Result<StoredAuthStatus, String> {
        let creds = self.load_map()?;
        let Some(cred) = creds.get(provider_id) else {
            return Ok(StoredAuthStatus::Missing);
        };
        Ok(match cred {
            Credential::OAuth(_) => StoredAuthStatus::OAuth,
            Credential::ApiKey(api) => {
                let Some(key) = api.key.as_deref() else {
                    return Ok(StoredAuthStatus::Missing);
                };
                if is_command_config_value(key) {
                    StoredAuthStatus::StoredCommand
                } else if is_config_value_configured(key, api.env.as_ref()) {
                    StoredAuthStatus::ConfiguredLiteralOrEnv
                } else {
                    StoredAuthStatus::MissingEnv
                }
            }
        })
    }
}

impl CredentialStore for ReadOnlyAuthStorage {
    fn inspect(&self, provider_id: &str) -> Option<CredentialInspection> {
        self.load_map()
            .ok()?
            .remove(provider_id)
            .map(inspect_credential)
    }

    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let mut creds = self.load_map()?;
            let Some(cred) = creds.remove(provider_id) else {
                return Ok(None);
            };
            match cred {
                Credential::ApiKey(mut api) => {
                    if let Some(raw_key) = api.key.take() {
                        if is_command_config_value(&raw_key) {
                            // 只读认证存储跳过命令执行，且不把命令字符串当作已解析密钥返回
                            api.key = None;
                        } else {
                            api.key = self.resolver.resolve(
                                &raw_key,
                                api.env.as_ref(),
                                ConfigValueSource::Global,
                                None,
                            );
                        }
                    }
                    Ok(Some(Credential::ApiKey(api)))
                }
                Credential::OAuth(oauth) => Ok(Some(Credential::OAuth(oauth))),
            }
        })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>> {
        Box::pin(async move {
            let creds = self.load_map()?;
            Ok(creds
                .into_iter()
                .map(|(provider_id, c)| CredentialInfo {
                    provider_id,
                    auth_type: c.auth_type(),
                })
                .collect())
        })
    }

    fn modify<'a>(
        &'a self,
        _provider_id: &'a str,
        _f: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async { Err("Read-only credential storage cannot modify auth.json".to_owned()) })
    }

    fn delete<'a>(&'a self, _provider_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("Read-only credential storage cannot modify auth.json".to_owned()) })
    }
}

/// 内存凭据存储（支持 `$ENV_VAR` 与受控 `!command` 解析）。
#[derive(Clone, Default)]
pub struct InMemoryAuthStorage {
    entries: Arc<RwLock<BTreeMap<String, Credential>>>,
    resolver: ConfigValueResolver,
    mutex: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for InMemoryAuthStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let providers: Vec<String> = self.entries.read().expect("lock").keys().cloned().collect();
        f.debug_struct("InMemoryAuthStorage")
            .field("providers", &providers)
            .finish()
    }
}

impl InMemoryAuthStorage {
    pub fn new(initial: BTreeMap<String, Credential>, resolver: ConfigValueResolver) -> Self {
        Self {
            entries: Arc::new(RwLock::new(initial)),
            resolver,
            mutex: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn check_stored_status(&self, provider_id: &str) -> StoredAuthStatus {
        let guard = self.entries.read().expect("lock");
        let Some(cred) = guard.get(provider_id) else {
            return StoredAuthStatus::Missing;
        };
        match cred {
            Credential::OAuth(_) => StoredAuthStatus::OAuth,
            Credential::ApiKey(api) => {
                let Some(key) = api.key.as_deref() else {
                    return StoredAuthStatus::Missing;
                };
                if is_command_config_value(key) {
                    StoredAuthStatus::StoredCommand
                } else if is_config_value_configured(key, api.env.as_ref()) {
                    StoredAuthStatus::ConfiguredLiteralOrEnv
                } else {
                    StoredAuthStatus::MissingEnv
                }
            }
        }
    }
}

impl CredentialStore for InMemoryAuthStorage {
    fn inspect(&self, provider_id: &str) -> Option<CredentialInspection> {
        self.entries
            .read()
            .expect("lock")
            .get(provider_id)
            .cloned()
            .map(inspect_credential)
    }

    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let cred = self.entries.read().expect("lock").get(provider_id).cloned();
            let Some(cred) = cred else {
                return Ok(None);
            };
            match cred {
                Credential::ApiKey(mut api) => {
                    if let Some(raw_key) = api.key.take() {
                        api.key = self
                            .resolver
                            .resolve_async(raw_key, api.env.clone(), ConfigValueSource::Global)
                            .await?;
                    }
                    Ok(Some(Credential::ApiKey(api)))
                }
                Credential::OAuth(oauth) => Ok(Some(Credential::OAuth(oauth))),
            }
        })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>> {
        Box::pin(async move {
            Ok(self
                .entries
                .read()
                .expect("lock")
                .iter()
                .map(|(provider_id, c)| CredentialInfo {
                    provider_id: provider_id.clone(),
                    auth_type: c.auth_type(),
                })
                .collect())
        })
    }

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let _guard = self.mutex.lock().await;
            let current = self.entries.read().expect("lock").get(provider_id).cloned();
            let next = f(current.clone()).await?;
            if let Some(ref updated) = next {
                self.entries
                    .write()
                    .expect("lock")
                    .insert(provider_id.to_owned(), updated.clone());
                self.resolver.clear_cache();
                Ok(Some(updated.clone()))
            } else {
                Ok(current)
            }
        })
    }

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let _guard = self.mutex.lock().await;
            self.entries.write().expect("lock").remove(provider_id);
            self.resolver.clear_cache();
            Ok(())
        })
    }
}

/// 内存级临时 API Key 覆盖层（对照 `RuntimeCredentials`）。
#[derive(Clone)]
pub struct RuntimeCredentials {
    store: Arc<dyn CredentialStore>,
    overrides: Arc<RwLock<BTreeMap<String, String>>>,
    generation: Arc<AtomicU64>,
    resolver: ConfigValueResolver,
}

impl std::fmt::Debug for RuntimeCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let override_providers: Vec<String> = self
            .overrides
            .read()
            .expect("overrides lock")
            .keys()
            .cloned()
            .collect();
        f.debug_struct("RuntimeCredentials")
            .field("override_providers", &override_providers)
            .finish()
    }
}

impl RuntimeCredentials {
    pub fn new(store: Arc<dyn CredentialStore>, resolver: ConfigValueResolver) -> Self {
        Self {
            store,
            overrides: Arc::new(RwLock::new(BTreeMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
            resolver,
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn inner_store(&self) -> &Arc<dyn CredentialStore> {
        &self.store
    }

    pub fn set_runtime_api_key(&self, provider_id: impl Into<String>, api_key: impl Into<String>) {
        self.overrides
            .write()
            .expect("overrides lock")
            .insert(provider_id.into(), api_key.into());
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.resolver.clear_cache();
    }

    pub fn remove_runtime_api_key(&self, provider_id: &str) {
        self.overrides
            .write()
            .expect("overrides lock")
            .remove(provider_id);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.resolver.clear_cache();
    }

    pub fn has_runtime_api_key(&self, provider_id: &str) -> bool {
        self.overrides
            .read()
            .expect("overrides lock")
            .contains_key(provider_id)
    }

    pub fn runtime_api_key(&self, provider_id: &str) -> Option<String> {
        self.overrides
            .read()
            .expect("overrides lock")
            .get(provider_id)
            .cloned()
    }
}

impl CredentialStore for RuntimeCredentials {
    fn inspect(&self, provider_id: &str) -> Option<CredentialInspection> {
        if let Some(key) = self.runtime_api_key(provider_id) {
            let configured = !key.is_empty();
            return Some(CredentialInspection {
                credential: Credential::ApiKey(ApiKeyCredential {
                    key: Some(key),
                    env: None,
                }),
                configured,
            });
        }
        self.store.inspect(provider_id)
    }

    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        if let Some(override_key) = self.runtime_api_key(provider_id) {
            return Box::pin(async move {
                Ok(Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some(override_key),
                    env: None,
                })))
            });
        }
        self.store.read(provider_id)
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>> {
        Box::pin(async move {
            let base = self.store.list().await?;
            let mut by_id: BTreeMap<String, CredentialInfo> = base
                .into_iter()
                .map(|info| (info.provider_id.clone(), info))
                .collect();
            for pid in self.overrides.read().expect("overrides lock").keys() {
                by_id.insert(
                    pid.clone(),
                    CredentialInfo {
                        provider_id: pid.clone(),
                        auth_type: pi_ai::AuthType::ApiKey,
                    },
                );
            }
            Ok(by_id.into_values().collect())
        })
    }

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let result = self.store.modify(provider_id, f).await?;
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.resolver.clear_cache();
            Ok(result)
        })
    }

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.store.delete(provider_id).await?;
            self.remove_runtime_api_key(provider_id);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn uncertain_auth_commit_blocks_further_writes_until_reload() {
        let path = PathBuf::from("E:/tmp/2026-11-04/argon-phase4-development/tests")
            .join(format!("uncertain-{}", Uuid::new_v4()))
            .join("auth.json");
        let store = FileAuthStorage::new(&path, ConfigValueResolver::default());
        let error = store
            .check_write_outcome(AtomicWriteOutcome::CommittedWithParentSyncWarning(
                "mock-private-io".into(),
            ))
            .unwrap_err();
        assert!(error.contains("committed"));
        assert!(!error.contains("mock-private"));
        assert!(store.delete("one").await.is_err());
        assert!(
            store
                .modify("one", Box::new(|_| Box::pin(async { Ok(None) })))
                .await
                .is_err()
        );
        store.reload().await.unwrap();
        store.delete("one").await.unwrap();
    }
}

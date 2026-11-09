use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use pi_ai::{
    BoxFuture, Credential, CredentialInfo, CredentialInspection, CredentialModifyFn,
    CredentialStore,
};
use pi_runtime::{
    ConfigValueResolver, ConfigValueSource, is_command_config_value, is_config_value_configured,
};
use serde_json::{Map, Value};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::runbook::{
    sequence_store::DirectoryLock,
    storage::{DirectoryLease, Storage},
};

/// One transaction covers read/validate/update/atomic commit for all auth JSON files.
#[derive(Clone)]
pub(crate) struct JsonFiles(Storage, Arc<crate::update::activity::ActivityGate>);
pub(crate) struct Transaction {
    files: JsonFiles,
    _activity: crate::update::activity::ActivityLease,
    _queue: OwnedMutexGuard<()>,
    _paths: DirectoryLease,
    _lock: DirectoryLock,
}
impl JsonFiles {
    pub fn new(root: &Path) -> Result<Self, String> {
        Ok(Self(
            Storage::new(root)?,
            crate::update::activity::ActivityGate::for_root(root)?,
        ))
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.0.root.join("agent/home").join(name)
    }
    pub fn read(&self, name: &str) -> Result<Option<Value>, String> {
        self.read_inner(name, false)
    }
    pub fn read_cache(&self) -> Result<Option<Value>, String> {
        self.read_inner("models-store.json", true)
    }
    fn read_inner(&self, name: &str, discard_invalid: bool) -> Result<Option<Value>, String> {
        // Do not create files while inspecting an offline kit.
        if !self.0.root.join("agent/home").exists() {
            return Ok(None);
        }
        let _paths = self
            .0
            .pin_directory(Path::new("agent/home"), false)
            .map_err(|_| format!("{name} 的目录不可读或包含链接，请检查工具包。"))?;
        let bytes = self
            .0
            .read_limited(&self.path(name), 8 * 1024 * 1024)
            .map_err(|_| format!("{name} 无法安全读取，请检查存储和文件链接。"))?;
        let parsed = bytes
            .map(|bytes| {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| format!("{name} 编码无效，原文件已保留。"))?;
                let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                    .map_err(|_| format!("{name} 无法解析，原文件已保留，请修正后重试。"))?;
                if !value.is_object() {
                    return Err(format!("{name} 必须是 JSON 对象，原文件已保留。"));
                }
                Ok(value)
            })
            .transpose();
        if discard_invalid && parsed.is_err() {
            Ok(None)
        } else {
            parsed
        }
    }
    pub async fn transaction(&self) -> Result<Transaction, String> {
        let activity = self.1.enter()?;
        static WRITERS: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
        let queue = WRITERS
            .get_or_init(|| Arc::new(Mutex::new(())))
            .clone()
            .lock_owned()
            .await;
        let files = self.clone();
        tokio::task::spawn_blocking(move || {
            let paths = files
                .0
                .pin_directory(Path::new("agent/home"), true)
                .map_err(|_| "配置目录不可写或包含链接。")?;
            let lock = DirectoryLock::acquire(&files.path(""))
                .map_err(|_| "配置正在被其他进程修改，请稍后重试。")?;
            Ok(Transaction {
                files,
                _activity: activity,
                _queue: queue,
                _paths: paths,
                _lock: lock,
            })
        })
        .await
        .map_err(|_| "配置事务启动失败。")?
    }
}
impl Transaction {
    pub fn read(&self, name: &str) -> Result<Value, String> {
        Ok(self
            .files
            .read(name)?
            .unwrap_or_else(|| Value::Object(Map::new())))
    }
    /// Rebuildable caches may be replaced only after preserving damaged bytes.
    /// Credential and user configuration files keep their strict read path.
    pub fn read_cache(&self) -> Result<Value, String> {
        if let Some(value) = self.files.read_cache()? {
            return Ok(value);
        }
        if let Some(bytes) = self
            .files
            .0
            .read_limited(&self.files.path("models-store.json"), 8 * 1024 * 1024)?
        {
            let backup = self.files.path(&format!(
                "models-store.corrupt-{}.json",
                uuid::Uuid::new_v4()
            ));
            self.files
                .0
                .atomic(&backup, &bytes, false, &CancellationToken::new())
                .map_err(|_| "模型目录缓存备份失败，原文件已保留。")?;
        }
        Ok(Value::Object(Map::new()))
    }
    pub fn write(&self, name: &str, value: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| "配置无法编码。")?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("配置文件超过 8 MiB。".into());
        }
        bytes.push(b'\n');
        self.files
            .0
            .atomic(
                &self.files.path(name),
                &bytes,
                true,
                &CancellationToken::new(),
            )
            .map_err(|_| format!("{name} 保存失败，请检查存储后重试。"))
    }
}

#[derive(Clone)]
pub(crate) struct KitCredentials(pub JsonFiles, pub ConfigValueResolver);
fn credential(value: Value) -> Result<Credential, String> {
    let parsed: Credential =
        serde_json::from_value(value).map_err(|_| "auth.json 凭据格式无效，原文件已保留。")?;
    match &parsed {
        Credential::ApiKey(c) if c.key.as_ref().is_none_or(|v| v.len() <= 8192) => Ok(parsed),
        Credential::OAuth(c)
            if !c.access.is_empty() && c.access.len() <= 16384 && c.refresh.len() <= 16384 =>
        {
            Ok(parsed)
        }
        _ => Err("auth.json 凭据格式无效，原文件已保留。".into()),
    }
}
impl CredentialStore for KitCredentials {
    fn inspect(&self, id: &str) -> Option<CredentialInspection> {
        let value = self.0.read("auth.json").ok()??.get(id)?.clone();
        let mut credential = credential(value).ok()?;
        let configured = match &mut credential {
            Credential::ApiKey(c) => match c.key.take() {
                Some(key) if is_command_config_value(&key) => true,
                Some(key) => {
                    let configured = is_config_value_configured(&key, c.env.as_ref());
                    c.key = if configured {
                        self.1
                            .resolve(&key, c.env.as_ref(), ConfigValueSource::Global, None)
                    } else {
                        None
                    };
                    configured
                }
                None => false,
            },
            Credential::OAuth(_) => true,
        };
        Some(CredentialInspection {
            credential,
            configured,
        })
    }
    fn read<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let value = self
                .0
                .read("auth.json")?
                .and_then(|v| v.get(id).cloned())
                .map(credential)
                .transpose()?;
            match value {
                Some(Credential::ApiKey(mut api)) => {
                    if let Some(key) = api.key.take() {
                        api.key = self
                            .1
                            .resolve_async(key, api.env.clone(), ConfigValueSource::Global)
                            .await
                            .map_err(|_| "auth.json 凭据暂时无法解析，请检查配置。")?;
                    }
                    Ok(Some(Credential::ApiKey(api)))
                }
                other => Ok(other),
            }
        })
    }
    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>> {
        Box::pin(async move {
            let value = self
                .0
                .read("auth.json")?
                .unwrap_or_else(|| serde_json::json!({}));
            value
                .as_object()
                .ok_or("auth.json 格式无效。")?
                .iter()
                .map(|(id, v)| {
                    Ok(CredentialInfo {
                        provider_id: id.clone(),
                        auth_type: credential(v.clone())?.auth_type(),
                    })
                })
                .collect()
        })
    }
    fn modify<'a>(
        &'a self,
        id: &'a str,
        f: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        Box::pin(async move {
            let tx = self.0.transaction().await?;
            let mut value = tx.read("auth.json")?;
            let previous = value.get(id).cloned().map(credential).transpose()?;
            let Some(next) = f(previous.clone()).await? else {
                return Ok(previous);
            };
            let mut encoded = serde_json::to_value(&next).map_err(|_| "凭据无法编码。")?;
            if let Some(old) = value.get(id).and_then(Value::as_object) {
                for (name, field) in old {
                    if !matches!(
                        name.as_str(),
                        "type" | "key" | "env" | "access" | "refresh" | "expires"
                    ) {
                        encoded
                            .as_object_mut()
                            .expect("credential")
                            .entry(name.clone())
                            .or_insert_with(|| field.clone());
                    }
                }
            }
            value[id] = encoded;
            tx.write("auth.json", &value)?;
            self.1.clear_cache();
            Ok(Some(next))
        })
    }
    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let tx = self.0.transaction().await?;
            let mut value = tx.read("auth.json")?;
            if value
                .as_object_mut()
                .ok_or("auth.json 格式无效。")?
                .remove(id)
                .is_some()
            {
                tx.write("auth.json", &value)?;
            }
            self.1.clear_cache();
            Ok(())
        })
    }
}

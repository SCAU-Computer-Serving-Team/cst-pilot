use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, RwLock};

use pi_ai::{
    AnyModel, BoxFuture, Credential, CredentialStore, Model, ModelsStore, ModelsStoreEntry,
    RefreshModelsInput,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use super::{oa::OaClient, storage::JsonFiles};
const TTL: u64 = 300_000;
const RETRY: u64 = 30_000;
#[derive(Default)]
struct State {
    entry: Option<ModelsStoreEntry>,
    prepared: Option<ModelsStoreEntry>,
    retry: Option<(String, u64)>,
}

/// Provider catalog + persistent ModelsStore adapter. Pi's Vec-returning fetch API
/// does not carry etags; this adapter preserves the credential-bound cache metadata.
pub(crate) struct Catalog {
    files: JsonFiles,
    pub client: OaClient,
    credentials: RwLock<Arc<dyn CredentialStore>>,
    official: Vec<Model>,
    state: Mutex<State>,
    fetch_lock: AsyncMutex<()>,
}
impl Catalog {
    pub fn new(files: JsonFiles, client: OaClient, credentials: Arc<dyn CredentialStore>) -> Self {
        Self {
            files,
            client,
            credentials: RwLock::new(credentials),
            official: pi_ai::builtin_providers()
                .iter()
                .flat_map(|p| p.get_models())
                .collect(),
            state: Mutex::new(State::default()),
            fetch_lock: AsyncMutex::new(()),
        }
    }
    pub fn use_credentials(&self, credentials: Arc<dyn CredentialStore>) {
        *self.credentials.write().expect("credentials") = credentials;
    }
    pub fn identity(&self, credential: &Credential) -> String {
        let (kind, token) = token(credential);
        format!(
            "{:x}",
            Sha256::digest(format!(
                "{}\0{kind}\0{token}",
                self.client.model_base(credential)
            ))
        )
    }
    async fn current(&self) -> Result<Option<Credential>, String> {
        let credentials = self.credentials.read().expect("credentials").clone();
        credentials
            .read("cstoa")
            .await
            .map_err(|_| "CSTOA 凭据暂时无法读取，请检查存储后重试。".into())
    }
    pub fn filter(&self, input: &[Model], credential: Option<&Credential>) -> Vec<Model> {
        let id = credential.map(|c| self.identity(c));
        let state = self.state.lock().expect("catalog");
        let Some(entry) = state
            .entry
            .as_ref()
            .filter(|e| id.is_some() && e.etag == id)
        else {
            return vec![];
        };
        let ids: BTreeSet<_> = entry.models.iter().map(AnyModel::id).collect();
        input
            .iter()
            .filter(|m| ids.contains(m.id.as_str()))
            .cloned()
            .collect()
    }
    fn convert(&self, id: &str, credential: &Credential) -> Result<Model, String> {
        let metadata_id = match id {
            "deepseek-v4.1-flash" | "deepseek-v4-flash" => "deepseek-flash",
            id => id,
        };
        // Builtins are ordered with first-party providers before aggregators.
        let known = self.official.iter().find(|m| m.id == metadata_id);
        let mut value = json!({"id":id,"name":id,"api":"openai-completions","provider":"cstoa","baseUrl":self.client.model_base(credential),
            "input":["text"],"reasoning":false,"contextWindow":32768,"maxTokens":4096,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"compat":{"maxTokensField":"max_tokens","supportsReasoningEffort":false}});
        if let Some(model) = known {
            for key in [
                "input",
                "inputLimits",
                "reasoning",
                "contextWindow",
                "maxTokens",
            ] {
                if let Some(field) = serde_json::to_value(model)
                    .ok()
                    .and_then(|v| v.get(key).cloned())
                {
                    value[key] = field;
                }
            }
            if model.api == "openai-completions" {
                let serialized = serde_json::to_value(model).map_err(|_| "模型规格编码失败。")?;
                for key in [
                    "thinkingLevelMap",
                    "compat",
                    "samplingParams",
                    "samplingParamsByThinkingLevel",
                ] {
                    if let Some(field) = serialized.get(key) {
                        value[key] = field.clone();
                    }
                }
            }
        }
        serde_json::from_value(value).map_err(|_| "模型规格格式无效。".into())
    }
    fn normalized(
        &self,
        mut entry: ModelsStoreEntry,
        credential: &Credential,
    ) -> Result<ModelsStoreEntry, String> {
        if entry.models.len() > 2000 {
            return Err("CSTOA 模型目录超过 2000 项。".into());
        }
        entry.models = entry
            .models
            .iter()
            .map(|model| self.convert(model.id(), credential).map(AnyModel::Chat))
            .collect::<Result<_, _>>()?;
        Ok(entry)
    }
    async fn query(
        &self,
        credential: &Credential,
        cancel: CancellationToken,
    ) -> Result<ModelsStoreEntry, String> {
        let (_, token) = token(credential);
        if token.is_empty() {
            return Err("CSTOA 缺少登录凭据。".into());
        }
        let base = self.client.model_base(credential);
        let (status, value) = self
            .client
            .request_url(format!("{base}/models"), None, Some(token), cancel, 8)
            .await?;
        if status == 401 || status == 403 {
            return Err("cstoa_login_required: CSTOA 登录已失效，请重新登录。".into());
        }
        if !(200..300).contains(&status) {
            return Err(format!("CSTOA 模型清单暂不可用（HTTP {status}）。"));
        }
        let rows = value
            .get("data")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= 2000)
            .ok_or("CSTOA 模型目录格式无效。")?;
        let mut seen = BTreeSet::new();
        let mut models = Vec::new();
        for row in rows {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| {
                    !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
                })
                .ok_or("CSTOA 模型 ID 格式无效。")?;
            if seen.insert(id) {
                models.push(AnyModel::Chat(self.convert(id, credential)?));
            }
        }
        Ok(ModelsStoreEntry {
            models,
            etag: Some(self.identity(credential)),
            checked_at: Some(self.client.clock.now()),
            last_modified: None,
        })
    }
    pub async fn prepare(
        &self,
        credential: &Credential,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let entry = self.query(credential, cancel).await?;
        self.state.lock().expect("catalog").prepared = Some(entry);
        Ok(())
    }
    pub async fn fetch(&self, input: RefreshModelsInput) -> Result<Vec<AnyModel>, String> {
        let _flight = tokio::select! { biased; _ = input.cancel_token.cancelled() => return Err("模型目录更新已取消。".into()), guard = self.fetch_lock.lock() => guard };
        let Some(credential) = input.credential.as_ref() else {
            return Ok(vec![]);
        };
        let id = self.identity(credential);
        if self.current().await?.as_ref().map(|c| self.identity(c)) != Some(id.clone()) {
            return Err("登录凭据已变更，已丢弃旧目录响应。".into());
        }
        let (existing, retry) = {
            let state = self.state.lock().expect("catalog");
            let entry = state
                .prepared
                .as_ref()
                .or(state.entry.as_ref())
                .filter(|e| e.etag.as_ref() == Some(&id))
                .cloned()
                .or(input
                    .stored
                    .clone()
                    .filter(|e| e.etag.as_ref() == Some(&id)));
            (entry, state.retry.clone())
        };
        let now = self.client.clock.now();
        if !input.force
            && (existing
                .as_ref()
                .is_some_and(|e| now.saturating_sub(e.checked_at.unwrap_or(0)) < TTL)
                || retry
                    .as_ref()
                    .is_some_and(|(key, until)| key == &id && now < *until))
        {
            let mut state = self.state.lock().expect("catalog");
            state.entry = existing.clone();
            return Ok(existing.map(|e| e.models).unwrap_or_default());
        }
        let result = self.query(credential, input.cancel_token.clone()).await;
        if input.cancel_token.is_cancelled() {
            return Err("模型目录更新已取消。".into());
        }
        if self.current().await?.as_ref().map(|c| self.identity(c)) != Some(id.clone()) {
            return Err("登录凭据已变更，已丢弃旧目录响应。".into());
        }
        let mut state = self.state.lock().expect("catalog");
        match result {
            Ok(entry) => {
                let models = entry.models.clone();
                state.entry = Some(entry);
                state.retry = None;
                state.prepared = None;
                Ok(models)
            }
            Err(error) => {
                state.entry = existing;
                state.retry = Some((id, now + RETRY));
                Err(error)
            }
        }
    }
}
impl ModelsStore for Catalog {
    fn read<'a>(
        &'a self,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, String>> {
        Box::pin(async move {
            let value = self
                .files
                .read_cache()?
                .and_then(|v| v.get(provider).cloned());
            let stored: Option<ModelsStoreEntry> =
                value.and_then(|v| serde_json::from_value(v).ok());
            if provider != "cstoa" {
                return Ok(stored);
            }
            let credential = self.current().await?;
            let entry = if let Some(credential) = credential {
                let id = self.identity(&credential);
                let memory = {
                    let state = self.state.lock().expect("catalog");
                    state
                        .prepared
                        .as_ref()
                        .filter(|e| e.etag.as_ref() == Some(&id))
                        .cloned()
                        .or_else(|| {
                            state
                                .entry
                                .as_ref()
                                .filter(|e| e.etag.as_ref() == Some(&id))
                                .cloned()
                        })
                };
                let entry = memory.or(stored.filter(|e| e.etag.as_ref() == Some(&id)));
                entry.map(|e| self.normalized(e, &credential)).transpose()?
            } else {
                None
            };
            self.state.lock().expect("catalog").entry = entry.clone();
            // Return an empty entry so Pi clears any previous account's dynamic models.
            Ok(Some(entry.unwrap_or_default()))
        })
    }
    fn write<'a>(
        &'a self,
        provider: &'a str,
        mut entry: ModelsStoreEntry,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if provider == "cstoa" {
                let id = self.current().await?.map(|c| self.identity(&c));
                let state = self.state.lock().expect("catalog");
                let Some(ready) = state
                    .entry
                    .as_ref()
                    .filter(|e| e.etag == id && id.is_some() && e.models == entry.models)
                else {
                    return Err("登录凭据已变更，旧目录不会写入缓存。".into());
                };
                entry = ready.clone();
            }
            let tx = self.files.transaction().await?;
            if provider == "cstoa" && self.current().await?.map(|c| self.identity(&c)) != entry.etag
            {
                return Err("登录凭据已变更，旧目录不会写入缓存。".into());
            }
            let mut value = tx.read("models-store.json")?;
            value[provider] = serde_json::to_value(entry).map_err(|_| "模型目录无法编码。")?;
            tx.write("models-store.json", &value)
        })
    }
    fn delete<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let tx = self.files.transaction().await?;
            let mut value = tx.read("models-store.json")?;
            value
                .as_object_mut()
                .ok_or("模型目录缓存格式无效。")?
                .remove(provider);
            tx.write("models-store.json", &value)?;
            if provider == "cstoa" {
                *self.state.lock().expect("catalog") = State::default();
            }
            Ok(())
        })
    }
}
pub(crate) fn token(credential: &Credential) -> (&'static str, &str) {
    match credential {
        Credential::ApiKey(c) => ("api_key", c.key.as_deref().unwrap_or("")),
        Credential::OAuth(c) => ("oauth", &c.access),
    }
}

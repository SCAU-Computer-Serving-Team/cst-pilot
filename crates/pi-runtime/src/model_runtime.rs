use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use pi_ai::{
    ApiKeyCredential, AssistantMessageEventStream, AuthCheck, AuthInteraction, AuthType,
    ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, CreateModelsOptions,
    Credential, CredentialStore, InMemoryModelsStore, LoginOptions, Model, Models,
    ModelsRefreshOptions, ModelsRefreshResult, ModelsStore, Provider, ProviderHeaders,
    SimpleStreamOptions, StreamOptions, TranscriptContext, builtin_providers,
    merge_provider_headers,
};
use pi_session::{ExtensionRunner, VirtualModelDefinition};
use thiserror::Error;

use crate::auth_storage::{FileAuthStorage, InMemoryAuthStorage, RuntimeCredentials};
use crate::config_value::{ConfigValueResolver, ConfigValueSource};
use crate::model_config::{
    ModelConfig, compose_provider_with_model_config, has_configured_models_json_auth,
    merge_string_headers_case_insensitive,
};
use crate::models_store::FileModelsStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSyncOperation {
    Login,
    Logout,
    SetRuntimeApiKey,
    RemoveRuntimeApiKey,
}

impl std::fmt::Display for CredentialSyncOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialSyncOperation::Login => write!(f, "login"),
            CredentialSyncOperation::Logout => write!(f, "logout"),
            CredentialSyncOperation::SetRuntimeApiKey => write!(f, "setRuntimeApiKey"),
            CredentialSyncOperation::RemoveRuntimeApiKey => write!(f, "removeRuntimeApiKey"),
        }
    }
}

/// 凭据已成功提交，但本地模型/认证快照同步失败（对照 `CredentialSynchronizationError`，全字段脱敏）。
#[derive(Clone, Error)]
#[error("Credential {operation} committed, but local synchronization failed")]
pub struct CredentialSynchronizationError {
    pub provider_id: String,
    pub operation: CredentialSyncOperation,
    pub credential: Option<Box<Credential>>,
    pub cause: String,
}

impl std::fmt::Debug for CredentialSynchronizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialSynchronizationError")
            .field("provider_id", &self.provider_id)
            .field("operation", &self.operation)
            .field(
                "credential_type",
                &self.credential.as_deref().map(Credential::auth_type),
            )
            .field("cause", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelRuntimeSnapshot {
    pub all: Vec<Model>,
    pub available: Vec<Model>,
    pub configured_providers: BTreeSet<String>,
    pub stored_providers: BTreeSet<String>,
    pub auth: BTreeMap<String, AuthCheck>,
}

#[derive(Clone, Default)]
pub struct CreateModelRuntimeOptions {
    pub credentials: Option<Arc<dyn CredentialStore>>,
    pub auth_path: Option<PathBuf>,
    pub models_path: Option<PathBuf>,
    pub models_store: Option<Arc<dyn ModelsStore>>,
    pub models_store_path: Option<PathBuf>,
    pub resolver: Option<ConfigValueResolver>,
    pub refresh_on_create: Option<bool>,
    pub include_builtins: Option<bool>,
}

/// 统一封装 `Arc<Models>`、`RuntimeCredentials`、`ModelConfig` 与 `ExtensionCatalog` 权威目录的运行时服务（对照 `ModelRuntime`）。
#[derive(Clone)]
pub struct ModelRuntime {
    models: Arc<Models>,
    credentials: RuntimeCredentials,
    file_auth: Option<FileAuthStorage>,
    mem_auth: Option<InMemoryAuthStorage>,
    models_store: Arc<dyn ModelsStore>,
    resolver: ConfigValueResolver,
    default_builtins: Arc<BTreeMap<String, Provider>>,
    config: Arc<RwLock<ModelConfig>>,
    models_path: Option<PathBuf>,
    composition_errors: Arc<RwLock<BTreeMap<String, String>>>,
    baseline_ids: Arc<Mutex<BTreeSet<String>>>,
    snapshot_generation: Arc<AtomicU64>,
    snapshot_credentials_generation: Arc<AtomicU64>,
    snapshot: Arc<RwLock<ModelRuntimeSnapshot>>,
    bound_runner: Arc<RwLock<Option<ExtensionRunner>>>,
    has_bound_session: Arc<AtomicBool>,
    refresh_generation: Arc<AtomicU64>,
    provider_locks: Arc<Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl std::fmt::Debug for ModelRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.snapshot.read().expect("snapshot lock");
        f.debug_struct("ModelRuntime")
            .field("all_count", &snap.all.len())
            .field("available_count", &snap.available.len())
            .field("configured_providers", &snap.configured_providers)
            .finish()
    }
}

impl ModelRuntime {
    pub async fn create(options: CreateModelRuntimeOptions) -> Self {
        let resolver = options.resolver.unwrap_or_default();
        let mut file_auth = None;
        let mut mem_auth = None;

        let base_store: Arc<dyn CredentialStore> = if let Some(custom) = options.credentials {
            custom
        } else if let Some(auth_path) = options.auth_path {
            let fa = FileAuthStorage::new(auth_path, resolver.clone());
            file_auth = Some(fa.clone());
            Arc::new(fa)
        } else {
            let ma = InMemoryAuthStorage::new(BTreeMap::new(), resolver.clone());
            mem_auth = Some(ma.clone());
            Arc::new(ma)
        };

        let credentials = RuntimeCredentials::new(base_store, resolver.clone());
        let config = ModelConfig::load(options.models_path.as_deref());
        let models_store: Arc<dyn ModelsStore> = if let Some(ms) = options.models_store {
            ms
        } else if let Some(store_path) = options.models_store_path.or_else(|| {
            options
                .models_path
                .as_ref()
                .and_then(|p| p.parent().map(|dir| dir.join("models-store.json")))
        }) {
            Arc::new(FileModelsStore::new(store_path))
        } else {
            Arc::new(InMemoryModelsStore::new())
        };

        let models = Arc::new(Models::new(Some(CreateModelsOptions {
            credentials: Some(Arc::new(credentials.clone())),
            models_store: Some(models_store.clone()),
            identity: None,
        })));

        let mut builtins_map = BTreeMap::new();
        if options.include_builtins.unwrap_or(true) {
            for p in builtin_providers() {
                builtins_map.insert(p.id().to_owned(), p);
            }
        }

        let runtime = Self {
            models,
            credentials,
            file_auth,
            mem_auth,
            models_store,
            resolver,
            default_builtins: Arc::new(builtins_map),
            config: Arc::new(RwLock::new(config)),
            models_path: options.models_path,
            composition_errors: Arc::new(RwLock::new(BTreeMap::new())),
            baseline_ids: Arc::new(Mutex::new(BTreeSet::new())),
            snapshot_generation: Arc::new(AtomicU64::new(0)),
            snapshot_credentials_generation: Arc::new(AtomicU64::new(u64::MAX)),
            snapshot: Arc::new(RwLock::new(ModelRuntimeSnapshot::default())),
            bound_runner: Arc::new(RwLock::new(None)),
            has_bound_session: Arc::new(AtomicBool::new(false)),
            refresh_generation: Arc::new(AtomicU64::new(1)),
            provider_locks: Arc::new(Mutex::new(BTreeMap::new())),
        };

        runtime.rebuild_baseline_providers();
        if options.refresh_on_create.unwrap_or(true) {
            let _ = runtime
                .refresh(Some(ModelsRefreshOptions {
                    allow_network: Some(false),
                    providers: None,
                    force: Some(false),
                }))
                .await;
        } else {
            runtime.refresh_snapshot_sync();
        }
        runtime
    }

    /// 为新会话获取或派生隔离的目录视图：
    /// 首个会话直接使用当前实例的 `Arc<Models>`；当同一 `ModelRuntime` 被第二个并发会话复用时，
    /// 派生共享同一套凭据、`ModelsStore` 与内置基线但拥有独立可变 `Arc<Models>` 的视图，
    /// 确保任一会话重载或关闭时不会撤销另一会话注册的扩展 Provider 或虚拟模型。
    pub fn acquire_session_view(&self) -> Self {
        if self
            .has_bound_session
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return self.clone();
        }
        self.fork_isolated_view()
    }

    pub fn fork_isolated_view(&self) -> Self {
        let isolated_models = Arc::new(Models::new(Some(CreateModelsOptions {
            credentials: Some(Arc::new(self.credentials.clone())),
            models_store: Some(self.models_store.clone()),
            identity: None,
        })));
        let view = Self {
            models: isolated_models,
            credentials: self.credentials.clone(),
            file_auth: self.file_auth.clone(),
            mem_auth: self.mem_auth.clone(),
            models_store: self.models_store.clone(),
            resolver: self.resolver.clone(),
            default_builtins: self.default_builtins.clone(),
            config: self.config.clone(),
            models_path: self.models_path.clone(),
            composition_errors: Arc::new(RwLock::new(BTreeMap::new())),
            baseline_ids: Arc::new(Mutex::new(BTreeSet::new())),
            snapshot_generation: Arc::new(AtomicU64::new(0)),
            snapshot_credentials_generation: Arc::new(AtomicU64::new(u64::MAX)),
            snapshot: Arc::new(RwLock::new(
                self.snapshot.read().expect("snapshot lock").clone(),
            )),
            bound_runner: Arc::new(RwLock::new(None)),
            has_bound_session: Arc::new(AtomicBool::new(true)),
            refresh_generation: Arc::new(AtomicU64::new(1)),
            provider_locks: self.provider_locks.clone(),
        };
        view.rebuild_baseline_providers();
        view.refresh_snapshot_sync();
        view
    }

    pub fn models(&self) -> &Arc<Models> {
        &self.models
    }

    pub fn credentials(&self) -> &RuntimeCredentials {
        &self.credentials
    }

    pub fn resolver(&self) -> &ConfigValueResolver {
        &self.resolver
    }

    pub fn get_error(&self) -> Option<String> {
        if let Some(cfg_err) = self.config.read().expect("config lock").get_error() {
            return Some(cfg_err.to_owned());
        }
        let comp_errs = self.composition_errors.read().expect("comp errors lock");
        if !comp_errs.is_empty() {
            let joined = comp_errs.values().cloned().collect::<Vec<_>>().join("\n");
            return Some(joined);
        }
        if let Some(ref fa) = self.file_auth
            && let Some(auth_err) = fa.last_error()
        {
            return Some(auth_err);
        }
        None
    }

    /// 绑定会话的 `ExtensionRunner`（共享同一个 `Arc<Models>`，并在目录变更锁释放后同步快照）。
    pub fn bind_extension_runner(&self, runner: &ExtensionRunner) {
        if let Some(prev) = self.bound_runner.write().expect("runner lock").take() {
            prev.set_on_catalog_changed(None);
        }
        // The observer uses snapshot inputs only; retaining bound_runner here would
        // create a runner -> observer -> runtime -> runner ownership cycle.
        let rt = Self {
            bound_runner: Arc::new(RwLock::new(None)),
            ..self.clone()
        };
        runner.set_on_catalog_changed(Some(Arc::new(move || {
            rt.refresh_generation.fetch_add(1, Ordering::SeqCst);
            rt.refresh_snapshot_sync();
        })));
        runner.bind_models(self.models.clone());
        *self.bound_runner.write().expect("runner lock") = Some(runner.clone());
        self.refresh_snapshot_sync();
    }

    pub fn get_virtual_model(&self, provider: &str, id: &str) -> Option<VirtualModelDefinition> {
        self.bound_runner
            .read()
            .expect("runner lock")
            .as_ref()
            .and_then(|r| r.get_virtual_model(provider, id))
    }

    pub fn register_provider(&self, provider: Provider) {
        if let Some(runner) = self.bound_runner.read().expect("runner lock").as_ref() {
            let _ = runner.runtime().register_provider(provider);
        } else {
            self.models.set_provider(provider);
            self.refresh_generation.fetch_add(1, Ordering::SeqCst);
            self.refresh_snapshot_sync();
        }
    }

    pub fn register_virtual_model(&self, definition: VirtualModelDefinition) {
        if let Some(runner) = self.bound_runner.read().expect("runner lock").as_ref() {
            let _ = runner.runtime().register_virtual_model(definition);
        }
    }

    pub fn rebuild_baseline_providers(&self) {
        let cfg = self.config.read().expect("config lock").clone();
        let mut errors = BTreeMap::new();

        let mut all_ids: BTreeSet<String> = self.default_builtins.keys().cloned().collect();
        for id in cfg.get_provider_ids() {
            all_ids.insert(id);
        }

        let runner = self.bound_runner.read().expect("runner lock").clone();
        // Restore catalog baselines before replacing them, then reapply registrations.
        if let Some(runner) = &runner {
            runner.detach_models();
        }
        let mut baseline_ids = self.baseline_ids.lock().expect("baseline ids lock");
        for removed in baseline_ids.difference(&all_ids) {
            self.models.delete_provider(removed);
        }
        *baseline_ids = all_ids.clone();
        for pid in all_ids {
            let base = self.default_builtins.get(&pid);
            let prov_cfg = cfg.get_provider(&pid);
            match compose_provider_with_model_config(&pid, base, prov_cfg, &self.resolver) {
                Ok(Some(composed)) => {
                    self.models.set_provider(composed);
                }
                Ok(None) => {}
                Err(err) => {
                    errors.insert(pid.clone(), err);
                    if let Some(b) = base {
                        self.models.set_provider(b.clone());
                    }
                }
            }
        }

        *self.composition_errors.write().expect("comp errors lock") = errors;
        drop(baseline_ids);
        if let Some(runner) = &runner {
            runner.bind_models(self.models.clone());
        }
    }

    /// 同步计算当前模型与认证状态快照（零 Shell 命令执行、零网络请求）。
    pub fn refresh_snapshot_sync(&self) {
        let generation = self.snapshot_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let credentials_generation = self.credentials.generation();
        let all = self.models.get_models(None);
        let cfg = self.config.read().expect("config lock").clone();
        let mut configured_providers = BTreeSet::new();
        let mut stored_providers = BTreeSet::new();
        let mut auth_map = BTreeMap::new();
        let mut available_keys = BTreeSet::new();

        let provider_ids = self.models.get_providers();
        for provider in &provider_ids {
            let pid = provider.id();
            let mut is_configured = false;
            let mut source: Option<String> = None;
            let mut auth_type = AuthType::ApiKey;

            let inspection = self.credentials.inspect(pid);
            if let Some(stored) = &inspection {
                is_configured = stored.configured;
                auth_type = stored.credential.auth_type();
                if self.credentials.has_runtime_api_key(pid) {
                    source = Some("runtime".to_owned());
                } else {
                    stored_providers.insert(pid.to_owned());
                    if is_configured {
                        source = Some("stored".to_owned());
                    }
                }
            }

            if !is_configured
                && let Some(p_cfg) = cfg.get_provider(pid)
                && let Some(cfg_src) = has_configured_models_json_auth(p_cfg)
            {
                is_configured = true;
                source = Some(cfg_src.to_owned());
            }

            if !is_configured && pi_ai::get_env_api_key(pid, None).is_some() {
                is_configured = true;
                source = Some("environment".to_owned());
            }

            if !is_configured {
                let auth = provider.auth();
                if (auth.api_key.is_none() && auth.oauth.is_none())
                    || auth
                        .api_key
                        .as_ref()
                        .is_some_and(|auth| auth.configured_without_credentials)
                {
                    is_configured = true;
                    source = Some("none".to_owned());
                }
            }

            if is_configured {
                configured_providers.insert(pid.to_owned());
                auth_map.insert(pid.to_owned(), AuthCheck { source, auth_type });
                let provider_models: Vec<Model> =
                    all.iter().filter(|m| m.provider == pid).cloned().collect();
                for model in provider.filter_chat_models(
                    &provider_models,
                    inspection.as_ref().map(|s| &s.credential),
                ) {
                    available_keys.insert((model.provider, model.id));
                }
            }
        }

        let available: Vec<Model> = all
            .iter()
            .filter(|m| {
                if pi_session::is_virtual_model(m) {
                    !configured_providers.is_empty()
                } else {
                    available_keys.contains(&(m.provider.clone(), m.id.clone()))
                }
            })
            .cloned()
            .collect();

        let mut snapshot = self.snapshot.write().expect("snapshot lock");
        if self.snapshot_generation.load(Ordering::SeqCst) != generation
            || self.credentials.generation() != credentials_generation
        {
            return;
        }
        self.snapshot_credentials_generation
            .store(credentials_generation, Ordering::SeqCst);
        *snapshot = ModelRuntimeSnapshot {
            all,
            available,
            configured_providers,
            stored_providers,
            auth: auth_map,
        };
    }

    pub async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> ModelsRefreshResult {
        let gen_before = self.refresh_generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(ref path) = self.models_path {
            *self.config.write().expect("config lock") = ModelConfig::load(Some(path));
            self.rebuild_baseline_providers();
        }
        self.resolver.clear_cache();
        let res = self.models.refresh(options).await;
        if self.refresh_generation.load(Ordering::SeqCst) == gen_before {
            self.refresh_snapshot_sync();
        }
        res
    }

    fn ensure_credential_snapshot(&self) {
        if self.snapshot_credentials_generation.load(Ordering::SeqCst)
            != self.credentials.generation()
        {
            self.refresh_snapshot_sync();
        }
    }

    pub fn get_all_models(&self) -> Vec<Model> {
        self.ensure_credential_snapshot();
        self.snapshot.read().expect("snapshot lock").all.clone()
    }

    pub fn get_available_models(&self) -> Vec<Model> {
        self.ensure_credential_snapshot();
        self.snapshot
            .read()
            .expect("snapshot lock")
            .available
            .clone()
    }

    pub fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.models.get_model(provider, model_id)
    }

    pub fn has_configured_auth(&self, provider: &str) -> bool {
        self.ensure_credential_snapshot();
        if provider == "pi-virtual" {
            return !self
                .snapshot
                .read()
                .expect("snapshot lock")
                .configured_providers
                .is_empty();
        }
        self.snapshot
            .read()
            .expect("snapshot lock")
            .configured_providers
            .contains(provider)
    }

    pub fn is_using_oauth(&self, provider: &str) -> bool {
        self.ensure_credential_snapshot();
        self.snapshot
            .read()
            .expect("snapshot lock")
            .auth
            .get(provider)
            .is_some_and(|a| a.auth_type == AuthType::OAuth)
    }

    fn lock_for_provider(&self, provider_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.provider_locks.lock().expect("provider locks");
        map.entry(provider_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn sync_after_credential_change(
        &self,
        provider_id: &str,
        operation: CredentialSyncOperation,
        credential: Option<Credential>,
    ) -> Result<(), CredentialSynchronizationError> {
        self.resolver.clear_cache();
        self.refresh_generation.fetch_add(1, Ordering::SeqCst);
        // Composition depends on static config, not credentials. Keep the active
        // provider (including session extensions) and refresh only its local cache.
        let composition_error = self
            .composition_errors
            .read()
            .expect("comp errors lock")
            .get(provider_id)
            .cloned();
        let cause = if composition_error.is_some() {
            composition_error
        } else {
            let mut result = self
                .models
                .refresh(Some(ModelsRefreshOptions {
                    allow_network: Some(false),
                    providers: Some(vec![provider_id.to_owned()]),
                    force: Some(false),
                }))
                .await;
            result
                .errors
                .remove(provider_id)
                .map(|error| error.to_string())
                .or_else(|| {
                    result
                        .aborted
                        .then(|| "Model refresh was aborted".to_owned())
                })
        };
        self.refresh_snapshot_sync();
        match cause {
            Some(cause) => Err(CredentialSynchronizationError {
                provider_id: provider_id.to_owned(),
                operation,
                credential: credential.map(Box::new),
                cause,
            }),
            None => Ok(()),
        }
    }

    pub async fn set_runtime_api_key(
        &self,
        provider_id: &str,
        api_key: &str,
    ) -> Result<(), CredentialSynchronizationError> {
        let lock = self.lock_for_provider(provider_id);
        let _guard = lock.lock().await;
        self.credentials.set_runtime_api_key(provider_id, api_key);
        let cred = Some(Credential::ApiKey(ApiKeyCredential {
            key: Some(api_key.to_owned()),
            env: None,
        }));
        self.sync_after_credential_change(
            provider_id,
            CredentialSyncOperation::SetRuntimeApiKey,
            cred,
        )
        .await
    }

    pub async fn remove_runtime_api_key(
        &self,
        provider_id: &str,
    ) -> Result<(), CredentialSynchronizationError> {
        let lock = self.lock_for_provider(provider_id);
        let _guard = lock.lock().await;
        self.credentials.remove_runtime_api_key(provider_id);
        self.sync_after_credential_change(
            provider_id,
            CredentialSyncOperation::RemoveRuntimeApiKey,
            None,
        )
        .await
    }

    pub async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: AuthInteraction,
        options: Option<LoginOptions>,
    ) -> Result<Credential, String> {
        let lock = self.lock_for_provider(provider_id);
        let _guard = lock.lock().await;
        let cred = self
            .models
            .login(provider_id, auth_type, &interaction, options.as_ref())
            .await
            .map_err(|e| e.to_string())?;
        self.sync_after_credential_change(
            provider_id,
            CredentialSyncOperation::Login,
            Some(cred.clone()),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(cred)
    }

    pub async fn logout(&self, provider_id: &str) -> Result<(), String> {
        let lock = self.lock_for_provider(provider_id);
        let _guard = lock.lock().await;
        self.credentials.delete(provider_id).await?;
        self.sync_after_credential_change(provider_id, CredentialSyncOperation::Logout, None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn prepare_model_and_headers_for_request(
        &self,
        mut model: Model,
        headers: Option<ProviderHeaders>,
        env: Option<BTreeMap<String, String>>,
    ) -> Result<(Model, Option<ProviderHeaders>), String> {
        let provider_headers = self
            .config
            .read()
            .expect("config lock")
            .get_provider(&model.provider)
            .and_then(|p| p.headers.clone());
        let merged_raw_headers = merge_string_headers_case_insensitive(
            provider_headers.as_ref(),
            model.headers.as_ref(),
        );
        let mut resolved_headers = None;
        if let Some(raw_headers) = merged_raw_headers {
            let mut resolved = ProviderHeaders::new();
            for (name, value) in raw_headers {
                let value = self
                    .resolver
                    .resolve_or_throw_async(
                        value,
                        "request header",
                        env.clone(),
                        ConfigValueSource::Global,
                    )
                    .await?;
                resolved.insert(name, Some(value));
            }
            resolved_headers = Some(resolved);
            model.headers = None;
        }
        Ok((
            model,
            merge_provider_headers(resolved_headers, headers.as_ref()),
        ))
    }

    pub async fn resolve_request_api_key(
        &self,
        provider_id: &str,
    ) -> Result<Option<String>, String> {
        self.resolve_request_api_key_with_env(provider_id, None)
            .await
    }

    async fn resolve_request_api_key_with_env(
        &self,
        provider_id: &str,
        env: Option<BTreeMap<String, String>>,
    ) -> Result<Option<String>, String> {
        match self.credentials.read(provider_id).await? {
            Some(Credential::OAuth(_)) => return Ok(None),
            Some(Credential::ApiKey(api)) => {
                if let Some(key) = api.key.filter(|key| !key.is_empty()) {
                    return Ok(Some(key));
                }
            }
            None => {}
        }
        let raw_key = self
            .config
            .read()
            .expect("config lock")
            .get_provider(provider_id)
            .and_then(|p| p.api_key.clone());
        if let Some(key) = raw_key {
            return self
                .resolver
                .resolve_or_throw_async(key, "provider API key", env, ConfigValueSource::Global)
                .await
                .map(Some);
        }
        Ok(pi_ai::get_env_api_key(provider_id, env.as_ref()))
    }

    async fn prepare_request(
        &self,
        model: Model,
        headers: Option<ProviderHeaders>,
        env: Option<BTreeMap<String, String>>,
        api_key: Option<String>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(Model, Option<ProviderHeaders>, Option<String>), RequestPreparationError> {
        let prepare = async {
            let (model, headers) = self
                .prepare_model_and_headers_for_request(model, headers, env.clone())
                .await?;
            let api_key = match api_key {
                Some(key) => Some(key),
                None => {
                    self.resolve_request_api_key_with_env(&model.provider, env)
                        .await?
                }
            };
            Ok((model, headers, api_key))
        };
        if let Some(cancel) = cancel {
            tokio::select! { biased;
                _ = cancel.cancelled() => Err(RequestPreparationError::Aborted),
                result = prepare => result,
            }
        } else {
            prepare.await
        }
    }

    pub async fn stream_simple(
        &self,
        model: Model,
        context: TranscriptContext,
        mut options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        if pi_session::is_unconfigured_model(&model) {
            return error_event_stream(
                &model,
                pi_session::format_no_models_available_message().into(),
            );
        }
        let (prepared_model, merged_headers, api_key) = match self
            .prepare_request(
                model.clone(),
                options.base.headers.take(),
                options.base.env.clone(),
                options.base.api_key.take(),
                options.base.cancel_token.as_ref(),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => return error_event_stream(&model, error),
        };
        options.base.headers = merged_headers;
        options.base.api_key = api_key;
        self.models
            .stream_simple_transcript(&prepared_model, context, Some(&options))
    }

    pub async fn stream(
        &self,
        model: Model,
        context: TranscriptContext,
        mut options: StreamOptions,
    ) -> AssistantMessageEventStream {
        if pi_session::is_unconfigured_model(&model) {
            return error_event_stream(
                &model,
                pi_session::format_no_models_available_message().into(),
            );
        }
        let (prepared_model, merged_headers, api_key) = match self
            .prepare_request(
                model.clone(),
                options.headers.take(),
                options.env.clone(),
                options.api_key.take(),
                options.cancel_token.as_ref(),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => return error_event_stream(&model, error),
        };
        options.headers = merged_headers;
        options.api_key = api_key;
        self.models
            .stream_transcript(&prepared_model, context, Some(&options))
    }

    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> ClassifierResult {
        self.models.classify(model, context, options.as_ref()).await
    }
}

enum RequestPreparationError {
    Aborted,
    Failed(String),
}

impl From<String> for RequestPreparationError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

fn error_event_stream(
    model: &Model,
    error: RequestPreparationError,
) -> AssistantMessageEventStream {
    let (reason, message) = match error {
        RequestPreparationError::Aborted => {
            (pi_ai::StopReason::Aborted, "Request was aborted".to_owned())
        }
        RequestPreparationError::Failed(message) => (pi_ai::StopReason::Error, message),
    };
    let stream = pi_ai::create_assistant_message_event_stream();
    let mut err_msg = pi_ai::AssistantMessage::new_pending(model);
    err_msg.stop_reason = reason;
    err_msg.error_message = Some(message);
    stream.push(pi_ai::AssistantMessageEvent::Error {
        reason,
        error: err_msg,
    });
    stream.end(None);
    stream
}

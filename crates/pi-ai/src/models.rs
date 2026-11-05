use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use futures_util::future::join_all;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::api::{
    anthropic_messages, google_generative_ai, openai_completions, openai_responses,
    typesafe_system_one,
};
use crate::auth::{
    ApiKeyCredential, AuthCheck, AuthInteraction, AuthResolutionOverrides, AuthResult, AuthType,
    BoxFuture, Credential, CredentialStore, InMemoryCredentialStore, LoginOptions, ModelsError,
    ModelsErrorCode, ProviderAuth, refresh_stored_oauth_credential_cancellable,
    resolve_provider_auth_cancellable,
};
use crate::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry};
use crate::transform_messages::normalize_context;
use crate::types::{
    AnyModel, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, ClassifierStopReason,
    ClientIdentity, Context, Model, ModelType, ProviderHeaders, SimpleStreamOptions, StopReason,
    StreamOptions, TranscriptContext, create_assistant_message_event_stream, known_api,
    known_classifier_api, merge_client_identity, now_ms,
};

pub type StreamFn = Arc<
    dyn Fn(&Model, &TranscriptContext, Option<&StreamOptions>) -> AssistantMessageEventStream
        + Send
        + Sync,
>;
pub type StreamSimpleFn = Arc<
    dyn Fn(&Model, &TranscriptContext, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct ProviderStreams {
    pub stream: StreamFn,
    pub stream_simple: StreamSimpleFn,
}

impl ProviderStreams {
    pub fn new(stream: StreamFn, stream_simple: StreamSimpleFn) -> Self {
        Self {
            stream,
            stream_simple,
        }
    }
}

pub fn openai_responses_api() -> ProviderStreams {
    ProviderStreams::new(
        Arc::new(openai_responses::stream),
        Arc::new(openai_responses::stream_simple),
    )
}

pub fn openai_completions_api() -> ProviderStreams {
    ProviderStreams::new(
        Arc::new(openai_completions::stream),
        Arc::new(openai_completions::stream_simple),
    )
}

pub fn anthropic_messages_api() -> ProviderStreams {
    ProviderStreams::new(
        Arc::new(anthropic_messages::stream),
        Arc::new(anthropic_messages::stream_simple),
    )
}

pub fn google_generative_ai_api() -> ProviderStreams {
    ProviderStreams::new(
        Arc::new(google_generative_ai::stream),
        Arc::new(google_generative_ai::stream_simple),
    )
}

pub fn standard_chat_api_config() -> ProviderApiConfig {
    let mut map = BTreeMap::new();
    map.insert(
        known_api::OPENAI_COMPLETIONS.to_owned(),
        openai_completions_api(),
    );
    map.insert(
        known_api::OPENAI_RESPONSES.to_owned(),
        openai_responses_api(),
    );
    map.insert(
        known_api::ANTHROPIC_MESSAGES.to_owned(),
        anthropic_messages_api(),
    );
    map.insert(
        known_api::GOOGLE_GENERATIVE_AI.to_owned(),
        google_generative_ai_api(),
    );
    ProviderApiConfig::ByApi(map)
}

pub type ClassifyFn = Arc<
    dyn Fn(
            &ClassifierModel,
            &ClassifierContext,
            Option<&ClassifierOptions>,
        ) -> BoxFuture<'static, ClassifierResult>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct ProviderClassifier {
    pub classify: ClassifyFn,
}

impl ProviderClassifier {
    pub fn new(classify: ClassifyFn) -> Self {
        Self { classify }
    }
}

pub fn typesafe_system_one_classifier() -> ProviderClassifier {
    ProviderClassifier::new(Arc::new(|m, c, o| {
        let m = m.clone();
        let c = c.clone();
        let o = o.cloned();
        Box::pin(async move { typesafe_system_one::classify(&m, &c, o.as_ref()).await })
    }))
}

#[derive(Clone)]
pub enum ProviderApiConfig {
    Single(ProviderStreams),
    ByApi(BTreeMap<String, ProviderStreams>),
}

impl ProviderApiConfig {
    pub fn get(&self, api: &str) -> Option<&ProviderStreams> {
        match self {
            ProviderApiConfig::Single(s) => Some(s),
            ProviderApiConfig::ByApi(map) => map.get(api),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            ProviderApiConfig::Single(_) => false,
            ProviderApiConfig::ByApi(map) => map.is_empty(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefreshModelsInput {
    pub credential: Option<Credential>,
    pub stored: Option<ModelsStoreEntry>,
    pub allow_network: bool,
    pub force: bool,
    pub base_url: Option<String>,
    /// Cancelled when this refresh completes or is superseded.
    pub cancel_token: CancellationToken,
}

pub type FetchModelsFn = Arc<
    dyn Fn(RefreshModelsInput) -> BoxFuture<'static, Result<Vec<AnyModel>, String>> + Send + Sync,
>;

pub type FilterModelsFn = Arc<dyn Fn(&[Model], Option<&Credential>) -> Vec<Model> + Send + Sync>;

#[derive(Clone)]
pub struct CreateProviderOptions {
    pub id: String,
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub auth: ProviderAuth,
    pub models: Vec<AnyModel>,
    pub fetch_models: Option<FetchModelsFn>,
    pub filter_models: Option<FilterModelsFn>,
    pub api: Option<ProviderApiConfig>,
    pub classifiers: Option<BTreeMap<String, ProviderClassifier>>,
}

struct ProviderInner {
    id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<ProviderHeaders>,
    auth: ProviderAuth,
    baseline_models: Vec<AnyModel>,
    dynamic_models: Arc<RwLock<Vec<AnyModel>>>,
    chat_model_overlay: Vec<Model>,
    fetch_models: Option<FetchModelsFn>,
    filter_models: Option<FilterModelsFn>,
    api: Option<ProviderApiConfig>,
    classifiers: BTreeMap<String, ProviderClassifier>,
}

#[derive(Clone)]
pub struct Provider {
    inner: Arc<ProviderInner>,
}

impl Provider {
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn base_url(&self) -> Option<&str> {
        self.inner.base_url.as_deref()
    }

    pub fn headers(&self) -> Option<&ProviderHeaders> {
        self.inner.headers.as_ref()
    }

    pub fn auth(&self) -> &ProviderAuth {
        &self.inner.auth
    }

    pub fn to_create_options(&self) -> CreateProviderOptions {
        CreateProviderOptions {
            id: self.inner.id.clone(),
            name: Some(self.inner.name.clone()),
            base_url: self.inner.base_url.clone(),
            headers: self.inner.headers.clone(),
            auth: self.inner.auth.clone(),
            models: self.get_all_models(),
            fetch_models: self.inner.fetch_models.clone(),
            filter_models: self.inner.filter_models.clone(),
            api: self.inner.api.clone(),
            classifiers: (!self.inner.classifiers.is_empty())
                .then(|| self.inner.classifiers.clone()),
        }
    }

    pub fn has_dynamic_refresh(&self) -> bool {
        self.inner.fetch_models.is_some()
    }

    pub fn get_all_models(&self) -> Vec<AnyModel> {
        let mut merged = self.inner.baseline_models.clone();
        {
            let dyn_models = self
                .inner
                .dynamic_models
                .read()
                .expect("dynamic models lock poisoned");
            for dm in dyn_models.iter() {
                if let Some(pos) = merged.iter().position(|entry| {
                    entry.model_type() == dm.model_type() && entry.id() == dm.id()
                }) {
                    merged[pos] = dm.clone();
                } else {
                    merged.push(dm.clone());
                }
            }
        }
        for model in &self.inner.chat_model_overlay {
            merged.retain(|entry| !matches!(entry, AnyModel::Chat(chat) if chat.id == model.id));
            merged.push(AnyModel::Chat(model.clone()));
        }
        merged
    }

    /// Overlay chat catalog entries while retaining native classifiers, refresh
    /// state and auth. Overlay entries bypass the native availability filter.
    /// The supplied API implementation must delegate native chat requests.
    pub fn with_chat_model_overlay(&self, models: Vec<Model>, api: ProviderApiConfig) -> Self {
        let base = self.clone();
        let ids: BTreeSet<_> = models.iter().map(|model| model.id.clone()).collect();
        let filter = Arc::new(move |input: &[Model], credential: Option<&Credential>| {
            let native: Vec<_> = input
                .iter()
                .filter(|m| !ids.contains(&m.id))
                .cloned()
                .collect();
            let mut filtered = base.filter_chat_models(&native, credential);
            filtered.extend(input.iter().filter(|m| ids.contains(&m.id)).cloned());
            filtered
        });
        Self {
            inner: Arc::new(ProviderInner {
                id: self.inner.id.clone(),
                name: self.inner.name.clone(),
                base_url: self.inner.base_url.clone(),
                headers: self.inner.headers.clone(),
                auth: self.inner.auth.clone(),
                baseline_models: self.inner.baseline_models.clone(),
                dynamic_models: self.inner.dynamic_models.clone(),
                chat_model_overlay: models,
                fetch_models: self.inner.fetch_models.clone(),
                filter_models: Some(filter),
                api: Some(api),
                classifiers: self.inner.classifiers.clone(),
            }),
        }
    }

    pub fn get_models(&self) -> Vec<Model> {
        self.get_all_models()
            .into_iter()
            .filter_map(|m| match m {
                AnyModel::Chat(chat) => Some(chat),
                AnyModel::Classifier(_) => None,
            })
            .collect()
    }

    pub fn get_classifiers(&self) -> Vec<ClassifierModel> {
        self.get_all_models()
            .into_iter()
            .filter_map(|m| match m {
                AnyModel::Classifier(c) => Some(c),
                AnyModel::Chat(_) => None,
            })
            .collect()
    }

    pub fn filter_chat_models(&self, models: &[Model], cred: Option<&Credential>) -> Vec<Model> {
        if let Some(filter) = &self.inner.filter_models {
            filter(models, cred)
        } else {
            models.to_vec()
        }
    }

    pub(crate) fn set_dynamic_models(&self, models: Vec<AnyModel>) {
        *self
            .inner
            .dynamic_models
            .write()
            .expect("dynamic models lock poisoned") = models;
    }

    pub(crate) fn fetch_models_fn(&self) -> Option<FetchModelsFn> {
        self.inner.fetch_models.clone()
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        let Some(streams) = self.inner.api.as_ref().and_then(|cfg| cfg.get(&model.api)) else {
            return stream_error(
                model,
                format!(
                    "Provider {} has no API implementation for '{}'",
                    self.inner.id, model.api
                ),
            );
        };
        (streams.stream)(model, context, options)
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let Some(streams) = self.inner.api.as_ref().and_then(|cfg| cfg.get(&model.api)) else {
            return stream_error(
                model,
                format!(
                    "Provider {} has no API implementation for '{}'",
                    self.inner.id, model.api
                ),
            );
        };
        (streams.stream_simple)(model, context, options)
    }

    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        let Some(clf) = self.inner.classifiers.get(&model.api) else {
            return ClassifierResult {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                answers: BTreeMap::new(),
                usage: None,
                stop_reason: ClassifierStopReason::Error,
                error_message: Some(format!(
                    "Provider {} has no classifier implementation for '{}'",
                    self.inner.id, model.api
                )),
                timestamp: now_ms(),
            };
        };
        (clf.classify)(model, context, options).await
    }
}

pub fn create_provider(input: CreateProviderOptions) -> Result<Provider, String> {
    let has_api = input.api.as_ref().is_some_and(|cfg| !cfg.is_empty());
    let has_classifiers = input.classifiers.as_ref().is_some_and(|m| !m.is_empty());
    if !has_api && !has_classifiers {
        return Err(format!(
            "Provider {}: at least one of 'api' or 'classifiers' is required.",
            input.id
        ));
    }

    Ok(Provider {
        inner: Arc::new(ProviderInner {
            name: input.name.unwrap_or_else(|| input.id.clone()),
            id: input.id,
            base_url: input.base_url,
            headers: input.headers,
            auth: input.auth,
            baseline_models: input.models,
            dynamic_models: Arc::new(RwLock::new(Vec::new())),
            chat_model_overlay: Vec::new(),
            fetch_models: input.fetch_models,
            filter_models: input.filter_models,
            api: input.api,
            classifiers: input.classifiers.unwrap_or_default(),
        }),
    })
}

fn stream_error(model: &Model, message: impl Into<String>) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let mut err_msg = AssistantMessage::new_pending(model);
    err_msg.stop_reason = StopReason::Error;
    err_msg.error_message = Some(message.into());
    stream.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error: err_msg,
    });
    stream.end(None);
    stream
}

pub fn merge_provider_headers(
    base: Option<ProviderHeaders>,
    override_headers: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
    let Some(ov) = override_headers else {
        return base;
    };
    let mut merged = base.unwrap_or_default();
    for (name, val) in ov {
        let lower = name.to_ascii_lowercase();
        merged.retain(|k, _| k.to_ascii_lowercase() != lower);
        merged.insert(name.clone(), val.clone());
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn static_headers_to_provider_headers(
    headers: Option<&BTreeMap<String, String>>,
) -> Option<ProviderHeaders> {
    let map = headers?;
    if map.is_empty() {
        return None;
    }
    Some(
        map.iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect(),
    )
}

// =============================================================================
// Models Collection (对照 models.ts)
// =============================================================================

#[derive(Clone, Default)]
pub struct CreateModelsOptions {
    pub credentials: Option<Arc<dyn CredentialStore>>,
    pub models_store: Option<Arc<dyn ModelsStore>>,
    pub identity: Option<ClientIdentity>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshOptions {
    pub allow_network: Option<bool>,
    pub providers: Option<Vec<String>>,
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshResult {
    pub aborted: bool,
    pub errors: BTreeMap<String, ModelsError>,
}

#[derive(Clone)]
struct ProviderRefresh {
    generation: u64,
    cancel_token: CancellationToken,
}

struct ModelsInner {
    providers: RwLock<BTreeMap<String, Provider>>,
    credentials: Arc<dyn CredentialStore>,
    models_store: Arc<dyn ModelsStore>,
    identity: Option<ClientIdentity>,
    refresh_generations: RwLock<BTreeMap<String, ProviderRefresh>>,
    publication_locks: std::sync::Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
}

/// 模型与服务商集合容器，无全局副作用，统一负责服务商注册、凭据解析与请求分发。
#[derive(Clone)]
pub struct Models {
    inner: Arc<ModelsInner>,
}

impl Default for Models {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Models {
    pub fn new(options: Option<CreateModelsOptions>) -> Self {
        let opts = options.unwrap_or_default();
        Self {
            inner: Arc::new(ModelsInner {
                providers: RwLock::new(BTreeMap::new()),
                credentials: opts
                    .credentials
                    .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
                models_store: opts
                    .models_store
                    .unwrap_or_else(|| Arc::new(InMemoryModelsStore::new())),
                identity: opts.identity,
                refresh_generations: RwLock::new(BTreeMap::new()),
                publication_locks: std::sync::Mutex::new(BTreeMap::new()),
            }),
        }
    }

    fn next_refresh_generation(&self, provider_id: &str) -> ProviderRefresh {
        let mut guard = self
            .inner
            .refresh_generations
            .write()
            .expect("refresh_generations lock poisoned");
        let generation = guard.get(provider_id).map_or(1, |previous| {
            previous.cancel_token.cancel();
            previous.generation + 1
        });
        let refresh = ProviderRefresh {
            generation,
            cancel_token: CancellationToken::new(),
        };
        guard.insert(provider_id.to_owned(), refresh.clone());
        refresh
    }

    fn is_current_generation(&self, provider_id: &str, generation: u64) -> bool {
        self.inner
            .refresh_generations
            .read()
            .expect("refresh_generations lock poisoned")
            .get(provider_id)
            .map(|refresh| refresh.generation)
            == Some(generation)
    }

    fn publication_lock(&self, provider_id: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self
            .inner
            .publication_locks
            .lock()
            .expect("publication_locks lock poisoned");
        map.entry(provider_id.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub fn set_provider(&self, provider: Provider) {
        let mut guard = self
            .inner
            .providers
            .write()
            .expect("providers lock poisoned");
        self.next_refresh_generation(provider.id());
        guard.insert(provider.id().to_owned(), provider);
    }

    pub fn delete_provider(&self, id: &str) {
        let mut guard = self
            .inner
            .providers
            .write()
            .expect("providers lock poisoned");
        self.next_refresh_generation(id);
        guard.remove(id);
    }

    pub fn clear_providers(&self) {
        let mut guard = self
            .inner
            .providers
            .write()
            .expect("providers lock poisoned");
        for id in guard.keys() {
            self.next_refresh_generation(id);
        }
        guard.clear();
    }

    pub fn get_providers(&self) -> Vec<Provider> {
        self.inner
            .providers
            .read()
            .expect("providers lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub fn get_provider(&self, id: &str) -> Option<Provider> {
        self.inner
            .providers
            .read()
            .expect("providers lock poisoned")
            .get(id)
            .cloned()
    }

    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        match provider {
            Some(pid) => self
                .get_provider(pid)
                .map(|p| p.get_models())
                .unwrap_or_default(),
            None => self
                .get_providers()
                .into_iter()
                .flat_map(|p| p.get_models())
                .collect(),
        }
    }

    pub fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
        match provider {
            Some(pid) => self
                .get_provider(pid)
                .map(|p| p.get_all_models())
                .unwrap_or_default(),
            None => self
                .get_providers()
                .into_iter()
                .flat_map(|p| p.get_all_models())
                .collect(),
        }
    }

    pub fn get_models_of_type(
        &self,
        model_type: ModelType,
        provider: Option<&str>,
    ) -> Vec<AnyModel> {
        self.get_all_models(provider)
            .into_iter()
            .filter(|m| m.model_type() == model_type)
            .collect()
    }

    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.get_models(Some(provider))
            .into_iter()
            .find(|m| m.id == id)
    }

    pub fn get_classifier_model(&self, provider: &str, id: &str) -> Option<ClassifierModel> {
        self.get_provider(provider)?
            .get_classifiers()
            .into_iter()
            .find(|m| m.id == id)
    }

    pub async fn get_auth(
        &self,
        provider_id: &str,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        self.get_auth_cancellable(provider_id, overrides, None)
            .await
    }

    pub async fn get_auth_cancellable(
        &self,
        provider_id: &str,
        overrides: Option<&AuthResolutionOverrides>,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        let resolved = resolve_provider_auth_cancellable(
            provider.id(),
            provider.auth(),
            self.inner.credentials.clone(),
            overrides,
            cancel_token,
        )
        .await?;
        let Some(mut res) = resolved else {
            return Ok(None);
        };
        if res.auth.base_url.is_none()
            && let Some(p_base) = provider.base_url()
        {
            res.auth.base_url = Some(p_base.to_owned());
        }
        res.auth.headers =
            merge_provider_headers(provider.headers().cloned(), res.auth.headers.as_ref());
        Ok(Some(res))
    }

    pub async fn check_auth(&self, provider_id: &str) -> Result<Option<AuthCheck>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        let stored = self
            .inner
            .credentials
            .read(provider_id)
            .await
            .map_err(|e| ModelsError::new(ModelsErrorCode::Auth, e))?;
        if let Some(Credential::OAuth(_)) = stored {
            return Ok(provider.auth().oauth.as_ref().map(|_| AuthCheck {
                source: Some("OAuth".to_owned()),
                auth_type: AuthType::OAuth,
            }));
        }
        let resolved = resolve_provider_auth_cancellable(
            provider.id(),
            provider.auth(),
            self.inner.credentials.clone(),
            None,
            None,
        )
        .await?;
        Ok(resolved.map(|r| AuthCheck {
            source: r.source,
            auth_type: AuthType::ApiKey,
        }))
    }

    pub async fn get_available(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<Model>, ModelsError> {
        let providers = match provider_id {
            Some(pid) => self.get_provider(pid).into_iter().collect(),
            None => self.get_providers(),
        };
        let mut available = Vec::new();
        for provider in providers {
            let stored = self
                .inner
                .credentials
                .read(provider.id())
                .await
                .map_err(|e| ModelsError::new(ModelsErrorCode::Auth, e))?;
            if self.check_auth(provider.id()).await?.is_some() {
                let models = provider.get_models();
                available.extend(provider.filter_chat_models(&models, stored.as_ref()));
            }
        }
        Ok(available)
    }

    /// 执行服务商归属的登录流程（API Key 或 OAuth），并在成功后原子持久化凭据。
    pub async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: &AuthInteraction,
        options: Option<&LoginOptions>,
    ) -> Result<Credential, ModelsError> {
        if interaction
            .cancel_token
            .as_ref()
            .is_some_and(|t| t.is_cancelled())
        {
            return Err(ModelsError::new(ModelsErrorCode::Auth, "Login cancelled"));
        }
        let Some(provider) = self.get_provider(provider_id) else {
            return Err(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            ));
        };

        let credential = match auth_type {
            AuthType::OAuth => {
                let Some(oauth) = &provider.auth().oauth else {
                    return Err(ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support oauth login", provider.name()),
                    ));
                };
                let oauth_cred = oauth
                    .login(interaction, options)
                    .await
                    .map_err(|e| ModelsError::new(ModelsErrorCode::OAuth, e))?;
                Credential::OAuth(oauth_cred)
            }
            AuthType::ApiKey => {
                let Some(api_key) = &provider.auth().api_key else {
                    return Err(ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name()),
                    ));
                };
                let api_cred = api_key
                    .login(interaction)
                    .await
                    .map_err(|e| ModelsError::new(ModelsErrorCode::Auth, e))?;
                Credential::ApiKey(api_cred)
            }
        };

        let cred_to_store = credential.clone();
        let cancel_token = interaction.cancel_token.clone();
        let save = self.inner.credentials.modify(
            provider_id,
            Box::new(move |_| {
                Box::pin(async move {
                    if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                        return Err("Login cancelled".to_owned());
                    }
                    Ok(Some(cred_to_store))
                })
            }),
        );
        let saved = if let Some(token) = &interaction.cancel_token {
            tokio::select! {
                biased;
                _ = token.cancelled() => return Err(ModelsError::new(ModelsErrorCode::Auth, "Login cancelled")),
                result = save => result,
            }
        } else {
            save.await
        };
        saved.map_err(|e| {
            ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Credential store modify failed for {provider_id}: {e}"),
            )
        })?;

        Ok(credential)
    }

    pub async fn logout(&self, provider_id: &str) -> Result<(), ModelsError> {
        self.inner
            .credentials
            .delete(provider_id)
            .await
            .map_err(|e| ModelsError::new(ModelsErrorCode::Auth, e))
    }

    pub async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> ModelsRefreshResult {
        let opts = options.unwrap_or_default();
        let allow_network = opts.allow_network.unwrap_or(true);
        let force = opts.force.unwrap_or(false);
        let selected: Option<BTreeSet<String>> =
            opts.providers.map(|list| list.into_iter().collect());

        let refreshable: Vec<Provider> = self
            .get_providers()
            .into_iter()
            .filter(|p| {
                p.has_dynamic_refresh() && selected.as_ref().is_none_or(|set| set.contains(p.id()))
            })
            .collect();

        // 并发启动所有动态服务商刷新任务，并通过代次（generation）与发布锁防止旧刷新覆盖新结果
        let tasks = refreshable.into_iter().map(|provider| {
            let models_self = self.clone();
            async move {
                let pid = provider.id().to_owned();
                let refresh = {
                    let providers = models_self
                        .inner
                        .providers
                        .read()
                        .expect("providers lock poisoned");
                    if !providers
                        .get(&pid)
                        .is_some_and(|current| Arc::ptr_eq(&current.inner, &provider.inner))
                    {
                        return None;
                    }
                    models_self.next_refresh_generation(&pid)
                };
                let generation = refresh.generation;
                let cancel_token = refresh.cancel_token;
                let _cancel_on_drop = cancel_token.clone().drop_guard();
                let pub_lock = models_self.publication_lock(&pid);
                let mut provider_err: Option<ModelsError> = None;

                // 1. 先恢复本地 ModelsStore 缓存，任何读取错误均记入 errors
                let cache_guard = cancel_token.run_until_cancelled(pub_lock.lock()).await?;
                if !models_self.is_current_generation(&pid, generation) {
                    return None;
                }
                let stored_entry = match cancel_token
                    .run_until_cancelled(models_self.inner.models_store.read(&pid))
                    .await?
                {
                    Ok(Some(stored)) => {
                        let restored: Vec<AnyModel> = stored
                            .models
                            .iter()
                            .filter(|m| m.provider() == pid)
                            .cloned()
                            .collect();
                        if models_self.is_current_generation(&pid, generation) {
                            provider.set_dynamic_models(restored);
                        }
                        Some(stored)
                    }
                    Ok(None) => None,
                    Err(read_err) => {
                        provider_err =
                            Some(ModelsError::new(ModelsErrorCode::ModelSource, read_err));
                        None
                    }
                };

                drop(cache_guard);
                if !models_self.is_current_generation(&pid, generation) {
                    return None;
                }
                if !allow_network {
                    return provider_err.map(|e| (pid, e));
                }

                // 2. 解析当前有效凭据（若无凭据则跳过网络刷新）
                let cred_res = cancel_token
                    .run_until_cancelled(
                        models_self.resolve_refresh_credential(&provider, &cancel_token),
                    )
                    .await?;
                let credential = match cred_res {
                    Ok(Some(c)) => c,
                    Ok(None) => return provider_err.map(|e| (pid, e)),
                    Err(err) => {
                        return (!cancel_token.is_cancelled()).then_some((pid, err));
                    }
                };

                if let Some(fetch_fn) = provider.fetch_models_fn() {
                    let input = RefreshModelsInput {
                        credential: Some(credential),
                        stored: stored_entry,
                        allow_network: true,
                        force,
                        base_url: provider.base_url().map(|s| s.to_owned()),
                        cancel_token: cancel_token.clone(),
                    };
                    let fetched = cancel_token
                        .run_until_cancelled(async { fetch_fn(input).await })
                        .await?;
                    match fetched {
                        Ok(fetched) => {
                            let _guard = cancel_token.run_until_cancelled(pub_lock.lock()).await?;
                            if models_self.is_current_generation(&pid, generation) {
                                provider.set_dynamic_models(fetched.clone());
                                if let Err(write_err) = models_self
                                    .inner
                                    .models_store
                                    .write(
                                        &pid,
                                        ModelsStoreEntry {
                                            models: fetched,
                                            last_modified: None,
                                            checked_at: Some(now_ms()),
                                            etag: None,
                                        },
                                    )
                                    .await
                                {
                                    provider_err = Some(ModelsError::new(
                                        ModelsErrorCode::ModelSource,
                                        write_err,
                                    ));
                                }
                                if !models_self.is_current_generation(&pid, generation)
                                    && let Err(error) =
                                        models_self.inner.models_store.delete(&pid).await
                                {
                                    provider_err =
                                        Some(ModelsError::new(ModelsErrorCode::ModelSource, error));
                                }
                            }
                        }
                        Err(e) => {
                            if models_self.is_current_generation(&pid, generation) {
                                provider_err =
                                    Some(ModelsError::new(ModelsErrorCode::ModelSource, e));
                            }
                        }
                    }
                }

                if cancel_token.is_cancelled() {
                    None
                } else {
                    provider_err.map(|e| (pid, e))
                }
            }
        });

        let results = join_all(tasks).await;
        let errors = results.into_iter().flatten().collect();

        ModelsRefreshResult {
            aborted: false,
            errors,
        }
    }

    async fn resolve_refresh_credential(
        &self,
        provider: &Provider,
        cancel_token: &CancellationToken,
    ) -> Result<Option<Credential>, ModelsError> {
        let stored = self
            .inner
            .credentials
            .read(provider.id())
            .await
            .map_err(|e| ModelsError::new(ModelsErrorCode::Auth, e))?;

        if let Some(Credential::OAuth(oauth_cred)) = stored {
            let Some(oauth_auth) = &provider.auth().oauth else {
                return Ok(None);
            };
            if now_ms() < oauth_cred.expires {
                return Ok(Some(Credential::OAuth(oauth_cred)));
            }
            let refreshed = refresh_stored_oauth_credential_cancellable(
                self.inner.credentials.clone(),
                provider.id(),
                oauth_auth,
                0,
                Some(cancel_token),
            )
            .await?;
            return Ok(refreshed.map(Credential::OAuth));
        }

        let Some(api_key_auth) = &provider.auth().api_key else {
            return Ok(None);
        };
        let api_cred = match stored {
            Some(Credential::ApiKey(c)) => Some(c),
            _ => None,
        };
        let resolved = api_key_auth
            .resolve_for_request(api_cred, None, None)
            .await
            .map_err(|e| ModelsError::new(ModelsErrorCode::Auth, e))?;
        Ok(resolved.map(|r| {
            Credential::ApiKey(ApiKeyCredential {
                key: r.auth.api_key,
                env: r.env,
            })
        }))
    }

    async fn prepare_chat_request(
        &self,
        model: &Model,
        options: Option<&StreamOptions>,
    ) -> Result<(Provider, Model, StreamOptions), String> {
        let cancel_token = options.and_then(|o| o.cancel_token.as_ref());
        if cancel_token.is_some_and(|t| t.is_cancelled()) {
            return Err("Request was aborted".to_owned());
        }

        let provider = self
            .get_provider(&model.provider)
            .ok_or_else(|| format!("Unknown provider: {}", model.provider))?;

        let overrides = AuthResolutionOverrides {
            api_key: options.and_then(|o| o.api_key.clone()),
            env: options.and_then(|o| o.env.clone()),
            min_oauth_validity_ms: None,
        };
        let resolution = self
            .get_auth_cancellable(&model.provider, Some(&overrides), cancel_token)
            .await
            .map_err(|e| e.message)?
            .ok_or_else(|| format!("Provider is not configured: {}", model.provider))?;

        let mut req_model = model.clone();
        if let Some(base_url) = resolution.auth.base_url {
            req_model.base_url = base_url;
        } else if let Some(p_base) = provider.base_url() {
            req_model.base_url = p_base.to_owned();
        }

        let mut req_options = options.cloned().unwrap_or_default();
        if req_options.api_key.is_none() {
            req_options.api_key = resolution.auth.api_key;
        }

        let model_hdrs = static_headers_to_provider_headers(model.headers.as_ref());
        let with_model = merge_provider_headers(provider.headers().cloned(), model_hdrs.as_ref());
        let with_auth = merge_provider_headers(with_model, resolution.auth.headers.as_ref());
        req_options.headers = merge_provider_headers(with_auth, req_options.headers.as_ref());

        if let Some(res_env) = resolution.env {
            let mut merged = res_env;
            if let Some(opt_e) = req_options.env {
                merged.extend(opt_e);
            }
            req_options.env = Some(merged);
        }
        req_options.identity =
            merge_client_identity([self.inner.identity.as_ref(), req_options.identity.as_ref()]);

        Ok((provider, req_model, req_options))
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_transcript(model, normalize_context(context), options)
    }

    pub fn stream_transcript(
        &self,
        model: &Model,
        transcript: TranscriptContext,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        let writer = outer.clone();
        let models_self = self.clone();
        let model = model.clone();
        let options = options.cloned();

        tokio::spawn(async move {
            let cancel_token = options.as_ref().and_then(|o| o.cancel_token.clone());
            match models_self
                .prepare_chat_request(&model, options.as_ref())
                .await
            {
                Ok((provider, req_model, req_options)) => {
                    use futures_util::StreamExt;
                    let mut inner_stream =
                        provider.stream(&req_model, &transcript, Some(&req_options));
                    while let Some(ev) = inner_stream.next().await {
                        writer.push(ev);
                    }
                    writer.end(None);
                }
                Err(err_msg) => {
                    let is_aborted = cancel_token.is_some_and(|t| t.is_cancelled())
                        || err_msg == "Request was aborted";
                    let reason = if is_aborted {
                        StopReason::Aborted
                    } else {
                        StopReason::Error
                    };
                    let mut failed = AssistantMessage::new_pending(&model);
                    failed.stop_reason = reason;
                    failed.error_message = Some(err_msg);
                    writer.push(AssistantMessageEvent::Error {
                        reason,
                        error: failed,
                    });
                    writer.end(None);
                }
            }
        });

        outer
    }

    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessage {
        let stream = self.stream(model, context, options);
        stream.discard_queued_events();
        stream.result().await
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_simple_transcript(model, normalize_context(context), options)
    }

    pub fn stream_simple_transcript(
        &self,
        model: &Model,
        transcript: TranscriptContext,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        let writer = outer.clone();
        let models_self = self.clone();
        let model = model.clone();
        let options = options.cloned();

        tokio::spawn(async move {
            let base_opts = options.as_ref().map(|o| &o.base);
            let cancel_token = base_opts.and_then(|o| o.cancel_token.clone());
            match models_self.prepare_chat_request(&model, base_opts).await {
                Ok((provider, req_model, req_base)) => {
                    use futures_util::StreamExt;
                    let simple_opts = SimpleStreamOptions {
                        base: req_base,
                        tool_choice: options.as_ref().and_then(|o| o.tool_choice),
                        reasoning: options.as_ref().and_then(|o| o.reasoning),
                        thinking_budgets: options.as_ref().and_then(|o| o.thinking_budgets.clone()),
                    };
                    let mut inner_stream =
                        provider.stream_simple(&req_model, &transcript, Some(&simple_opts));
                    while let Some(ev) = inner_stream.next().await {
                        writer.push(ev);
                    }
                    writer.end(None);
                }
                Err(err_msg) => {
                    let is_aborted = cancel_token.is_some_and(|t| t.is_cancelled())
                        || err_msg == "Request was aborted";
                    let reason = if is_aborted {
                        StopReason::Aborted
                    } else {
                        StopReason::Error
                    };
                    let mut failed = AssistantMessage::new_pending(&model);
                    failed.stop_reason = reason;
                    failed.error_message = Some(err_msg);
                    writer.push(AssistantMessageEvent::Error {
                        reason,
                        error: failed,
                    });
                    writer.end(None);
                }
            }
        });

        outer
    }

    pub async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
    ) -> AssistantMessage {
        let stream = self.stream_simple(model, context, options);
        stream.discard_queued_events();
        stream.result().await
    }

    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        let cancel_token = options.and_then(|o| o.cancel_token.as_ref());
        if cancel_token.is_some_and(|t| t.is_cancelled()) {
            return ClassifierResult {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                answers: BTreeMap::new(),
                usage: None,
                stop_reason: ClassifierStopReason::Aborted,
                error_message: Some("Request was aborted".to_owned()),
                timestamp: now_ms(),
            };
        }

        let Some(provider) = self.get_provider(&model.provider) else {
            return ClassifierResult {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                answers: BTreeMap::new(),
                usage: None,
                stop_reason: ClassifierStopReason::Error,
                error_message: Some(format!("Unknown provider: {}", model.provider)),
                timestamp: now_ms(),
            };
        };

        let overrides = AuthResolutionOverrides {
            api_key: options.and_then(|o| o.api_key.clone()),
            env: options.and_then(|o| o.env.clone()),
            min_oauth_validity_ms: None,
        };
        let resolution = match self
            .get_auth_cancellable(&model.provider, Some(&overrides), cancel_token)
            .await
        {
            Ok(Some(res)) => res,
            Ok(None) => {
                return ClassifierResult {
                    api: model.api.clone(),
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    answers: BTreeMap::new(),
                    usage: None,
                    stop_reason: ClassifierStopReason::Error,
                    error_message: Some(format!("Provider is not configured: {}", model.provider)),
                    timestamp: now_ms(),
                };
            }
            Err(err) => {
                let is_aborted = cancel_token.is_some_and(|t| t.is_cancelled())
                    || err.message == "Request was aborted";
                return ClassifierResult {
                    api: model.api.clone(),
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    answers: BTreeMap::new(),
                    usage: None,
                    stop_reason: if is_aborted {
                        ClassifierStopReason::Aborted
                    } else {
                        ClassifierStopReason::Error
                    },
                    error_message: Some(err.message),
                    timestamp: now_ms(),
                };
            }
        };

        let mut req_model = model.clone();
        if let Some(base_url) = resolution.auth.base_url {
            req_model.base_url = base_url;
        } else if let Some(p_base) = provider.base_url() {
            req_model.base_url = p_base.to_owned();
        }

        let mut req_options = options.cloned().unwrap_or_default();
        if req_options.api_key.is_none() {
            req_options.api_key = resolution.auth.api_key;
        }

        let model_hdrs = static_headers_to_provider_headers(model.headers.as_ref());
        let with_model = merge_provider_headers(provider.headers().cloned(), model_hdrs.as_ref());
        let with_auth = merge_provider_headers(with_model, resolution.auth.headers.as_ref());
        req_options.headers = merge_provider_headers(with_auth, req_options.headers.as_ref());

        if let Some(res_env) = resolution.env {
            let mut merged = res_env;
            if let Some(opt_e) = req_options.env {
                merged.extend(opt_e);
            }
            req_options.env = Some(merged);
        }
        req_options.identity =
            merge_client_identity([self.inner.identity.as_ref(), req_options.identity.as_ref()]);

        provider
            .classify(&req_model, context, Some(&req_options))
            .await
    }
}

pub fn create_models(options: Option<CreateModelsOptions>) -> Models {
    Models::new(options)
}

pub fn default_api_map() -> BTreeMap<String, ProviderStreams> {
    BTreeMap::from([
        (
            known_api::OPENAI_RESPONSES.to_owned(),
            openai_responses_api(),
        ),
        ("openai-codex-responses".to_owned(), openai_responses_api()),
        (
            known_api::OPENAI_COMPLETIONS.to_owned(),
            openai_completions_api(),
        ),
        (
            known_api::ANTHROPIC_MESSAGES.to_owned(),
            anthropic_messages_api(),
        ),
        (
            known_api::GOOGLE_GENERATIVE_AI.to_owned(),
            google_generative_ai_api(),
        ),
    ])
}

pub fn default_classifier_map() -> BTreeMap<String, ProviderClassifier> {
    BTreeMap::from([(
        known_classifier_api::TYPESAFE_SYSTEM_ONE.to_owned(),
        typesafe_system_one_classifier(),
    )])
}

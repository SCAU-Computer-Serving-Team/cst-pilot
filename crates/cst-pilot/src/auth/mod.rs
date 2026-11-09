pub(crate) mod account;
pub mod custom_provider;
mod device;
mod models;
pub mod oa;
mod provider;
mod storage;

use std::path::Path;
use std::sync::Arc;

use pi_ai::{
    AuthInteraction, AuthType, Credential, ModelsRefreshOptions, ModelsRefreshResult, Provider,
};
use pi_runtime::{ConfigValueResolver, CreateModelRuntimeOptions, ModelRuntime};

use models::Catalog;
use oa::OaClient;
use storage::{JsonFiles, KitCredentials};

#[derive(Debug, Clone, Copy, Default)]
pub enum Channel {
    #[default]
    Tui,
    Gui,
}

/// One trusted host shares credentials/catalog persistence across its sessions.
/// UI callers receive masked profile/config queries; raw runtime stays host-side.
#[derive(Clone)]
pub struct AuthRuntime {
    pub(crate) runtime: ModelRuntime,
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) files: JsonFiles,
    channel: Channel,
    pub(crate) accounts: Arc<account::AccountService>,
    config_writes: Arc<tokio::sync::Mutex<()>>,
}
impl AuthRuntime {
    pub async fn create(
        root: impl AsRef<Path>,
        channel: Channel,
        client: OaClient,
    ) -> Result<Self, String> {
        let files = JsonFiles::new(root.as_ref())?;
        // Validate without printing source text or replacing a damaged file.
        files.read("auth.json")?;
        files.read("models.json")?;
        let resolver = ConfigValueResolver::new(root.as_ref(), files.path(""), false);
        let credentials = Arc::new(KitCredentials(files.clone(), resolver.clone()));
        let catalog = Arc::new(Catalog::new(files.clone(), client, credentials.clone()));
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(credentials),
            resolver: Some(resolver),
            models_path: Some(files.path("models.json")),
            models_store: Some(catalog.clone()),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await;
        if runtime.get_error().is_some() {
            return Err("models.json 配置无效，原文件已保留，请修正后重试。".into());
        }
        catalog.use_credentials(Arc::new(runtime.credentials().clone()));
        let auth = Self {
            runtime,
            catalog,
            files,
            channel,
            accounts: Arc::default(),
            config_writes: Arc::default(),
        };
        auth.runtime.register_provider(auth.provider()?);
        let restored = auth
            .runtime
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(false),
                providers: Some(vec!["cstoa".into()]),
                force: Some(false),
            }))
            .await;
        if !restored.errors.is_empty() {
            return Err("模型目录缓存无法读取，原文件已保留，请修正后重试。".into());
        }
        Ok(auth)
    }
    pub(crate) fn with_runtime(mut self, runtime: ModelRuntime) -> Self {
        self.runtime = runtime;
        self
    }
    pub fn runtime(&self) -> &ModelRuntime {
        &self.runtime
    }
    pub fn provider(&self) -> Result<Provider, String> {
        provider::create(self.files.clone(), self.catalog.clone(), self.channel)
    }
    pub async fn login(
        &self,
        kind: AuthType,
        interaction: AuthInteraction,
    ) -> Result<Credential, String> {
        self.invalidate_account();
        let credential = self.runtime.login("cstoa", kind, interaction, None).await?;
        self.invalidate_account();
        let _ = self.refresh(false).await;
        Ok(credential)
    }
    pub async fn logout(&self, provider: &str) -> Result<(), String> {
        self.invalidate_account();
        self.runtime.logout(provider).await?;
        self.invalidate_account();
        if provider == "cstoa" {
            pi_ai::ModelsStore::delete(self.catalog.as_ref(), "cstoa").await?;
        }
        Ok(())
    }
    pub async fn refresh(&self, force: bool) -> ModelsRefreshResult {
        if let Err(error) = self.files.read("models.json") {
            let mut result = ModelsRefreshResult::default();
            result.errors.insert(
                "cstoa".into(),
                pi_ai::ModelsError::new(pi_ai::ModelsErrorCode::ModelSource, error),
            );
            return result;
        }
        if force {
            self.invalidate_account();
        }
        self.runtime
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["cstoa".into()]),
                force: Some(force),
            }))
            .await
    }
}

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock as StdRwLock};

mod login;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::api::get_provider_env;
use crate::api::openai_responses::extract_chatgpt_account_id;
use crate::types::{ProviderEnv, ProviderHeaders, now_ms};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelsErrorCode {
    Provider,
    Auth,
    #[serde(rename = "oauth")]
    OAuth,
    ModelSource,
    Stream,
}

#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct ModelsError {
    pub code: ModelsErrorCode,
    pub message: String,
}

impl ModelsError {
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAuth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl std::fmt::Debug for ModelAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_key = self.api_key.as_ref().map(|_| "[REDACTED]");
        let header_keys: Vec<&str> = self
            .headers
            .as_ref()
            .map(|h| h.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        f.debug_struct("ModelAuth")
            .field("api_key", &redacted_key)
            .field("header_keys", &header_keys)
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyCredential {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

impl std::fmt::Debug for ApiKeyCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_key = self.key.as_ref().map(|_| "[REDACTED]");
        let env_keys: Vec<&str> = self
            .env
            .as_ref()
            .map(|e| e.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        f.debug_struct("ApiKeyCredential")
            .field("key", &redacted_key)
            .field("env_keys", &env_keys)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCredential {
    pub refresh: String,
    pub access: String,
    pub expires: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl std::fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let extra_keys: Vec<&str> = self.extra.keys().map(|k| k.as_str()).collect();
        f.debug_struct("OAuthCredential")
            .field("refresh", &"[REDACTED]")
            .field("access", &"[REDACTED]")
            .field("expires", &self.expires)
            .field("extra_keys", &extra_keys)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::ApiKey(c) => f.debug_tuple("ApiKey").field(c).finish(),
            Credential::OAuth(c) => f.debug_tuple("OAuth").field(c).finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthType {
    #[serde(rename = "api_key")]
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth,
}

impl Credential {
    pub fn auth_type(&self) -> AuthType {
        match self {
            Credential::ApiKey(_) => AuthType::ApiKey,
            Credential::OAuth(_) => AuthType::OAuth,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    pub provider_id: String,
    #[serde(rename = "type")]
    pub auth_type: AuthType,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct AuthResult {
    pub auth: ModelAuth,
    pub env: Option<ProviderEnv>,
    pub source: Option<String>,
}

impl std::fmt::Debug for AuthResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env_keys: Vec<&str> = self
            .env
            .as_ref()
            .map(|e| e.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        f.debug_struct("AuthResult")
            .field("auth", &self.auth)
            .field("env_keys", &env_keys)
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheck {
    pub source: Option<String>,
    pub auth_type: AuthType,
}

// =============================================================================
// Login Interaction Types (对照 auth/types.ts)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthPromptSelectOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthPrompt {
    Text {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Secret {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Select {
        message: String,
        options: Vec<AuthPromptSelectOption>,
    },
    ManualCode {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthInfoLink {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthEvent {
    Info {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        links: Option<Vec<AuthInfoLink>>,
    },
    AuthUrl {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    DeviceCode {
        user_code: String,
        verification_uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        open_browser_on_confirm: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        copy_code_on_confirm: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        interval_seconds: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_in_seconds: Option<u64>,
    },
    Progress {
        message: String,
    },
}

pub type AuthPromptFn =
    Arc<dyn Fn(AuthPrompt) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;
pub type AuthNotifyFn = Arc<dyn Fn(AuthEvent) + Send + Sync>;

#[derive(Clone)]
pub struct AuthInteraction {
    pub cancel_token: Option<CancellationToken>,
    pub prompt: AuthPromptFn,
    pub notify: AuthNotifyFn,
}

impl AuthInteraction {
    pub fn new(prompt: AuthPromptFn, notify: AuthNotifyFn) -> Self {
        Self {
            cancel_token: None,
            prompt,
            notify,
        }
    }

    pub async fn prompt_user(&self, prompt: AuthPrompt) -> Result<String, String> {
        if self.cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
            return Err("Login cancelled".to_owned());
        }
        let fut = (self.prompt)(prompt);
        if let Some(token) = &self.cancel_token {
            tokio::select! {
                _ = token.cancelled() => Err("Login cancelled".to_owned()),
                res = fut => res,
            }
        } else {
            fut.await
        }
    }

    pub fn notify_user(&self, event: AuthEvent) {
        (self.notify)(event);
    }
}

#[derive(Clone, Default)]
pub struct LoginOptions {
    pub get_device_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub agent_name: Option<String>,
    /// Optional endpoints for hosted deployments and isolated integration tests.
    pub endpoints: Option<OAuthEndpoints>,
}

#[derive(Clone, Default)]
pub struct OAuthEndpoints {
    pub authorize_url: Option<String>,
    pub token_url: Option<String>,
    pub device_code_url: Option<String>,
    pub copilot_token_url: Option<String>,
    pub copilot_models_url: Option<String>,
}

// =============================================================================
// CredentialStore Trait & InMemoryCredentialStore (按服务商细粒度加锁)
// =============================================================================

pub type CredentialModifyFn<'a> = Box<
    dyn FnOnce(Option<Credential>) -> BoxFuture<'static, Result<Option<Credential>, String>>
        + Send
        + 'a,
>;

pub trait CredentialStore: Send + Sync {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>>;

    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>>;

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>>;

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

#[derive(Clone, Default)]
pub struct InMemoryCredentialStore {
    entries: Arc<StdRwLock<BTreeMap<String, Credential>>>,
    provider_locks: Arc<std::sync::Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_for_provider(&self, provider_id: &str) -> Arc<Mutex<()>> {
        let mut map = self
            .provider_locks
            .lock()
            .expect("provider_locks mutex poisoned");
        map.entry(provider_id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn set(&self, provider_id: impl Into<String>, credential: Credential) {
        let pid = provider_id.into();
        let provider_lock = self.lock_for_provider(&pid);
        let _guard = provider_lock.lock().await;
        let mut entries = self.entries.write().expect("entries lock poisoned");
        entries.insert(pid, credential);
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        let res = self
            .entries
            .read()
            .expect("entries lock poisoned")
            .get(provider_id)
            .cloned();
        Box::pin(async move { Ok(res) })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<CredentialInfo>, String>> {
        let list = self
            .entries
            .read()
            .expect("entries lock poisoned")
            .iter()
            .map(|(k, v)| CredentialInfo {
                provider_id: k.clone(),
                auth_type: v.auth_type(),
            })
            .collect();
        Box::pin(async move { Ok(list) })
    }

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: CredentialModifyFn<'a>,
    ) -> BoxFuture<'a, Result<Option<Credential>, String>> {
        let pid = provider_id.to_owned();
        let provider_lock = self.lock_for_provider(&pid);
        let entries = self.entries.clone();
        Box::pin(async move {
            let _p_guard = provider_lock.lock().await;
            let current = entries
                .read()
                .expect("entries lock poisoned")
                .get(&pid)
                .cloned();
            let next = f(current).await?;
            let mut write_guard = entries.write().expect("entries lock poisoned");
            if let Some(updated) = next {
                write_guard.insert(pid, updated.clone());
                Ok(Some(updated))
            } else {
                Ok(write_guard.get(&pid).cloned())
            }
        })
    }

    fn delete<'a>(&'a self, provider_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        let pid = provider_id.to_owned();
        let provider_lock = self.lock_for_provider(&pid);
        let entries = self.entries.clone();
        Box::pin(async move {
            let _p_guard = provider_lock.lock().await;
            entries.write().expect("entries lock poisoned").remove(&pid);
            Ok(())
        })
    }
}

// =============================================================================
// Environment Variable API Key Lookup (对照 env-api-keys.ts)
// =============================================================================

pub fn get_api_key_env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "moonshotai" | "moonshotai-cn" => &["MOONSHOT_API_KEY"],
        "kimi-coding" => &["KIMI_API_KEY"],
        "zai-coding-cn" => &["ZAI_CODING_CN_API_KEY"],
        "zai" => &["ZAI_API_KEY"],
        "minimax-cn" => &["MINIMAX_CN_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "qwen-token-plan-cn" => &["QWEN_TOKEN_PLAN_CN_API_KEY"],
        "qwen-token-plan" | "qwen-token-plan-individual" => &["QWEN_TOKEN_PLAN_API_KEY"],
        "xiaomi-token-plan-cn" => &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
        "xiaomi" => &["XIAOMI_API_KEY"],
        "ant-ling" => &["ANT_LING_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "anthropic" => &[
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
        ],
        "google" => &["GEMINI_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "opencode" | "opencode-go" => &["OPENCODE_API_KEY"],
        "typesafe" => &["TYPESAFE_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "vercel-ai-gateway" => &["AI_GATEWAY_API_KEY"],
        "huggingface" => &["HF_TOKEN"],
        "github-copilot" => &["COPILOT_GITHUB_TOKEN"],
        _ => &[],
    }
}

pub fn find_env_keys(provider: &str, env: Option<&ProviderEnv>) -> Option<Vec<String>> {
    let vars = get_api_key_env_vars(provider);
    if vars.is_empty() {
        return None;
    }
    let found: Vec<String> = vars
        .iter()
        .filter(|&&var| get_provider_env(var, env).is_some())
        .map(|&var| var.to_owned())
        .collect();
    if found.is_empty() { None } else { Some(found) }
}

pub fn get_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
    let keys = find_env_keys(provider, env)?;
    let var_name = if provider == "anthropic" {
        keys.iter()
            .find(|k| k.as_str() != "ANTHROPIC_AUTH_TOKEN")
            .or_else(|| keys.first())?
    } else {
        keys.first()?
    };
    get_provider_env(var_name, env)
}

// =============================================================================
// ProviderAuth & Built-in OAuth Flows (对照 auth/types.ts, auth/oauth/*.ts)
// =============================================================================

pub type ApiKeyResolverFn = Arc<
    dyn Fn(Option<ApiKeyCredential>, Option<ProviderEnv>) -> Result<Option<AuthResult>, String>
        + Send
        + Sync,
>;

pub type ApiKeyLoginFn = Arc<
    dyn Fn(AuthInteraction) -> BoxFuture<'static, Result<ApiKeyCredential, String>> + Send + Sync,
>;

pub type OAuthRefreshFn = Arc<
    dyn Fn(OAuthCredential) -> BoxFuture<'static, Result<OAuthCredential, String>> + Send + Sync,
>;
pub type OAuthToAuthFn = Arc<dyn Fn(OAuthCredential) -> Result<ModelAuth, String> + Send + Sync>;
pub type OAuthLoginFn = Arc<
    dyn Fn(
            AuthInteraction,
            Option<LoginOptions>,
        ) -> BoxFuture<'static, Result<OAuthCredential, String>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct ApiKeyAuth {
    pub name: String,
    pub resolve: ApiKeyResolverFn,
    pub login_fn: Option<ApiKeyLoginFn>,
}

impl ApiKeyAuth {
    pub fn from_env(name: impl Into<String>, provider_id: impl Into<String>) -> Self {
        let provider_id = provider_id.into();
        let env_vars = get_api_key_env_vars(&provider_id);
        let default_label = env_vars.first().copied().unwrap_or("API key").to_owned();

        Self {
            name: name.into(),
            login_fn: None,
            resolve: Arc::new(move |cred, env| {
                if let Some(c) = &cred
                    && let Some(key) = &c.key
                    && !key.is_empty()
                {
                    return Ok(Some(AuthResult {
                        auth: ModelAuth {
                            api_key: Some(key.clone()),
                            headers: None,
                            base_url: None,
                        },
                        env: c.env.clone(),
                        source: Some("credentials".to_owned()),
                    }));
                }

                let merged_env = match (cred.and_then(|c| c.env), env) {
                    (Some(mut a), Some(b)) => {
                        a.extend(b);
                        Some(a)
                    }
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };

                if provider_id == "anthropic"
                    && let Some(token) =
                        get_provider_env("ANTHROPIC_AUTH_TOKEN", merged_env.as_ref())
                {
                    return Ok(Some(AuthResult {
                        auth: ModelAuth {
                            api_key: None,
                            headers: Some(BTreeMap::from([(
                                "authorization".to_owned(),
                                Some(format!("Bearer {token}")),
                            )])),
                            base_url: None,
                        },
                        env: merged_env,
                        source: Some("ANTHROPIC_AUTH_TOKEN".to_owned()),
                    }));
                }

                if let Some(found_vars) = find_env_keys(&provider_id, merged_env.as_ref())
                    && let Some(first_var) = found_vars.first()
                    && let Some(val) = get_provider_env(first_var, merged_env.as_ref())
                {
                    return Ok(Some(AuthResult {
                        auth: ModelAuth {
                            api_key: Some(val),
                            headers: None,
                            base_url: None,
                        },
                        env: merged_env,
                        source: Some(first_var.clone()),
                    }));
                }

                let _ = &default_label;
                Ok(None)
            }),
        }
    }

    pub fn keyless(name: impl Into<String>, placeholder_key: impl Into<String>) -> Self {
        let key = placeholder_key.into();
        Self {
            name: name.into(),
            login_fn: None,
            resolve: Arc::new(move |cred, env| {
                let api_key = cred
                    .as_ref()
                    .and_then(|c| c.key.clone())
                    .unwrap_or_else(|| key.clone());
                Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(api_key),
                        headers: None,
                        base_url: None,
                    },
                    env,
                    source: Some("keyless".to_owned()),
                }))
            }),
        }
    }

    pub fn with_login(mut self, login_fn: ApiKeyLoginFn) -> Self {
        self.login_fn = Some(login_fn);
        self
    }

    pub async fn login(&self, interaction: &AuthInteraction) -> Result<ApiKeyCredential, String> {
        let login = async {
            if let Some(login_fn) = &self.login_fn {
                return login_fn(interaction.clone()).await;
            }
            self.login_default(interaction).await
        };
        login::cancellable(interaction, login).await
    }

    async fn login_default(
        &self,
        interaction: &AuthInteraction,
    ) -> Result<ApiKeyCredential, String> {
        let key = interaction
            .prompt_user(AuthPrompt::Secret {
                message: format!("Enter {}", self.name),
                placeholder: None,
            })
            .await?;
        let trimmed = key.trim().to_owned();
        if trimmed.is_empty() {
            return Err(format!("{} cannot be empty", self.name));
        }
        Ok(ApiKeyCredential {
            key: Some(trimmed),
            env: None,
        })
    }
}

#[derive(Clone)]
pub struct OAuthAuth {
    pub name: String,
    pub is_subscription: bool,
    pub refresh: OAuthRefreshFn,
    pub to_auth: OAuthToAuthFn,
    pub login_fn: Option<OAuthLoginFn>,
}

const OAUTH_HTTP_TIMEOUT_MS: u64 = 15_000;
const OAUTH_REFRESH_SKEW_MS: u64 = 5 * 60 * 1000;

fn resolve_oauth_token_url(cred: &OAuthCredential, env_var: &str, default_url: &str) -> String {
    if let Some(url) = cred
        .extra
        .get("tokenUrl")
        .or_else(|| cred.extra.get("token_url"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return url.trim().to_owned();
    }
    if let Some(env_url) = get_provider_env(env_var, None) {
        return env_url;
    }
    if let Some(global_url) = get_provider_env("PI_OAUTH_TOKEN_URL", None) {
        return global_url;
    }
    default_url.to_owned()
}

/// 从 OAuth 错误响应中仅提取安全的错误码字段，绝不回显原始响应体中的令牌或秘密。
fn format_safe_oauth_http_error(status: u16, body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let code = error.and_then(|v| {
        v.as_str().or_else(|| {
            v.get("code")
                .or_else(|| v.get("type"))
                .and_then(Value::as_str)
        })
    });
    // Only standard public codes may cross this boundary; arbitrary strings can contain tokens.
    match code {
        Some(
            code @ ("invalid_request"
            | "invalid_client"
            | "invalid_grant"
            | "unauthorized_client"
            | "unsupported_grant_type"
            | "invalid_scope"
            | "access_denied"
            | "authorization_denied"
            | "authorization_pending"
            | "slow_down"
            | "expired_token"
            | "server_error"
            | "temporarily_unavailable"),
        ) => format!("HTTP {status}: {code}"),
        _ => format!("HTTP {status}"),
    }
}

async fn post_oauth_form(
    url: &str,
    params: &[(&str, &str)],
) -> Result<BTreeMap<String, Value>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(OAUTH_HTTP_TIMEOUT_MS))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
    let encoded_body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for &(k, v) in params {
            serializer.append_pair(k, v);
        }
        serializer.finish()
    };
    let resp = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encoded_body)
        .send()
        .await
        .map_err(|e| format!("OAuth token request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "OAuth token refresh failed ({})",
            format_safe_oauth_http_error(status.as_u16(), &text)
        ));
    }
    serde_json::from_str::<BTreeMap<String, Value>>(&text)
        .map_err(|e| format!("OAuth token response returned invalid JSON: {e}"))
}

async fn post_oauth_json(url: &str, body: &Value) -> Result<BTreeMap<String, Value>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(OAUTH_HTTP_TIMEOUT_MS))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
    let resp = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("OAuth token request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "OAuth token refresh failed ({})",
            format_safe_oauth_http_error(status.as_u16(), &text)
        ));
    }
    serde_json::from_str::<BTreeMap<String, Value>>(&text)
        .map_err(|e| format!("OAuth token response returned invalid JSON: {e}"))
}

pub fn generate_pkce() -> (String, String) {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let raw = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes());
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

pub fn parse_oauth_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    if let Ok(parsed_url) = url::Url::parse(trimmed) {
        let mut code = None;
        let mut state = None;
        for (k, v) in parsed_url.query_pairs() {
            if k == "code" && !v.is_empty() {
                code = Some(v.into_owned());
            } else if k == "state" && !v.is_empty() {
                state = Some(v.into_owned());
            }
        }
        if code.is_some() {
            return (code, state);
        }
    }
    if let Some((code, state)) = trimmed.split_once('#') {
        return (
            Some(code.trim().to_owned()).filter(|s| !s.is_empty()),
            Some(state.trim().to_owned()).filter(|s| !s.is_empty()),
        );
    }
    if trimmed.contains("code=") {
        let mut code = None;
        let mut state = None;
        for (k, v) in url::form_urlencoded::parse(trimmed.as_bytes()) {
            if k == "code" && !v.is_empty() {
                code = Some(v.into_owned());
            } else if k == "state" && !v.is_empty() {
                state = Some(v.into_owned());
            }
        }
        if code.is_some() {
            return (code, state);
        }
    }
    (Some(trimmed.to_owned()), None)
}

pub fn derive_github_copilot_base_url(token: &str, enterprise_domain: Option<&str>) -> String {
    for part in token.split(';') {
        if let Some(proxy_host) = part.trim().strip_prefix("proxy-ep=")
            && !proxy_host.is_empty()
        {
            let api_host = proxy_host
                .strip_prefix("proxy.")
                .map(|rest| format!("api.{rest}"))
                .unwrap_or_else(|| proxy_host.to_owned());
            return format!("https://{api_host}");
        }
    }
    if let Some(domain) = enterprise_domain
        && !domain.trim().is_empty()
    {
        let host = domain
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        return format!("https://copilot-api.{host}");
    }
    "https://api.individual.githubcopilot.com".to_owned()
}

impl OAuthAuth {
    pub fn for_provider(provider_id: &str, display_name: &str, is_subscription: bool) -> Self {
        match provider_id {
            "anthropic" => Self::anthropic(),
            "openai" => Self::openai_chatgpt(),
            "openai-codex" => Self::openai_codex(),
            "kimi-coding" => Self::kimi_coding(),
            "xai" => Self::xai(),
            "openrouter" => Self::openrouter(),
            "github-copilot" => Self::github_copilot(),
            _ => Self::bearer(format!("{display_name} OAuth"), is_subscription),
        }
    }

    pub fn with_login(mut self, login_fn: OAuthLoginFn) -> Self {
        self.login_fn = Some(login_fn);
        self
    }

    pub async fn login(
        &self,
        interaction: &AuthInteraction,
        options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, String> {
        let login_fn = self
            .login_fn
            .as_ref()
            .ok_or_else(|| format!("{} does not provide an OAuth login flow", self.name))?;
        login::cancellable(interaction, login_fn(interaction.clone(), options.cloned())).await
    }

    pub fn bearer(name: impl Into<String>, is_subscription: bool) -> Self {
        Self {
            name: name.into(),
            is_subscription,
            login_fn: None,
            refresh: Arc::new(|cred| {
                Box::pin(async move {
                    if now_ms() < cred.expires {
                        return Ok(cred);
                    }
                    let token_url = resolve_oauth_token_url(&cred, "PI_OAUTH_TOKEN_URL", "");
                    if token_url.is_empty() {
                        return Err("OAuth token expired and no tokenUrl is configured".to_owned());
                    }
                    let body = post_oauth_form(
                        &token_url,
                        &[
                            ("grant_type", "refresh_token"),
                            ("refresh_token", &cred.refresh),
                        ],
                    )
                    .await?;
                    let access = body
                        .get("access_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "OAuth response missing access_token".to_owned())?
                        .to_owned();
                    let refresh = body
                        .get("refresh_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&cred.refresh)
                        .to_owned();
                    let expires_in = body
                        .get("expires_in")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(3600);
                    Ok(OAuthCredential {
                        access,
                        refresh,
                        expires: (now_ms() + expires_in * 1000)
                            .saturating_sub(OAUTH_REFRESH_SKEW_MS),
                        extra: cred.extra,
                    })
                })
            }),
            to_auth: Arc::new(|cred| {
                Ok(ModelAuth {
                    api_key: Some(cred.access),
                    headers: None,
                    base_url: None,
                })
            }),
        }
    }

    pub fn anthropic() -> Self {
        const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
        const DEFAULT_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
        Self {
            name: "Anthropic (Claude Pro/Max)".to_owned(),
            is_subscription: true,
            login_fn: Some(Arc::new(|interaction, options| {
                Box::pin(login::anthropic(interaction, options))
            })),
            refresh: Arc::new(|cred| {
                Box::pin(async move {
                    let url = resolve_oauth_token_url(
                        &cred,
                        "PI_ANTHROPIC_OAUTH_TOKEN_URL",
                        DEFAULT_TOKEN_URL,
                    );
                    let payload = serde_json::json!({
                        "grant_type": "refresh_token",
                        "client_id": CLIENT_ID,
                        "refresh_token": cred.refresh,
                    });
                    let data = post_oauth_json(&url, &payload).await?;
                    let access = data
                        .get("access_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "Anthropic OAuth response missing access_token".to_owned())?
                        .to_owned();
                    let refresh = data
                        .get("refresh_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&cred.refresh)
                        .to_owned();
                    let expires_in = data
                        .get("expires_in")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(3600);
                    Ok(OAuthCredential {
                        access,
                        refresh,
                        expires: (now_ms() + expires_in * 1000)
                            .saturating_sub(OAUTH_REFRESH_SKEW_MS),
                        extra: cred.extra,
                    })
                })
            }),
            to_auth: Arc::new(|cred| {
                Ok(ModelAuth {
                    api_key: Some(cred.access),
                    headers: None,
                    base_url: None,
                })
            }),
        }
    }

    pub fn openai_chatgpt() -> Self {
        const DEFAULT_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
        const RESOURCE: &str = "https://api.openai.com/v1";
        Self {
            name: "OpenAI (ChatGPT subscription)".to_owned(),
            is_subscription: true,
            login_fn: Some(Arc::new(|interaction, options| {
                Box::pin(login::chatgpt(interaction, options))
            })),
            refresh: Arc::new(|cred| {
                Box::pin(async move {
                    let url = resolve_oauth_token_url(
                        &cred,
                        "PI_OPENAI_OAUTH_TOKEN_URL",
                        DEFAULT_TOKEN_URL,
                    );
                    let client_id = cred
                        .extra
                        .get("clientId")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| {
                            "Stored OpenAI OAuth credential missing issued client ID".to_owned()
                        })?
                        .to_owned();
                    let data = post_oauth_form(
                        &url,
                        &[
                            ("grant_type", "refresh_token"),
                            ("client_id", &client_id),
                            ("refresh_token", &cred.refresh),
                            ("resource", RESOURCE),
                        ],
                    )
                    .await?;
                    let mut refreshed = login::chatgpt_credential(&data, &client_id)?;
                    let mut extra = cred.extra;
                    extra.extend(refreshed.extra);
                    refreshed.extra = extra;
                    Ok(refreshed)
                })
            }),
            to_auth: Arc::new(|cred| {
                Ok(ModelAuth {
                    api_key: Some(cred.access),
                    headers: None,
                    base_url: None,
                })
            }),
        }
    }

    pub fn openai_codex() -> Self {
        const DEFAULT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
        const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
        Self {
            name: "OpenAI (ChatGPT Plus/Pro)".to_owned(),
            is_subscription: true,
            login_fn: Some(Arc::new(|interaction, options| {
                Box::pin(login::codex(interaction, options))
            })),
            refresh: Arc::new(|mut cred| {
                Box::pin(async move {
                    let url = resolve_oauth_token_url(
                        &cred,
                        "PI_OPENAI_CODEX_OAUTH_TOKEN_URL",
                        DEFAULT_TOKEN_URL,
                    );
                    let data = post_oauth_form(
                        &url,
                        &[
                            ("grant_type", "refresh_token"),
                            ("refresh_token", &cred.refresh),
                            ("client_id", CLIENT_ID),
                        ],
                    )
                    .await?;
                    let access = data
                        .get("access_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            "OpenAI Codex OAuth response missing access_token".to_owned()
                        })?
                        .to_owned();
                    let refresh = data
                        .get("refresh_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&cred.refresh)
                        .to_owned();
                    let expires_in = data
                        .get("expires_in")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(3600);
                    if let Some(account_id) = extract_chatgpt_account_id(&access) {
                        cred.extra
                            .insert("accountId".to_owned(), Value::String(account_id));
                    }
                    Ok(OAuthCredential {
                        access,
                        refresh,
                        expires: now_ms() + expires_in * 1000,
                        extra: cred.extra,
                    })
                })
            }),
            to_auth: Arc::new(|cred| {
                let account_id = cred
                    .extra
                    .get("accountId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned())
                    .or_else(|| extract_chatgpt_account_id(&cred.access));
                let headers = account_id
                    .map(|acc| BTreeMap::from([("ChatGPT-Account-Id".to_owned(), Some(acc))]));
                Ok(ModelAuth {
                    api_key: Some(cred.access),
                    headers,
                    base_url: None,
                })
            }),
        }
    }

    pub fn kimi_coding() -> Self {
        const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
        const DEFAULT_HOST: &str = "https://auth.kimi.com";
        Self {
            name: "Kimi Code (subscription)".to_owned(),
            is_subscription: true,
            login_fn: Some(Arc::new(|interaction, options| {
                Box::pin(login::kimi(interaction, options))
            })),
            refresh: Arc::new(|cred| {
                Box::pin(async move {
                    let default_url = {
                        let host = get_provider_env("KIMI_CODE_OAUTH_HOST", None)
                            .or_else(|| get_provider_env("KIMI_OAUTH_HOST", None))
                            .unwrap_or_else(|| DEFAULT_HOST.to_owned());
                        format!("{}/api/oauth/token", host.trim_end_matches('/'))
                    };
                    let url =
                        resolve_oauth_token_url(&cred, "PI_KIMI_OAUTH_TOKEN_URL", &default_url);
                    let data = post_oauth_form(
                        &url,
                        &[
                            ("client_id", CLIENT_ID),
                            ("grant_type", "refresh_token"),
                            ("refresh_token", &cred.refresh),
                        ],
                    )
                    .await?;
                    let access = data
                        .get("access_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "Kimi Code OAuth response missing access_token".to_owned())?
                        .to_owned();
                    let refresh = data
                        .get("refresh_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&cred.refresh)
                        .to_owned();
                    let expires_in = data
                        .get("expires_in")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(3600);
                    Ok(OAuthCredential {
                        access,
                        refresh,
                        expires: now_ms() + expires_in * 1000,
                        extra: cred.extra,
                    })
                })
            }),
            to_auth: Arc::new(|cred| {
                Ok(ModelAuth {
                    api_key: None,
                    headers: Some(BTreeMap::from([(
                        "Authorization".to_owned(),
                        Some(format!("Bearer {}", cred.access)),
                    )])),
                    base_url: None,
                })
            }),
        }
    }

    pub fn xai() -> Self {
        const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
        const DEFAULT_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
        Self {
            name: "xAI (Grok/X subscription)".to_owned(),
            is_subscription: true,
            login_fn: Some(Arc::new(|interaction, options| {
                Box::pin(login::xai(interaction, options))
            })),
            refresh: Arc::new(|cred| {
                Box::pin(async move {
                    let url =
                        resolve_oauth_token_url(&cred, "XAI_OAUTH_TOKEN_URL", DEFAULT_TOKEN_URL);
                    let data = post_oauth_form(
                        &url,
                        &[
                            ("grant_type", "refresh_token"),
                            ("client_id", CLIENT_ID),
                            ("refresh_token", &cred.refresh),
                        ],
                    )
                    .await?;
                    let access = data
                        .get("access_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "xAI OAuth response missing access_token".to_owned())?
                        .to_owned();
                    let refresh = data
                        .get("refresh_token")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&cred.refresh)
                        .to_owned();
                    let expires_in = data
                        .get("expires_in")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(3600);
                    Ok(OAuthCredential {
                        access,
                        refresh,
                        expires: (now_ms() + expires_in * 1000)
                            .saturating_sub(OAUTH_REFRESH_SKEW_MS),
                        extra: cred.extra,
                    })
                })
            }),
            to_auth: Arc::new(|cred| {
                Ok(ModelAuth {
                    api_key: Some(cred.access),
                    headers: None,
                    base_url: None,
                })
            }),
        }
    }

    pub fn openrouter() -> Self {
        Self {
            name: "OpenRouter OAuth".to_owned(),
            is_subscription: false,
            login_fn: Some(Arc::new(|interaction, options| {
                Box::pin(login::openrouter(interaction, options))
            })),
            refresh: Arc::new(|mut cred| {
                Box::pin(async move {
                    if cred.access.is_empty() {
                        return Err("OpenRouter OAuth credential has empty key".to_owned());
                    }
                    cred.expires = u64::MAX / 2;
                    Ok(cred)
                })
            }),
            to_auth: Arc::new(|cred| {
                Ok(ModelAuth {
                    api_key: Some(cred.access),
                    headers: None,
                    base_url: None,
                })
            }),
        }
    }

    pub fn github_copilot() -> Self {
        Self {
            name: "GitHub Copilot".to_owned(),
            is_subscription: true,
            login_fn: Some(Arc::new(|interaction, options| {
                Box::pin(login::copilot(interaction, options))
            })),
            refresh: Arc::new(|cred| Box::pin(login::refresh_copilot(cred, false))),
            to_auth: Arc::new(|cred| {
                let enterprise = cred
                    .extra
                    .get("enterpriseUrl")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty());
                let base_url = derive_github_copilot_base_url(&cred.access, enterprise);
                Ok(ModelAuth {
                    api_key: Some(cred.access),
                    headers: None,
                    base_url: Some(base_url),
                })
            }),
        }
    }
}

#[derive(Clone, Default)]
pub struct ProviderAuth {
    pub api_key: Option<ApiKeyAuth>,
    pub oauth: Option<OAuthAuth>,
}

impl ProviderAuth {
    pub fn api_key(auth: ApiKeyAuth) -> Self {
        Self {
            api_key: Some(auth),
            oauth: None,
        }
    }

    pub fn oauth(auth: OAuthAuth) -> Self {
        Self {
            api_key: None,
            oauth: Some(auth),
        }
    }

    pub fn both(api_key: ApiKeyAuth, oauth: OAuthAuth) -> Self {
        Self {
            api_key: Some(api_key),
            oauth: Some(oauth),
        }
    }
}

#[derive(Clone, Default)]
pub struct AuthResolutionOverrides {
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub min_oauth_validity_ms: Option<u64>,
}

impl std::fmt::Debug for AuthResolutionOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_key = self.api_key.as_ref().map(|_| "[REDACTED]");
        let env_keys: Option<Vec<&str>> = self
            .env
            .as_ref()
            .map(|e| e.keys().map(|k| k.as_str()).collect());
        f.debug_struct("AuthResolutionOverrides")
            .field("api_key", &redacted_key)
            .field("env_keys", &env_keys)
            .field("min_oauth_validity_ms", &self.min_oauth_validity_ms)
            .finish()
    }
}

const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: u64 = 5 * 60 * 1000;

pub async fn refresh_stored_oauth_credential(
    credentials: Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: &OAuthAuth,
    min_validity_ms: u64,
) -> Result<Option<OAuthCredential>, ModelsError> {
    refresh_stored_oauth_credential_cancellable(
        credentials,
        provider_id,
        oauth,
        min_validity_ms,
        None,
    )
    .await
}

pub async fn refresh_stored_oauth_credential_cancellable(
    credentials: Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: &OAuthAuth,
    min_validity_ms: u64,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<OAuthCredential>, ModelsError> {
    if cancel_token.is_some_and(|t| t.is_cancelled()) {
        return Err(ModelsError::new(
            ModelsErrorCode::Auth,
            "Request was aborted",
        ));
    }

    let oauth_clone = oauth.clone();
    let pid = provider_id.to_owned();

    let cancel_before_start = cancel_token.cloned();
    let modify_fut = tokio::spawn(async move {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started_in_modify = started.clone();
        let cancel_wait = cancel_before_start.clone();
        let store_pid = pid.clone();
        let modification = credentials.modify(
            &store_pid,
            Box::new(move |current| {
                Box::pin(async move {
                    if cancel_before_start
                        .as_ref()
                        .is_some_and(|t| t.is_cancelled())
                    {
                        return Err("Request was aborted".to_owned());
                    }
                    let Some(Credential::OAuth(oauth_cred)) = current else {
                        return Ok(None);
                    };
                    if now_ms().saturating_add(min_validity_ms) < oauth_cred.expires {
                        return Ok(None);
                    }
                    started_in_modify.store(true, std::sync::atomic::Ordering::Release);
                    // Once the server starts rotating a token, cancellation must not discard its replacement.
                    let refreshed = tokio::time::timeout(
                        Duration::from_millis(OAUTH_HTTP_TIMEOUT_MS),
                        (oauth_clone.refresh)(oauth_cred),
                    )
                    .await
                    .map_err(|_| format!("OAuth refresh failed for {pid}: timed out"))?
                    .map_err(|e| format!("OAuth refresh failed for {pid}: {e}"))?;
                    Ok(Some(Credential::OAuth(refreshed)))
                })
            }),
        );
        tokio::pin!(modification);
        if let Some(token) = cancel_wait {
            tokio::select! {
                result = &mut modification => result,
                _ = token.cancelled() => {
                    if started.load(std::sync::atomic::Ordering::Acquire) {
                        modification.await
                    } else {
                        Err("Request was aborted".to_owned())
                    }
                }
            }
        } else {
            modification.await
        }
    });

    let joined = if let Some(token) = cancel_token {
        tokio::select! {
            biased;
            _ = token.cancelled() => return Err(ModelsError::new(ModelsErrorCode::Auth, "Request was aborted")),
            result = modify_fut => result,
        }
    } else {
        modify_fut.await
    };
    let post_res = joined
        .map_err(|_| ModelsError::new(ModelsErrorCode::OAuth, "OAuth refresh task failed"))?;

    let post = post_res.map_err(|e| {
        if e.starts_with("OAuth refresh failed") {
            ModelsError::new(ModelsErrorCode::OAuth, e)
        } else {
            ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Credential store modify failed for {provider_id}: {e}"),
            )
        }
    })?;

    Ok(match post {
        Some(Credential::OAuth(c)) => Some(c),
        _ => None,
    })
}

pub async fn resolve_provider_auth(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: Arc<dyn CredentialStore>,
    overrides: Option<&AuthResolutionOverrides>,
) -> Result<Option<AuthResult>, ModelsError> {
    resolve_provider_auth_cancellable(provider_id, auth, credentials, overrides, None).await
}

pub async fn resolve_provider_auth_cancellable(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: Arc<dyn CredentialStore>,
    overrides: Option<&AuthResolutionOverrides>,
    cancel_token: Option<&CancellationToken>,
) -> Result<Option<AuthResult>, ModelsError> {
    if cancel_token.is_some_and(|t| t.is_cancelled()) {
        return Err(ModelsError::new(
            ModelsErrorCode::Auth,
            "Request was aborted",
        ));
    }

    let override_env = overrides.and_then(|o| o.env.clone());

    if let Some(explicit_key) = overrides.and_then(|o| o.api_key.clone())
        && let Some(api_key_auth) = &auth.api_key
    {
        return (api_key_auth.resolve)(
            Some(ApiKeyCredential {
                key: Some(explicit_key),
                env: override_env,
            }),
            None,
        )
        .map_err(|e| {
            ModelsError::new(
                ModelsErrorCode::Auth,
                format!("API key auth failed for provider {provider_id}: {e}"),
            )
        });
    }

    let read_fut = credentials.read(provider_id);
    let stored_res = if let Some(token) = cancel_token {
        tokio::select! {
            _ = token.cancelled() => {
                return Err(ModelsError::new(ModelsErrorCode::Auth, "Request was aborted"));
            }
            res = read_fut => res,
        }
    } else {
        read_fut.await
    };

    let stored = stored_res.map_err(|e| {
        ModelsError::new(
            ModelsErrorCode::Auth,
            format!("Credential store read failed for {provider_id}: {e}"),
        )
    })?;

    if let Some(cred) = stored {
        match cred {
            Credential::OAuth(oauth_cred) => {
                let Some(oauth_auth) = &auth.oauth else {
                    return Ok(None);
                };
                let min_validity = overrides
                    .and_then(|o| o.min_oauth_validity_ms)
                    .unwrap_or(0)
                    .max(DEFAULT_OAUTH_MINIMUM_VALIDITY_MS);

                let active_cred = if now_ms().saturating_add(min_validity) >= oauth_cred.expires {
                    let Some(refreshed) = refresh_stored_oauth_credential_cancellable(
                        credentials,
                        provider_id,
                        oauth_auth,
                        min_validity,
                        cancel_token,
                    )
                    .await?
                    else {
                        return Ok(None);
                    };
                    if overrides.and_then(|o| o.min_oauth_validity_ms).is_some()
                        && now_ms().saturating_add(min_validity) >= refreshed.expires
                    {
                        return Err(ModelsError::new(
                            ModelsErrorCode::OAuth,
                            format!(
                                "OAuth refresh returned a token that expires too soon for {provider_id}"
                            ),
                        ));
                    }
                    refreshed
                } else {
                    oauth_cred
                };

                let model_auth = (oauth_auth.to_auth)(active_cred).map_err(|e| {
                    ModelsError::new(
                        ModelsErrorCode::OAuth,
                        format!("OAuth auth derivation failed for {provider_id}: {e}"),
                    )
                })?;

                return Ok(Some(AuthResult {
                    auth: model_auth,
                    env: None,
                    source: Some("OAuth".to_owned()),
                }));
            }
            Credential::ApiKey(mut api_cred) => {
                let Some(api_key_auth) = &auth.api_key else {
                    return Ok(None);
                };
                if let Some(ov_env) = override_env {
                    let mut merged = api_cred.env.unwrap_or_default();
                    merged.extend(ov_env);
                    api_cred.env = Some(merged);
                }
                return (api_key_auth.resolve)(Some(api_cred), None).map_err(|e| {
                    ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("API key auth failed for provider {provider_id}: {e}"),
                    )
                });
            }
        }
    }

    if let Some(api_key_auth) = &auth.api_key {
        return (api_key_auth.resolve)(None, override_env).map_err(|e| {
            ModelsError::new(
                ModelsErrorCode::Auth,
                format!("API key auth failed for provider {provider_id}: {e}"),
            )
        });
    }

    Ok(None)
}

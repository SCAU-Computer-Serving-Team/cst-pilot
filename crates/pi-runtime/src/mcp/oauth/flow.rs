use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use pi_agent::BoxFuture;
use pi_ai::{JsonValue, now_ms};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::discovery::{http_client, json_response, secure_url};
use super::{
    McpOAuthCallbackServer, McpOAuthChallenge, McpOAuthCredential, McpOAuthMetadata, McpOAuthStore,
    discover_mcp_oauth, parse_oauth_challenge,
};
use crate::mcp::config::McpOAuthConfig;

pub type McpOAuthRedirectFn =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

pub fn generate_mcp_pkce() -> (String, String) {
    let mut random = Vec::with_capacity(32);
    random.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    random.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

pub struct McpOAuthSession {
    name: String,
    server_url: String,
    config: McpOAuthConfig,
    store: McpOAuthStore,
    challenge: Mutex<McpOAuthChallenge>,
    gate: tokio::sync::Mutex<()>,
}

impl McpOAuthSession {
    pub fn new(
        name: impl Into<String>,
        server_url: &str,
        config: McpOAuthConfig,
        store: McpOAuthStore,
    ) -> Result<Self, String> {
        let mut url = Url::parse(server_url).map_err(|_| "Invalid MCP OAuth server URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Invalid MCP OAuth server URL".to_owned());
        }
        url.set_fragment(None);
        Ok(Self {
            name: name.into(),
            server_url: url.to_string(),
            config,
            store,
            challenge: Mutex::new(McpOAuthChallenge::default()),
            gate: tokio::sync::Mutex::new(()),
        })
    }

    fn owns(&self, credential: &McpOAuthCredential) -> bool {
        credential
            .metadata
            .as_ref()
            .is_some_and(|metadata| metadata.server_url == self.server_url)
    }

    async fn load(&self) -> Result<Option<McpOAuthCredential>, String> {
        let store = self.store.clone();
        let name = self.name.clone();
        let credential = tokio::task::spawn_blocking(move || {
            store.try_load_all().map(|mut all| all.remove(&name))
        })
        .await
        .map_err(|_| "MCP OAuth credential read task failed")??;
        Ok(credential.filter(|credential| self.owns(credential)))
    }

    pub async fn access_token(&self) -> Result<Option<String>, String> {
        let Some(credential) = self.load().await? else {
            return Ok(None);
        };
        secure_url(&self.server_url)?;
        if valid_token(&credential) {
            return Ok(Some(credential.access_token));
        }
        if credential.refresh_token.is_none() {
            return Ok(None);
        }
        self.refresh(None).await
    }

    pub async fn on_unauthorized(
        &self,
        header: &str,
        rejected_token: Option<&str>,
    ) -> Result<Option<String>, String> {
        let challenge = parse_oauth_challenge(header);
        let needs_scope = challenge.error.as_deref() == Some("insufficient_scope");
        *self.challenge.lock().expect("OAuth challenge lock") = challenge;
        if needs_scope {
            return Ok(None);
        }
        self.refresh(rejected_token).await
    }

    async fn refresh(&self, rejected_token: Option<&str>) -> Result<Option<String>, String> {
        let _gate = self.gate.lock().await;
        self.store
            .transact(self.name.clone(), |credential| async move {
                let Some(mut credential) = credential.filter(|credential| self.owns(credential))
                else {
                    return Ok((None, None));
                };
                if valid_token(&credential)
                    && rejected_token.is_none_or(|rejected| rejected != credential.access_token)
                {
                    let token = credential.access_token.clone();
                    return Ok((Some(credential), Some(token)));
                }
                let Some(refresh) = credential.refresh_token.clone() else {
                    return Ok((Some(credential), None));
                };
                let metadata = credential
                    .metadata
                    .as_ref()
                    .ok_or("Missing OAuth discovery metadata")?;
                let params = vec![
                    ("grant_type", "refresh_token".to_owned()),
                    ("refresh_token", refresh),
                ];
                match token_request(metadata, &credential, params).await {
                    Ok(tokens) => {
                        apply_tokens(&mut credential, &tokens)?;
                        let token = credential.access_token.clone();
                        Ok((Some(credential), Some(token)))
                    }
                    Err(error) if error == "OAuth grant rejected" => {
                        credential.access_token.clear();
                        credential.refresh_token = None;
                        credential.expires_at = None;
                        Ok((Some(credential), None))
                    }
                    Err(error) => Err(error),
                }
            })
            .await
    }

    pub async fn login(
        &self,
        redirect: McpOAuthRedirectFn,
        cancel: CancellationToken,
    ) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        let login = async {
            let challenge = self.challenge.lock().expect("OAuth challenge lock").clone();
            let mut metadata = discover_mcp_oauth(&self.server_url, &challenge).await?;
            if let Some(scope) = self
                .config
                .scope
                .clone()
                .or_else(|| self.config.scopes.as_ref().map(|scopes| scopes.join(" ")))
            {
                metadata.scope = Some(scope);
            }
            let mut credential = self
                .load()
                .await?
                .filter(|credential| {
                    credential
                        .metadata
                        .as_ref()
                        .is_some_and(|old| old.issuer == metadata.issuer)
                })
                .unwrap_or_default();
            let remembered_redirect = credential
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.redirect_uri.as_deref());
            let callback = McpOAuthCallbackServer::listen(
                self.config.callback_url.as_deref().or(remembered_redirect),
                self.config.callback_port,
            )
            .await?;
            let redirect_uri = callback.redirect_uri.clone();
            let registered_redirect = credential
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.redirect_uri.as_deref());
            if let Some(client_id) = &self.config.client_id {
                credential.client_id = Some(client_id.clone());
                credential.client_secret = self.config.client_secret.clone();
            } else if credential.client_id.is_none()
                || registered_redirect != Some(redirect_uri.as_str())
            {
                let endpoint = metadata
                    .registration_endpoint
                    .as_ref()
                    .ok_or("OAuth server requires a preconfigured clientId")?;
                let response = http_client().post(secure_url(endpoint)?).json(&serde_json::json!({
                    "client_name": self.config.client_name.as_deref().unwrap_or("pi"), "application_type":"native", "redirect_uris":[redirect_uri], "grant_types":["authorization_code","refresh_token"], "response_types":["code"], "token_endpoint_auth_method":"none"
                })).send().await.map_err(|_| "OAuth client registration request failed")?;
                if !response.status().is_success() {
                    return Err(format!(
                        "OAuth registration failed with HTTP {}",
                        response.status().as_u16()
                    ));
                }
                let registered = json_response(response).await?;
                credential.client_id = Some(
                    registered
                        .get("client_id")
                        .and_then(JsonValue::as_str)
                        .filter(|id| !id.is_empty())
                        .ok_or("OAuth registration response lacks client_id")?
                        .to_owned(),
                );
                credential.client_secret = registered
                    .get("client_secret")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned);
            }
            metadata.redirect_uri = Some(redirect_uri.clone());
            credential.metadata = Some(metadata.clone());
            let store = self.store.clone();
            let name = self.name.clone();
            let saved = credential.clone();
            tokio::task::spawn_blocking(move || store.set(&name, saved))
                .await
                .map_err(|_| "OAuth registration save task failed")??;
            let (verifier, challenge) = generate_mcp_pkce();
            let (state, _) = generate_mcp_pkce();
            let mut url = secure_url(&metadata.authorization_endpoint)?;
            {
                let mut query = url.query_pairs_mut();
                query
                    .append_pair("response_type", "code")
                    .append_pair(
                        "client_id",
                        credential
                            .client_id
                            .as_deref()
                            .ok_or("Missing OAuth client ID")?,
                    )
                    .append_pair("redirect_uri", &redirect_uri)
                    .append_pair("code_challenge", &challenge)
                    .append_pair("code_challenge_method", "S256")
                    .append_pair("state", &state);
                if let Some(scope) = &metadata.scope {
                    query.append_pair("scope", scope);
                    if scope
                        .split_whitespace()
                        .any(|scope| scope == "offline_access")
                    {
                        query.append_pair("prompt", "consent");
                    }
                }
                if let Some(resource) = &metadata.resource {
                    query.append_pair("resource", resource);
                }
            }
            let callback_cancel = cancel.child_token();
            let wait_state = state.clone();
            let callback_task = tokio::spawn(async move {
                callback
                    .wait(&wait_state, Duration::from_secs(300), callback_cancel)
                    .await
            });
            struct AbortOnDrop(tokio::task::AbortHandle);
            impl Drop for AbortOnDrop {
                fn drop(&mut self) {
                    self.0.abort();
                }
            }
            let _abort = AbortOnDrop(callback_task.abort_handle());
            redirect(url.to_string()).await?;
            let callback = callback_task
                .await
                .map_err(|_| "OAuth callback task failed")??;
            if callback.issuer.as_ref().is_some_and(|issuer| {
                issuer.trim_end_matches('/') != metadata.issuer.trim_end_matches('/')
            }) || (metadata.require_issuer && callback.issuer.is_none())
            {
                return Err("OAuth callback issuer mismatch".to_owned());
            }
            let params = vec![
                ("grant_type", "authorization_code".to_owned()),
                ("code", callback.code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ];
            let tokens = token_request(&metadata, &credential, params).await?;
            apply_tokens(&mut credential, &tokens)?;
            let store = self.store.clone();
            let name = self.name.clone();
            tokio::task::spawn_blocking(move || store.set(&name, credential))
                .await
                .map_err(|_| "OAuth token save task failed")??;
            Ok(())
        };
        tokio::select! { biased; () = cancel.cancelled() => Err("OAuth authorization cancelled".to_owned()), result = tokio::time::timeout(Duration::from_secs(300), login) => result.map_err(|_| "OAuth authorization timed out".to_owned())? }
    }

    pub async fn logout(&self) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        let store = self.store.clone();
        let name = self.name.clone();
        tokio::task::spawn_blocking(move || store.remove(&name))
            .await
            .map_err(|_| "OAuth logout task failed")??;
        Ok(())
    }
}

fn valid_token(credential: &McpOAuthCredential) -> bool {
    !credential.access_token.is_empty()
        && credential
            .expires_at
            .is_none_or(|expiry| expiry > now_ms().saturating_add(30_000))
}

fn apply_tokens(credential: &mut McpOAuthCredential, tokens: &JsonValue) -> Result<(), String> {
    if !tokens
        .get("token_type")
        .and_then(JsonValue::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"))
    {
        return Err("OAuth server returned an unsupported token type".to_owned());
    }
    credential.access_token = tokens
        .get("access_token")
        .and_then(JsonValue::as_str)
        .filter(|token| !token.is_empty())
        .ok_or("OAuth response lacks access_token")?
        .to_owned();
    if let Some(refresh) = tokens.get("refresh_token").and_then(JsonValue::as_str) {
        credential.refresh_token = Some(refresh.to_owned());
    }
    credential.expires_at = match tokens.get("expires_in") {
        None | Some(JsonValue::Null) => None,
        Some(value) => {
            let seconds = value
                .as_f64()
                .or_else(|| value.as_str()?.parse().ok())
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .ok_or("Invalid OAuth expires_in")?;
            Some(now_ms().saturating_add((seconds * 1000.0) as u64))
        }
    };
    Ok(())
}

async fn token_request(
    metadata: &McpOAuthMetadata,
    credential: &McpOAuthCredential,
    params: Vec<(&str, String)>,
) -> Result<JsonValue, String> {
    let request = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in params {
            form.append_pair(key, &value);
        }
        if let Some(resource) = &metadata.resource {
            form.append_pair("resource", resource);
        }
        let id = credential
            .client_id
            .as_deref()
            .ok_or("Missing OAuth client ID")?;
        let methods = &metadata.token_auth_methods;
        let basic = credential.client_secret.is_some()
            && (methods.is_empty() || methods.iter().any(|method| method == "client_secret_basic"));
        let mut request = http_client()
            .post(secure_url(&metadata.token_endpoint)?)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded");
        if basic {
            request = request.basic_auth(id, credential.client_secret.as_deref());
        } else {
            form.append_pair("client_id", id);
            if let Some(secret) = &credential.client_secret {
                if methods.iter().any(|method| method == "client_secret_post") {
                    form.append_pair("client_secret", secret);
                } else if !methods.iter().any(|method| method == "none") {
                    return Err(
                        "OAuth server has no supported token authentication method".to_owned()
                    );
                }
            } else if !methods.is_empty() && !methods.iter().any(|method| method == "none") {
                return Err("OAuth server requires a client secret".to_owned());
            }
        }
        request.body(form.finish())
    };
    let response = request
        .send()
        .await
        .map_err(|_| "OAuth token request failed".to_owned())?;
    let status = response.status();
    let tokens = json_response(response).await?;
    if !status.is_success() {
        if matches!(
            tokens.get("error").and_then(JsonValue::as_str),
            Some("invalid_grant" | "invalid_token")
        ) {
            return Err("OAuth grant rejected".to_owned());
        }
        return Err(format!(
            "OAuth token request rejected with HTTP {}",
            status.as_u16()
        ));
    }
    Ok(tokens)
}

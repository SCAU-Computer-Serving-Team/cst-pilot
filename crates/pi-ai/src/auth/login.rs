use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(super) async fn cancellable<T>(
    interaction: &AuthInteraction,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    if let Some(token) = &interaction.cancel_token {
        tokio::select! {
            biased;
            _ = token.cancelled() => Err("Login cancelled".to_owned()),
            result = future => result,
        }
    } else {
        future.await
    }
}

fn endpoint(
    options: &Option<LoginOptions>,
    select: impl Fn(&OAuthEndpoints) -> &Option<String>,
    default: &str,
) -> String {
    options
        .as_ref()
        .and_then(|o| o.endpoints.as_ref())
        .and_then(|e| select(e).clone())
        .unwrap_or_else(|| default.to_owned())
}

fn required(data: &BTreeMap<String, Value>, key: &str) -> Result<String, String> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("OAuth response missing {key}"))
}

fn credential(data: &BTreeMap<String, Value>, skew: u64) -> Result<OAuthCredential, String> {
    let lifetime = data
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| "OAuth response missing positive expires_in".to_owned())?;
    Ok(OAuthCredential {
        access: required(data, "access_token")?,
        refresh: required(data, "refresh_token")?,
        expires: now_ms()
            .saturating_add(lifetime.saturating_mul(1000))
            .saturating_sub(skew),
        extra: BTreeMap::new(),
    })
}

fn authorization_url(base: &str, fields: &[(&str, &str)]) -> Result<String, String> {
    let mut url = url::Url::parse(base).map_err(|_| "Invalid OAuth authorize URL".to_owned())?;
    url.query_pairs_mut().extend_pairs(fields.iter().copied());
    Ok(url.into())
}

enum AuthorizationResponse {
    Code(url::Url),
    Denied,
}

fn parse_callback(
    input: &str,
    redirect: &str,
    state: &str,
) -> Result<AuthorizationResponse, String> {
    let url = url::Url::parse(input.trim())
        .map_err(|_| "Paste the full OAuth callback URL".to_owned())?;
    let expected = url::Url::parse(redirect).map_err(|_| "Invalid redirect URI".to_owned())?;
    if url.origin() != expected.origin() || url.path() != expected.path() {
        return Err("Unexpected OAuth callback URL".to_owned());
    }
    if !url.query_pairs().any(|(k, v)| k == "state" && v == state) {
        return Err("OAuth state missing or mismatched".to_owned());
    }
    if url.query_pairs().any(|(k, _)| k == "error") {
        return Ok(AuthorizationResponse::Denied);
    }
    if !url.query_pairs().any(|(k, v)| k == "code" && !v.is_empty()) {
        return Err("OAuth callback missing code".to_owned());
    }
    Ok(AuthorizationResponse::Code(url))
}

fn validate_callback(input: &str, redirect: &str, state: &str) -> Result<url::Url, String> {
    match parse_callback(input, redirect, state)? {
        AuthorizationResponse::Code(url) => Ok(url),
        AuthorizationResponse::Denied => Err("OAuth authorization denied".to_owned()),
    }
}

// Bind before announcing the URL. Dropping this future closes the listener and connections.
async fn authorization_input(
    interaction: &AuthInteraction,
    auth_url: String,
    redirect: &str,
    state: &str,
) -> Result<String, String> {
    let expected = url::Url::parse(redirect).map_err(|_| "Invalid redirect URI".to_owned())?;
    let port = expected
        .port()
        .ok_or_else(|| "OAuth callback requires a port".to_owned())?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|_| format!("OAuth callback port {port} is unavailable"))?;
    interaction.notify_user(AuthEvent::AuthUrl {
        url: auth_url,
        instructions: Some("Complete browser sign-in, or paste the final callback URL.".to_owned()),
    });
    let prompt = interaction.prompt_user(AuthPrompt::ManualCode {
        message: "Complete browser sign-in, or paste the final callback URL:".to_owned(),
        placeholder: Some(redirect.to_owned()),
    });
    let callback = async {
        loop {
            let (mut socket, _) = listener
                .accept()
                .await
                .map_err(|_| "OAuth callback accept failed".to_owned())?;
            let mut bytes = Vec::new();
            let read = async {
                loop {
                    let mut chunk = [0; 1024];
                    let n = socket
                        .read(&mut chunk)
                        .await
                        .map_err(|_| "OAuth callback read failed".to_owned())?;
                    if n == 0 {
                        return Err("OAuth callback closed".to_owned());
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        return Ok(());
                    }
                    if bytes.len() > 8192 {
                        return Err("OAuth callback headers too large".to_owned());
                    }
                }
            };
            if !matches!(
                tokio::time::timeout(Duration::from_secs(2), read).await,
                Ok(Ok(()))
            ) {
                continue;
            }
            let line = String::from_utf8_lossy(&bytes);
            let target = line
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("GET "))
                .and_then(|l| l.split_whitespace().next());
            let candidate = target.and_then(|t| expected.join(t).ok()).map(String::from);
            let outcome = candidate
                .as_ref()
                .and_then(|s| parse_callback(s, redirect, state).ok());
            let valid = matches!(outcome, Some(AuthorizationResponse::Code(_)));
            let response: &[u8] = if valid {
                b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 19\r\n\r\nLogin complete.\r\nOK"
            } else {
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 16\r\n\r\nInvalid callback"
            };
            let _ = tokio::time::timeout(Duration::from_secs(2), socket.write_all(response)).await;
            match outcome {
                Some(AuthorizationResponse::Code(url)) => return Ok(url.into()),
                Some(AuthorizationResponse::Denied) => {
                    return Err("OAuth authorization denied".to_owned());
                }
                None => (),
            }
        }
    };
    tokio::select! { result = callback => result, result = prompt => result }
}

pub(super) async fn anthropic(
    interaction: AuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, String> {
    const CLIENT: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    const REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";
    let (verifier, challenge) = generate_pkce();
    let auth_url = authorization_url(
        &endpoint(
            &options,
            |e| &e.authorize_url,
            "https://claude.ai/oauth/authorize",
        ),
        &[
            ("code", "true"),
            ("client_id", CLIENT),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT),
            ("scope", "org:create_api_key user:profile user:inference"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &verifier),
        ],
    )?;
    interaction.notify_user(AuthEvent::AuthUrl {
        url: auth_url,
        instructions: None,
    });
    let input = interaction
        .prompt_user(AuthPrompt::ManualCode {
            message: "Paste the authorization code or callback URL:".to_owned(),
            placeholder: Some(REDIRECT.to_owned()),
        })
        .await?;
    let (code, state) = parse_oauth_authorization_input(&input);
    if state.as_ref().is_some_and(|s| s != &verifier) {
        return Err("OAuth state mismatch".to_owned());
    }
    let code = code.ok_or_else(|| "Missing authorization code".to_owned())?;
    let default_url = get_provider_env("PI_ANTHROPIC_OAUTH_TOKEN_URL", None)
        .or_else(|| get_provider_env("PI_OAUTH_TOKEN_URL", None))
        .unwrap_or_else(|| "https://platform.claude.com/v1/oauth/token".to_owned());
    let data = post_oauth_json(&endpoint(&options, |e| &e.token_url, &default_url), &serde_json::json!({
        "grant_type":"authorization_code", "client_id":CLIENT, "code":code, "state":state.unwrap_or_else(|| verifier.clone()),
        "redirect_uri":REDIRECT, "code_verifier":verifier,
    })).await?;
    credential(&data, OAUTH_REFRESH_SKEW_MS)
}

pub(super) async fn openrouter(
    interaction: AuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, String> {
    let (verifier, challenge) = generate_pkce();
    const REDIRECT: &str = "http://127.0.0.1:3000/oauth/callback";
    let state = uuid::Uuid::new_v4().to_string();
    let callback_url = authorization_url(REDIRECT, &[("state", &state)])?;
    let auth_url = authorization_url(
        &endpoint(&options, |e| &e.authorize_url, "https://openrouter.ai/auth"),
        &[
            ("callback_url", &callback_url),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ],
    )?;
    let input = authorization_input(&interaction, auth_url, REDIRECT, &state).await?;
    let callback = validate_callback(&input, REDIRECT, &state)?;
    let code = callback
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| "Missing authorization code".to_owned())?;
    let data = post_oauth_json(
        &endpoint(
            &options,
            |e| &e.token_url,
            "https://openrouter.ai/api/v1/auth/keys",
        ),
        &serde_json::json!({
            "code":code, "code_verifier":verifier, "code_challenge_method":"S256",
        }),
    )
    .await?;
    Ok(OAuthCredential {
        access: required(&data, "key")?,
        refresh: String::new(),
        expires: u64::MAX / 2,
        extra: BTreeMap::new(),
    })
}

pub(super) async fn chatgpt(
    interaction: AuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, String> {
    const REDIRECT: &str = "http://127.0.0.1:1455/auth/callback";
    const RESOURCE: &str = "https://api.openai.com/v1";
    let device_id = options
        .as_ref()
        .and_then(|o| o.get_device_id.as_ref())
        .map(|f| f())
        .ok_or_else(|| "ChatGPT login requires a device UUID".to_owned())?;
    let device_id = uuid::Uuid::parse_str(&device_id)
        .map_err(|_| "ChatGPT login requires a device UUID".to_owned())?;
    let host_id = format!("urn:uuid:{device_id}");
    let (verifier, challenge) = generate_pkce();
    let state = uuid::Uuid::new_v4().to_string();
    let nonce = uuid::Uuid::new_v4().to_string();
    let agent = options
        .as_ref()
        .and_then(|o| o.agent_name.as_deref())
        .unwrap_or("Pi");
    let auth_url = authorization_url(
        &endpoint(
            &options,
            |e| &e.authorize_url,
            "https://auth.openai.com/api/accounts/authorize",
        ),
        &[
            ("client_id", "dynamic_agent_client"),
            ("agent_name_hint", agent),
            ("ext_agent_host_id", &host_id),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT),
            ("resource", RESOURCE),
            (
                "scope",
                "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            ),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("nonce", &nonce),
        ],
    )?;
    let input = authorization_input(&interaction, auth_url, REDIRECT, &state).await?;
    let callback = validate_callback(&input, REDIRECT, &state)?;
    let field = |key: &str| {
        callback
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("OAuth callback missing {key}"))
    };
    let code = field("code")?;
    let client_id = field("client_id")?;
    let data = post_oauth_form(
        &endpoint(
            &options,
            |e| &e.token_url,
            "https://auth.openai.com/api/accounts/oauth/token",
        ),
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", REDIRECT),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    required(&data, "id_token")?;
    chatgpt_credential(&data, &client_id)
}

pub(super) fn chatgpt_credential(
    data: &BTreeMap<String, Value>,
    client_id: &str,
) -> Result<OAuthCredential, String> {
    let scopes: Vec<String> = required(data, "scope")?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if !scopes.iter().any(|s| s == "chatgpt.tokens.use.direct") {
        return Err("ChatGPT OAuth grant missing chatgpt.tokens.use.direct".to_owned());
    }
    let mut result = credential(data, 3 * 60 * 1000)?;
    result
        .extra
        .insert("clientId".to_owned(), Value::String(client_id.to_owned()));
    result
        .extra
        .insert("scopes".to_owned(), serde_json::json!(scopes));
    Ok(result)
}

pub(super) async fn codex(
    interaction: AuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, String> {
    const CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
    const REDIRECT: &str = "http://localhost:1455/auth/callback";
    let (verifier, challenge) = generate_pkce();
    let state = uuid::Uuid::new_v4().to_string();
    let auth_url = authorization_url(
        &endpoint(
            &options,
            |e| &e.authorize_url,
            "https://auth.openai.com/oauth/authorize",
        ),
        &[
            ("client_id", CLIENT),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT),
            ("scope", "openid profile email offline_access"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "pi"),
        ],
    )?;
    let input = authorization_input(&interaction, auth_url, REDIRECT, &state).await?;
    let (code, got_state) = parse_oauth_authorization_input(&input);
    if got_state.as_ref().is_some_and(|s| s != &state) {
        return Err("OAuth state mismatch".to_owned());
    }
    let code = code.ok_or_else(|| "Missing authorization code".to_owned())?;
    let data = post_oauth_form(
        &endpoint(
            &options,
            |e| &e.token_url,
            "https://auth.openai.com/oauth/token",
        ),
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", REDIRECT),
        ],
    )
    .await?;
    let mut result = credential(&data, 0)?;
    let account = extract_chatgpt_account_id(&result.access)
        .ok_or_else(|| "Codex OAuth token missing account ID".to_owned())?;
    result
        .extra
        .insert("accountId".to_owned(), Value::String(account));
    Ok(result)
}

async fn device_form(
    url: &str,
    fields: &[(&str, &str)],
) -> Result<(u16, BTreeMap<String, Value>), String> {
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.extend_pairs(fields.iter().copied());
        serializer.finish()
    };
    let response = reqwest::Client::builder()
        .timeout(Duration::from_millis(OAUTH_HTTP_TIMEOUT_MS))
        .build()
        .map_err(|_| "OAuth HTTP client failed".to_owned())?
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .body(body)
        .send()
        .await
        .map_err(|_| "OAuth device request failed".to_owned())?;
    let status = response.status().as_u16();
    let body = response
        .json()
        .await
        .map_err(|_| format!("Invalid OAuth device JSON (HTTP {status})"))?;
    Ok((status, body))
}

fn verification_url(raw: &str) -> Result<String, String> {
    let url = url::Url::parse(raw).map_err(|_| "Invalid OAuth verification URI".to_owned())?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("Untrusted OAuth verification URI".to_owned());
    }
    Ok(url.into())
}

async fn device_login(
    interaction: &AuthInteraction,
    device_url: &str,
    token_url: &str,
    client_id: &str,
    extra: &[(&str, &str)],
) -> Result<BTreeMap<String, Value>, String> {
    let mut fields = vec![("client_id", client_id)];
    fields.extend_from_slice(extra);
    let (status, device) = device_form(device_url, &fields).await?;
    if !(200..300).contains(&status) {
        return Err(format_safe_oauth_http_error(
            status,
            &serde_json::to_string(&device).unwrap_or_default(),
        ));
    }
    let code = required(&device, "device_code")?;
    let user_code = required(&device, "user_code")?;
    let uri = verification_url(&required(&device, "verification_uri")?)?;
    let uri = match device
        .get("verification_uri_complete")
        .and_then(Value::as_str)
    {
        Some(u) => verification_url(u)?,
        None => uri,
    };
    let expiry = device
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(900);
    let mut interval = device
        .get("interval")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(5);
    interaction.notify_user(AuthEvent::DeviceCode {
        user_code,
        verification_uri: uri,
        open_browser_on_confirm: None,
        copy_code_on_confirm: None,
        interval_seconds: Some(interval),
        expires_in_seconds: Some(expiry),
    });
    let poll = async {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let (status, data) = device_form(
                token_url,
                &[
                    ("client_id", client_id),
                    ("device_code", &code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ],
            )
            .await?;
            match data.get("error").and_then(Value::as_str) {
                Some("authorization_pending") if status < 500 => continue,
                Some("slow_down") if status < 500 => {
                    interval = interval
                        .saturating_add(5)
                        .max(data.get("interval").and_then(Value::as_u64).unwrap_or(0));
                }
                Some(_) => {
                    return Err(format_safe_oauth_http_error(
                        status,
                        &serde_json::to_string(&data).unwrap_or_default(),
                    ));
                }
                None if (200..300).contains(&status) => {
                    required(&data, "access_token")?;
                    return Ok(data);
                }
                None => return Err(format!("OAuth device polling failed (HTTP {status})")),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(expiry), poll)
        .await
        .map_err(|_| "OAuth device code expired".to_owned())?
}

pub(super) async fn kimi(
    interaction: AuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, String> {
    let host = get_provider_env("KIMI_CODE_OAUTH_HOST", None)
        .or_else(|| get_provider_env("KIMI_OAUTH_HOST", None))
        .unwrap_or_else(|| "https://auth.kimi.com".to_owned());
    let device_url = endpoint(
        &options,
        |e| &e.device_code_url,
        &format!(
            "{}/api/oauth/device_authorization",
            host.trim_end_matches('/')
        ),
    );
    let token_url = endpoint(
        &options,
        |e| &e.token_url,
        &format!("{}/api/oauth/token", host.trim_end_matches('/')),
    );
    let data = device_login(
        &interaction,
        &device_url,
        &token_url,
        "17e5f671-d194-4dfb-9706-5516cb48c098",
        &[],
    )
    .await?;
    credential(&data, 0)
}

pub(super) async fn xai(
    interaction: AuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, String> {
    let device_url = endpoint(
        &options,
        |e| &e.device_code_url,
        "https://auth.x.ai/oauth2/device/code",
    );
    let token_url = endpoint(&options, |e| &e.token_url, "https://auth.x.ai/oauth2/token");
    let mut data = device_login(
        &interaction,
        &device_url,
        &token_url,
        "b1a00492-073a-47ea-816f-4c329264a828",
        &[
            (
                "scope",
                "openid profile email offline_access grok-cli:access api:access",
            ),
            ("referrer", "pi"),
        ],
    )
    .await?;
    data.entry("expires_in".to_owned())
        .or_insert(Value::from(3600));
    credential(&data, OAUTH_REFRESH_SKEW_MS)
}

pub(super) async fn copilot(
    interaction: AuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, String> {
    let input = interaction
        .prompt_user(AuthPrompt::Text {
            message: "GitHub Enterprise URL/domain (blank for github.com)".to_owned(),
            placeholder: Some("company.ghe.com".to_owned()),
        })
        .await?;
    let mut extra = BTreeMap::new();
    let domain = if input.trim().is_empty() {
        "github.com".to_owned()
    } else {
        let raw = if input.contains("://") {
            input.trim().to_owned()
        } else {
            format!("https://{}", input.trim())
        };
        let url =
            url::Url::parse(&raw).map_err(|_| "Invalid GitHub Enterprise domain".to_owned())?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err("Invalid GitHub Enterprise domain".to_owned());
        }
        let domain = url
            .host_str()
            .ok_or_else(|| "Invalid GitHub Enterprise domain".to_owned())?
            .to_owned();
        extra.insert("enterpriseUrl".to_owned(), Value::String(domain.clone()));
        domain
    };
    let device_url = endpoint(
        &options,
        |e| &e.device_code_url,
        &format!("https://{domain}/login/device/code"),
    );
    let token_url = endpoint(
        &options,
        |e| &e.token_url,
        &format!("https://{domain}/login/oauth/access_token"),
    );
    let data = device_login(
        &interaction,
        &device_url,
        &token_url,
        "Iv1.b507a08c87ecfe98",
        &[("scope", "read:user")],
    )
    .await?;
    if let Some(url) = options
        .as_ref()
        .and_then(|o| o.endpoints.as_ref())
        .and_then(|e| e.copilot_token_url.as_ref())
    {
        extra.insert("tokenUrl".to_owned(), Value::String(url.clone()));
    }
    if let Some(url) = options
        .as_ref()
        .and_then(|o| o.endpoints.as_ref())
        .and_then(|e| e.copilot_models_url.as_ref())
    {
        extra.insert("modelsUrl".to_owned(), Value::String(url.clone()));
    }
    let initial = OAuthCredential {
        access: String::new(),
        refresh: required(&data, "access_token")?,
        expires: 0,
        extra,
    };
    refresh_copilot(initial, true).await
}

fn copilot_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
) -> reqwest::RequestBuilder {
    client
        .request(method, url)
        .header("Accept", "application/json")
        .bearer_auth(token)
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .header("Editor-Version", "vscode/1.107.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.35.0")
        .header("Copilot-Integration-Id", "vscode-chat")
}

async fn copilot_json(request: reqwest::RequestBuilder) -> Result<BTreeMap<String, Value>, String> {
    let response = request
        .send()
        .await
        .map_err(|_| "Copilot HTTP request failed".to_owned())?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|_| "Copilot response read failed".to_owned())?;
    if !status.is_success() {
        return Err(format_safe_oauth_http_error(status.as_u16(), &text));
    }
    serde_json::from_str(&text).map_err(|_| "Invalid Copilot response JSON".to_owned())
}

pub(super) async fn refresh_copilot(
    cred: OAuthCredential,
    enable_policies: bool,
) -> Result<OAuthCredential, String> {
    let enterprise = cred.extra.get("enterpriseUrl").and_then(Value::as_str);
    let domain = enterprise.unwrap_or("github.com");
    let token_url = resolve_oauth_token_url(
        &cred,
        "PI_COPILOT_OAUTH_TOKEN_URL",
        &format!("https://api.{domain}/copilot_internal/v2/token"),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(OAUTH_HTTP_TIMEOUT_MS))
        .build()
        .map_err(|_| "Copilot HTTP client failed".to_owned())?;
    let data = copilot_json(copilot_request(
        &client,
        reqwest::Method::GET,
        &token_url,
        &cred.refresh,
    ))
    .await?;
    let access = required(&data, "token")?;
    let expires = data
        .get("expires_at")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Copilot token response missing expires_at".to_owned())?;
    let base_url = derive_github_copilot_base_url(&access, enterprise);
    let models_url = cred
        .extra
        .get("modelsUrl")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{base_url}/models"));
    let catalog = copilot_json(
        copilot_request(&client, reqwest::Method::GET, &models_url, &access)
            .header("X-GitHub-Api-Version", "2026-06-01"),
    )
    .await?;
    let models = catalog
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "Copilot catalog missing data".to_owned())?;
    let usable: Vec<&Value> = models
        .iter()
        .filter(|m| {
            m.get("id").and_then(Value::as_str).is_some()
                && m.pointer("/capabilities/supports/tool_calls")
                    .and_then(Value::as_bool)
                    != Some(false)
        })
        .collect();
    let mut available: Vec<String> = usable
        .iter()
        .filter(|m| {
            m.get("model_picker_enabled").and_then(Value::as_bool) == Some(true)
                && m.pointer("/policy/state").and_then(Value::as_str) != Some("disabled")
        })
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let fallback = available.is_empty() && base_url == "https://api.individual.githubcopilot.com";
    if fallback {
        available.extend(
            usable
                .iter()
                .filter(|m| m.pointer("/policy/state").and_then(Value::as_str) == Some("enabled"))
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned)),
        );
    }
    if enable_policies {
        let known = crate::providers::github_copilot::github_copilot_provider().get_models();
        for model in usable {
            let id = model.get("id").and_then(Value::as_str).unwrap();
            if model.pointer("/policy/state").and_then(Value::as_str) != Some("unconfigured")
                || !(fallback
                    || model.get("model_picker_enabled").and_then(Value::as_bool) == Some(true))
                || !known.iter().any(|m| m.id == id)
            {
                continue;
            }
            let mut policy_url = url::Url::parse(&models_url)
                .map_err(|_| "Invalid Copilot catalog URL".to_owned())?;
            policy_url
                .path_segments_mut()
                .map_err(|_| "Invalid Copilot catalog URL".to_owned())?
                .pop_if_empty()
                .push(id)
                .push("policy");
            let response =
                copilot_request(&client, reqwest::Method::POST, policy_url.as_str(), &access)
                    .header("openai-intent", "chat-policy")
                    .header("x-interaction-type", "chat-policy")
                    .json(&serde_json::json!({"state":"enabled"}))
                    .send()
                    .await;
            match response {
                Ok(r) if r.status().is_success() => available.push(id.to_owned()),
                Ok(r) if r.status().as_u16() == 429 => break,
                _ => (),
            }
        }
    }
    available.sort();
    available.dedup();
    let mut extra = cred.extra;
    extra.insert("availableModelIds".to_owned(), serde_json::json!(available));
    Ok(OAuthCredential {
        access,
        refresh: cred.refresh,
        expires: expires
            .saturating_mul(1000)
            .saturating_sub(OAUTH_REFRESH_SKEW_MS),
        extra,
    })
}

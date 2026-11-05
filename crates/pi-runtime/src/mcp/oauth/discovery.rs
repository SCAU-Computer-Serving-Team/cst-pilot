use pi_ai::JsonValue;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthMetadata {
    pub server_url: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub resource: Option<String>,
    pub scope: Option<String>,
    pub token_auth_methods: Vec<String>,
    pub require_issuer: bool,
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpOAuthChallenge {
    pub resource_metadata_url: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

pub fn parse_oauth_challenge(header: &str) -> McpOAuthChallenge {
    let mut challenge = McpOAuthChallenge::default();
    let Some((scheme, fields)) = header.trim().split_once(char::is_whitespace) else {
        return challenge;
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return challenge;
    }
    // Commas inside quoted values belong to the value. Escaped quotes are supported.
    let mut fields = fields.trim();
    while !fields.is_empty() {
        fields = fields.trim_start_matches([',', ' ', '\t']);
        let Some(equal) = fields.find('=') else {
            break;
        };
        let name = fields[..equal].trim().to_ascii_lowercase();
        fields = &fields[equal + 1..];
        let mut value = String::new();
        if let Some(rest) = fields.strip_prefix('"') {
            let mut escaped = false;
            let mut consumed = rest.len();
            for (index, character) in rest.char_indices() {
                if escaped {
                    value.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    consumed = index + 1;
                    break;
                } else {
                    value.push(character);
                }
            }
            fields = &rest[consumed..];
        } else {
            let end = fields.find(',').unwrap_or(fields.len());
            value.push_str(fields[..end].trim());
            fields = &fields[end..];
        }
        if value.is_empty() {
            continue;
        }
        match name.as_str() {
            "resource_metadata" => challenge.resource_metadata_url = Some(value),
            "scope" => challenge.scope = Some(value),
            "error" => challenge.error = Some(value),
            _ => {}
        }
    }
    challenge
}

pub(super) fn secure_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "Invalid OAuth endpoint URL".to_owned())?;
    let loopback = url
        .host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"));
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("OAuth endpoints require HTTPS or an HTTP loopback address".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("OAuth endpoint contains invalid user information or fragment".to_owned());
    }
    Ok(url)
}

pub(super) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("OAuth HTTP client configuration")
}

pub(super) async fn json_response(mut response: reqwest::Response) -> Result<JsonValue, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Failed to read OAuth response".to_owned())?
    {
        if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err("OAuth response exceeds size limit".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid OAuth JSON response".to_owned())
}

async fn metadata(client: &reqwest::Client, url: &str) -> Result<Option<JsonValue>, String> {
    let url = secure_url(url)?;
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| "OAuth discovery request failed".to_owned())?;
    if response.status().is_client_error() || response.status().as_u16() == 502 {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!(
            "OAuth discovery failed with HTTP {}",
            response.status().as_u16()
        ));
    }
    json_response(response).await.map(Some)
}

pub async fn discover_mcp_oauth(
    server_url: &str,
    challenge: &McpOAuthChallenge,
) -> Result<McpOAuthMetadata, String> {
    let server = secure_url(server_url)?;
    let client = http_client();
    let origin = server.origin().ascii_serialization();
    let path = server.path().trim_end_matches('/');
    let resource_candidates = match &challenge.resource_metadata_url {
        Some(url) => vec![url.clone()],
        None => vec![
            format!("{origin}/.well-known/oauth-protected-resource{path}"),
            format!("{origin}/.well-known/oauth-protected-resource"),
        ],
    };
    let mut resource = None;
    let mut advertised_scopes = None;
    let mut issuer = format!("{origin}/");
    for candidate in resource_candidates {
        if let Some(document) = metadata(&client, &candidate).await? {
            let advertised = document
                .get("resource")
                .and_then(JsonValue::as_str)
                .ok_or("OAuth resource metadata lacks resource")?;
            let protected = secure_url(advertised)?;
            let protected_path = format!("{}/", protected.path().trim_end_matches('/'));
            let requested_path = format!("{}/", server.path().trim_end_matches('/'));
            if server.origin() != protected.origin() || !requested_path.starts_with(&protected_path)
            {
                return Err("OAuth protected resource does not match the MCP endpoint".to_owned());
            }
            resource = Some(protected.to_string());
            if let Some(authority) = document
                .get("authorization_servers")
                .and_then(JsonValue::as_array)
                .and_then(|servers| servers.first())
                .and_then(JsonValue::as_str)
            {
                issuer = secure_url(authority)?.to_string();
            }
            advertised_scopes = document
                .get("scopes_supported")
                .and_then(JsonValue::as_array)
                .map(|scopes| {
                    scopes
                        .iter()
                        .filter_map(JsonValue::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            break;
        }
    }
    let issuer_url = secure_url(&issuer)?;
    let issuer_origin = issuer_url.origin().ascii_serialization();
    let issuer_path = issuer_url.path().trim_end_matches('/');
    let candidates = [
        format!("{issuer_origin}/.well-known/oauth-authorization-server{issuer_path}"),
        format!("{issuer_origin}/.well-known/openid-configuration{issuer_path}"),
        format!("{issuer_origin}{issuer_path}/.well-known/openid-configuration"),
    ];
    let mut discovered = None;
    for candidate in candidates {
        if let Some(document) = metadata(&client, &candidate).await? {
            discovered = Some(document);
            break;
        }
    }
    let document = discovered.unwrap_or_else(|| serde_json::json!({
        "issuer": issuer, "authorization_endpoint": format!("{issuer_origin}/authorize"), "token_endpoint": format!("{issuer_origin}/token"), "registration_endpoint": format!("{issuer_origin}/register"), "response_types_supported": ["code"]
    }));
    let published_issuer = document
        .get("issuer")
        .and_then(JsonValue::as_str)
        .ok_or("OAuth metadata lacks issuer")?;
    if secure_url(published_issuer)?.as_str().trim_end_matches('/')
        != issuer_url.as_str().trim_end_matches('/')
    {
        return Err("OAuth authorization server issuer mismatch".to_owned());
    }
    if !document
        .get("response_types_supported")
        .and_then(JsonValue::as_array)
        .is_some_and(|types| types.iter().any(|value| value == "code"))
    {
        return Err("OAuth server does not support authorization codes".to_owned());
    }
    if let Some(methods) = document
        .get("code_challenge_methods_supported")
        .and_then(JsonValue::as_array)
        && !methods.iter().any(|method| method == "S256")
    {
        return Err("OAuth server does not support PKCE S256".to_owned());
    }
    let endpoint = |name: &str| -> Result<String, String> {
        secure_url(
            document
                .get(name)
                .and_then(JsonValue::as_str)
                .ok_or_else(|| format!("OAuth metadata lacks {name}"))?,
        )
        .map(|url| url.to_string())
    };
    Ok(McpOAuthMetadata {
        server_url: server.to_string(),
        issuer: issuer_url.to_string(),
        authorization_endpoint: endpoint("authorization_endpoint")?,
        token_endpoint: endpoint("token_endpoint")?,
        registration_endpoint: document
            .get("registration_endpoint")
            .and_then(JsonValue::as_str)
            .map(secure_url)
            .transpose()?
            .map(|url| url.to_string()),
        resource,
        scope: challenge.scope.clone().or(advertised_scopes),
        token_auth_methods: document
            .get("token_endpoint_auth_methods_supported")
            .and_then(JsonValue::as_array)
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        require_issuer: document
            .get("authorization_response_iss_parameter_supported")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        redirect_uri: None,
    })
}

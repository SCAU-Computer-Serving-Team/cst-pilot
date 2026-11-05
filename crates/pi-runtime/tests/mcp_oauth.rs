#[path = "support/mcp_http.rs"]
mod fixture;

use base64::Engine;
use fixture::{Response, Server, initialize};
use pi_runtime::{
    McpClient, McpClientOptions, McpOAuthCallbackServer, McpOAuthConfig, McpOAuthSession,
    McpOAuthStore, McpRequestOptions, StreamableHttpTransport, StreamableHttpTransportOptions,
    generate_mcp_pkce, parse_oauth_challenge,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn oauth_challenge_pkce_callback_refresh_and_http_authorization_work_together() {
    let origin = Arc::new(Mutex::new(String::new()));
    let pkce = Arc::new(Mutex::new(String::new()));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let server=Server::spawn({let origin=origin.clone();let pkce=pkce.clone();let refreshes=refreshes.clone();move|request|{let origin=origin.lock().unwrap().clone();let pkce=pkce.clone();let refreshes=refreshes.clone();async move{
        if request.path.starts_with("/.well-known/oauth-protected-resource") {return Response::json(json!({"resource":format!("{origin}/mcp"),"authorization_servers":[origin],"scopes_supported":["read","offline_access"]}));}
        if request.path.starts_with("/.well-known/oauth-authorization-server") {return Response::json(json!({"issuer":origin,"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token"),"registration_endpoint":format!("{origin}/register"),"response_types_supported":["code"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"],"authorization_response_iss_parameter_supported":true}));}
        if request.path=="/register" {let value=request.json();assert_eq!(value["application_type"],"native");assert_eq!(value["token_endpoint_auth_method"],"none");return Response::json(json!({"client_id":"registered"}));}
        if request.path.starts_with("/authorize?") {
            let parsed=url::Url::parse(&format!("{origin}{}",request.path)).unwrap();let fields:BTreeMap<_,_>=parsed.query_pairs().into_owned().collect();
            assert_eq!(fields["code_challenge_method"],"S256");assert_eq!(fields["resource"],format!("{origin}/mcp"));assert!(fields["scope"].contains("offline_access"));
            *pkce.lock().unwrap()=fields["code_challenge"].clone();
            let mut callback=url::Url::parse(&fields["redirect_uri"]).unwrap();callback.query_pairs_mut().append_pair("code","authorization-code").append_pair("state",&fields["state"]).append_pair("iss",&origin);
            return Response::status(302).header("Location",callback.to_string());
        }
        if request.path=="/token" {
            let fields:BTreeMap<_,_>=url::form_urlencoded::parse(&request.body).into_owned().collect();assert_eq!(fields["client_id"],"registered");assert!(!fields.contains_key("client_secret"));assert_eq!(fields["resource"],format!("{origin}/mcp"));
            if fields["grant_type"]=="authorization_code" {assert_eq!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(fields["code_verifier"].as_bytes())),*pkce.lock().unwrap());return Response::json(json!({"access_token":"initial","token_type":"Bearer","refresh_token":"refresh-1","expires_in":3600}));}
            refreshes.fetch_add(1,Ordering::SeqCst);assert_eq!(fields["refresh_token"],"refresh-1");return Response::json(json!({"access_token":"refreshed","token_type":"Bearer","refresh_token":"refresh-2","expires_in":3600}));
        }
        if request.path=="/mcp" {
            if !request.headers.get("authorization").is_some_and(|value|value=="Bearer initial"||value=="Bearer refreshed") {return Response::status(401).header("WWW-Authenticate",format!("Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\""));}
            let message=request.json();if message.get("id").is_none(){return Response::status(202)}
            return Response::json(json!({"jsonrpc":"2.0","id":message["id"],"result":if message["method"]=="initialize" {initialize("1")} else if message["method"]=="tools/list" {json!({"tools":[{"name":"authorized","inputSchema":{"type":"object"}}]})} else {json!({"authorized":true})}}));
        }
        Response::status(404)
    }}}).await;
    *origin.lock().unwrap() = server.url.clone();
    let directory = std::env::temp_dir().join(format!("pi-mcp-oauth-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let store = McpOAuthStore::new(&directory);
    let oauth = Arc::new(
        McpOAuthSession::new(
            "local",
            &format!("{}/mcp", server.url),
            McpOAuthConfig::default(),
            store.clone(),
        )
        .unwrap(),
    );
    let options = StreamableHttpTransportOptions {
        url: format!("{}/mcp", server.url),
        oauth: Some(oauth.clone()),
        open_get_stream: false,
        ..Default::default()
    };
    let unauthenticated = McpClient::new(McpClientOptions::default());
    assert!(
        unauthenticated
            .connect(Arc::new(StreamableHttpTransport::with_options(
                options.clone()
            )))
            .await
            .unwrap_err()
            .contains("authorization required")
    );
    unauthenticated.close().await.unwrap();
    oauth
        .login(
            Arc::new(|url| {
                Box::pin(async move {
                    let response = reqwest::Client::new()
                        .get(url)
                        .send()
                        .await
                        .map_err(|error| error.to_string())?;
                    assert_eq!(response.status(), 200);
                    Ok(())
                })
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        oauth.access_token().await.unwrap().as_deref(),
        Some("initial")
    );
    let client = McpClient::new(McpClientOptions::default());
    client
        .connect(Arc::new(StreamableHttpTransport::with_options(options)))
        .await
        .unwrap();
    assert_eq!(
        client
            .request("authorized", None, McpRequestOptions::default())
            .await
            .unwrap(),
        json!({"authorized":true})
    );
    client.close().await.unwrap();
    let mut expired = store.get("local").unwrap();
    expired.expires_at = Some(0);
    store.set("local", expired).unwrap();
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let oauth = oauth.clone();
        calls.spawn(async move { oauth.access_token().await.unwrap() });
    }
    while let Some(result) = calls.join_next().await {
        assert_eq!(result.unwrap().as_deref(), Some("refreshed"));
    }
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.get("local").unwrap().refresh_token.as_deref(),
        Some("refresh-2")
    );
    let changed = McpOAuthSession::new(
        "local",
        &format!("{}/another-resource", server.url),
        McpOAuthConfig::default(),
        store.clone(),
    )
    .unwrap();
    assert!(changed.access_token().await.unwrap().is_none());
    oauth.logout().await.unwrap();
    assert!(store.get("local").is_none());
    // Exercise the SDK manager command, configured client, injected browser redirect and reconnection.
    std::fs::write(directory.join("mcp.json"), serde_json::to_vec(&json!({"mcpServers":{"local":{"url":format!("{}/mcp",server.url),"exposure":"direct","oauth":{"clientId":"registered","clientSecret":"configured-secret"}}}})).unwrap()).unwrap();
    let created = pi_runtime::create_agent_session(pi_runtime::CreateAgentSessionOptions {
        cwd: Some(directory.clone()),
        agent_dir: Some(directory.clone()),
        extension_factories: vec![pi_runtime::create_mcp_extension(
            pi_runtime::McpExtensionOptions {
                cwd: Some(directory.clone()),
                agent_dir: Some(directory.clone()),
                on_oauth_redirect: Some(Arc::new(|url| {
                    Box::pin(async move {
                        reqwest::Client::new()
                            .get(url)
                            .send()
                            .await
                            .map_err(|_| "Browser redirect failed".to_owned())?;
                        Ok(())
                    })
                })),
                ..Default::default()
            },
        )],
        ..Default::default()
    })
    .await
    .unwrap();
    let session = created.session;
    let runner = session.extension_runner();
    let command = runner.get_command("mcp").unwrap();
    (command.command.handler)("auth local".to_owned(), runner.create_command_context())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if session
                .get_all_tools()
                .iter()
                .any(|tool| tool.name == "mcp__local__authorized")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.get("local").unwrap().access_token, "initial");
    (command.command.handler)("logout local".to_owned(), runner.create_command_context())
        .await
        .unwrap();
    assert!(store.get("local").is_none());
    session.shutdown().await.unwrap();
    drop(runner);
    drop(session);
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn callback_rejects_wrong_state_and_duplicate_fields_without_consuming_the_login() {
    let callback = McpOAuthCallbackServer::listen(None, None).await.unwrap();
    let url = callback.redirect_uri.clone();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(callback.wait("correct", Duration::from_secs(2), cancel));
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{url}?code=bad&state=wrong"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .get(format!("{url}?code=bad&state=correct&state=wrong"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .get(format!(
                "{url}?code=good&state=correct&iss=https%3A%2F%2Fissuer.invalid"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let response = task.await.unwrap().unwrap();
    assert_eq!(response.code, "good");
    assert_eq!(response.issuer.as_deref(), Some("https://issuer.invalid"));
    let callback = McpOAuthCallbackServer::listen(None, None).await.unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        callback
            .wait("state", Duration::from_secs(2), cancel)
            .await
            .unwrap_err()
            .contains("cancelled")
    );
}

#[test]
fn pkce_and_challenge_parsing_preserve_protocol_values() {
    let (verifier, challenge) = generate_mcp_pkce();
    assert!((43..=128).contains(&verifier.len()));
    assert_eq!(
        challenge,
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()))
    );
    let parsed = parse_oauth_challenge(
        "Bearer resource_metadata=\"https://auth.invalid/resource\", scope=\"read,write extra\", error=\"insufficient_scope\"",
    );
    assert_eq!(parsed.scope.as_deref(), Some("read,write extra"));
    assert_eq!(parsed.error.as_deref(), Some("insufficient_scope"));
}

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn capture_request() -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
            if let Some(i) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..i]);
                let size: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= i + 4 + size {
                    break;
                }
            }
        }
        socket
            .write_all(b"HTTP/1.1 400 Mock\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
        String::from_utf8(request).unwrap()
    });
    (url, server)
}

#[tokio::test]
async fn all_chat_protocols_apply_header_hook_after_auth_and_defaults() {
    for (api_name, auth_header) in [
        (known_api::OPENAI_COMPLETIONS, "authorization"),
        (known_api::OPENAI_RESPONSES, "authorization"),
        (known_api::ANTHROPIC_MESSAGES, "x-api-key"),
        (known_api::GOOGLE_GENERATIVE_AI, "x-goog-api-key"),
    ] {
        let (url, server) = capture_request().await;
        let models = Models::default();
        let mut model = faux::register_faux_provider(&models).model;
        model.api = api_name.into();
        model.provider = "local".into();
        model.base_url = url;
        model.headers = Some(BTreeMap::from([("x-source".into(), "model".into())]));
        let called = Arc::new(Mutex::new(false));
        let seen = called.clone();
        let options = SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("local-key".into()),
                headers: Some(BTreeMap::from([("X-Source".into(), Some("caller".into()))])),
                on_headers: Some(Arc::new(move |mut headers, _| {
                    assert!(headers[auth_header].as_ref().unwrap().contains("local-key"));
                    assert_eq!(headers["x-source"].as_deref(), Some("caller"));
                    assert_eq!(headers["content-type"].as_deref(), Some("application/json"));
                    headers.insert(auth_header.to_ascii_uppercase(), None);
                    headers.insert("User-Agent".into(), None);
                    headers.insert("Content-Type".into(), None);
                    headers.insert("X-Source".into(), Some("hook".into()));
                    *seen.lock().unwrap() = true;
                    Box::pin(async move { headers })
                })),
                ..Default::default()
            },
            ..Default::default()
        };
        let context = TranscriptContext::default();
        let stream = match api_name {
            known_api::OPENAI_COMPLETIONS => {
                api::openai_completions::stream_simple(&model, &context, Some(&options))
            }
            known_api::OPENAI_RESPONSES => {
                api::openai_responses::stream_simple(&model, &context, Some(&options))
            }
            known_api::ANTHROPIC_MESSAGES => {
                api::anthropic_messages::stream_simple(&model, &context, Some(&options))
            }
            _ => api::google_generative_ai::stream_simple(&model, &context, Some(&options)),
        };
        tokio::time::timeout(Duration::from_secs(10), stream.result())
            .await
            .unwrap();
        let request = tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap()
            .to_ascii_lowercase();
        assert!(*called.lock().unwrap(), "{api_name}");
        assert!(!request.contains(&format!("{auth_header}:")), "{api_name}");
        assert!(!request.contains("user-agent:"), "{api_name}");
        assert!(!request.contains("content-type:"), "{api_name}");
        assert!(request.contains("x-source: hook"), "{api_name}");
    }
}

async fn protocol_request(
    mut model: Model,
    path: &str,
    context: &TranscriptContext,
    options: &StreamOptions,
) -> (String, BTreeMap<String, String>, serde_json::Value) {
    let (url, server) = capture_request().await;
    model.base_url = format!("{url}{path}");
    let stream = match model.api.as_str() {
        known_api::OPENAI_RESPONSES => {
            api::openai_responses::stream(&model, context, Some(options))
        }
        known_api::ANTHROPIC_MESSAGES => {
            api::anthropic_messages::stream(&model, context, Some(options))
        }
        _ => api::openai_completions::stream(&model, context, Some(options)),
    };
    let (result, request) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(stream.result(), server)
    })
    .await
    .expect("loopback request timed out");
    assert_eq!(result.stop_reason, StopReason::Error); // The mock returns 400.
    let request = request.unwrap();
    let (head, body) = request.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let route = lines.next().unwrap().to_owned();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    (route, headers, serde_json::from_str(body).unwrap())
}

fn model_for_test(provider: &str, api: &str) -> Model {
    let mut model = faux::register_faux_provider(&Models::default()).model;
    model.provider = provider.to_owned();
    model.api = api.to_owned();
    model
}

fn wire_options(token: &str) -> StreamOptions {
    StreamOptions {
        api_key: Some(token.to_owned()),
        identity: Some(ClientIdentity::with_pi_attribution()),
        session_id: Some("test-session".to_owned()),
        cache_retention: Some(CacheRetention::Short),
        timeout_ms: Some(5_000),
        ..Default::default()
    }
}

fn assert_no_openrouter(headers: &BTreeMap<String, String>) {
    for name in [
        "x-openrouter-title",
        "http-referer",
        "x-openrouter-categories",
    ] {
        assert!(!headers.contains_key(name), "unexpected {name}");
    }
}

#[tokio::test]
async fn codex_sse_defaults_use_the_same_bounded_session_key_on_the_wire() {
    use base64::Engine;
    let claim = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"test-account"}}"#);
    let token = format!("test.{claim}.unsigned");
    let mut options = wire_options(&token);
    options.session_id = Some("a".repeat(70));
    let model = model_for_test("openai-codex", known_api::OPENAI_RESPONSES);
    let context = TranscriptContext::default();
    let (route, headers, body) =
        protocol_request(model.clone(), "/backend-api", &context, &options).await;
    assert_eq!(route, "POST /backend-api/codex/responses HTTP/1.1");
    assert_eq!(headers["originator"], "pi");
    assert_eq!(headers["openai-beta"], "responses=experimental");
    assert_eq!(headers["accept"], "text/event-stream");
    assert_eq!(headers["session-id"], "a".repeat(64));
    assert_eq!(headers["x-client-request-id"], headers["session-id"]);
    assert_eq!(body["prompt_cache_key"], headers["session-id"]);
    assert!(!headers.contains_key("session_id"));
    assert_eq!(headers["chatgpt-account-id"], "test-account");
    assert_eq!(headers["authorization"], format!("Bearer {token}"));
    assert!(headers["user-agent"].starts_with("pi/1.6.0 ("));
    assert_no_openrouter(&headers);

    options.cache_retention = Some(CacheRetention::None);
    options.api_key = Some("fake-codex-token-without-claims".into());
    options.headers = Some(BTreeMap::from([
        ("ORIGINATOR".into(), Some("test-app".into())),
        ("openAI-beta".into(), None),
    ]));
    let (_, headers, body) = protocol_request(model, "/backend-api", &context, &options).await;
    assert_eq!(headers["originator"], "test-app");
    for name in [
        "openai-beta",
        "session-id",
        "session_id",
        "x-client-request-id",
        "chatgpt-account-id",
    ] {
        assert!(!headers.contains_key(name), "unexpected {name}");
    }
    assert!(body.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn codex_without_an_account_id_reports_a_clear_error_before_sending() {
    let mut model = model_for_test("openai-codex", known_api::OPENAI_RESPONSES);
    // No network traffic even if validation fails to stop the request.
    model.base_url = "file:///not-a-network-endpoint".into();
    let result = api::openai_responses::stream(
        &model,
        &TranscriptContext::default(),
        Some(&wire_options("fake-codex-token-without-claims")),
    )
    .result()
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .unwrap()
            .contains("requires ChatGPT-Account-Id")
    );
}

#[test]
fn custom_provider_attribution_uses_the_endpoint_hostname() {
    let identity = ClientIdentity::with_pi_attribution();
    for (url, expected) in [
        ("https://openrouter.ai/api/v1", Some("x-openrouter-title")),
        (
            "https://integrate.api.nvidia.com/v1",
            Some("x-billing-invoke-origin"),
        ),
        (
            "https://opencode.ai/inference/v1",
            Some("x-opencode-client"),
        ),
        ("https://openrouter.ai.example.test/v1", None),
        ("https://openrouter.ai@example.test/v1", None),
        ("https://example.test/v1?host=openrouter.ai", None),
    ] {
        let headers = api::build_header_map(
            ("custom", url),
            Some(&identity),
            Some("sid"),
            &[],
            None,
            None,
        );
        for name in [
            "x-openrouter-title",
            "x-billing-invoke-origin",
            "x-opencode-client",
        ] {
            assert_eq!(
                headers.contains_key(name),
                expected == Some(name),
                "{url}: {name}"
            );
        }
        assert_eq!(
            headers.contains_key("x-opencode-session"),
            expected == Some("x-opencode-client")
        );
    }
}

#[tokio::test]
async fn provider_attribution_is_scoped_and_can_be_disabled() {
    let context = TranscriptContext::default();
    let mut options = wire_options("local-key");
    for provider in [
        "openrouter",
        "nvidia",
        "opencode",
        "opencode-go",
        "custom",
        "openai",
    ] {
        let model = model_for_test(provider, known_api::OPENAI_RESPONSES);
        // A gateway mentioning OpenRouter in its path is not an OpenRouter endpoint.
        let (_, headers, _) =
            protocol_request(model, "/openrouter.ai/v1", &context, &options).await;
        if provider == "openrouter" {
            assert_eq!(headers["x-openrouter-title"], "pi");
            assert_eq!(headers["http-referer"], "https://pi.dev");
            assert_eq!(headers["x-openrouter-categories"], "cli-agent");
        } else {
            assert_no_openrouter(&headers);
        }
        if provider == "nvidia" {
            assert_eq!(headers["x-billing-invoke-origin"], "Pi");
        } else {
            assert!(!headers.contains_key("x-billing-invoke-origin"));
        }
        if matches!(provider, "opencode" | "opencode-go") {
            assert_eq!(headers["x-opencode-client"], "pi");
            assert_eq!(headers["x-opencode-session"], "test-session");
        } else {
            assert!(!headers.contains_key("x-opencode-client"));
            assert!(!headers.contains_key("x-opencode-session"));
        }
        if provider == "openai" {
            assert_eq!(headers["session_id"], "test-session");
            assert_eq!(headers["x-client-request-id"], "test-session");
            assert!(!headers.contains_key("session-id"));
            assert!(!headers.contains_key("originator"));
            assert!(!headers.contains_key("openai-beta"));
        }
    }

    options.identity = Some(ClientIdentity {
        attribution: Some(None),
        app_name: Some("must-not-leak".into()),
        user_agent: Some("test-agent".into()),
        ..Default::default()
    });
    let model = model_for_test("openrouter", known_api::OPENAI_COMPLETIONS);
    let (_, headers, _) = protocol_request(model, "", &context, &options).await;
    assert_no_openrouter(&headers);
    assert_eq!(headers["user-agent"], "test-agent");
}

#[tokio::test]
async fn anthropic_key_oauth_and_copilot_send_distinct_auth_and_feature_headers() {
    use serde_json::json;
    let context = normalize_context(&Context {
        messages: vec![Message::User(UserMessage::new("test"))],
        tools: Some(vec![Tool {
            name: "read".into(),
            description: "Local fixture".into(),
            parameters: json!({"type": "object"}),
            constrained_sampling: None,
        }]),
        system_prompt: None,
    });
    let mut model = model_for_test("anthropic", known_api::ANTHROPIC_MESSAGES);
    model.reasoning = true;
    model.max_tokens = 8192;
    model.context_window = 100_000;
    model.compat = Some(ProviderCompat {
        supports_eager_tool_input_streaming: Some(false),
        ..Default::default()
    });
    model.headers = Some(BTreeMap::from([(
        "Anthropic-Beta".into(),
        " model-beta , fine-grained-tool-streaming-2025-05-14 ".into(),
    )]));
    let mut options = wire_options("sk-ant-api-fake");
    options.reasoning_effort = Some(ThinkingLevel::Low);
    options.headers = Some(BTreeMap::from([(
        "ANTHROPIC-BETA".into(),
        Some("model-beta,request-beta, request-beta".into()),
    )]));
    let (_, headers, body) = protocol_request(model.clone(), "", &context, &options).await;
    assert_no_openrouter(&headers);
    assert_eq!(headers["x-api-key"], "sk-ant-api-fake");
    assert!(!headers.contains_key("authorization"));
    assert!(headers["user-agent"].starts_with("pi/1.6.0 ("));
    assert_eq!(body["thinking"]["type"], "enabled");
    assert!(body["tools"][0].get("eager_input_streaming").is_none());
    assert_eq!(
        headers["anthropic-beta"],
        "fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14,model-beta,request-beta"
    );

    options.api_key = Some("sk-ant-oat-fake".into());
    options.identity.as_mut().unwrap().user_agent = Some("must-not-replace-oauth-ua".into());
    // Give this fixture an attribution-eligible provider ID to verify OAuth suppresses identity.
    model.provider = "openrouter".into();
    let (_, headers, body) = protocol_request(model.clone(), "", &context, &options).await;
    assert_no_openrouter(&headers);
    assert_eq!(headers["authorization"], "Bearer sk-ant-oat-fake");
    assert!(!headers.contains_key("x-api-key"));
    assert_eq!(headers["user-agent"], "claude-cli/2.1.280");
    assert_eq!(headers["x-app"], "cli");
    assert_eq!(body["tools"][0]["name"], "Read");
    assert_eq!(
        headers["anthropic-beta"],
        "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14,model-beta,request-beta"
    );

    model.provider = "github-copilot".into();
    model.headers = Some(BTreeMap::from([(
        "User-Agent".into(),
        "GitHubCopilotChat/0.35.0".into(),
    )]));
    model.compat = Some(ProviderCompat {
        force_adaptive_thinking: Some(true),
        ..Default::default()
    });
    let (_, headers, body) = protocol_request(model.clone(), "", &context, &options).await;
    assert_no_openrouter(&headers);
    assert_eq!(headers["authorization"], "Bearer sk-ant-oat-fake");
    assert!(!headers.contains_key("x-api-key"));
    assert!(!headers.contains_key("x-app"));
    assert_eq!(headers["user-agent"], "GitHubCopilotChat/0.35.0");
    assert_eq!(headers["x-initiator"], "user");
    assert_eq!(headers["openai-intent"], "conversation-edits");
    assert_eq!(headers["anthropic-beta"], "model-beta,request-beta");
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["eager_input_streaming"], true);
    assert!(body.get("system").is_none());

    options.headers = Some(BTreeMap::from([("ANTHROPIC-BETA".into(), None)]));
    let (_, headers, _) = protocol_request(model.clone(), "", &context, &options).await;
    assert!(!headers.contains_key("anthropic-beta"));

    // Header-only auth on Anthropic & Google must not inject bogus placeholder keys
    let hdr_only_opts = StreamOptions {
        headers: Some(BTreeMap::from([(
            "Authorization".into(),
            Some("Bearer custom-proxy-token".into()),
        )])),
        ..Default::default()
    };
    let mut anth_proxy = model_for_test("custom-anth", known_api::ANTHROPIC_MESSAGES);
    anth_proxy.headers = None;
    let (_, headers, _) = protocol_request(anth_proxy, "", &context, &hdr_only_opts).await;
    assert_eq!(headers["authorization"], "Bearer custom-proxy-token");
    assert!(!headers.contains_key("x-api-key"));

    let goog_proxy = model_for_test("custom-goog", known_api::GOOGLE_GENERATIVE_AI);
    let (_, headers, _) = protocol_request(goog_proxy, "", &context, &hdr_only_opts).await;
    assert_eq!(headers["authorization"], "Bearer custom-proxy-token");
    assert!(!headers.contains_key("x-goog-api-key"));
}

#[test]
fn openai_completions_compat_respects_reasoning_effort_templates_and_off_map() {
    let context = TranscriptContext::default();

    // 1. supports_reasoning_effort = false suppresses top-level reasoning_effort
    let mut cf_model = model_for_test("cloudflare-ai-gateway", known_api::OPENAI_COMPLETIONS);
    cf_model.reasoning = true;
    cf_model.compat = Some(ProviderCompat {
        supports_reasoning_effort: Some(false),
        thinking_format: Some("chat-template".into()),
        chat_template_kwargs: Some(BTreeMap::from([(
            "enable_thinking".into(),
            serde_json::json!({ "$var": "thinking.enabled" }),
        )])),
        ..Default::default()
    });
    let on_opts = StreamOptions {
        reasoning_effort: Some(ThinkingLevel::High),
        ..Default::default()
    };
    let on_body = pi_ai::api::openai_completions::build_completions_params(
        &cf_model,
        &context,
        Some(&on_opts),
    );
    assert!(on_body.get("reasoning_effort").is_none());
    assert_eq!(
        on_body["chat_template_kwargs"]["enable_thinking"],
        serde_json::json!(true)
    );
    let off_body =
        pi_ai::api::openai_completions::build_completions_params(&cf_model, &context, None);
    assert_eq!(
        off_body["chat_template_kwargs"]["enable_thinking"],
        serde_json::json!(false)
    );

    // 2. together & baseten formats + thinkingLevelMap.off
    let mut together_model = model_for_test("together", known_api::OPENAI_COMPLETIONS);
    together_model.reasoning = true;
    let tg_on = pi_ai::api::openai_completions::build_completions_params(
        &together_model,
        &context,
        Some(&on_opts),
    );
    assert_eq!(tg_on["reasoning"], serde_json::json!({ "enabled": true }));
    assert!(tg_on.get("reasoning_effort").is_none());

    let mut off_mapped_model = model_for_test("openai", known_api::OPENAI_COMPLETIONS);
    off_mapped_model.reasoning = true;
    off_mapped_model.thinking_level_map = Some(BTreeMap::from([(
        pi_ai::ModelThinkingLevel::Off,
        Some("none".to_owned()),
    )]));
    let off_mapped_body =
        pi_ai::api::openai_completions::build_completions_params(&off_mapped_model, &context, None);
    assert_eq!(off_mapped_body["reasoning_effort"], "none");
}

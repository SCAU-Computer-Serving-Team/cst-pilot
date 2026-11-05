use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn all_chat_protocols_apply_header_hook_after_auth_and_defaults() {
    for (api_name, auth_header) in [
        (known_api::OPENAI_COMPLETIONS, "authorization"),
        (known_api::OPENAI_RESPONSES, "authorization"),
        (known_api::ANTHROPIC_MESSAGES, "x-api-key"),
        (known_api::GOOGLE_GENERATIVE_AI, "x-goog-api-key"),
    ] {
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
            String::from_utf8(request).unwrap().to_ascii_lowercase()
        });
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
        let request = server.await.unwrap();
        assert!(*called.lock().unwrap(), "{api_name}");
        assert!(!request.contains(&format!("{auth_header}:")), "{api_name}");
        assert!(!request.contains("user-agent:"), "{api_name}");
        assert!(!request.contains("content-type:"), "{api_name}");
        assert!(request.contains("x-source: hook"), "{api_name}");
    }
}

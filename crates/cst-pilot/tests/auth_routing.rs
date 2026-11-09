use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use cst_pilot::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_pilot::{Kit, SKILL_NAMES};
use pi_runtime::CreateAgentSessionOptions;
use pi_session::SessionManager;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("cst-auth-routing-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("agent/home")).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join("agent/home").join(name)
    }
    fn resources(&self) {
        fs::write(self.file("APPEND_SYSTEM.md"), "固定规则。").unwrap();
        for name in SKILL_NAMES {
            let dir = self.file(&format!("skills/{name}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: 指南\n---\n工具使用说明。"),
            )
            .unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn both_credential_modes_use_real_chat_completions_routes_and_bearer_headers() {
    for oauth in [false, true] {
        let fixture = Fixture::new();
        fixture.resources();
        let credential = if oauth {
            json!({"type":"oauth","access":"synthetic-oauth","refresh":"","expires":pi_ai::now_ms()+86400000})
        } else {
            json!({"type":"api_key","key":"synthetic-api-key"})
        };
        fs::write(
            fixture.file("auth.json"),
            json!({"cstoa":credential}).to_string(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                    assert!(bytes.len() < 1024 * 1024);
                }
                let request = String::from_utf8(bytes).unwrap();
                let model_request = request.lines().next().unwrap().contains("/models");
                captured.lock().unwrap().push(request);
                let (content_type, body) = if model_request {
                    (
                        "application/json",
                        json!({"data":[{"id":"test-chat"}]}).to_string(),
                    )
                } else {
                    (
                        "text/event-stream",
                        format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            json!({"id":"mock-chat","object":"chat.completion.chunk","created":1,"model":"test-chat","choices":[{"index":0,"delta":{"role":"assistant","content":"路由验证通过"},"finish_reason":null}]}),
                            json!({"id":"mock-chat","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}})
                        ),
                    )
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let auth = AuthRuntime::create(&fixture.0, Channel::Gui, OaClient::new(&host).unwrap())
            .await
            .unwrap();
        assert!(auth.refresh(true).await.errors.is_empty());
        let model = auth.runtime().get_model("cstoa", "test-chat").unwrap();
        let session = Kit::new(&fixture.0)
            .unwrap()
            .create_session_with_auth(
                CreateAgentSessionOptions {
                    model: Some(model),
                    session_manager: Some(
                        SessionManager::in_memory(fixture.0.to_string_lossy(), None, None).unwrap(),
                    ),
                    ..Default::default()
                },
                auth,
            )
            .await
            .unwrap();
        session
            .created
            .session
            .prompt("验证路由", None)
            .await
            .unwrap();
        let messages = session.created.session.state().messages;
        assert!(
            serde_json::to_string(&messages)
                .unwrap()
                .contains("路由验证通过")
        );
        session.shutdown().await.unwrap();
        let captured = requests.lock().unwrap().clone();
        let prefix = if oauth { "/api/agent/llm/v1" } else { "/v1" };
        assert!(
            captured
                .iter()
                .any(|r| r.starts_with(&format!("GET {prefix}/models ")))
        );
        assert!(
            captured
                .iter()
                .any(|r| r.starts_with(&format!("POST {prefix}/chat/completions ")))
        );
        let key = if oauth {
            "synthetic-oauth"
        } else {
            "synthetic-api-key"
        };
        assert!(captured.iter().all(|r| {
            r.lines()
                .any(|l| l.eq_ignore_ascii_case(&format!("authorization: Bearer {key}")))
        }));
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn auth_entrypoints_reject_hard_links_before_reading_or_replacing_external_files() {
    for name in ["auth.json", "models.json", "models-store.json"] {
        let f = Fixture::new();
        let external = f.0.join("external.json");
        let original = b"{\"private\":\"synthetic-only\"}";
        fs::write(&external, original).unwrap();
        fs::hard_link(&external, f.file(name)).unwrap();
        let result = AuthRuntime::create(
            &f.0,
            Channel::Tui,
            OaClient::new("http://127.0.0.1:1").unwrap(),
        )
        .await;
        let error = result.err().expect("hard links must fail");
        assert!(!error.contains("synthetic-only"));
        assert_eq!(fs::read(&external).unwrap(), original);
    }
    let f = Fixture::new();
    let auth = AuthRuntime::create(
        &f.0,
        Channel::Tui,
        OaClient::new("http://127.0.0.1:1").unwrap(),
    )
    .await
    .unwrap();
    let external = f.0.join("external.json");
    fs::write(&external, "{}").unwrap();
    fs::hard_link(&external, f.file("models.json")).unwrap();
    assert!(auth.refresh(false).await.errors.contains_key("cstoa"));
    assert_eq!(fs::read_to_string(external).unwrap(), "{}");
    assert_eq!(
        auth.account(CancellationToken::new()).await.unwrap()["signedIn"],
        false
    );
}

#[cfg(windows)]
#[tokio::test]
async fn auth_home_junction_is_rejected_without_touching_its_target() {
    let f = Fixture::new();
    let external = f.0.join("external-home");
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("auth.json"), "{}").unwrap();
    fs::remove_dir(f.file("")).unwrap();
    let target = f.file("");
    let output = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&target)
        .arg(&external)
        .output()
        .unwrap();
    assert!(output.status.success());
    let result = AuthRuntime::create(
        &f.0,
        Channel::Tui,
        OaClient::new("http://127.0.0.1:1").unwrap(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(external.join("auth.json")).unwrap(),
        "{}"
    );
    fs::remove_dir(target).unwrap();
}

use std::collections::VecDeque;
use std::sync::Mutex;

use super::*;

struct FixedDns;
impl DnsResolver for FixedDns {
    fn resolve(&self, _: String, port: u16) -> BoxFuture<'_, Result<Vec<SocketAddr>, String>> {
        Box::pin(async move { Ok(vec![SocketAddr::new("8.8.8.8".parse().unwrap(), port)]) })
    }
}

struct MockConnector {
    replies: Mutex<VecDeque<HttpHop>>,
    calls: Mutex<Vec<(Url, Vec<SocketAddr>)>>,
}
impl Connector for MockConnector {
    fn send(
        &self,
        url: Url,
        addresses: Vec<SocketAddr>,
        _: HttpRequest,
    ) -> BoxFuture<'_, Result<HttpHop, String>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((url, addresses));
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("no reply".into())
        })
    }
}

fn client(replies: Vec<HttpHop>) -> (SafeHttpClient, Arc<MockConnector>) {
    let connector = Arc::new(MockConnector {
        replies: Mutex::new(replies.into()),
        calls: Mutex::new(Vec::new()),
    });
    (
        SafeHttpClient {
            policy: NetworkPolicy::default(),
            resolver: Arc::new(FixedDns),
            connector: connector.clone(),
        },
        connector,
    )
}
fn redirect(location: &str) -> HttpHop {
    HttpHop {
        status: 302,
        location: Some(location.into()),
        body: String::new(),
    }
}
fn get() -> HttpRequest {
    HttpRequest {
        url: "https://public.example/page".into(),
        body: None,
        headers: vec![],
    }
}

#[tokio::test]
async fn redirects_revalidate_private_addresses_and_prevent_https_downgrade() {
    for location in [
        "http://127.0.0.1/secret",
        "https://10.0.0.1/secret",
        "https://[::1]",
        "http://public.example/unsafe",
    ] {
        let (client, connector) = client(vec![redirect(location)]);
        assert!(
            client
                .request(get(), CancellationToken::new())
                .await
                .is_err(),
            "{location}"
        );
        assert_eq!(connector.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn public_redirects_keep_validated_addresses_and_resolve_each_target() {
    let (client, connector) = client(vec![
        redirect("https://next.example/path"),
        HttpHop {
            status: 200,
            location: None,
            body: "ok".into(),
        },
    ]);
    assert_eq!(
        client
            .request(get(), CancellationToken::new())
            .await
            .unwrap()
            .body,
        "ok"
    );
    let calls = connector.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].0.host_str(), Some("next.example"));
    assert_eq!(
        calls[1].1,
        vec!["8.8.8.8:443".parse::<SocketAddr>().unwrap()]
    );
}

#[tokio::test]
async fn credentialed_and_post_requests_never_follow_redirects() {
    for request in [
        HttpRequest {
            headers: vec![("Authorization".into(), "synthetic-secret".into())],
            ..get()
        },
        HttpRequest {
            body: Some(serde_json::json!({"query":"x"})),
            ..get()
        },
    ] {
        let (client, connector) = client(vec![redirect("https://next.example")]);
        let error = client
            .request(request, CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(!error.contains("synthetic-secret"));
        assert_eq!(connector.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn redirect_loops_stop_after_the_bounded_hop_count() {
    let (client, connector) = client((0..=MAX_REDIRECTS).map(|_| redirect("/loop")).collect());
    assert!(
        client
            .request(get(), CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(connector.calls.lock().unwrap().len(), MAX_REDIRECTS + 1);
}

#[tokio::test]
async fn production_connector_handles_loopback_mock_body_limits_and_invalid_utf8() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for bytes in [
        b"mock body".to_vec(),
        vec![b'x'; MAX_BODY_BYTES + 1],
        vec![0xff, 0xfe],
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected_ok = bytes == b"mock body";
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&bytes).await;
        });
        // Exercise the low-level connector only. The production policy still denies loopback.
        let result = ReqwestConnector
            .send(
                Url::parse(&format!("http://mock.example:{}", address.port())).unwrap(),
                vec![address],
                get(),
            )
            .await;
        assert_eq!(result.is_ok(), expected_ok);
        server.await.unwrap();
    }
}

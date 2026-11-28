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
        timeout_ms: None,
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

#[test]
fn decode_http_body_supports_utf8_and_gb18030_while_rejecting_binary() {
    let (gb_bytes, _, _) = encoding_rs::GB18030.encode("中文页面内容 2026");
    assert_eq!(
        decode_http_body(&gb_bytes, Some("text/html; charset=gbk")).unwrap(),
        "中文页面内容 2026"
    );
    assert_eq!(
        decode_http_body(&gb_bytes, None).unwrap(),
        "中文页面内容 2026"
    );
    assert_eq!(
        decode_http_body("UTF-8 中文".as_bytes(), Some("text/plain; charset=utf-8")).unwrap(),
        "UTF-8 中文"
    );
    assert!(decode_http_body(&[0xff, 0xfe], None).is_err());
    assert!(decode_http_body(b"abc\0def", None).is_err());
    assert_eq!(DEFAULT_TIMEOUT_MS, 30_000);
}

#[tokio::test]
async fn allow_tun_strips_companion_ula_and_never_connects_to_private_v6() {
    struct ListDns(Vec<SocketAddr>);
    impl DnsResolver for ListDns {
        fn resolve(&self, _: String, _: u16) -> BoxFuture<'_, Result<Vec<SocketAddr>, String>> {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    let tun_policy = NetworkPolicy { allow_tun: true };
    let strict_policy = NetworkPolicy { allow_tun: false };

    // Prefix boundaries and IPv4-mapped IPv6 checks
    assert!(
        strict_policy
            .validate_ip("198.17.255.255".parse().unwrap())
            .is_ok()
    );
    assert!(
        strict_policy
            .validate_ip("198.20.0.0".parse().unwrap())
            .is_ok()
    );
    assert!(
        strict_policy
            .validate_ip("198.18.0.0".parse().unwrap())
            .is_err()
    );
    assert!(
        strict_policy
            .validate_ip("198.19.255.255".parse().unwrap())
            .is_err()
    );
    assert!(
        tun_policy
            .validate_ip("198.18.0.0".parse().unwrap())
            .is_ok()
    );
    assert!(
        tun_policy
            .validate_ip("198.19.255.255".parse().unwrap())
            .is_ok()
    );
    assert!(
        tun_policy
            .validate_ip("::ffff:198.18.1.205".parse().unwrap())
            .is_ok()
    );
    assert!(
        strict_policy
            .validate_ip("::ffff:198.18.1.205".parse().unwrap())
            .is_err()
    );
    assert!(
        tun_policy
            .validate_ip("::ffff:127.0.0.1".parse().unwrap())
            .is_err()
    );

    // ULA IPv6 literals and ULA-only DNS records are ALWAYS rejected even when allow_tun is true
    for ula_lit in ["https://[fc00::1234]/x", "https://[fdfe:dcba:9876::1]/x"] {
        assert!(tun_policy.validate_url(ula_lit).is_err(), "{ula_lit}");
    }
    for ula_only in [
        "[fc00::1234]:443",
        "[fdfe:dcba:9876::1]:443",
        "[fd12:3456::1]:443",
    ] {
        let client_ula_only = SafeHttpClient::with_resolver(
            tun_policy.clone(),
            Arc::new(ListDns(vec![ula_only.parse().unwrap()])),
        );
        assert!(
            client_ula_only
                .resolve_target("https://www.cstoa.top/api/agent/releases/latest")
                .await
                .is_err(),
            "{ula_only}"
        );
    }

    // Dual-stack TUN DNS (`198.18.0.0/15` + ULA Fake-IPv6) strips all ULA addresses before connecting
    let connector = Arc::new(MockConnector {
        replies: Mutex::new(VecDeque::from([HttpHop {
            status: 200,
            location: None,
            body: "ok".into(),
        }])),
        calls: Mutex::new(Vec::new()),
    });
    let dual_stack_client = SafeHttpClient {
        policy: tun_policy.clone(),
        resolver: Arc::new(ListDns(vec![
            "198.18.1.205:443".parse().unwrap(),
            "[fc00::1234]:443".parse().unwrap(),
            "[fdfe:dcba:9876::1]:443".parse().unwrap(),
        ])),
        connector: connector.clone(),
    };
    let resp = dual_stack_client
        .request(
            HttpRequest {
                url: "https://www.cstoa.top/api/agent/releases/latest".into(),
                ..get()
            },
            CancellationToken::new(),
        )
        .await
        .expect("dual-stack TUN fake-ip must succeed via 198.18.0.0/15 only");
    assert_eq!(resp.status, 200);
    {
        let calls = connector.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            vec!["198.18.1.205:443".parse::<SocketAddr>().unwrap()]
        );
    }

    // Mixed public + ULA or TUN + loopback/mapped-loopback must still be rejected as a whole
    for rebind in [
        vec!["8.8.8.8:443", "[fc00::1234]:443"],
        vec!["8.8.8.8:443", "[fd12:3456::1]:443"],
        vec!["198.18.1.205:443", "127.0.0.1:443"],
        vec!["198.18.1.205:443", "[::1]:443"],
        vec!["198.18.1.205:443", "[::ffff:127.0.0.1]:443"],
    ] {
        let bad = SafeHttpClient::with_resolver(
            tun_policy.clone(),
            Arc::new(ListDns(
                rebind.into_iter().map(|s| s.parse().unwrap()).collect(),
            )),
        );
        assert!(
            bad.resolve_target("https://rebind.example/x")
                .await
                .is_err()
        );
    }
}

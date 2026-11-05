use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_runtime::{InMemoryTransport, McpClient, McpClientOptions, McpRequestOptions, McpTransport};
use serde_json::json;

#[tokio::test]
async fn progress_callback_panic_does_not_stop_mcp_receiver() {
    let (client_transport, server_transport) = InMemoryTransport::pair();
    let mut server_rx = server_transport.start().await.unwrap();
    let server = tokio::spawn(async move {
        while let Some(msg) = server_rx.recv().await {
            let Some(id) = msg.get("id") else {
                continue;
            };
            let result = if msg["method"] == "initialize" {
                json!({
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "serverInfo": {"name":"test", "version":"1"}
                })
            } else {
                if let Some(token) = msg.pointer("/params/_meta/progressToken") {
                    server_transport
                        .send(json!({
                            "jsonrpc":"2.0",
                            "method":"notifications/progress",
                            "params":{"progressToken":token, "progress":1}
                        }))
                        .await
                        .unwrap();
                }
                json!({"ok":true})
            };
            server_transport
                .send(json!({"jsonrpc":"2.0", "id":id, "result":result}))
                .await
                .unwrap();
        }
    });
    let client = McpClient::new(McpClientOptions {
        request_timeout_ms: 500,
        ..Default::default()
    });
    client.connect(client_transport).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let result = client
        .request(
            "probe",
            None,
            McpRequestOptions {
                on_progress: Some(Arc::new({
                    let calls = calls.clone();
                    move |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        panic!("progress handler panic");
                    }
                })),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.unwrap(), json!({"ok":true}));
    assert_eq!(
        client
            .request("probe-again", None, McpRequestOptions::default())
            .await
            .unwrap(),
        json!({"ok":true})
    );
    client.close().await.unwrap();
    server.abort();
}

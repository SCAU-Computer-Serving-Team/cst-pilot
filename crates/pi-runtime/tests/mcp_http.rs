#[path = "support/mcp_http.rs"]
mod fixture;

use fixture::{Response, Server, initialize};
use pi_runtime::{
    McpClient, McpClientOptions, McpRequestOptions, StreamableHttpTransport,
    StreamableHttpTransportOptions,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn expired_http_session_reinitializes_once_without_replaying_uncertain_writes() {
    let initializations = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let server=Server::spawn({let initializations=initializations.clone();let effects=effects.clone();move|request|{let initializations=initializations.clone();let effects=effects.clone();async move{
        let message=request.json();
        if message["method"]=="initialize" {let n=initializations.fetch_add(1,Ordering::SeqCst)+1;return Response::json(json!({"jsonrpc":"2.0","id":message["id"],"result":initialize(&n.to_string())})).header("Mcp-Session-Id",format!("session-{n}"));}
        if request.method=="DELETE" || message.get("id").is_none(){return Response::status(202)}
        if request.headers.get("mcp-session-id").map(String::as_str)==Some("session-1"){return Response::status(404)}
        assert_eq!(request.headers.get("mcp-session-id").map(String::as_str),Some("session-2"));
        effects.fetch_add(1,Ordering::SeqCst);
        Response::json(json!({"jsonrpc":"2.0","id":message["id"],"result":{"ok":true}}))
    }}}).await;
    let transport = Arc::new(StreamableHttpTransport::with_options(
        StreamableHttpTransportOptions {
            url: format!("{}/mcp", server.url),
            open_get_stream: false,
            ..Default::default()
        },
    ));
    let client = McpClient::new(McpClientOptions::default());
    client.connect(transport.clone()).await.unwrap();
    assert_eq!(
        client
            .request("write", None, McpRequestOptions::default())
            .await
            .unwrap(),
        json!({"ok":true})
    );
    assert_eq!(initializations.load(Ordering::SeqCst), 2);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(transport.session_id().as_deref(), Some("session-2"));
    assert_eq!(client.server_info().unwrap().version, "2");
    client.close().await.unwrap();
}

#[tokio::test]
async fn interrupted_post_sse_resumes_with_get_and_never_resends_the_tool_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let request_id = Arc::new(Mutex::new(Value::Null));
    let server=Server::spawn({let calls=calls.clone();let request_id=request_id.clone();move|request|{let calls=calls.clone();let request_id=request_id.clone();async move{
        let message=request.json();
        if request.method=="GET" {
            assert_eq!(request.headers.get("last-event-id").map(String::as_str),Some("event-1"));
            return Response::sse(format!("id: event-2\ndata: {}\n\n",json!({"jsonrpc":"2.0","id":request_id.lock().unwrap().clone(),"result":{"done":true}})));
        }
        if message["method"]=="initialize" {return Response::json(json!({"jsonrpc":"2.0","id":message["id"],"result":initialize("1")})).header("Mcp-Session-Id","session");}
        if message.get("id").is_none(){return Response::status(202)}
        calls.fetch_add(1,Ordering::SeqCst);*request_id.lock().unwrap()=message["id"].clone();
        let mut response=Response::sse(format!("id: event-1\nretry: 1\ndata: {}\n\n",json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":message["id"],"progress":1}})));
        response.announced_length=Some(response.body.len()+1024);response
    }}}).await;
    let client = McpClient::new(McpClientOptions {
        request_timeout_ms: 1000,
        ..Default::default()
    });
    client
        .connect(Arc::new(StreamableHttpTransport::with_options(
            StreamableHttpTransportOptions {
                url: format!("{}/mcp", server.url),
                open_get_stream: false,
                reconnect_initial_delay_ms: 1,
                ..Default::default()
            },
        )))
        .await
        .unwrap();
    assert_eq!(
        client
            .request("write", None, McpRequestOptions::default())
            .await
            .unwrap(),
        json!({"done":true})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    client.close().await.unwrap();
}

#[tokio::test]
async fn server_get_stream_reconnects_with_event_cursor_and_delivers_notifications() {
    let gets = Arc::new(AtomicUsize::new(0));
    let server=Server::spawn({let gets=gets.clone();move|request|{let gets=gets.clone();async move{
        if request.method=="GET" {
            let n=gets.fetch_add(1,Ordering::SeqCst);
            if n==0 {return Response::sse("id: get-1\nretry: 1\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n".to_owned())}
            assert_eq!(request.headers.get("last-event-id").map(String::as_str),Some("get-1"));
            let mut response=Response::sse("id: get-2\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/list_changed\"}\n\n".to_owned());response.keep_open=true;return response;
        }
        let message=request.json();if message["method"]=="initialize" {return Response::json(json!({"jsonrpc":"2.0","id":message["id"],"result":initialize("1")})).header("Mcp-Session-Id","session");}
        Response::status(202)
    }}}).await;
    let client = McpClient::new(McpClientOptions::default());
    let mut notifications = client.subscribe_notifications();
    client
        .connect(Arc::new(StreamableHttpTransport::with_options(
            StreamableHttpTransportOptions {
                url: format!("{}/mcp", server.url),
                reconnect_initial_delay_ms: 1,
                ..Default::default()
            },
        )))
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.0, "notifications/tools/list_changed");
    assert_eq!(second.0, "notifications/resources/list_changed");
    client.close().await.unwrap();
}

#[tokio::test]
async fn connection_loss_without_an_event_cursor_does_not_retry_a_side_effect() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = Server::spawn({
        let calls = calls.clone();
        move |request| {
            let calls = calls.clone();
            async move {
                let message = request.json();
                if message["method"] == "initialize" {
                    return Response::json(
                        json!({"jsonrpc":"2.0","id":message["id"],"result":initialize("1")}),
                    );
                }
                if message.get("id").is_none() {
                    return Response::status(202);
                }
                calls.fetch_add(1, Ordering::SeqCst);
                let mut response = Response::sse(": disconnected\n\n".to_owned());
                response.announced_length = Some(4096);
                response
            }
        }
    })
    .await;
    let client = McpClient::new(McpClientOptions::default());
    client
        .connect(Arc::new(StreamableHttpTransport::new(
            format!("{}/mcp", server.url),
            BTreeMap::new(),
        )))
        .await
        .unwrap();
    assert!(
        client
            .request("write", None, McpRequestOptions::default())
            .await
            .unwrap_err()
            .contains("unknown")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    client.close().await.unwrap();
}

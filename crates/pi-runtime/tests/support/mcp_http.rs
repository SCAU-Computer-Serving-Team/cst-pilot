#![allow(dead_code)]
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
impl Request {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub announced_length: Option<usize>,
    pub keep_open: bool,
}
impl Response {
    pub fn json(body: serde_json::Value) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body: serde_json::to_vec(&body).unwrap(),
            announced_length: None,
            keep_open: false,
        }
    }
    pub fn status(status: u16) -> Self {
        Self {
            status,
            ..Self::json(serde_json::Value::Null)
        }
    }
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }
    pub fn sse(body: String) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".to_owned(), "text/event-stream".to_owned())],
            body: body.into_bytes(),
            announced_length: None,
            keep_open: false,
        }
    }
}
pub struct Server {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    pub async fn spawn<F, Fut>(handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let handler = handler.clone();
                connections.spawn(async move {
                    let mut raw = Vec::new();
                    loop {
                        let Ok(byte) = socket.read_u8().await else {
                            return;
                        };
                        raw.push(byte);
                        if raw.ends_with(b"\r\n\r\n") {
                            break;
                        }
                        if raw.len() > 16384 {
                            return;
                        }
                    }
                    let text = String::from_utf8(raw).unwrap();
                    let mut lines = text.lines();
                    let mut first = lines.next().unwrap().split_whitespace();
                    let method = first.next().unwrap().to_owned();
                    let path = first.next().unwrap().to_owned();
                    let headers: BTreeMap<_, _> = lines
                        .filter_map(|line| {
                            line.split_once(':').map(|(key, value)| {
                                (key.to_ascii_lowercase(), value.trim().to_owned())
                            })
                        })
                        .collect();
                    let length: usize = headers
                        .get("content-length")
                        .map(|value| value.parse().unwrap())
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    if socket.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    let response = handler(Request {
                        method,
                        path,
                        headers,
                        body,
                    })
                    .await;
                    let mut head = format!(
                        "HTTP/1.1 {} Result\r\nConnection: close\r\n",
                        response.status
                    );
                    for (key, value) in response.headers {
                        head.push_str(&format!("{key}: {value}\r\n"));
                    }
                    if response.keep_open {
                        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
                    } else {
                        head.push_str(&format!(
                            "Content-Length: {}\r\n\r\n",
                            response.announced_length.unwrap_or(response.body.len())
                        ));
                    }
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    if response.keep_open {
                        let _ = socket
                            .write_all(format!("{:x}\r\n", response.body.len()).as_bytes())
                            .await;
                    }
                    if socket.write_all(&response.body).await.is_err() {
                        return;
                    }
                    if response.keep_open {
                        let _ = socket.write_all(b"\r\n").await;
                        let _ = socket.read_u8().await;
                    }
                });
            }
        });
        Self { url, task }
    }
}

pub fn initialize(version: &str) -> serde_json::Value {
    serde_json::json!({"protocolVersion":"2026-07-28","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":version}})
}

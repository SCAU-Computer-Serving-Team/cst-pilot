use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::http::AppHttpClient;

/// Maximum buffered SSE event size (`16 MiB`) to protect client memory from malformed frames.
const MAX_SSE_EVENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSseFrame {
    pub event: String,
    pub id: Option<String>,
    pub data: String,
}

/// Incremental byte-chunk SSE parser supporting `\n` and `\r\n` frame boundaries.
#[derive(Default)]
pub struct SseParser {
    line_buf: Vec<u8>,
    event_name: String,
    event_id: Option<String>,
    data_lines: Vec<String>,
    data_bytes: usize,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<ParsedSseFrame> {
        let mut frames = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                if self.line_buf.last() == Some(&b'\r') {
                    self.line_buf.pop();
                }
                let line = String::from_utf8_lossy(&self.line_buf).into_owned();
                self.line_buf.clear();
                if let Some(frame) = self.process_line(&line) {
                    frames.push(frame);
                }
            } else if self.line_buf.len() < MAX_SSE_EVENT_BYTES {
                self.line_buf.push(byte);
            }
        }
        frames
    }

    fn process_line(&mut self, line: &str) -> Option<ParsedSseFrame> {
        if line.is_empty() {
            if self.data_lines.is_empty() && self.event_name.is_empty() && self.event_id.is_none() {
                return None;
            }
            let event = if self.event_name.is_empty() {
                "message".to_owned()
            } else {
                std::mem::take(&mut self.event_name)
            };
            let id = self.event_id.take();
            let data = self.data_lines.join("\n");
            self.data_lines.clear();
            self.data_bytes = 0;
            return Some(ParsedSseFrame { event, id, data });
        }

        if line.starts_with(':') {
            return None;
        }

        let (field, raw_val) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };

        match field {
            "event" => {
                self.event_name = raw_val.to_owned();
            }
            "id" if !raw_val.contains('\0') => {
                self.event_id = Some(raw_val.to_owned());
            }
            "data" if self.data_bytes.saturating_add(raw_val.len()) <= MAX_SSE_EVENT_BYTES => {
                self.data_bytes += raw_val.len();
                self.data_lines.push(raw_val.to_owned());
            }
            _ => {}
        }
        None
    }
}

#[derive(Debug)]
pub enum KeepaliveSignal {
    Connected,
    Disconnected,
    Reset,
    StateEvent {
        cursor_id: Option<String>,
        payload: Value,
    },
}

#[derive(Debug)]
pub enum MultiplexSignal {
    StreamReady {
        subscribed_sessions: Vec<String>,
    },
    StateEvent {
        cursor_id: Option<String>,
        payload: Value,
    },
    SessionEvent {
        cursor_id: Option<String>,
        session_id: String,
        event: Value,
    },
    QueueEvent {
        cursor_id: Option<String>,
        session_id: String,
        queue: Value,
    },
    ResetGlobal,
    ResetSession {
        session_id: String,
    },
    Disconnected,
}

/// Runs the dedicated `/api/app/events` keepalive loop.
/// Never awaits UI rendering or HTTP snapshot reloads, ensuring the kernel TCP buffer stays drained
/// even if the UI main thread blocks for >30s.
pub async fn run_keepalive_sse_loop(
    http: AppHttpClient,
    tx: mpsc::UnboundedSender<KeepaliveSignal>,
    stop: CancellationToken,
) {
    let mut last_event_id: Option<String> = None;
    let mut backoff_ms = 120u64;

    while !stop.is_cancelled() {
        let open_res = tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            r = http.open_sse_stream("/api/app/events", last_event_id.as_deref()) => r,
        };

        let resp = match open_res {
            Ok(resp) => {
                backoff_ms = 120;
                let _ = tx.send(KeepaliveSignal::Connected);
                resp
            }
            Err(_) => {
                let _ = tx.send(KeepaliveSignal::Disconnected);
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
                }
                backoff_ms = (backoff_ms * 2).min(2_000);
                continue;
            }
        };

        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::default();

        loop {
            let next_chunk = tokio::select! {
                biased;
                _ = stop.cancelled() => return,
                c = stream.next() => c,
            };
            let Some(Ok(bytes)) = next_chunk else {
                let _ = tx.send(KeepaliveSignal::Disconnected);
                break;
            };
            for frame in parser.feed(&bytes) {
                if let Some(ref id) = frame.id {
                    last_event_id = Some(id.clone());
                }
                match frame.event.as_str() {
                    "heartbeat" => {}
                    "reset" => {
                        let _ = tx.send(KeepaliveSignal::Reset);
                    }
                    "state" => {
                        if let Ok(payload) = serde_json::from_str::<Value>(&frame.data) {
                            let _ = tx.send(KeepaliveSignal::StateEvent {
                                cursor_id: frame.id,
                                payload,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(120)) => {}
        }
    }
}

/// Runs the `/api/events/stream?clientId=<uuid>` multiplex business loop while in `AppActive`.
/// Cancelling `mode_cancel` cleanly disconnects the stream (e.g. when entering `WebActive`).
pub async fn run_multiplex_sse_loop(
    http: AppHttpClient,
    client_id: String,
    initial_cursor: Option<String>,
    tx: mpsc::UnboundedSender<MultiplexSignal>,
    mode_cancel: CancellationToken,
) {
    let mut last_cursor = initial_cursor;
    let mut backoff_ms = 150u64;
    let path = format!("/api/events/stream?clientId={client_id}");

    while !mode_cancel.is_cancelled() {
        let open_res = tokio::select! {
            biased;
            _ = mode_cancel.cancelled() => break,
            r = http.open_sse_stream(&path, last_cursor.as_deref()) => r,
        };

        let resp = match open_res {
            Ok(resp) => {
                backoff_ms = 150;
                resp
            }
            Err(_) => {
                let _ = tx.send(MultiplexSignal::Disconnected);
                tokio::select! {
                    biased;
                    _ = mode_cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
                }
                backoff_ms = (backoff_ms * 2).min(2_000);
                continue;
            }
        };

        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::default();

        loop {
            let next_chunk = tokio::select! {
                biased;
                _ = mode_cancel.cancelled() => return,
                c = stream.next() => c,
            };
            let Some(Ok(bytes)) = next_chunk else {
                let _ = tx.send(MultiplexSignal::Disconnected);
                break;
            };
            for frame in parser.feed(&bytes) {
                if let Some(ref id) = frame.id {
                    last_cursor = Some(id.clone());
                }
                match frame.event.as_str() {
                    "heartbeat" => {}
                    "reset" => {
                        if let Ok(payload) = serde_json::from_str::<Value>(&frame.data)
                            && payload.get("scope").and_then(Value::as_str) == Some("session")
                            && let Some(sid) = payload.get("sessionId").and_then(Value::as_str)
                        {
                            let _ = tx.send(MultiplexSignal::ResetSession {
                                session_id: sid.to_owned(),
                            });
                        } else {
                            let _ = tx.send(MultiplexSignal::ResetGlobal);
                        }
                    }
                    "state" => {
                        if let Ok(payload) = serde_json::from_str::<Value>(&frame.data) {
                            if payload.get("type").and_then(Value::as_str) == Some("stream_ready") {
                                let subs = payload
                                    .get("subscribedSessions")
                                    .and_then(Value::as_array)
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(Value::as_str)
                                            .map(str::to_owned)
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let _ = tx.send(MultiplexSignal::StreamReady {
                                    subscribed_sessions: subs,
                                });
                            } else {
                                let _ = tx.send(MultiplexSignal::StateEvent {
                                    cursor_id: frame.id,
                                    payload,
                                });
                            }
                        }
                    }
                    "session" => {
                        if let Ok(wrapper) = serde_json::from_str::<Value>(&frame.data)
                            && let Some(sid) = wrapper.get("sessionId").and_then(Value::as_str)
                            && let Some(ev) = wrapper.get("event")
                        {
                            let _ = tx.send(MultiplexSignal::SessionEvent {
                                cursor_id: frame.id,
                                session_id: sid.to_owned(),
                                event: ev.clone(),
                            });
                        }
                    }
                    "queue" => {
                        if let Ok(wrapper) = serde_json::from_str::<Value>(&frame.data)
                            && let Some(sid) = wrapper.get("sessionId").and_then(Value::as_str)
                            && let Some(q) = wrapper.get("queue")
                        {
                            let _ = tx.send(MultiplexSignal::QueueEvent {
                                cursor_id: frame.id,
                                session_id: sid.to_owned(),
                                queue: q.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        tokio::select! {
            biased;
            _ = mode_cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(150)) => {}
        }
    }
}

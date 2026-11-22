use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::http::AppHttpClient;

/// Maximum buffered SSE event size (`16 MiB`) to protect client memory from malformed frames.
const MAX_SSE_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// Default maximum in-memory byte budget for queued multiplex SSE signals (`8 MiB`).
pub const DEFAULT_MAX_MULTIPLEX_QUEUE_BYTES: usize = 8 * 1024 * 1024;

pub type SessionCursor = (String, u64);

/// Parses a single `<epoch>:<seq>` cursor string.
pub fn parse_single_cursor(raw: &str) -> Option<SessionCursor> {
    let trimmed = raw.trim();
    let (epoch, seq_str) = trimmed.rsplit_once(':')?;
    if epoch.is_empty() {
        return None;
    }
    let seq = seq_str.parse::<u64>().ok()?;
    Some((epoch.to_owned(), seq))
}

/// Formats a `<epoch>:<seq>` cursor string.
pub fn format_single_cursor((epoch, seq): &SessionCursor) -> String {
    format!("{epoch}:{seq}")
}

/// Parses a multiplex `Last-Event-ID` vector cursor (`global:<e0>:<s0>,<sid1>:<e1>:<s1>,...`).
pub fn parse_vector_cursor(raw: &str) -> BTreeMap<String, SessionCursor> {
    let mut out = BTreeMap::new();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut iter = trimmed.rsplitn(3, ':');
        let Some(seq_str) = iter.next() else {
            continue;
        };
        let Some(epoch) = iter.next() else {
            continue;
        };
        let Some(source) = iter.next() else {
            continue;
        };
        if let Ok(seq) = seq_str.parse::<u64>()
            && !source.is_empty()
            && !epoch.is_empty()
        {
            out.insert(source.to_owned(), (epoch.to_owned(), seq));
        }
    }
    out
}

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
    data: String,
    has_data: bool,
    discard_frame: bool,
    overflowed: bool,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<ParsedSseFrame> {
        let mut frames = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                if self.line_buf.last() == Some(&b'\r') {
                    self.line_buf.pop();
                }
                if self.discard_frame {
                    if self.line_buf.is_empty() {
                        self.discard_frame = false;
                        self.event_name.clear();
                        self.event_id = None;
                        self.data.clear();
                        self.has_data = false;
                    }
                } else {
                    let line = String::from_utf8_lossy(&self.line_buf).into_owned();
                    if let Some(frame) = self.process_line(&line) {
                        frames.push(frame);
                    }
                    if self.discard_frame {
                        frames.clear();
                    }
                }
                self.line_buf.clear();
            } else if self.discard_frame {
                // Track only enough to recognize the next blank line (LF or CRLF).
                if self.line_buf.len() < 2 {
                    self.line_buf.push(byte);
                }
            } else if self.line_buf.len() < MAX_SSE_EVENT_BYTES {
                self.line_buf.push(byte);
            } else {
                self.discard_frame = true;
                self.overflowed = true;
                frames.clear();
            }
        }
        // The consumers publish a reset/overflow before these frames. Do not
        // reinsert frames from before the gap after the reset.
        frames
    }

    fn take_overflow(&mut self) -> bool {
        std::mem::take(&mut self.overflowed)
    }

    fn process_line(&mut self, line: &str) -> Option<ParsedSseFrame> {
        if line.is_empty() {
            if !self.has_data && self.event_name.is_empty() && self.event_id.is_none() {
                return None;
            }
            let event = if self.event_name.is_empty() {
                "message".to_owned()
            } else {
                std::mem::take(&mut self.event_name)
            };
            let id = self.event_id.take();
            let data = std::mem::take(&mut self.data);
            self.has_data = false;
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
            "data" => {
                let bytes = self.data.len() + usize::from(self.has_data) + raw_val.len();
                if bytes > MAX_SSE_EVENT_BYTES {
                    self.discard_frame = true;
                    self.overflowed = true;
                } else {
                    if self.has_data {
                        self.data.push('\n');
                    }
                    self.data.push_str(raw_val);
                    self.has_data = true;
                }
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
        session_cursor: Option<SessionCursor>,
        session_id: String,
        event: Value,
        approx_bytes: usize,
    },
    QueueEvent {
        session_cursor: Option<SessionCursor>,
        session_id: String,
        queue: Value,
        approx_bytes: usize,
    },
    ResetGlobal,
    ResetSession {
        session_id: String,
    },
    QueueOverflow,
    Disconnected,
}

#[derive(Debug)]
pub struct MultiplexEnvelope {
    pub mux_generation: u64,
    pub vector_cursors: BTreeMap<String, SessionCursor>,
    pub signal: MultiplexSignal,
}

struct MultiplexQueueInner {
    queue: VecDeque<(usize, MultiplexEnvelope)>,
    queued_bytes: usize,
    max_bytes: usize,
    overflow_generation: Option<u64>,
    sender_count: usize,
    receiver_closed: bool,
}

/// Non-blocking byte-bounded sender for multiplex business SSE signals.
/// When queued frames exceed `max_bytes`, drops queued payloads and coalesces a `QueueOverflow`
/// signal so socket draining never stalls. The cap applies to the estimated size of all
/// queued envelopes, including control signals.
pub struct BoundedMultiplexSender {
    inner: Arc<Mutex<MultiplexQueueInner>>,
    notify: Arc<Notify>,
}

pub struct BoundedMultiplexReceiver {
    inner: Arc<Mutex<MultiplexQueueInner>>,
    notify: Arc<Notify>,
}

pub fn bounded_multiplex_channel(
    max_bytes: usize,
) -> (BoundedMultiplexSender, BoundedMultiplexReceiver) {
    let inner = Arc::new(Mutex::new(MultiplexQueueInner {
        queue: VecDeque::new(),
        queued_bytes: 0,
        max_bytes: max_bytes.max(256),
        overflow_generation: None,
        sender_count: 1,
        receiver_closed: false,
    }));
    let notify = Arc::new(Notify::new());
    (
        BoundedMultiplexSender {
            inner: Arc::clone(&inner),
            notify: Arc::clone(&notify),
        },
        BoundedMultiplexReceiver { inner, notify },
    )
}

impl Clone for BoundedMultiplexSender {
    fn clone(&self) -> Self {
        self.inner
            .lock()
            .expect("multiplex queue lock")
            .sender_count += 1;
        Self {
            inner: Arc::clone(&self.inner),
            notify: Arc::clone(&self.notify),
        }
    }
}

impl Drop for BoundedMultiplexSender {
    fn drop(&mut self) {
        let mut guard = self.inner.lock().expect("multiplex queue lock");
        guard.sender_count -= 1;
        if guard.sender_count == 0 {
            self.notify.notify_one();
        }
    }
}

impl Drop for BoundedMultiplexReceiver {
    fn drop(&mut self) {
        let mut guard = self.inner.lock().expect("multiplex queue lock");
        guard.receiver_closed = true;
        guard.queue.clear();
    }
}

impl BoundedMultiplexSender {
    pub fn send(&self, envelope: MultiplexEnvelope, approx_bytes: usize) {
        let cost = approx_bytes.max(64);
        let mut guard = self.inner.lock().expect("multiplex queue lock");
        if guard.receiver_closed {
            return;
        }
        if guard.queued_bytes.saturating_add(cost) > guard.max_bytes
            || matches!(envelope.signal, MultiplexSignal::QueueOverflow)
        {
            // A cancelled stream may still have frames in the queue. Report the newest
            // affected generation so the reducer can recover its active subscriptions.
            guard.overflow_generation = guard
                .queue
                .iter()
                .map(|(_, env)| env.mux_generation)
                .chain(guard.overflow_generation)
                .chain(Some(envelope.mux_generation))
                .max();
            guard.queue.clear();
            guard.queued_bytes = 0;
        } else {
            guard.queued_bytes += cost;
            guard.queue.push_back((cost, envelope));
        }
        drop(guard);
        self.notify.notify_one();
    }
}

impl BoundedMultiplexReceiver {
    pub async fn recv(&mut self) -> Option<MultiplexEnvelope> {
        loop {
            let notified = self.notify.notified();
            {
                let mut guard = self.inner.lock().expect("multiplex queue lock");
                if let Some(gen_id) = guard.overflow_generation.take() {
                    return Some(MultiplexEnvelope {
                        mux_generation: gen_id,
                        vector_cursors: BTreeMap::new(),
                        signal: MultiplexSignal::QueueOverflow,
                    });
                }
                if let Some((cost, env)) = guard.queue.pop_front() {
                    guard.queued_bytes = guard.queued_bytes.saturating_sub(cost);
                    return Some(env);
                }
                if guard.sender_count == 0 {
                    return None;
                }
            }
            notified.await;
        }
    }
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
            let frames = parser.feed(&bytes);
            if parser.take_overflow() {
                let _ = tx.send(KeepaliveSignal::Reset);
            }
            for frame in frames {
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
    mux_generation: u64,
    initial_cursor: Option<String>,
    tx: BoundedMultiplexSender,
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
                tx.send(
                    MultiplexEnvelope {
                        mux_generation,
                        vector_cursors: BTreeMap::new(),
                        signal: MultiplexSignal::Disconnected,
                    },
                    64,
                );
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
                tx.send(
                    MultiplexEnvelope {
                        mux_generation,
                        vector_cursors: BTreeMap::new(),
                        signal: MultiplexSignal::Disconnected,
                    },
                    64,
                );
                break;
            };
            let frames = parser.feed(&bytes);
            if parser.take_overflow() {
                tx.send(
                    MultiplexEnvelope {
                        mux_generation,
                        vector_cursors: BTreeMap::new(),
                        signal: MultiplexSignal::QueueOverflow,
                    },
                    0,
                );
            }
            for frame in frames {
                let vector_cursors = frame
                    .id
                    .as_deref()
                    .map(parse_vector_cursor)
                    .unwrap_or_default();
                if let Some(ref id) = frame.id {
                    last_cursor = Some(id.clone());
                }
                let raw_cost = frame
                    .data
                    .len()
                    .saturating_add(frame.id.as_ref().map_or(0, String::len))
                    .saturating_add(96);
                match frame.event.as_str() {
                    "heartbeat" => {}
                    "reset" => {
                        let signal = if let Ok(payload) = serde_json::from_str::<Value>(&frame.data)
                            && payload.get("scope").and_then(Value::as_str) == Some("session")
                            && let Some(sid) = payload.get("sessionId").and_then(Value::as_str)
                        {
                            MultiplexSignal::ResetSession {
                                session_id: sid.to_owned(),
                            }
                        } else {
                            MultiplexSignal::ResetGlobal
                        };
                        tx.send(
                            MultiplexEnvelope {
                                mux_generation,
                                vector_cursors,
                                signal,
                            },
                            raw_cost,
                        );
                    }
                    "state" => {
                        if let Ok(payload) = serde_json::from_str::<Value>(&frame.data) {
                            let signal = if payload.get("type").and_then(Value::as_str)
                                == Some("stream_ready")
                            {
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
                                MultiplexSignal::StreamReady {
                                    subscribed_sessions: subs,
                                }
                            } else {
                                MultiplexSignal::StateEvent {
                                    cursor_id: frame.id,
                                    payload,
                                }
                            };
                            tx.send(
                                MultiplexEnvelope {
                                    mux_generation,
                                    vector_cursors,
                                    signal,
                                },
                                raw_cost,
                            );
                        }
                    }
                    "session" => {
                        if let Ok(wrapper) = serde_json::from_str::<Value>(&frame.data)
                            && let Some(sid) = wrapper.get("sessionId").and_then(Value::as_str)
                            && let Some(ev) = wrapper.get("event")
                        {
                            let session_cursor = vector_cursors.get(sid).cloned();
                            tx.send(
                                MultiplexEnvelope {
                                    mux_generation,
                                    vector_cursors,
                                    signal: MultiplexSignal::SessionEvent {
                                        session_cursor,
                                        session_id: sid.to_owned(),
                                        event: ev.clone(),
                                        approx_bytes: raw_cost,
                                    },
                                },
                                raw_cost,
                            );
                        }
                    }
                    "queue" => {
                        if let Ok(wrapper) = serde_json::from_str::<Value>(&frame.data)
                            && let Some(sid) = wrapper.get("sessionId").and_then(Value::as_str)
                            && let Some(q) = wrapper.get("queue")
                        {
                            let session_cursor = vector_cursors.get(sid).cloned();
                            tx.send(
                                MultiplexEnvelope {
                                    mux_generation,
                                    vector_cursors,
                                    signal: MultiplexSignal::QueueEvent {
                                        session_cursor,
                                        session_id: sid.to_owned(),
                                        queue: q.clone(),
                                        approx_bytes: raw_cost,
                                    },
                                },
                                raw_cost,
                            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discards_over_budget_data_with_separators_and_recovers_on_next_frame() {
        let mut parser = SseParser::default();
        let prefix = format!(
            "id: old\r\ndata: {}\r\n",
            "a".repeat(MAX_SSE_EVENT_BYTES - 10)
        );
        assert!(parser.feed(prefix.as_bytes()).is_empty());
        assert!(parser.feed(b"data: 0123456789\r\n\r").is_empty());
        assert!(parser.take_overflow());

        let frames = parser.feed(b"\nid: next\r\ndata:\r\ndata: ok\r\n\r\n");
        assert_eq!(
            frames,
            [ParsedSseFrame {
                event: "message".into(),
                id: Some("next".into()),
                data: "\nok".into(),
            }],
        );
    }

    #[test]
    fn overflow_does_not_requeue_earlier_frames_from_the_same_chunk() {
        let chunk = format!(
            "id: before\ndata: ok\n\ndata: {}\n\nid: after\ndata: ok\n\n",
            "a".repeat(MAX_SSE_EVENT_BYTES)
        );
        let mut parser = SseParser::default();
        let frames = parser.feed(chunk.as_bytes());
        assert!(parser.take_overflow());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id.as_deref(), Some("after"));
    }

    #[tokio::test]
    async fn control_signals_are_bounded_and_only_the_last_sender_closes_the_queue() {
        let (sender, mut receiver) = bounded_multiplex_channel(256);
        let old_stream = sender.clone();
        for generation in [2, 2, 2, 2, 1] {
            old_stream.send(
                MultiplexEnvelope {
                    mux_generation: generation,
                    vector_cursors: BTreeMap::new(),
                    signal: MultiplexSignal::Disconnected,
                },
                64,
            );
        }
        drop(old_stream);
        let overflow = receiver.recv().await.unwrap();
        assert_eq!(overflow.mux_generation, 2);
        assert!(matches!(overflow.signal, MultiplexSignal::QueueOverflow));
        drop(sender);
        assert!(receiver.recv().await.is_none());
    }
}

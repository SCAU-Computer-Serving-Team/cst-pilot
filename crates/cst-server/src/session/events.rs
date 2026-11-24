use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::Bytes;
use cst_kit::bitlocker::credentials::redact;
use http_body::{Body, Frame};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::mode::CallerIdentity;

pub const RING_BUFFER_CAPACITY: usize = 256;
pub const DEFAULT_MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
pub const DEFAULT_MULTIPLEX_RETENTION: Duration = Duration::from_secs(60);
pub const MAX_MULTIPLEX_SUBSCRIPTIONS: usize = 32;

const HIDDEN_PASSWORD_TEXT: &str = "[BitLocker 恢复密码已隐藏]";

/// Sanitizes any JSON value before sending to a client snapshot or SSE stream:
/// - Strips `assistantMessageEvent` / `assistant_message_event` streaming delta copies.
/// - Replaces `recoveryPassword` / `recovery_password` fields with `[BitLocker 恢复密码已隐藏]`.
/// - Redacts any 48-digit BitLocker recovery password pattern in strings.
pub fn browser_json_value(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(s) => Value::String(redact(s)),
        Value::Array(items) => Value::Array(items.iter().map(browser_json_value).collect()),
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                if k == "assistantMessageEvent" || k == "assistant_message_event" {
                    continue;
                }
                if (k == "recoveryPassword" || k == "recovery_password") && !v.is_null() {
                    out.insert(k.clone(), Value::String(HIDDEN_PASSWORD_TEXT.to_owned()));
                } else {
                    out.insert(k.clone(), browser_json_value(v));
                }
            }
            Value::Object(out)
        }
    }
}

pub fn browser_json_string(value: &Value) -> String {
    let sanitized = browser_json_value(value);
    serde_json::to_string(&sanitized).unwrap_or_else(|_| "{}".to_owned())
}

#[derive(Clone)]
pub struct HistoryRecord {
    pub sequence: u64,
    pub event_name: &'static str,
    pub data_json: Arc<str>,
    pub single_frame: Bytes,
}

struct SubscriberQueueState {
    queue: VecDeque<Bytes>,
    waker: Option<Waker>,
    closed: bool,
}

/// Per-connection SSE subscriber with atomic application-layer byte accounting (`queued_bytes`)
/// and an independent outer connection cancellation token (`conn_cancel`).
pub struct SseSubscriber {
    pub id: u64,
    pub caller: CallerIdentity,
    pub conn_cancel: CancellationToken,
    pub heartbeat_stop: CancellationToken,
    pub max_queued_bytes: usize,
    pub queued_bytes: AtomicUsize,
    overflowed: AtomicBool,
    state: Mutex<SubscriberQueueState>,
    on_overflow: Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>,
}

impl SseSubscriber {
    pub fn new(
        id: u64,
        caller: CallerIdentity,
        conn_cancel: CancellationToken,
        max_queued_bytes: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            caller,
            conn_cancel,
            heartbeat_stop: CancellationToken::new(),
            max_queued_bytes,
            queued_bytes: AtomicUsize::new(0),
            overflowed: AtomicBool::new(false),
            state: Mutex::new(SubscriberQueueState {
                queue: VecDeque::new(),
                waker: None,
                closed: false,
            }),
            on_overflow: Mutex::new(None),
        })
    }

    pub fn set_on_overflow(&self, cb: impl FnOnce() + Send + Sync + 'static) {
        *self.on_overflow.lock().expect("overflow callback") = Some(Box::new(cb));
    }

    /// Enqueues a serialized SSE frame onto this subscriber.
    /// If `queued_bytes + frame.len() > max_queued_bytes`, synchronously clears the queue,
    /// stops the heartbeat timer, removes the subscriber, and triggers `conn_cancel.cancel()`.
    pub fn enqueue(&self, frame: Bytes) -> bool {
        let accepted = self.enqueue_deferred(frame);
        self.finish_overflow();
        accepted
    }

    // Replay holds the source lock to preserve event order. Defer callbacks that
    // acquire that lock, while still cancelling the connection immediately.
    fn enqueue_deferred(&self, frame: Bytes) -> bool {
        let len = frame.len();
        let mut guard = self.state.lock().expect("subscriber queue");
        if guard.closed {
            return false;
        }
        let current = self.queued_bytes.load(Ordering::SeqCst);
        if current.saturating_add(len) > self.max_queued_bytes {
            guard.queue.clear();
            guard.closed = true;
            self.queued_bytes.store(0, Ordering::SeqCst);
            self.overflowed.store(true, Ordering::SeqCst);
            let waker = guard.waker.take();
            drop(guard);

            self.heartbeat_stop.cancel();
            self.conn_cancel.cancel();
            if let Some(w) = waker {
                w.wake();
            }
            return false;
        }

        self.queued_bytes.fetch_add(len, Ordering::SeqCst);
        guard.queue.push_back(frame);
        let waker = guard.waker.take();
        drop(guard);
        if let Some(w) = waker {
            w.wake();
        }
        true
    }

    fn finish_overflow(&self) {
        if self.is_overflowed() {
            let callback = self.on_overflow.lock().expect("overflow callback").take();
            if let Some(callback) = callback {
                callback();
            }
        }
    }

    pub fn close(&self) {
        self.heartbeat_stop.cancel();
        let waker = {
            let mut guard = self.state.lock().expect("subscriber queue");
            if guard.closed {
                return;
            }
            guard.closed = true;
            guard.queue.clear();
            self.queued_bytes.store(0, Ordering::SeqCst);
            guard.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    pub fn close_and_cancel_connection(&self) {
        self.close();
        self.conn_cancel.cancel();
    }

    pub fn is_overflowed(&self) -> bool {
        self.overflowed.load(Ordering::SeqCst)
    }

    pub fn current_queued_bytes(&self) -> usize {
        self.queued_bytes.load(Ordering::SeqCst)
    }
}

/// HTTP response body stream backed by `Arc<SseSubscriber>`.
pub struct SseStreamBody {
    subscriber: Arc<SseSubscriber>,
    on_drop: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl SseStreamBody {
    pub fn new(
        subscriber: Arc<SseSubscriber>,
        on_drop: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        Self {
            subscriber,
            on_drop: Some(Box::new(on_drop)),
        }
    }

    pub fn subscriber(&self) -> &Arc<SseSubscriber> {
        &self.subscriber
    }
}

impl Body for SseStreamBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let mut guard = this.subscriber.state.lock().expect("subscriber queue");
        if let Some(frame) = guard.queue.pop_front() {
            this.subscriber
                .queued_bytes
                .fetch_sub(frame.len(), Ordering::SeqCst);
            return Poll::Ready(Some(Ok(Frame::data(frame))));
        }
        if guard.closed {
            return Poll::Ready(None);
        }
        guard.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for SseStreamBody {
    fn drop(&mut self) {
        self.subscriber.close();
        if let Some(cleanup) = self.on_drop.take() {
            cleanup();
        }
    }
}

type MultiplexForwarder = Arc<dyn Fn(&SessionEvents, u64, &'static str, &str) + Send + Sync>;

struct SessionEventsInner {
    sequence: u64,
    history: VecDeque<HistoryRecord>,
    subscribers: HashMap<u64, Arc<SseSubscriber>>,
    multiplex_forwarders: HashMap<u64, MultiplexForwarder>,
    multiplex_ref_count: usize,
}

/// Bounded 256-entry `Bytes` ring buffer for either the global event bus or a single session.
#[derive(Clone)]
pub struct SessionEvents {
    epoch: String,
    next_id: Arc<AtomicU64>,
    max_queued_bytes: usize,
    heartbeat_interval: Duration,
    inner: Arc<Mutex<SessionEventsInner>>,
}

impl Default for SessionEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionEvents {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_QUEUED_BYTES, DEFAULT_HEARTBEAT_INTERVAL)
    }

    pub fn with_limits(max_queued_bytes: usize, heartbeat_interval: Duration) -> Self {
        Self {
            epoch: Uuid::new_v4().to_string(),
            next_id: Arc::new(AtomicU64::new(1)),
            max_queued_bytes,
            heartbeat_interval,
            inner: Arc::new(Mutex::new(SessionEventsInner {
                sequence: 0,
                history: VecDeque::with_capacity(RING_BUFFER_CAPACITY),
                subscribers: HashMap::new(),
                multiplex_forwarders: HashMap::new(),
                multiplex_ref_count: 0,
            })),
        }
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn current_sequence(&self) -> u64 {
        self.inner.lock().expect("events lock").sequence
    }

    /// Total active subscribers (single-stream SSE connections + multiplex slot references).
    /// Used by `SessionPool` to pin sessions with active SSE listeners against LRU eviction.
    pub fn active_subscriber_count(&self) -> usize {
        let guard = self.inner.lock().expect("events lock");
        guard.subscribers.len() + guard.multiplex_ref_count
    }

    pub fn increment_multiplex_ref(&self) {
        self.inner.lock().expect("events lock").multiplex_ref_count += 1;
    }

    pub fn decrement_multiplex_ref(&self) {
        let mut guard = self.inner.lock().expect("events lock");
        guard.multiplex_ref_count = guard.multiplex_ref_count.saturating_sub(1);
    }

    pub fn register_multiplex_forwarder(&self, forwarder: MultiplexForwarder) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.inner
            .lock()
            .expect("events lock")
            .multiplex_forwarders
            .insert(id, forwarder);
        id
    }

    pub fn unregister_multiplex_forwarder(&self, id: u64) {
        self.inner
            .lock()
            .expect("events lock")
            .multiplex_forwarders
            .remove(&id);
    }

    /// Publishes an event (`"state"`, `"session"`, or `"queue"`), sanitizing its JSON payload once
    /// and storing the immutable `Bytes` frame in the 256-entry ring buffer.
    pub fn publish(&self, event_name: &'static str, value: &Value) -> u64 {
        let data_json: Arc<str> = browser_json_string(value).into();
        let (seq, subs, forwarders) = {
            let mut guard = self.inner.lock().expect("events lock");
            guard.sequence += 1;
            let seq = guard.sequence;
            let single_frame = Bytes::from(format!(
                "id: {}:{seq}\nevent: {event_name}\ndata: {data_json}\n\n",
                self.epoch
            ));
            if guard.history.len() >= RING_BUFFER_CAPACITY {
                guard.history.pop_front();
            }
            guard.history.push_back(HistoryRecord {
                sequence: seq,
                event_name,
                data_json: data_json.clone(),
                single_frame: single_frame.clone(),
            });
            // Enqueue single-stream frames inside the events lock (`events -> subscriber`) so
            // concurrent publishers cannot deliver out-of-order sequences to single-stream readers.
            let subs: Vec<Arc<SseSubscriber>> = guard.subscribers.values().cloned().collect();
            for sub in &subs {
                let _ = sub.enqueue_deferred(single_frame.clone());
            }
            let fwds: Vec<MultiplexForwarder> =
                guard.multiplex_forwarders.values().cloned().collect();
            (seq, subs, fwds)
        };

        for sub in subs {
            sub.finish_overflow();
        }
        for fwd in forwarders {
            fwd(self, seq, event_name, &data_json);
        }
        seq
    }

    fn replay_range(
        &self,
        epoch: &str,
        from_seq_inclusive: u64,
        to_seq_inclusive: u64,
    ) -> Vec<HistoryRecord> {
        if self.epoch != epoch || from_seq_inclusive > to_seq_inclusive {
            return Vec::new();
        }
        let guard = self.inner.lock().expect("events lock");
        guard
            .history
            .iter()
            .filter(|r| r.sequence >= from_seq_inclusive && r.sequence <= to_seq_inclusive)
            .cloned()
            .collect()
    }

    /// Subscribes a single-stream SSE client (`/api/events`, `/api/app/events`, or `/api/sessions/:id/events`).
    pub fn subscribe_single(
        &self,
        caller: CallerIdentity,
        last_event_id: Option<&str>,
        conn_cancel: CancellationToken,
        extra_cleanup: Option<Box<dyn FnOnce() + Send + Sync>>,
    ) -> SseStreamBody {
        let sub_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let subscriber = SseSubscriber::new(sub_id, caller, conn_cancel, self.max_queued_bytes);

        let events_for_overflow = self.clone();
        subscriber.set_on_overflow(move || {
            events_for_overflow
                .inner
                .lock()
                .expect("events lock")
                .subscribers
                .remove(&sub_id);
        });

        {
            let mut guard = self.inner.lock().expect("events lock");
            if let Some(last) = last_event_id {
                match Self::replay_records_locked(&self.epoch, &guard, last) {
                    Some(records) => {
                        for record in records {
                            if !subscriber.enqueue_deferred(record.single_frame) {
                                break;
                            }
                        }
                    }
                    None => {
                        let _ = subscriber
                            .enqueue_deferred(Bytes::from_static(b"event: reset\ndata: {}\n\n"));
                    }
                }
            } else {
                let _ = subscriber.enqueue_deferred(Bytes::from_static(b": connected\n\n"));
            }
            if !subscriber.is_overflowed() {
                guard.subscribers.insert(sub_id, subscriber.clone());
            }
        }

        subscriber.finish_overflow();
        spawn_heartbeat_task(&subscriber, self.heartbeat_interval);

        let events_for_drop = self.clone();
        SseStreamBody::new(subscriber, move || {
            events_for_drop
                .inner
                .lock()
                .expect("events lock")
                .subscribers
                .remove(&sub_id);
            if let Some(extra) = extra_cleanup {
                extra();
            }
        })
    }

    fn replay_records_locked(
        current_epoch: &str,
        guard: &SessionEventsInner,
        last_event_id: &str,
    ) -> Option<Vec<HistoryRecord>> {
        let (epoch, seq_str) = last_event_id.split_once(':')?;
        let seq: u64 = seq_str.parse().ok()?;
        let oldest = guard
            .history
            .front()
            .map_or(guard.sequence + 1, |r| r.sequence);
        if epoch != current_epoch || seq < oldest.saturating_sub(1) || seq > guard.sequence {
            return None;
        }
        Some(
            guard
                .history
                .iter()
                .filter(|r| r.sequence > seq)
                .cloned()
                .collect(),
        )
    }

    fn replay_for_multiplex(
        &self,
        last_cursor: Option<(&str, u64)>,
    ) -> (u64, Option<Vec<HistoryRecord>>) {
        let guard = self.inner.lock().expect("events lock");
        let records = last_cursor.and_then(|(epoch, seq)| {
            let oldest = guard
                .history
                .front()
                .map_or(guard.sequence + 1, |r| r.sequence);
            if epoch != self.epoch || seq < oldest.saturating_sub(1) || seq > guard.sequence {
                return None;
            }
            Some(
                guard
                    .history
                    .iter()
                    .filter(|r| r.sequence > seq)
                    .cloned()
                    .collect(),
            )
        });
        (guard.sequence, records)
    }

    /// Force-closes and cancels all SSE connections established with a Web identity.
    pub fn revoke_web_subscribers(&self) {
        let web_subs: Vec<Arc<SseSubscriber>> = {
            let mut guard = self.inner.lock().expect("events lock");
            let ids: Vec<u64> = guard
                .subscribers
                .iter()
                .filter(|(_, sub)| matches!(sub.caller, CallerIdentity::Web { .. }))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| guard.subscribers.remove(&id))
                .collect()
        };
        for sub in web_subs {
            sub.close_and_cancel_connection();
        }
    }

    pub fn close_all(&self) {
        let subs: Vec<Arc<SseSubscriber>> = {
            let mut guard = self.inner.lock().expect("events lock");
            guard.subscribers.drain().map(|(_, s)| s).collect()
        };
        for sub in subs {
            sub.close_and_cancel_connection();
        }
    }
}

fn spawn_heartbeat_task(subscriber: &Arc<SseSubscriber>, interval: Duration) {
    let weak = Arc::downgrade(subscriber);
    let stop = subscriber.heartbeat_stop.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    let Some(sub) = weak.upgrade() else {
                        break;
                    };
                    if !sub.enqueue(Bytes::from_static(b"event: heartbeat\ndata: {}\n\n")) {
                        break;
                    }
                }
            }
        }
    });
}

// =============================================================================
// Multi-Session Multiplex SSE Hub (`/api/events/stream`)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SlotOwnerKey {
    caller_tag: String,
    client_id: String,
}

impl SlotOwnerKey {
    fn new(caller: CallerIdentity, client_id: &str) -> Self {
        let caller_tag = match caller {
            CallerIdentity::App => "app".to_owned(),
            CallerIdentity::Web { cookie_epoch } => format!("web:{cookie_epoch}"),
        };
        Self {
            caller_tag,
            client_id: client_id.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MultiplexSubscriptionUpdate {
    pub sessions: Vec<String>,
    #[serde(default)]
    pub last_event_ids: BTreeMap<String, String>,
}

struct SessionAttachment {
    events: SessionEvents,
    forwarder_id: u64,
}

struct MultiplexSlotState {
    caller: CallerIdentity,
    subscribed_sessions: BTreeSet<String>,
    attachments: HashMap<String, SessionAttachment>,
    emitted_cursors: BTreeMap<String, (String, u64)>,
    active_subscriber: Option<Arc<SseSubscriber>>,
    expiry_task: Option<JoinHandle<()>>,
}

/// Resolves a session's `SessionEvents` by session ID when attaching multiplex subscriptions.
pub type SessionEventsResolver = Arc<dyn Fn(&str) -> Option<SessionEvents> + Send + Sync>;

#[derive(Clone)]
pub struct MultiplexHub {
    global_events: SessionEvents,
    next_id: Arc<AtomicU64>,
    max_queued_bytes: usize,
    heartbeat_interval: Duration,
    retention_timeout: Duration,
    slots: Arc<Mutex<HashMap<SlotOwnerKey, Arc<Mutex<MultiplexSlotState>>>>>,
    global_forwarder_id: Arc<AtomicU64>,
}

impl MultiplexHub {
    pub fn new(global_events: SessionEvents) -> Self {
        Self::with_limits(
            global_events,
            DEFAULT_MAX_QUEUED_BYTES,
            DEFAULT_HEARTBEAT_INTERVAL,
            DEFAULT_MULTIPLEX_RETENTION,
        )
    }

    pub fn with_limits(
        global_events: SessionEvents,
        max_queued_bytes: usize,
        heartbeat_interval: Duration,
        retention_timeout: Duration,
    ) -> Self {
        let hub = Self {
            global_events: global_events.clone(),
            next_id: Arc::new(AtomicU64::new(1)),
            max_queued_bytes,
            heartbeat_interval,
            retention_timeout,
            slots: Arc::new(Mutex::new(HashMap::new())),
            global_forwarder_id: Arc::new(AtomicU64::new(0)),
        };

        let weak_slots = Arc::downgrade(&hub.slots);
        let fwd_id = global_events.register_multiplex_forwarder(Arc::new(
            move |events, seq, event_name, data_json| {
                let Some(slots_arc) = weak_slots.upgrade() else {
                    return;
                };
                let slot_list: Vec<Arc<Mutex<MultiplexSlotState>>> = {
                    let map = slots_arc.lock().expect("multiplex slots");
                    map.values().cloned().collect()
                };
                for slot in slot_list {
                    Self::emit_to_slot(&slot, "global", events, seq, event_name, data_json);
                }
            },
        ));
        hub.global_forwarder_id.store(fwd_id, Ordering::SeqCst);
        hub
    }

    /// Formats the deterministic full vector cursor `global:<e0>:<s0>,<sid1>:<e1>:<s1>,...`.
    fn format_vector_cursor(cursors: &BTreeMap<String, (String, u64)>) -> String {
        let mut parts = Vec::with_capacity(cursors.len());
        if let Some((epoch, seq)) = cursors.get("global") {
            parts.push(format!("global:{epoch}:{seq}"));
        }
        for (source, (epoch, seq)) in cursors {
            if source != "global" {
                parts.push(format!("{source}:{epoch}:{seq}"));
            }
        }
        parts.join(",")
    }

    /// Parses a `Last-Event-ID` full vector cursor (`global:<e0>:<s0>,<sid1>:<e1>:<s1>`).
    pub fn parse_vector_cursor(raw: &str) -> BTreeMap<String, (String, u64)> {
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

    fn emit_to_slot(
        slot: &Arc<Mutex<MultiplexSlotState>>,
        source_key: &str,
        source_events: &SessionEvents,
        seq: u64,
        event_name: &'static str,
        raw_data_json: &str,
    ) {
        let epoch = source_events.epoch();
        let sub = {
            let mut guard = slot.lock().expect("slot lock");
            if source_key != "global"
                && !guard
                    .attachments
                    .get(source_key)
                    .is_some_and(|attachment| attachment.events.epoch() == epoch)
            {
                return;
            }
            let Some(sub) = guard.active_subscriber.clone() else {
                return;
            };
            // Enforce strict per-source monotonic delivery: drop stale/duplicate frames arriving
            // out of order from concurrent publishers or replay, and backfill any gap from the
            // source ring buffer before emitting `seq` (`slot -> events -> subscriber`).
            if let Some((prev_epoch, prev_seq)) = guard.emitted_cursors.get(source_key).cloned()
                && prev_epoch == epoch
            {
                if seq <= prev_seq {
                    return;
                }
                if seq > prev_seq + 1 {
                    let missing = source_events.replay_range(epoch, prev_seq + 1, seq - 1);
                    if missing.len() as u64 != seq - prev_seq - 1 {
                        // The gap has already fallen out of the ring. Recover from a snapshot
                        // instead of acknowledging a partial replay past undelivered events.
                        Self::enqueue_reset(&sub, source_key);
                    } else {
                        for rec in missing {
                            Self::enqueue_record(
                                &mut guard,
                                &sub,
                                source_key,
                                epoch,
                                rec.sequence,
                                rec.event_name,
                                &rec.data_json,
                            );
                        }
                    }
                }
            }
            Self::enqueue_record(
                &mut guard,
                &sub,
                source_key,
                epoch,
                seq,
                event_name,
                raw_data_json,
            );
            sub
        };
        sub.finish_overflow();
    }

    fn enqueue_record(
        slot: &mut MultiplexSlotState,
        sub: &SseSubscriber,
        source: &str,
        epoch: &str,
        seq: u64,
        event_name: &'static str,
        raw_data_json: &str,
    ) {
        slot.emitted_cursors
            .insert(source.to_owned(), (epoch.to_owned(), seq));
        let id = Self::format_vector_cursor(&slot.emitted_cursors);
        let data = Self::wrap_multiplex_payload(source, event_name, raw_data_json);
        sub.enqueue_deferred(Bytes::from(format!(
            "id: {id}\nevent: {event_name}\ndata: {data}\n\n"
        )));
    }

    fn enqueue_reset(sub: &SseSubscriber, source: &str) {
        let data = if source == "global" {
            json!({"scope": "global"})
        } else {
            json!({"scope": "session", "sessionId": source})
        };
        sub.enqueue_deferred(Bytes::from(format!("event: reset\ndata: {data}\n\n")));
    }

    fn wrap_multiplex_payload(
        source_key: &str,
        event_name: &'static str,
        raw_data_json: &str,
    ) -> String {
        if source_key == "global" {
            return raw_data_json.to_owned();
        }
        let sid_json = serde_json::to_string(source_key).unwrap_or_else(|_| "\"\"".to_owned());
        match event_name {
            "session" => format!("{{\"sessionId\":{sid_json},\"event\":{raw_data_json}}}"),
            "queue" => format!("{{\"sessionId\":{sid_json},\"queue\":{raw_data_json}}}"),
            _ => raw_data_json.to_owned(),
        }
    }

    /// Connects or reconnects `GET /api/events/stream?clientId=<uuid>`.
    /// Replaces any prior connection on the same slot, inherits the current subscribed sessions set,
    /// performs vector replay or targeted `reset`, and emits `stream_ready`.
    pub fn connect(
        &self,
        caller: CallerIdentity,
        client_id: &str,
        last_event_id: Option<&str>,
        conn_cancel: CancellationToken,
    ) -> SseStreamBody {
        let key = SlotOwnerKey::new(caller, client_id);
        let mut map = self.slots.lock().expect("multiplex slots");
        let slot = map
            .entry(key.clone())
            .or_insert_with(|| {
                let mut cursors = BTreeMap::new();
                cursors.insert(
                    "global".to_owned(),
                    (
                        self.global_events.epoch().to_owned(),
                        self.global_events.current_sequence(),
                    ),
                );
                Arc::new(Mutex::new(MultiplexSlotState {
                    caller,
                    subscribed_sessions: BTreeSet::new(),
                    attachments: HashMap::new(),
                    emitted_cursors: cursors,
                    active_subscriber: None,
                    expiry_task: None,
                }))
            })
            .clone();

        let sub_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let subscriber = SseSubscriber::new(sub_id, caller, conn_cancel, self.max_queued_bytes);

        let hub_for_overflow = self.clone();
        let key_for_overflow = key.clone();
        subscriber.set_on_overflow(move || {
            hub_for_overflow.on_connection_dropped(&key_for_overflow, sub_id);
        });

        let old_subscriber = {
            let mut guard = slot.lock().expect("slot lock");
            drop(map);
            if let Some(timer) = guard.expiry_task.take() {
                timer.abort();
            }
            let previous = guard.active_subscriber.replace(subscriber.clone());

            // Seed every source from the client vector before emitting any replay frame.
            // The previous connection may have queued events the client never received.
            if let Some(raw_vector) = last_event_id {
                let parsed = Self::parse_vector_cursor(raw_vector);
                guard.emitted_cursors.clear();
                let sources = std::iter::once(("global".to_owned(), self.global_events.clone()))
                    .chain(guard.subscribed_sessions.iter().filter_map(|sid| {
                        guard
                            .attachments
                            .get(sid)
                            .map(|a| (sid.clone(), a.events.clone()))
                    }))
                    .collect::<Vec<_>>();
                let mut replays = Vec::new();
                for (source, events) in sources {
                    let epoch = events.epoch().to_owned();
                    let cursor = parsed.get(&source).map(|(e, s)| (e.as_str(), *s));
                    let (current_seq, records) = events.replay_for_multiplex(cursor);
                    if let (Some((_, from_seq)), Some(records)) = (cursor, records) {
                        guard
                            .emitted_cursors
                            .insert(source.clone(), (epoch.clone(), from_seq));
                        replays.push((source, epoch, records));
                    } else {
                        Self::enqueue_reset(&subscriber, &source);
                        guard.emitted_cursors.insert(source, (epoch, current_seq));
                    }
                }
                for (source, epoch, records) in replays {
                    for rec in records {
                        Self::enqueue_record(
                            &mut guard,
                            &subscriber,
                            &source,
                            &epoch,
                            rec.sequence,
                            rec.event_name,
                            &rec.data_json,
                        );
                    }
                }
            } else {
                guard.emitted_cursors.insert(
                    "global".to_owned(),
                    (
                        self.global_events.epoch().to_owned(),
                        self.global_events.current_sequence(),
                    ),
                );
            }

            // Always emit the `stream_ready` handshake frame with the current subscribedSessions
            let subscribed_list: Vec<String> = guard.subscribed_sessions.iter().cloned().collect();
            let ready_data = json!({
                "type": "stream_ready",
                "subscribedSessions": subscribed_list,
            })
            .to_string();
            let cid = Self::format_vector_cursor(&guard.emitted_cursors);
            let _ = subscriber.enqueue_deferred(Bytes::from(format!(
                "id: {cid}\nevent: state\ndata: {ready_data}\n\n"
            )));

            previous
        };

        subscriber.finish_overflow();

        // Cancel the replaced connection outside the lock
        if let Some(prev) = old_subscriber {
            prev.close_and_cancel_connection();
        }

        spawn_heartbeat_task(&subscriber, self.heartbeat_interval);

        let hub_for_drop = self.clone();
        SseStreamBody::new(subscriber, move || {
            hub_for_drop.on_connection_dropped(&key, sub_id);
        })
    }

    fn on_connection_dropped(&self, key: &SlotOwnerKey, sub_id: u64) {
        let Some(slot) = self
            .slots
            .lock()
            .expect("multiplex slots")
            .get(key)
            .cloned()
        else {
            return;
        };
        let mut guard = slot.lock().expect("slot lock");
        if !guard
            .active_subscriber
            .as_ref()
            .is_some_and(|s| s.id == sub_id)
        {
            return;
        }
        guard.active_subscriber = None;
        if let Some(timer) = guard.expiry_task.take() {
            timer.abort();
        }
        self.schedule_expiry(key, &mut guard);
    }

    fn schedule_expiry(&self, key: &SlotOwnerKey, slot: &mut MultiplexSlotState) {
        let timeout = self.retention_timeout;
        let hub = self.clone();
        let key = key.clone();
        slot.expiry_task = Some(tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            hub.expire_disconnected_slot(&key, tokio::task::id());
        }));
    }

    fn expire_disconnected_slot(&self, key: &SlotOwnerKey, timer_id: tokio::task::Id) {
        let removed = {
            let mut map = self.slots.lock().expect("multiplex slots");
            let Some(slot) = map.get(key) else {
                return;
            };
            let expired = {
                let guard = slot.lock().expect("slot lock");
                guard.active_subscriber.is_none()
                    && guard
                        .expiry_task
                        .as_ref()
                        .is_some_and(|task| task.id() == timer_id)
            };
            if expired { map.remove(key) } else { None }
        };
        if let Some(slot) = removed {
            Self::destroy_slot(&slot);
        }
    }

    /// Atomically updates the subscribed session set for `(caller, client_id)`.
    /// Increments/decrements session LRU subscriber counts and replays or sends targeted `reset`
    /// for newly added sessions.
    pub fn update_subscriptions(
        &self,
        caller: CallerIdentity,
        client_id: &str,
        update: MultiplexSubscriptionUpdate,
        resolve_session: &SessionEventsResolver,
    ) -> Result<Vec<String>, String> {
        if update.sessions.len() > MAX_MULTIPLEX_SUBSCRIPTIONS {
            return Err(format!(
                "单条聚合流最多同时订阅 {MAX_MULTIPLEX_SUBSCRIPTIONS} 个会话"
            ));
        }
        let mut desired = BTreeSet::new();
        for sid in update.sessions {
            if sid.is_empty()
                || sid.len() > 128
                || !sid
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err("订阅的会话 ID 格式无效".to_owned());
            }
            desired.insert(sid);
        }

        let key = SlotOwnerKey::new(caller, client_id);
        let mut map = self.slots.lock().expect("multiplex slots");
        let slot = map
            .entry(key.clone())
            .or_insert_with(|| {
                let mut cursors = BTreeMap::new();
                cursors.insert(
                    "global".to_owned(),
                    (
                        self.global_events.epoch().to_owned(),
                        self.global_events.current_sequence(),
                    ),
                );
                Arc::new(Mutex::new(MultiplexSlotState {
                    caller,
                    subscribed_sessions: BTreeSet::new(),
                    attachments: HashMap::new(),
                    emitted_cursors: cursors,
                    active_subscriber: None,
                    expiry_task: None,
                }))
            })
            .clone();

        let mut guard = slot.lock().expect("slot lock");
        drop(map);
        // Even a rejected first subscription must not keep a disconnected slot forever.
        if guard.active_subscriber.is_none() && guard.expiry_task.is_none() {
            self.schedule_expiry(&key, &mut guard);
        }
        let removed: Vec<String> = guard
            .subscribed_sessions
            .difference(&desired)
            .cloned()
            .collect();
        let added: Vec<(String, SessionEvents)> = desired
            .difference(&guard.subscribed_sessions)
            .map(|id| {
                resolve_session(id)
                    .map(|events| (id.clone(), events))
                    .ok_or_else(|| "订阅的会话不存在或尚未加载".to_owned())
            })
            .collect::<Result<_, _>>()?;

        for sid in &removed {
            guard.subscribed_sessions.remove(sid);
            guard.emitted_cursors.remove(sid);
            if let Some(att) = guard.attachments.remove(sid) {
                att.events.unregister_multiplex_forwarder(att.forwarder_id);
                att.events.decrement_multiplex_ref();
            }
        }

        for (sid, sess_events) in added {
            sess_events.increment_multiplex_ref();
            let weak_slot = Arc::downgrade(&slot);
            let sid_for_fwd = sid.clone();
            let fwd_id = sess_events.register_multiplex_forwarder(Arc::new(
                move |events, seq, event_name, data_json| {
                    if let Some(slot_arc) = weak_slot.upgrade() {
                        Self::emit_to_slot(
                            &slot_arc,
                            &sid_for_fwd,
                            events,
                            seq,
                            event_name,
                            data_json,
                        );
                    }
                },
            ));

            guard.subscribed_sessions.insert(sid.clone());
            guard.attachments.insert(
                sid.clone(),
                SessionAttachment {
                    events: sess_events.clone(),
                    forwarder_id: fwd_id,
                },
            );

            if let Some(sub) = guard.active_subscriber.clone() {
                if let Some(last_id) = update.last_event_ids.get(&sid) {
                    let cursor = last_id
                        .split_once(':')
                        .and_then(|(e, s)| s.parse::<u64>().ok().map(|seq| (e, seq)));
                    let (current_seq, records) = sess_events.replay_for_multiplex(cursor);
                    match records {
                        Some(records) => {
                            if let Some((e, s)) = cursor {
                                guard.emitted_cursors.insert(sid.clone(), (e.to_owned(), s));
                            }
                            for rec in records {
                                Self::enqueue_record(
                                    &mut guard,
                                    &sub,
                                    &sid,
                                    sess_events.epoch(),
                                    rec.sequence,
                                    rec.event_name,
                                    &rec.data_json,
                                );
                            }
                        }
                        None => {
                            guard
                                .emitted_cursors
                                .insert(sid.clone(), (sess_events.epoch().to_owned(), current_seq));
                            Self::enqueue_reset(&sub, &sid);
                        }
                    }
                } else {
                    guard.emitted_cursors.insert(
                        sid.clone(),
                        (
                            sess_events.epoch().to_owned(),
                            sess_events.current_sequence(),
                        ),
                    );
                }
            } else {
                guard.emitted_cursors.insert(
                    sid.clone(),
                    (
                        sess_events.epoch().to_owned(),
                        sess_events.current_sequence(),
                    ),
                );
            }
        }

        let subscribed = guard.subscribed_sessions.iter().cloned().collect();
        let subscriber = guard.active_subscriber.clone();
        if subscriber.is_none() {
            if let Some(timer) = guard.expiry_task.take() {
                timer.abort();
            }
            self.schedule_expiry(&key, &mut guard);
        }
        drop(guard);
        if let Some(subscriber) = subscriber {
            subscriber.finish_overflow();
        }
        Ok(subscribed)
    }

    pub fn revoke_web_slots(&self) {
        let removed: Vec<Arc<Mutex<MultiplexSlotState>>> = {
            let mut map = self.slots.lock().expect("multiplex slots");
            let keys: Vec<SlotOwnerKey> = map
                .iter()
                .filter(|(_, slot)| {
                    matches!(
                        slot.lock().expect("slot lock").caller,
                        CallerIdentity::Web { .. }
                    )
                })
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter().filter_map(|k| map.remove(&k)).collect()
        };
        for slot in removed {
            Self::destroy_slot(&slot);
        }
    }

    pub fn close_all(&self) {
        let removed: Vec<Arc<Mutex<MultiplexSlotState>>> = {
            let mut map = self.slots.lock().expect("multiplex slots");
            map.drain().map(|(_, s)| s).collect()
        };
        for slot in removed {
            Self::destroy_slot(&slot);
        }
    }

    fn destroy_slot(slot: &Arc<Mutex<MultiplexSlotState>>) {
        let (sub, attachments, timer) = {
            let mut guard = slot.lock().expect("slot lock");
            guard.subscribed_sessions.clear();
            guard.emitted_cursors.clear();
            let sub = guard.active_subscriber.take();
            let attachments: Vec<SessionAttachment> =
                guard.attachments.drain().map(|(_, a)| a).collect();
            let timer = guard.expiry_task.take();
            (sub, attachments, timer)
        };
        if let Some(t) = timer {
            t.abort();
        }
        for att in attachments {
            att.events.unregister_multiplex_forwarder(att.forwarder_id);
            att.events.decrement_multiplex_ref();
        }
        if let Some(s) = sub {
            s.close_and_cancel_connection();
        }
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    async fn frame_text(body: &mut SseStreamBody) -> String {
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("SSE frame timed out")
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        String::from_utf8(frame.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn reconnect_never_advertises_unreplayed_cursors_from_previous_connection() {
        let global = SessionEvents::new();
        let session = SessionEvents::new();
        let hub = MultiplexHub::new(global.clone());
        let caller = CallerIdentity::App;
        let source = session.clone();
        let resolver: SessionEventsResolver = Arc::new(move |_| Some(source.clone()));
        hub.update_subscriptions(
            caller,
            "client",
            MultiplexSubscriptionUpdate {
                sessions: vec!["session-a".into()],
                ..Default::default()
            },
            &resolver,
        )
        .unwrap();
        let mut first = hub.connect(caller, "client", None, CancellationToken::new());
        let initial = frame_text(&mut first).await;
        let last = initial
            .lines()
            .next()
            .unwrap()
            .strip_prefix("id: ")
            .unwrap();
        global.publish("state", &json!({"type": "settings_changed"}));
        session.publish("session", &json!({"type": "agent_end"}));
        drop(first); // Neither frame reached the client.

        let mut second = hub.connect(caller, "client", Some(last), CancellationToken::new());
        let replayed = frame_text(&mut second).await;
        let last = replayed
            .lines()
            .next()
            .unwrap()
            .strip_prefix("id: ")
            .unwrap();
        let cursors = MultiplexHub::parse_vector_cursor(last);
        assert_eq!(cursors["global"].1, 1);
        assert_eq!(cursors["session-a"].1, 0);
        drop(second); // Drop again before receiving the session frame.

        let mut third = hub.connect(caller, "client", Some(last), CancellationToken::new());
        let replayed = frame_text(&mut third).await;
        assert!(replayed.contains("\"sessionId\":\"session-a\""));
        assert!(replayed.contains("\"type\":\"agent_end\""));
    }

    #[tokio::test]
    async fn live_gap_that_has_left_the_ring_sends_reset_before_advancing() {
        let source = SessionEvents::new();
        let hub = MultiplexHub::new(source.clone());
        let caller = CallerIdentity::App;
        let mut body = hub.connect(caller, "test", None, CancellationToken::new());
        assert!(frame_text(&mut body).await.contains("stream_ready"));
        source.unregister_multiplex_forwarder(hub.global_forwarder_id.load(Ordering::SeqCst));
        let mut latest = 0;
        for _ in 0..=RING_BUFFER_CAPACITY {
            latest = source.publish("state", &json!({}));
        }
        let slot = hub.slots.lock().unwrap()[&SlotOwnerKey::new(caller, "test")].clone();
        MultiplexHub::emit_to_slot(&slot, "global", &source, latest, "state", "{}");
        assert_eq!(
            frame_text(&mut body).await,
            "event: reset\ndata: {\"scope\":\"global\"}\n\n"
        );
        let live = frame_text(&mut body).await;
        assert!(live.starts_with(&format!("id: global:{}:{latest}\n", source.epoch())));
    }
}

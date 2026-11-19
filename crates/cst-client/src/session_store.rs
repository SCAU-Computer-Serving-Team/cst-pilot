use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};

pub const DEFAULT_MAX_CACHED_SESSIONS: usize = 12;
pub const DEFAULT_MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SessionViewSnapshot {
    pub id: String,
    pub revision: u64,
    pub running: bool,
    pub messages: Arc<Vec<Value>>,
    pub entries: Arc<Vec<Value>>,
    pub context_usage: Option<Value>,
    pub queue: Option<Value>,
    pub ui: Option<Value>,
    pub bitlocker: Option<Value>,
    pub streaming_assistant: Option<Value>,
    pub active_tools: Arc<BTreeMap<String, Value>>,
    pub active_turn_started_ms: Option<u64>,
    pub estimated_bytes: usize,
}

impl SessionViewSnapshot {
    pub fn from_http_payload(payload: Value, revision: u64) -> Option<Self> {
        let obj = payload.as_object()?;
        let id = obj.get("id")?.as_str()?.to_owned();
        let running = obj.get("running").and_then(Value::as_bool).unwrap_or(false);
        let messages_vec = obj
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let entries_vec = obj
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let estimated_bytes = serde_json::to_vec(&messages_vec)
            .map(|b| b.len())
            .unwrap_or(1024)
            + serde_json::to_vec(&entries_vec)
                .map(|b| b.len())
                .unwrap_or(512);
        Some(Self {
            id,
            revision,
            running,
            messages: Arc::new(messages_vec),
            entries: Arc::new(entries_vec),
            context_usage: obj.get("contextUsage").cloned(),
            queue: obj.get("queue").cloned(),
            ui: obj.get("ui").cloned(),
            bitlocker: obj.get("bitlocker").cloned(),
            streaming_assistant: None,
            active_tools: Arc::new(BTreeMap::new()),
            active_turn_started_ms: running.then(now_epoch_ms),
            estimated_bytes,
        })
    }

    pub fn server_inbox_ids(&self) -> Vec<String> {
        self.queue
            .as_ref()
            .and_then(|q| q.get("items"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub enum BufferedSessionFrame {
    SessionEvent(Value),
    QueueSnapshot(Value),
}

enum SlotState {
    Loading {
        buffered: Vec<BufferedSessionFrame>,
    },
    Ready {
        snapshot: Arc<SessionViewSnapshot>,
        last_touched: u64,
    },
}

/// On-demand per-session snapshot cache with zero-gap initial subscription merge and pinned-exempt LRU eviction.
pub struct SessionStore {
    slots: BTreeMap<String, SlotState>,
    next_revision: u64,
    next_touch: u64,
    max_sessions: usize,
    max_bytes: usize,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CACHED_SESSIONS, DEFAULT_MAX_CACHED_BYTES)
    }
}

impl SessionStore {
    pub fn new(max_sessions: usize, max_bytes: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            next_revision: 1,
            next_touch: 1,
            max_sessions: max_sessions.max(1),
            max_bytes: max_bytes.max(1024 * 1024),
        }
    }

    fn alloc_revision(&mut self) -> u64 {
        let rev = self.next_revision;
        self.next_revision = self.next_revision.wrapping_add(1);
        rev
    }

    fn alloc_touch(&mut self) -> u64 {
        let t = self.next_touch;
        self.next_touch = self.next_touch.wrapping_add(1);
        t
    }

    pub fn has_ready(&self, session_id: &str) -> bool {
        matches!(self.slots.get(session_id), Some(SlotState::Ready { .. }))
    }

    pub fn is_loading(&self, session_id: &str) -> bool {
        matches!(self.slots.get(session_id), Some(SlotState::Loading { .. }))
    }

    /// Marks `session_id` as loading BEFORE establishing its SSE subscription so in-flight frames
    /// arriving while `GET /api/sessions/<id>` is running are buffered and merged without gaps.
    pub fn begin_loading_if_absent(&mut self, session_id: &str) -> bool {
        if self.slots.contains_key(session_id) {
            return false;
        }
        self.slots.insert(
            session_id.to_owned(),
            SlotState::Loading {
                buffered: Vec::new(),
            },
        );
        true
    }

    /// Forces `session_id` back into `Loading` (e.g. upon receiving `reset { scope: "session" }`).
    pub fn reset_to_loading(&mut self, session_id: &str) {
        self.slots.insert(
            session_id.to_owned(),
            SlotState::Loading {
                buffered: Vec::new(),
            },
        );
    }

    pub fn cancel_loading(&mut self, session_id: &str) {
        if matches!(self.slots.get(session_id), Some(SlotState::Loading { .. })) {
            self.slots.remove(session_id);
        }
    }

    pub fn remove(&mut self, session_id: &str) -> bool {
        self.slots.remove(session_id).is_some()
    }

    pub fn touch(&mut self, session_id: &str) {
        let t = self.alloc_touch();
        if let Some(SlotState::Ready { last_touched, .. }) = self.slots.get_mut(session_id) {
            *last_touched = t;
        }
    }

    /// Installs the full HTTP snapshot for `session_id` and idempotently replays any buffered SSE frames.
    pub fn finish_loading_snapshot(
        &mut self,
        session_id: &str,
        http_payload: Value,
    ) -> Option<Arc<SessionViewSnapshot>> {
        let rev = self.alloc_revision();
        let touch = self.alloc_touch();
        let mut snap = SessionViewSnapshot::from_http_payload(http_payload, rev)?;
        let buffered = match self.slots.remove(session_id) {
            Some(SlotState::Loading { buffered }) => buffered,
            _ => Vec::new(),
        };
        for frame in buffered {
            apply_frame_to_snapshot(&mut snap, frame);
        }
        snap.revision = self.alloc_revision();
        let arc = Arc::new(snap);
        self.slots.insert(
            session_id.to_owned(),
            SlotState::Ready {
                snapshot: Arc::clone(&arc),
                last_touched: touch,
            },
        );
        Some(arc)
    }

    pub fn apply_session_event(&mut self, session_id: &str, event: Value) -> bool {
        match self.slots.get_mut(session_id) {
            Some(SlotState::Loading { buffered }) => {
                buffered.push(BufferedSessionFrame::SessionEvent(event));
                false
            }
            Some(SlotState::Ready {
                snapshot,
                last_touched,
            }) => {
                let mut updated = (**snapshot).clone();
                apply_frame_to_snapshot(&mut updated, BufferedSessionFrame::SessionEvent(event));
                updated.revision = self.next_revision;
                self.next_revision = self.next_revision.wrapping_add(1);
                *last_touched = self.next_touch;
                self.next_touch = self.next_touch.wrapping_add(1);
                *snapshot = Arc::new(updated);
                true
            }
            None => false,
        }
    }

    pub fn apply_queue_snapshot(&mut self, session_id: &str, queue: Value) -> bool {
        match self.slots.get_mut(session_id) {
            Some(SlotState::Loading { buffered }) => {
                buffered.push(BufferedSessionFrame::QueueSnapshot(queue));
                false
            }
            Some(SlotState::Ready {
                snapshot,
                last_touched,
            }) => {
                let mut updated = (**snapshot).clone();
                apply_frame_to_snapshot(&mut updated, BufferedSessionFrame::QueueSnapshot(queue));
                updated.revision = self.next_revision;
                self.next_revision = self.next_revision.wrapping_add(1);
                *last_touched = self.next_touch;
                self.next_touch = self.next_touch.wrapping_add(1);
                *snapshot = Arc::new(updated);
                true
            }
            None => false,
        }
    }

    /// Evicts oldest unpinned sessions when cached count > `max_sessions` or total bytes > `max_bytes`.
    /// Exempts `pinned_ids` (active session, running sessions, sessions with unresolved outbox or unsent drafts).
    pub fn evict_lru(&mut self, pinned_ids: &BTreeSet<String>) -> bool {
        let mut evicted_any = false;
        loop {
            let mut ready_count = 0usize;
            let mut total_bytes = 0usize;
            let mut oldest_candidate: Option<(u64, String)> = None;

            for (id, state) in &self.slots {
                if let SlotState::Ready {
                    snapshot,
                    last_touched,
                } = state
                {
                    ready_count += 1;
                    total_bytes = total_bytes.saturating_add(snapshot.estimated_bytes);
                    if !snapshot.running && !pinned_ids.contains(id) {
                        match &oldest_candidate {
                            None => oldest_candidate = Some((*last_touched, id.clone())),
                            Some((best_t, _)) if *last_touched < *best_t => {
                                oldest_candidate = Some((*last_touched, id.clone()));
                            }
                            _ => {}
                        }
                    }
                }
            }

            if ready_count <= self.max_sessions && total_bytes <= self.max_bytes {
                break;
            }
            let Some((_, victim_id)) = oldest_candidate else {
                break;
            };
            self.slots.remove(&victim_id);
            evicted_any = true;
        }
        evicted_any
    }

    pub fn export_ready_map(&self) -> BTreeMap<String, Arc<SessionViewSnapshot>> {
        let mut out = BTreeMap::new();
        for (id, state) in &self.slots {
            if let SlotState::Ready { snapshot, .. } = state {
                out.insert(id.clone(), Arc::clone(snapshot));
            }
        }
        out
    }

    pub fn ready_count(&self) -> usize {
        self.slots
            .values()
            .filter(|s| matches!(s, SlotState::Ready { .. }))
            .count()
    }
}

fn apply_frame_to_snapshot(snap: &mut SessionViewSnapshot, frame: BufferedSessionFrame) {
    match frame {
        BufferedSessionFrame::QueueSnapshot(queue) => {
            let new_ver = queue.get("version").and_then(Value::as_u64);
            let cur_ver = snap
                .queue
                .as_ref()
                .and_then(|q| q.get("version"))
                .and_then(Value::as_u64);
            if match (new_ver, cur_ver) {
                (Some(nv), Some(cv)) => nv >= cv,
                _ => true,
            } {
                snap.queue = Some(queue);
            }
        }
        BufferedSessionFrame::SessionEvent(ev) => {
            let Some(ev_type) = ev.get("type").and_then(Value::as_str) else {
                return;
            };
            match ev_type {
                "agent_start" | "turn_start" => {
                    snap.running = true;
                    if snap.active_turn_started_ms.is_none() {
                        snap.active_turn_started_ms = Some(now_epoch_ms());
                    }
                }
                "agent_end" => {
                    snap.running = false;
                    snap.streaming_assistant = None;
                    snap.active_turn_started_ms = None;
                    if !snap.active_tools.is_empty() {
                        snap.active_tools = Arc::new(BTreeMap::new());
                    }
                }
                "message_start" => {
                    if let Some(msg) = ev.get("message") {
                        if msg.get("role").and_then(Value::as_str) == Some("assistant") {
                            snap.running = true;
                            snap.streaming_assistant = Some(msg.clone());
                        } else {
                            upsert_message(&mut snap.messages, msg.clone());
                        }
                    }
                }
                "message_update" => {
                    if let Some(msg) = ev.get("message") {
                        snap.running = true;
                        snap.streaming_assistant = Some(msg.clone());
                    } else if let Some(delta_ev) = ev.get("assistantMessageEvent") {
                        apply_assistant_delta(&mut snap.streaming_assistant, delta_ev);
                    }
                }
                "message_end" => {
                    if let Some(msg) = ev.get("message") {
                        if msg.get("role").and_then(Value::as_str) == Some("assistant") {
                            snap.streaming_assistant = None;
                        }
                        if msg.get("role").and_then(Value::as_str) != Some("system") {
                            let added_bytes = serde_json::to_vec(msg).map(|b| b.len()).unwrap_or(0);
                            if upsert_message(&mut snap.messages, msg.clone()) {
                                snap.estimated_bytes =
                                    snap.estimated_bytes.saturating_add(added_bytes);
                            }
                        }
                    }
                }
                "tool_execution_start" | "tool_execution_update" => {
                    if let Some(tc_id) = ev.get("toolCallId").and_then(Value::as_str) {
                        let mut map = (*snap.active_tools).clone();
                        map.insert(tc_id.to_owned(), ev.clone());
                        snap.active_tools = Arc::new(map);
                    }
                }
                "tool_execution_end" => {
                    if let Some(tc_id) = ev.get("toolCallId").and_then(Value::as_str) {
                        let mut map = (*snap.active_tools).clone();
                        map.remove(tc_id);
                        snap.active_tools = Arc::new(map);
                    }
                }
                "ui_requests" => {
                    if let Some(reqs) = ev.get("requests") {
                        snap.ui = Some(json!({ "requests": reqs }));
                    } else {
                        snap.ui = Some(ev);
                    }
                }
                "bitlocker_requests_changed" => {
                    if let Some(bl) = ev.get("bitlocker") {
                        snap.bitlocker = Some(bl.clone());
                    }
                }
                _ => {}
            }
        }
    }
}

fn message_dedup_key(msg: &Value) -> Option<String> {
    let obj = msg.as_object()?;
    if let Some(id) = obj.get("id").and_then(Value::as_str) {
        return Some(format!("id:{id}"));
    }
    if let Some(cmid) = obj.get("clientMessageId").and_then(Value::as_str) {
        return Some(format!("cmid:{cmid}"));
    }
    if obj.get("role").and_then(Value::as_str) == Some("toolResult")
        && let Some(tcid) = obj.get("toolCallId").and_then(Value::as_str)
    {
        return Some(format!("tool:{tcid}"));
    }
    None
}

/// Inserts or replaces `msg` idempotently. Returns `true` if appended as a new item.
fn upsert_message(messages: &mut Arc<Vec<Value>>, msg: Value) -> bool {
    let mut vec = (**messages).clone();
    if let Some(key) = message_dedup_key(&msg)
        && let Some(existing) = vec
            .iter_mut()
            .find(|m| message_dedup_key(m).as_deref() == Some(key.as_str()))
    {
        *existing = msg;
        *messages = Arc::new(vec);
        return false;
    }
    vec.push(msg);
    *messages = Arc::new(vec);
    true
}

fn apply_assistant_delta(slot: &mut Option<Value>, delta_ev: &Value) {
    let Some(kind) = delta_ev.get("type").and_then(Value::as_str) else {
        return;
    };
    if let Some(partial) = delta_ev.get("partial")
        && partial.is_object()
    {
        *slot = Some(partial.clone());
        return;
    }
    if kind == "text_delta"
        && let Some(delta) = delta_ev.get("delta").and_then(Value::as_str)
    {
        let current = slot.get_or_insert_with(|| {
            json!({
                "role": "assistant",
                "content": [{ "type": "text", "text": "" }],
            })
        });
        if let Some(arr) = current.get_mut("content").and_then(Value::as_array_mut) {
            if let Some(last_text) = arr
                .iter_mut()
                .rev()
                .find(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                && let Some(s) = last_text.get_mut("text")
            {
                let combined = format!("{}{delta}", s.as_str().unwrap_or(""));
                *s = Value::String(combined);
                return;
            }
            arr.push(json!({ "type": "text", "text": delta }));
        }
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

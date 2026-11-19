use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::sse::{SessionCursor, parse_single_cursor};

pub const DEFAULT_MAX_CACHED_SESSIONS: usize = 12;
pub const DEFAULT_MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_MAX_LOADING_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SessionViewSnapshot {
    pub id: String,
    pub revision: u64,
    pub event_cursor: Option<SessionCursor>,
    pub leaf_id: Option<String>,
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
        let event_cursor = obj
            .get("eventCursor")
            .and_then(Value::as_str)
            .and_then(parse_single_cursor);
        let leaf_id = obj.get("leafId").and_then(Value::as_str).map(str::to_owned);
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
            event_cursor,
            leaf_id,
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
    SessionEvent {
        cursor: Option<SessionCursor>,
        event: Value,
    },
    QueueSnapshot {
        cursor: Option<SessionCursor>,
        queue: Value,
    },
}

enum SlotState {
    Loading {
        generation: u64,
        buffered: Vec<BufferedSessionFrame>,
        buffered_bytes: usize,
        overflowed: bool,
    },
    Ready {
        snapshot: Arc<SessionViewSnapshot>,
        last_touched: u64,
    },
}

#[derive(Debug)]
pub enum FinishLoadingOutcome {
    Installed(Arc<SessionViewSnapshot>),
    StaleGeneration,
    BufferOverflowNeedsReload,
    InvalidPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorDisposition {
    ApplyAndAdvance,
    ApplyWithoutCursor,
    SkipAlreadyCovered,
    EpochMismatch,
}

fn evaluate_frame_cursor(
    snapshot_cursor: &Option<SessionCursor>,
    frame_cursor: &Option<SessionCursor>,
) -> CursorDisposition {
    match (snapshot_cursor, frame_cursor) {
        (Some((snap_ep, snap_seq)), Some((frame_ep, frame_seq))) => {
            if snap_ep != frame_ep {
                CursorDisposition::EpochMismatch
            } else if frame_seq <= snap_seq {
                CursorDisposition::SkipAlreadyCovered
            } else {
                CursorDisposition::ApplyAndAdvance
            }
        }
        (None, Some(_)) => CursorDisposition::ApplyAndAdvance,
        _ => CursorDisposition::ApplyWithoutCursor,
    }
}

/// On-demand per-session snapshot cache with generation-guarded loading, SSE cursor watermarks,
/// bounded in-flight frame buffering, and pinned-exempt LRU eviction.
pub struct SessionStore {
    slots: BTreeMap<String, SlotState>,
    next_revision: u64,
    next_touch: u64,
    next_generation: u64,
    max_sessions: usize,
    max_bytes: usize,
    max_loading_buffer_bytes: usize,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::with_limits(
            DEFAULT_MAX_CACHED_SESSIONS,
            DEFAULT_MAX_CACHED_BYTES,
            DEFAULT_MAX_LOADING_BUFFER_BYTES,
        )
    }
}

impl SessionStore {
    pub fn new(max_sessions: usize, max_bytes: usize) -> Self {
        Self::with_limits(max_sessions, max_bytes, DEFAULT_MAX_LOADING_BUFFER_BYTES)
    }

    pub fn with_limits(
        max_sessions: usize,
        max_bytes: usize,
        max_loading_buffer_bytes: usize,
    ) -> Self {
        Self {
            slots: BTreeMap::new(),
            next_revision: 1,
            next_touch: 1,
            next_generation: 1,
            max_sessions: max_sessions.max(1),
            max_bytes: max_bytes.max(1024 * 1024),
            max_loading_buffer_bytes: max_loading_buffer_bytes.max(512),
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

    fn alloc_generation(&mut self) -> u64 {
        let g = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        g
    }

    pub fn has_ready(&self, session_id: &str) -> bool {
        matches!(self.slots.get(session_id), Some(SlotState::Ready { .. }))
    }

    pub fn ready_cursor(&self, session_id: &str) -> Option<SessionCursor> {
        match self.slots.get(session_id) {
            Some(SlotState::Ready { snapshot, .. }) => snapshot.event_cursor.clone(),
            _ => None,
        }
    }

    pub fn is_loading(&self, session_id: &str) -> bool {
        matches!(self.slots.get(session_id), Some(SlotState::Loading { .. }))
    }

    pub fn loading_generation(&self, session_id: &str) -> Option<u64> {
        match self.slots.get(session_id) {
            Some(SlotState::Loading { generation, .. }) => Some(*generation),
            _ => None,
        }
    }

    /// Marks `session_id` as loading BEFORE establishing its SSE subscription so in-flight frames
    /// arriving while `GET /api/sessions/<id>` is running are buffered and merged without gaps.
    pub fn begin_loading_if_absent(&mut self, session_id: &str) -> Option<u64> {
        if self.slots.contains_key(session_id) {
            return None;
        }
        let generation = self.alloc_generation();
        self.slots.insert(
            session_id.to_owned(),
            SlotState::Loading {
                generation,
                buffered: Vec::new(),
                buffered_bytes: 0,
                overflowed: false,
            },
        );
        Some(generation)
    }

    /// Forces `session_id` back into `Loading` under a new generation (invalidating any older in-flight HTTP fetch).
    pub fn reset_to_loading(&mut self, session_id: &str) -> u64 {
        let generation = self.alloc_generation();
        self.slots.insert(
            session_id.to_owned(),
            SlotState::Loading {
                generation,
                buffered: Vec::new(),
                buffered_bytes: 0,
                overflowed: false,
            },
        );
        generation
    }

    pub fn cancel_loading(&mut self, session_id: &str) {
        if matches!(self.slots.get(session_id), Some(SlotState::Loading { .. })) {
            self.slots.remove(session_id);
        }
    }

    pub fn cancel_loading_if_generation(&mut self, session_id: &str, generation: u64) -> bool {
        if matches!(
            self.slots.get(session_id),
            Some(SlotState::Loading { generation: g, .. }) if *g == generation
        ) {
            self.slots.remove(session_id);
            return true;
        }
        false
    }

    pub fn remove(&mut self, session_id: &str) -> bool {
        self.slots.remove(session_id).is_some()
    }

    pub fn tracked_session_ids(&self) -> Vec<String> {
        self.slots.keys().cloned().collect()
    }

    pub fn touch(&mut self, session_id: &str) {
        let t = self.alloc_touch();
        if let Some(SlotState::Ready { last_touched, .. }) = self.slots.get_mut(session_id) {
            *last_touched = t;
        }
    }

    /// Installs the full HTTP snapshot for `(session_id, generation)` and replays newer buffered SSE frames.
    pub fn finish_loading_snapshot(
        &mut self,
        session_id: &str,
        generation: u64,
        http_payload: Value,
    ) -> FinishLoadingOutcome {
        match self.slots.get(session_id) {
            Some(SlotState::Loading {
                generation: cur_gen,
                ..
            }) if *cur_gen == generation => {}
            _ => return FinishLoadingOutcome::StaleGeneration,
        }

        let rev = self.alloc_revision();
        let touch = self.alloc_touch();
        let Some(mut snap) = SessionViewSnapshot::from_http_payload(http_payload, rev)
            .filter(|snapshot| snapshot.id == session_id)
        else {
            self.slots.remove(session_id);
            return FinishLoadingOutcome::InvalidPayload;
        };

        let (buffered, overflowed) = match self.slots.remove(session_id) {
            Some(SlotState::Loading {
                buffered,
                overflowed,
                ..
            }) => (buffered, overflowed),
            _ => (Vec::new(), false),
        };
        if overflowed {
            return FinishLoadingOutcome::BufferOverflowNeedsReload;
        }

        for frame in buffered {
            let frame_cursor = match &frame {
                BufferedSessionFrame::SessionEvent { cursor, .. }
                | BufferedSessionFrame::QueueSnapshot { cursor, .. } => cursor.clone(),
            };
            match evaluate_frame_cursor(&snap.event_cursor, &frame_cursor) {
                CursorDisposition::SkipAlreadyCovered => continue,
                CursorDisposition::EpochMismatch => {
                    return FinishLoadingOutcome::BufferOverflowNeedsReload;
                }
                CursorDisposition::ApplyAndAdvance => {
                    apply_frame_to_snapshot(&mut snap, frame);
                    snap.event_cursor = frame_cursor;
                }
                CursorDisposition::ApplyWithoutCursor => {
                    apply_frame_to_snapshot(&mut snap, frame);
                }
            }
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
        FinishLoadingOutcome::Installed(arc)
    }

    pub fn apply_session_event(
        &mut self,
        session_id: &str,
        cursor: Option<SessionCursor>,
        event: Value,
        approx_bytes: usize,
    ) -> bool {
        let max_buf = self.max_loading_buffer_bytes;
        match self.slots.get_mut(session_id) {
            Some(SlotState::Loading {
                buffered,
                buffered_bytes,
                overflowed,
                ..
            }) => {
                if !*overflowed {
                    let cost = approx_bytes.max(64);
                    if buffered_bytes.saturating_add(cost) > max_buf {
                        buffered.clear();
                        *buffered_bytes = 0;
                        *overflowed = true;
                    } else {
                        *buffered_bytes += cost;
                        buffered.push(BufferedSessionFrame::SessionEvent { cursor, event });
                    }
                }
                false
            }
            Some(SlotState::Ready {
                snapshot,
                last_touched,
            }) => {
                let disp = evaluate_frame_cursor(&snapshot.event_cursor, &cursor);
                if matches!(disp, CursorDisposition::SkipAlreadyCovered) {
                    return false;
                }
                let mut updated = (**snapshot).clone();
                apply_frame_to_snapshot(
                    &mut updated,
                    BufferedSessionFrame::SessionEvent {
                        cursor: cursor.clone(),
                        event,
                    },
                );
                if matches!(
                    disp,
                    CursorDisposition::ApplyAndAdvance | CursorDisposition::EpochMismatch
                ) && cursor.is_some()
                {
                    updated.event_cursor = cursor;
                }
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

    pub fn apply_queue_snapshot(
        &mut self,
        session_id: &str,
        cursor: Option<SessionCursor>,
        queue: Value,
        approx_bytes: usize,
    ) -> bool {
        let max_buf = self.max_loading_buffer_bytes;
        match self.slots.get_mut(session_id) {
            Some(SlotState::Loading {
                buffered,
                buffered_bytes,
                overflowed,
                ..
            }) => {
                if !*overflowed {
                    let cost = approx_bytes.max(64);
                    if buffered_bytes.saturating_add(cost) > max_buf {
                        buffered.clear();
                        *buffered_bytes = 0;
                        *overflowed = true;
                    } else {
                        *buffered_bytes += cost;
                        buffered.push(BufferedSessionFrame::QueueSnapshot { cursor, queue });
                    }
                }
                false
            }
            Some(SlotState::Ready {
                snapshot,
                last_touched,
            }) => {
                let disp = evaluate_frame_cursor(&snapshot.event_cursor, &cursor);
                if matches!(disp, CursorDisposition::SkipAlreadyCovered) {
                    return false;
                }
                let mut updated = (**snapshot).clone();
                apply_frame_to_snapshot(
                    &mut updated,
                    BufferedSessionFrame::QueueSnapshot {
                        cursor: cursor.clone(),
                        queue,
                    },
                );
                if matches!(
                    disp,
                    CursorDisposition::ApplyAndAdvance | CursorDisposition::EpochMismatch
                ) && cursor.is_some()
                {
                    updated.event_cursor = cursor;
                }
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
        BufferedSessionFrame::QueueSnapshot { queue, .. } => {
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
        BufferedSessionFrame::SessionEvent { event: ev, .. } => {
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
                    if let Some(msg) = ev.get("message")
                        && msg.get("role").and_then(Value::as_str) == Some("assistant")
                    {
                        snap.running = true;
                        snap.streaming_assistant = Some(msg.clone());
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
                            let previous_bytes = upsert_message(&mut snap.messages, msg.clone());
                            snap.estimated_bytes = snap
                                .estimated_bytes
                                .saturating_sub(previous_bytes)
                                .saturating_add(added_bytes);
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
                    let candidate = ev.get("bitlocker").unwrap_or(&ev);
                    let new_rev = candidate.get("revision").and_then(Value::as_i64);
                    let cur_rev = snap
                        .bitlocker
                        .as_ref()
                        .and_then(|b| b.get("revision"))
                        .and_then(Value::as_i64);
                    if match (new_rev, cur_rev) {
                        (Some(nr), Some(cr)) => nr >= cr,
                        _ => true,
                    } {
                        snap.bitlocker = Some(candidate.clone());
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
    Some(format!(
        "timestamp:{}:{}",
        obj.get("role")?.as_str()?,
        obj.get("timestamp")?
    ))
}

/// Merge messages already covered by the HTTP snapshot and return the replaced size.
fn upsert_message(messages: &mut Arc<Vec<Value>>, msg: Value) -> usize {
    let vec = Arc::make_mut(messages);
    let key = message_dedup_key(&msg);
    if let Some(existing) = vec
        .iter_mut()
        .find(|existing| **existing == msg || (key.is_some() && key == message_dedup_key(existing)))
    {
        let previous = serde_json::to_vec(existing).map_or(0, |bytes| bytes.len());
        *existing = msg;
        return previous;
    }
    vec.push(msg);
    0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_assistant_completion_overlapping_http_snapshot_is_applied_once() {
        let assistant = json!({"role": "assistant", "timestamp": 100, "content": [{"type": "text", "text": "完成"}]});
        let mut store = SessionStore::default();
        let g = store.begin_loading_if_absent("s1").unwrap();
        store.apply_session_event(
            "s1",
            Some(("ep1".into(), 5)),
            json!({"type": "message_end", "message": assistant}),
            128,
        );
        let FinishLoadingOutcome::Installed(snap) = store.finish_loading_snapshot(
            "s1",
            g,
            json!({"id": "s1", "eventCursor": "ep1:5", "messages": [assistant]}),
        ) else {
            panic!("expected Installed");
        };
        assert_eq!(snap.messages.as_slice(), &[assistant]);
        assert_eq!(snap.event_cursor, Some(("ep1".into(), 5)));
    }

    #[test]
    fn stale_generation_snapshot_is_rejected_and_overflow_requests_reload() {
        let mut store = SessionStore::with_limits(4, 2 * 1024 * 1024, 512);
        let g1 = store.begin_loading_if_absent("s1").unwrap();
        let g2 = store.reset_to_loading("s1");
        assert!(matches!(
            store.finish_loading_snapshot("s1", g1, json!({"id": "s1", "messages": []})),
            FinishLoadingOutcome::StaleGeneration
        ));
        assert!(store.is_loading("s1"));

        // Overflow g2's loading buffer
        store.apply_session_event(
            "s1",
            Some(("ep1".into(), 1)),
            json!({"type": "turn_start"}),
            400,
        );
        store.apply_session_event(
            "s1",
            Some(("ep1".into(), 2)),
            json!({"type": "agent_end"}),
            400,
        );
        assert!(matches!(
            store.finish_loading_snapshot("s1", g2, json!({"id": "s1", "messages": []})),
            FinishLoadingOutcome::BufferOverflowNeedsReload
        ));
    }
}

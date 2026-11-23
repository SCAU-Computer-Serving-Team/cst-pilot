use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::sse::{SessionCursor, parse_single_cursor};

pub const DEFAULT_MAX_CACHED_SESSIONS: usize = 16;
pub const DEFAULT_MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_MAX_LOADING_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SessionViewSnapshot {
    pub id: String,
    pub revision: u64,
    pub event_cursor: Option<SessionCursor>,
    pub leaf_id: Option<String>,
    pub running: bool,
    pub compacting: bool,
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
        let compacting = obj
            .get("compacting")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
            compacting,
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
        previous: Option<(Arc<SessionViewSnapshot>, u64)>,
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
                previous: None,
            },
        );
        Some(generation)
    }

    /// Start a new generation while continuing to serve the last usable snapshot.
    pub fn reset_to_loading(&mut self, session_id: &str) -> u64 {
        let generation = self.alloc_generation();
        let previous = match self.slots.remove(session_id) {
            Some(SlotState::Ready {
                snapshot,
                last_touched,
            }) => Some((snapshot, last_touched)),
            Some(SlotState::Loading { previous, .. }) => previous,
            None => None,
        };
        self.slots.insert(
            session_id.to_owned(),
            SlotState::Loading {
                generation,
                buffered: Vec::new(),
                buffered_bytes: 0,
                overflowed: false,
                previous,
            },
        );
        generation
    }

    pub(crate) fn has_reload_fallback(&self, session_id: &str) -> bool {
        matches!(
            self.slots.get(session_id),
            Some(SlotState::Loading {
                previous: Some(_),
                ..
            })
        )
    }

    pub fn cancel_loading(&mut self, session_id: &str) {
        if let Some(generation) = self.loading_generation(session_id) {
            self.cancel_loading_if_generation(session_id, generation);
        }
    }

    pub fn cancel_loading_if_generation(&mut self, session_id: &str, generation: u64) -> bool {
        if matches!(
            self.slots.get(session_id),
            Some(SlotState::Loading { generation: g, .. }) if *g == generation
        ) {
            if let Some(SlotState::Loading {
                previous: Some((snapshot, last_touched)),
                ..
            }) = self.slots.remove(session_id)
            {
                self.slots.insert(
                    session_id.to_owned(),
                    SlotState::Ready {
                        snapshot,
                        last_touched,
                    },
                );
            }
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
            return FinishLoadingOutcome::InvalidPayload;
        };

        let Some(SlotState::Loading {
            buffered,
            overflowed,
            ..
        }) = self.slots.get_mut(session_id)
        else {
            unreachable!("generation already checked");
        };
        if *overflowed {
            return FinishLoadingOutcome::BufferOverflowNeedsReload;
        }
        let buffered = std::mem::take(buffered);

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
        self.apply_frame(
            session_id,
            BufferedSessionFrame::SessionEvent { cursor, event },
            approx_bytes,
        )
    }

    pub fn apply_queue_snapshot(
        &mut self,
        session_id: &str,
        cursor: Option<SessionCursor>,
        queue: Value,
        approx_bytes: usize,
    ) -> bool {
        self.apply_frame(
            session_id,
            BufferedSessionFrame::QueueSnapshot { cursor, queue },
            approx_bytes,
        )
    }

    fn apply_frame(
        &mut self,
        session_id: &str,
        frame: BufferedSessionFrame,
        approx_bytes: usize,
    ) -> bool {
        let (snapshot, last_touched) = match self.slots.get_mut(session_id) {
            Some(SlotState::Loading {
                buffered,
                buffered_bytes,
                overflowed,
                previous,
                ..
            }) => {
                if !*overflowed {
                    let cost = approx_bytes.max(64);
                    if buffered_bytes.saturating_add(cost) > self.max_loading_buffer_bytes {
                        buffered.clear();
                        *buffered_bytes = 0;
                        *overflowed = true;
                    } else {
                        *buffered_bytes += cost;
                        buffered.push(frame.clone());
                    }
                }
                let Some((snapshot, last_touched)) = previous else {
                    return false;
                };
                (snapshot, last_touched)
            }
            Some(SlotState::Ready {
                snapshot,
                last_touched,
            }) => (snapshot, last_touched),
            None => return false,
        };
        let cursor = match &frame {
            BufferedSessionFrame::SessionEvent { cursor, .. }
            | BufferedSessionFrame::QueueSnapshot { cursor, .. } => cursor.clone(),
        };
        let disposition = evaluate_frame_cursor(&snapshot.event_cursor, &cursor);
        if disposition == CursorDisposition::SkipAlreadyCovered {
            return false;
        }
        let mut updated = (**snapshot).clone();
        apply_frame_to_snapshot(&mut updated, frame);
        if matches!(
            disposition,
            CursorDisposition::ApplyAndAdvance | CursorDisposition::EpochMismatch
        ) {
            updated.event_cursor = cursor;
        }
        updated.revision = self.next_revision;
        self.next_revision = self.next_revision.wrapping_add(1);
        *last_touched = self.next_touch;
        self.next_touch = self.next_touch.wrapping_add(1);
        *snapshot = Arc::new(updated);
        true
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
            match state {
                SlotState::Ready { snapshot, .. }
                | SlotState::Loading {
                    previous: Some((snapshot, _)),
                    ..
                } => {
                    out.insert(id.clone(), Arc::clone(snapshot));
                }
                SlotState::Loading { .. } => {}
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
                "compaction_start" => {
                    snap.compacting = true;
                }
                "compaction_end" => {
                    snap.compacting = false;
                }
                "message_end" => {
                    if let Some(msg) = ev.get("message") {
                        if msg.get("role").and_then(Value::as_str) == Some("assistant") {
                            snap.streaming_assistant = None;
                        }
                        if msg.get("role").and_then(Value::as_str) != Some("system") {
                            let added_bytes = serde_json::to_vec(msg).map(|b| b.len()).unwrap_or(0);
                            let previous_bytes = upsert_message(&mut snap.messages, msg.clone());
                            let (old_entry, new_entry) =
                                upsert_entry_message(&mut snap.entries, msg.clone());
                            snap.estimated_bytes = snap
                                .estimated_bytes
                                .saturating_sub(previous_bytes + old_entry)
                                .saturating_add(added_bytes + new_entry);
                        }
                    }
                }
                "entry_appended" => {
                    if let Some(entry) = ev.get("entry") {
                        if let Some(eid) = entry
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            snap.leaf_id = Some(eid.to_owned());
                        }
                        if let Some(normalized) = normalize_appended_entry(entry) {
                            let added_bytes = serde_json::to_vec(&normalized)
                                .map(|b| b.len())
                                .unwrap_or(0);
                            let previous_bytes =
                                upsert_persisted_entry(&mut snap.entries, normalized.clone());
                            // Summaries belong to the tree projection. Compaction/tree events
                            // reload the model context instead of appending synthetic messages.
                            let (old_message, new_message) =
                                if entry.get("type").and_then(Value::as_str) == Some("message")
                                    && let Some(msg) = normalized.get("message")
                                {
                                    (
                                        upsert_message(&mut snap.messages, msg.clone()),
                                        json_size(msg),
                                    )
                                } else {
                                    (0, 0)
                                };
                            snap.estimated_bytes = snap
                                .estimated_bytes
                                .saturating_sub(previous_bytes + old_message)
                                .saturating_add(added_bytes + new_message);
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

fn same_message(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    let Some(key) = message_dedup_key(left) else {
        return false;
    };
    message_dedup_key(right).as_ref() == Some(&key)
        && (!key.starts_with("timestamp:") || left.get("content") == right.get("content"))
}

fn json_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

/// Merge messages already covered by the HTTP snapshot and return the replaced size.
fn upsert_message(messages: &mut Arc<Vec<Value>>, msg: Value) -> usize {
    let vec = Arc::make_mut(messages);
    if let Some(existing) = vec
        .iter_mut()
        .rfind(|existing| same_message(existing, &msg))
    {
        let previous = serde_json::to_vec(existing).map_or(0, |bytes| bytes.len());
        *existing = msg;
        return previous;
    }
    vec.push(msg);
    0
}

fn normalize_appended_entry(entry: &Value) -> Option<Value> {
    let obj = entry.as_object()?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let parent_id = obj.get("parentId").cloned().unwrap_or(Value::Null);
    let entry_type = obj.get("type").and_then(Value::as_str).unwrap_or("message");
    let msg = match entry_type {
        "message" => {
            let m = obj.get("message")?;
            if m.get("role").and_then(Value::as_str) == Some("system") {
                return None;
            }
            m.clone()
        }
        "compaction" => json!({
            "role": "compactionSummary",
            "summary": obj.get("summary").and_then(Value::as_str).unwrap_or(""),
            "tokensBefore": obj.get("tokensBefore").and_then(Value::as_u64).unwrap_or(0),
            "timestamp": obj.get("timestamp").cloned().unwrap_or(Value::Null),
        }),
        "branch_summary" => json!({
            "role": "branchSummary",
            "summary": obj.get("summary").and_then(Value::as_str).unwrap_or(""),
            "fromId": obj.get("fromId").cloned().unwrap_or(Value::Null),
            "timestamp": obj.get("timestamp").cloned().unwrap_or(Value::Null),
        }),
        _ => return None,
    };
    Some(json!({
        "id": id,
        "parentId": parent_id,
        "message": msg,
    }))
}

fn upsert_persisted_entry(entries: &mut Arc<Vec<Value>>, entry: Value) -> usize {
    let vec = Arc::make_mut(entries);
    let target_id = entry.get("id").and_then(Value::as_str);
    let msg = entry.get("message");
    if let Some(existing) = vec.iter_mut().rfind(|existing| {
        let existing_id = existing.get("id").and_then(Value::as_str);
        if target_id.is_some() && existing_id == target_id {
            return true;
        }
        let is_synthetic =
            existing_id.is_none_or(|id| id.starts_with("stream-") || id.starts_with("entry-"));
        is_synthetic
            && msg.is_some_and(|msg| same_message(existing.get("message").unwrap_or(existing), msg))
    }) {
        let previous = serde_json::to_vec(existing).map_or(0, |bytes| bytes.len());
        *existing = entry;
        return previous;
    }
    vec.push(entry);
    0
}

fn upsert_entry_message(entries: &mut Arc<Vec<Value>>, msg: Value) -> (usize, usize) {
    if let Some(index) = entries
        .iter()
        .rposition(|entry| same_message(entry.get("message").unwrap_or(entry), &msg))
    {
        let existing = &entries[index];
        if existing.get("parentId").is_some() {
            // The persisted entry is authoritative, including any snapshot-only decoration.
            return (0, 0);
        }
        let previous = json_size(existing);
        let existing = &mut Arc::make_mut(entries)[index];
        if let Some(obj) = existing.as_object_mut()
            && obj.contains_key("message")
        {
            obj.insert("message".to_owned(), msg);
        } else {
            *existing = msg;
        }
        return (previous, json_size(existing));
    }
    let entry = json!({
        "id": format!("stream-{}-{}", msg["role"].as_str().unwrap_or("msg"), entries.len()),
        "message": msg,
    });
    let size = json_size(&entry);
    Arc::make_mut(entries).push(entry);
    (0, size)
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
    fn failed_reload_keeps_visible_snapshot_and_merges_later_sse_on_retry() {
        let mut store = SessionStore::default();
        let initial = store.begin_loading_if_absent("s1").unwrap();
        assert!(matches!(
            store.finish_loading_snapshot(
                "s1",
                initial,
                json!({"id":"s1", "eventCursor":"ep:3", "compacting":true}),
            ),
            FinishLoadingOutcome::Installed(_)
        ));

        let reload = store.reset_to_loading("s1");
        assert!(store.export_ready_map()["s1"].compacting);
        assert!(matches!(
            store.finish_loading_snapshot("s1", reload, Value::Null),
            FinishLoadingOutcome::InvalidPayload
        ));
        assert!(store.has_reload_fallback("s1"));

        assert!(store.apply_session_event(
            "s1",
            Some(("ep".into(), 4)),
            json!({"type":"compaction_end"}),
            64
        ));
        assert!(!store.export_ready_map()["s1"].compacting);
        let message = json!({"role":"assistant", "timestamp":123, "content":"post-reload"});
        store.apply_session_event(
            "s1",
            Some(("ep".into(), 5)),
            json!({"type":"message_end","message":message}),
            96,
        );
        let FinishLoadingOutcome::Installed(loaded) =
            store.finish_loading_snapshot("s1", reload, json!({"id":"s1", "eventCursor":"ep:4"}))
        else {
            panic!("expected recovered snapshot");
        };
        assert_eq!(loaded.messages.as_slice(), &[message]);
        assert_eq!(loaded.event_cursor, Some(("ep".into(), 5)));
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

    #[test]
    fn same_millisecond_messages_keep_distinct_entries_and_account_for_both_copies() {
        let mut store = SessionStore::default();
        let generation = store.begin_loading_if_absent("s").unwrap();
        store.finish_loading_snapshot(
            "s",
            generation,
            json!({"id": "s", "messages": [], "entries": []}),
        );
        for (id, content) in [("a1", "first"), ("a2", "second")] {
            let message = json!({"role": "assistant", "timestamp": 5, "content": content});
            store.apply_session_event(
                "s",
                None,
                json!({"type": "message_end", "message": message}),
                0,
            );
            store.apply_session_event(
                "s",
                None,
                json!({
                    "type": "entry_appended", "entry": {
                        "id": id, "type": "message", "parentId": null, "message": message
                    }
                }),
                0,
            );
        }
        let map = store.export_ready_map();
        let snapshot = &map["s"];
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries[0]["id"], "a1");
        assert_eq!(snapshot.entries[1]["id"], "a2");
        assert_eq!(snapshot.messages.len(), 2);
        // Array separators are ignored by the incremental estimate.
        assert_eq!(
            snapshot.estimated_bytes,
            serde_json::to_vec(snapshot.messages.as_ref())
                .unwrap()
                .len()
                + serde_json::to_vec(snapshot.entries.as_ref()).unwrap().len()
                - 2,
        );
    }

    #[test]
    fn turn_zero_entry_appended_and_message_end_populate_real_entry_ids_in_either_order() {
        let user_msg = json!({
            "role": "user",
            "timestamp": 1000,
            "clientMessageId": "cm-1",
            "content": [{"type": "text", "text": "你好"}]
        });
        let asst_msg = json!({
            "role": "assistant",
            "timestamp": 1200,
            "stopReason": "stop",
            "content": [{"type": "text", "text": "你好，有什么可以帮你的？"}]
        });
        let user_entry = json!({
            "id": "u-real-1",
            "parentId": null,
            "type": "message",
            "message": user_msg
        });
        let asst_entry = json!({
            "id": "a-real-1",
            "parentId": "u-real-1",
            "type": "message",
            "message": asst_msg
        });

        // Order A: `message_end` arrives before `entry_appended` on a brand-new Turn 0 session (`entries: []`).
        let mut store_a = SessionStore::default();
        let g_a = store_a.begin_loading_if_absent("s0").unwrap();
        assert!(matches!(
            store_a.finish_loading_snapshot(
                "s0",
                g_a,
                json!({"id": "s0", "eventCursor": "ep:1", "messages": [], "entries": []}),
            ),
            FinishLoadingOutcome::Installed(_)
        ));
        store_a.apply_session_event(
            "s0",
            Some(("ep".into(), 2)),
            json!({"type": "message_end", "message": user_msg}),
            128,
        );
        store_a.apply_session_event(
            "s0",
            Some(("ep".into(), 3)),
            json!({"type": "message_end", "message": asst_msg}),
            128,
        );
        let mid_a = store_a.export_ready_map()["s0"].clone();
        assert_eq!(mid_a.entries.len(), 2);
        assert!(
            mid_a.entries[0]["id"]
                .as_str()
                .unwrap()
                .starts_with("stream-user-")
        );

        store_a.apply_session_event(
            "s0",
            Some(("ep".into(), 4)),
            json!({"type": "entry_appended", "entry": user_entry}),
            128,
        );
        store_a.apply_session_event(
            "s0",
            Some(("ep".into(), 5)),
            json!({"type": "entry_appended", "entry": asst_entry}),
            128,
        );
        let final_a = store_a.export_ready_map()["s0"].clone();
        assert_eq!(final_a.leaf_id.as_deref(), Some("a-real-1"));
        assert_eq!(final_a.messages.len(), 2);
        assert_eq!(final_a.entries.len(), 2);
        assert_eq!(final_a.entries[0]["id"], "u-real-1");
        assert_eq!(final_a.entries[1]["id"], "a-real-1");
        assert_eq!(final_a.entries[1]["parentId"], "u-real-1");

        // Order B: `entry_appended` arrives before `message_end` on a brand-new Turn 0 session (`entries: []`).
        let mut store_b = SessionStore::default();
        let g_b = store_b.begin_loading_if_absent("s0").unwrap();
        assert!(matches!(
            store_b.finish_loading_snapshot(
                "s0",
                g_b,
                json!({"id": "s0", "eventCursor": "ep:1", "messages": [], "entries": []}),
            ),
            FinishLoadingOutcome::Installed(_)
        ));
        store_b.apply_session_event(
            "s0",
            Some(("ep".into(), 2)),
            json!({"type": "entry_appended", "entry": user_entry}),
            128,
        );
        store_b.apply_session_event(
            "s0",
            Some(("ep".into(), 3)),
            json!({"type": "message_end", "message": user_msg}),
            128,
        );
        store_b.apply_session_event(
            "s0",
            Some(("ep".into(), 4)),
            json!({"type": "entry_appended", "entry": asst_entry}),
            128,
        );
        store_b.apply_session_event(
            "s0",
            Some(("ep".into(), 5)),
            json!({"type": "message_end", "message": asst_msg}),
            128,
        );
        let final_b = store_b.export_ready_map()["s0"].clone();
        assert_eq!(final_b.leaf_id.as_deref(), Some("a-real-1"));
        assert_eq!(final_b.messages.len(), 2);
        assert_eq!(final_b.entries.len(), 2);
        assert_eq!(final_b.entries[0]["id"], "u-real-1");
        assert_eq!(final_b.entries[1]["id"], "a-real-1");
        assert_eq!(final_b.entries[1]["parentId"], "u-real-1");
    }
}

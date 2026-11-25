use std::collections::{BTreeSet, HashSet, VecDeque};
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use cst_kit::HostedSession;
use pi_agent::{AgentMessage, BoxFuture};
use pi_ai::{ImageContent, StopReason};
use pi_session::{
    AgentSession, AgentSessionEvent, InputSource, PromptOptions, SessionEntry,
    StreamingInputBehavior, parse_iso_timestamp_ms,
};
use serde_json::{Value, json};

use crate::discovery::SessionOwnerLock;
use crate::session::attachments::{ImageRef, ImageStore};
use crate::session::decorators::{
    INPUT_MESSAGE_CUSTOM_TYPE, MessageDecorators, is_browser_visible_message,
};
use crate::session::events::{SessionEvents, browser_json_value};
use crate::session::inbox::{InboxError, InboxExecutor, SessionInbox};
use crate::session::interactions::{BitLockerSessionBridge, ServerUiBridge};
use crate::session::search_index::SessionSearchIndex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactSessionError {
    AlreadyCompacted(String),
    NothingToCompact(String),
    Busy(String),
    Failed(String),
}

impl CompactSessionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AlreadyCompacted(_) => "already_compacted",
            Self::NothingToCompact(_) => "nothing_to_compact",
            Self::Busy(_) => "conflict",
            Self::Failed(_) => "invalid_request",
        }
    }

    pub fn status_code(&self) -> u16 {
        match self {
            Self::AlreadyCompacted(_) | Self::NothingToCompact(_) | Self::Busy(_) => 409,
            Self::Failed(_) => 400,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::AlreadyCompacted(m)
            | Self::NothingToCompact(m)
            | Self::Busy(m)
            | Self::Failed(m) => m.as_str(),
        }
    }
}

/// Tracks durable inbox input acceptance directly from `AgentSession` entries and events,
/// independent of UI `MessageDecorators`.
#[derive(Clone, Default)]
struct AcceptedInputTracker {
    inner: Arc<Mutex<AcceptedInputState>>,
}

#[derive(Default)]
struct AcceptedInputState {
    pending: VecDeque<String>,
    accepted: HashSet<String>,
}

impl AcceptedInputTracker {
    fn from_session(session: &AgentSession) -> Self {
        let mut accepted = HashSet::new();
        let entries = session.with_session_manager(|sm| sm.get_entries());
        for entry in entries {
            if let SessionEntry::Custom(custom) = entry
                && custom.custom_type == INPUT_MESSAGE_CUSTOM_TYPE
                && let Some(id) = custom
                    .data
                    .as_ref()
                    .and_then(|d| d.get("id"))
                    .and_then(Value::as_str)
            {
                accepted.insert(id.to_owned());
            }
        }
        Self {
            inner: Arc::new(Mutex::new(AcceptedInputState {
                pending: VecDeque::new(),
                accepted,
            })),
        }
    }

    fn on_event(&self, event: &AgentSessionEvent) {
        match event {
            AgentSessionEvent::MessageStart {
                message: AgentMessage::User(_),
            } => {
                let mut guard = self.inner.lock().expect("accepted_inputs lock");
                if let Some(id) = guard.pending.pop_front() {
                    guard.accepted.insert(id);
                }
            }
            AgentSessionEvent::AgentEnd { .. } => {
                self.inner
                    .lock()
                    .expect("accepted_inputs lock")
                    .pending
                    .clear();
            }
            _ => {}
        }
    }

    async fn track<T, F, Fut>(&self, id: &str, deferred: bool, action: F) -> Result<T, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, String>>,
    {
        self.inner
            .lock()
            .expect("accepted_inputs lock")
            .pending
            .push_back(id.to_owned());
        let res = action().await;
        if res.is_err() || !deferred {
            let mut guard = self.inner.lock().expect("accepted_inputs lock");
            if let Some(pos) = guard.pending.iter().position(|item| item == id) {
                guard.pending.remove(pos);
            }
        }
        res
    }

    fn contains(&self, id: &str) -> bool {
        self.inner
            .lock()
            .expect("accepted_inputs lock")
            .accepted
            .contains(id)
    }
}

struct SlotInboxExecutor {
    session_id: String,
    hosted: Arc<HostedSession>,
    decorators: MessageDecorators,
    accepted_inputs: AcceptedInputTracker,
    images: ImageStore,
    global_events: SessionEvents,
    invalid_auth: Arc<Mutex<BTreeSet<String>>>,
}

struct AttemptListener(Option<Box<dyn FnOnce() + Send + Sync>>);

impl Drop for AttemptListener {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self.0.take() {
            unsubscribe();
        }
    }
}

fn record_attempt_error(error: &mut Option<String>, message: &AgentMessage) {
    if let AgentMessage::Assistant(assistant) = message {
        *error = if assistant.stop_reason == StopReason::Error {
            assistant.error_message.clone()
        } else {
            None
        };
    }
}

impl SlotInboxExecutor {
    async fn load_images(&self, refs: &[ImageRef]) -> Result<Vec<ImageContent>, String> {
        let mut out = Vec::with_capacity(refs.len());
        for r in refs {
            let bytes = self
                .images
                .read(&self.session_id, &r.id)
                .await
                .map_err(|e| e.to_string())?;
            out.push(ImageContent {
                mime_type: r.mime_type.clone(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
        Ok(out)
    }

    fn format_skill_message(text: &str, skill: Option<&str>) -> String {
        match skill {
            Some(s) if text.is_empty() => format!("/skill:{s}"),
            Some(s) => format!("/skill:{s} {text}"),
            None => text.to_owned(),
        }
    }

    fn is_auth_failure_message(msg: &str) -> bool {
        let lower = msg.to_ascii_lowercase();
        lower.contains("401")
            || lower.contains("unauthorized")
            || lower.contains("invalid api key")
            || lower.contains("invalid_api_key")
            || lower.contains("authentication failed")
            || lower.contains("authentication_failed")
    }

    fn check_and_mark_auth_error(&self, provider: String, error_text: Option<&str>) {
        if error_text.is_some_and(Self::is_auth_failure_message) {
            self.invalid_auth
                .lock()
                .expect("invalid_auth lock")
                .insert(provider.clone());
            self.global_events.publish(
                "state",
                &json!({
                    "type": "auth_changed",
                    "providerId": provider,
                }),
            );
        }
    }
}

impl InboxExecutor for SlotInboxExecutor {
    fn is_busy(&self) -> bool {
        !self.hosted.session().is_idle()
    }

    fn prompt(
        &self,
        text: String,
        images: Vec<ImageRef>,
        skill: Option<String>,
        message_id: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session_id = self.session_id.clone();
        let hosted = self.hosted.clone();
        let decorators = self.decorators.clone();
        let accepted_inputs = self.accepted_inputs.clone();
        let images_store = self.images.clone();
        let global_events = self.global_events.clone();
        let invalid_auth = self.invalid_auth.clone();
        Box::pin(async move {
            let exec = SlotInboxExecutor {
                session_id,
                hosted: hosted.clone(),
                decorators: decorators.clone(),
                accepted_inputs: accepted_inputs.clone(),
                images: images_store,
                global_events,
                invalid_auth,
            };
            let session = hosted.session();
            let selected = session.model();
            if let Some(latest) = hosted
                .auth
                .runtime()
                .get_model(&selected.provider, &selected.id)
                && (latest.base_url != selected.base_url || latest.api != selected.api)
            {
                let _ = session.set_model(latest).await;
            }
            let image_blocks = exec.load_images(&images).await?;
            let formatted = Self::format_skill_message(&text, skill.as_deref());
            let has_skill = skill.is_some();
            let session_clone = session.clone();
            let msg_id_for_dec = message_id.clone();
            let attempt_error = Arc::new(Mutex::new(None));
            let seen_error = attempt_error.clone();
            let listener = AttemptListener(Some(session.subscribe(Arc::new(move |event| {
                if let AgentSessionEvent::MessageEnd { message } = event {
                    record_attempt_error(
                        &mut seen_error.lock().expect("attempt error lock"),
                        &message,
                    );
                }
            }))));
            let res = accepted_inputs
                .track(&message_id, false, || async move {
                    decorators
                        .track_input(&msg_id_for_dec, false, move || async move {
                            session_clone
                                .prompt(
                                    &formatted,
                                    Some(PromptOptions {
                                        expand_prompt_templates: Some(has_skill),
                                        images: Some(image_blocks),
                                        source: Some(InputSource::Rpc),
                                        ..Default::default()
                                    }),
                                )
                                .await
                                .map(|_| ())
                        })
                        .await
                })
                .await;

            drop(listener);
            let attempt_error = attempt_error.lock().expect("attempt error lock");
            exec.check_and_mark_auth_error(
                selected.provider,
                attempt_error
                    .as_deref()
                    .or_else(|| res.as_ref().err().map(String::as_str)),
            );
            match res {
                Ok(()) => Ok(()),
                Err(err) => {
                    // If the user message was already persisted into the session, treat model error as `delivered`.
                    if accepted_inputs.contains(&message_id) {
                        Ok(())
                    } else {
                        Err(err)
                    }
                }
            }
        })
    }

    fn steer(
        &self,
        text: String,
        images: Vec<ImageRef>,
        skill: Option<String>,
        message_id: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session_id = self.session_id.clone();
        let hosted = self.hosted.clone();
        let decorators = self.decorators.clone();
        let accepted_inputs = self.accepted_inputs.clone();
        let images_store = self.images.clone();
        let global_events = self.global_events.clone();
        let invalid_auth = self.invalid_auth.clone();
        Box::pin(async move {
            let exec = SlotInboxExecutor {
                session_id,
                hosted: hosted.clone(),
                decorators: decorators.clone(),
                accepted_inputs: accepted_inputs.clone(),
                images: images_store,
                global_events,
                invalid_auth,
            };
            let image_blocks = exec.load_images(&images).await?;
            let session_clone = hosted.session().clone();
            let msg_id_for_dec = message_id.clone();
            accepted_inputs
                .track(&message_id, true, || async move {
                    decorators
                        .track_input(&msg_id_for_dec, true, move || async move {
                            if let Some(ref s) = skill {
                                let formatted = Self::format_skill_message(&text, Some(s));
                                session_clone
                                    .prompt(
                                        &formatted,
                                        Some(PromptOptions {
                                            expand_prompt_templates: Some(true),
                                            images: Some(image_blocks),
                                            streaming_behavior: Some(StreamingInputBehavior::Steer),
                                            source: Some(InputSource::Rpc),
                                            ..Default::default()
                                        }),
                                    )
                                    .await
                                    .map(|_| ())
                            } else {
                                session_clone.steer(&text, Some(image_blocks)).await
                            }
                        })
                        .await
                })
                .await
        })
    }

    fn was_accepted(&self, id: &str) -> bool {
        self.accepted_inputs.contains(id)
    }
}

type UnsubscribeFn = Box<dyn FnOnce() + Send + Sync>;

/// Keeps a loaded session usable and out of LRU eviction for the handle's lifetime.
pub struct SessionLease {
    slot: Option<Arc<SessionSlot>>,
    on_release: Arc<dyn Fn() + Send + Sync>,
}

impl std::ops::Deref for SessionLease {
    type Target = SessionSlot;

    fn deref(&self) -> &Self::Target {
        self.slot.as_deref().expect("live session lease")
    }
}

impl Clone for SessionLease {
    fn clone(&self) -> Self {
        self.slot
            .as_ref()
            .expect("live session lease")
            .lease(self.on_release.clone())
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        let slot = self.slot.take().expect("live session lease");
        let last = slot.active_leases.fetch_sub(1, Ordering::SeqCst) == 1;
        // Release the raw Arc before scheduling eviction, including its owner lock.
        drop(slot);
        if last {
            (self.on_release)();
        }
    }
}

/// Single active session instance inside `SessionPool`.
pub struct SessionSlot {
    pub id: String,
    pub hosted: Arc<HostedSession>,
    _owner_lock: Arc<SessionOwnerLock>,
    pub decorators: MessageDecorators,
    pub inbox: SessionInbox,
    pub events: SessionEvents,
    pub ui: ServerUiBridge,
    pub bitlocker: BitLockerSessionBridge,
    last_touched: AtomicU64,
    active_leases: AtomicUsize,
    unsubscribers: Mutex<Vec<UnsubscribeFn>>,
}

impl SessionSlot {
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        id: String,
        hosted: HostedSession,
        owner_lock: SessionOwnerLock,
        ui: ServerUiBridge,
        bitlocker: BitLockerSessionBridge,
        inbox_root: &Path,
        images: ImageStore,
        global_events: SessionEvents,
        invalid_auth: Arc<Mutex<BTreeSet<String>>>,
        search_index: SessionSearchIndex,
        on_flushed: Arc<dyn Fn(&str) + Send + Sync>,
        initial_touch: u64,
    ) -> Result<Arc<Self>, String> {
        let session = hosted.session().clone();
        bitlocker.bind_session(session.clone());
        let accepted_inputs = AcceptedInputTracker::from_session(&session);
        let decorators = MessageDecorators::attach(session.clone());
        let events = SessionEvents::new();
        let hosted = Arc::new(hosted);

        let events_for_ui = events.clone();
        ui.set_on_change(move |questions| {
            events_for_ui.publish(
                "session",
                &json!({
                    "type": "ui_requests",
                    "questions": questions,
                }),
            );
        });

        let events_for_bitlocker = events.clone();
        bitlocker.set_on_change(move |snapshot| {
            events_for_bitlocker.publish(
                "session",
                &BitLockerSessionBridge::to_sse_event_value(&snapshot),
            );
        });

        let executor = Arc::new(SlotInboxExecutor {
            session_id: id.clone(),
            hosted: hosted.clone(),
            decorators: decorators.clone(),
            accepted_inputs: accepted_inputs.clone(),
            images,
            global_events: global_events.clone(),
            invalid_auth,
        });

        let events_for_queue = events.clone();
        let inbox = SessionInbox::new(
            inbox_root,
            id.clone(),
            executor,
            Some(Arc::new(move |queue_snap| {
                if let Ok(val) = serde_json::to_value(queue_snap) {
                    events_for_queue.publish("queue", &val);
                }
            })),
        )
        .map_err(|e| e.to_string())?;

        let events_for_session = events.clone();
        let decorators_for_session = decorators.clone();
        let accepted_for_session = accepted_inputs;
        let global_for_session = global_events;
        let hosted_for_session = hosted.clone();
        let inbox_for_session = inbox.clone();
        let id_for_session = id.clone();
        let search_for_session = search_index;

        let unsub = session.subscribe(Arc::new(move |event| {
            accepted_for_session.on_event(&event);
            if let Some(projected) = decorators_for_session.project_browser_event(&event) {
                events_for_session.publish("session", &projected);
            }
            let is_non_visible_message_lifecycle = matches!(
                &event,
                AgentSessionEvent::MessageStart { message }
                    | AgentSessionEvent::MessageEnd { message }
                    if !is_browser_visible_message(message)
            );

            let is_now_flushed = hosted_for_session
                .session()
                .with_session_manager(|sm| sm.is_flushed());
            if is_now_flushed {
                on_flushed(&id_for_session);
            }

            if !is_non_visible_message_lifecycle
                && matches!(
                    event,
                    AgentSessionEvent::MessageEnd { .. }
                        | AgentSessionEvent::AgentEnd { .. }
                        | AgentSessionEvent::CompactionEnd { .. }
                        | AgentSessionEvent::SessionInfoChanged { .. }
                )
            {
                hosted_for_session.session().with_session_manager(|sm| {
                    search_for_session.upsert_from_manager(sm);
                });
            }

            match &event {
                AgentSessionEvent::AgentStart | AgentSessionEvent::AgentSettled { .. } => {
                    let inbox_wake = inbox_for_session.clone();
                    tokio::spawn(async move {
                        tokio::task::yield_now().await;
                        inbox_wake.wake();
                    });
                }
                _ => {}
            }

            if matches!(
                event,
                AgentSessionEvent::AgentEnd { .. } | AgentSessionEvent::CompactionEnd { .. }
            ) || matches!(
                &event,
                AgentSessionEvent::EntryAppended {
                    entry: SessionEntry::ContextEdit(_)
                }
            ) {
                global_for_session.publish(
                    "state",
                    &json!({
                        "type": "usage_changed",
                        "sessionId": id_for_session,
                    }),
                );
            }
            if matches!(event, AgentSessionEvent::AgentEnd { .. }) {
                let provider = hosted_for_session.session().model().provider;
                if hosted_for_session
                    .auth
                    .invalidate_provider_account(&provider)
                {
                    global_for_session.publish(
                        "state",
                        &json!({
                            "type": "quota_changed",
                            "providerId": provider,
                        }),
                    );
                }
            }

            if !is_non_visible_message_lifecycle
                && matches!(
                    event,
                    AgentSessionEvent::MessageStart { .. }
                        | AgentSessionEvent::MessageEnd { .. }
                        | AgentSessionEvent::AgentStart
                        | AgentSessionEvent::AgentEnd { .. }
                        | AgentSessionEvent::SessionInfoChanged { .. }
                )
            {
                global_for_session.publish(
                    "state",
                    &json!({
                        "type": "sessions_changed",
                        "sessionId": id_for_session,
                    }),
                );
            }
        }));

        Ok(Arc::new(Self {
            id,
            hosted,
            _owner_lock: Arc::new(owner_lock),
            decorators,
            inbox,
            events,
            ui,
            bitlocker,
            last_touched: AtomicU64::new(initial_touch),
            active_leases: AtomicUsize::new(0),
            unsubscribers: Mutex::new(vec![unsub]),
        }))
    }

    pub fn session(&self) -> &AgentSession {
        self.hosted.session()
    }

    pub(crate) fn lease(self: &Arc<Self>, on_release: Arc<dyn Fn() + Send + Sync>) -> SessionLease {
        self.active_leases.fetch_add(1, Ordering::SeqCst);
        SessionLease {
            slot: Some(self.clone()),
            on_release,
        }
    }

    pub fn touch(&self, seq: u64) {
        self.last_touched.store(seq, Ordering::SeqCst);
    }

    pub fn last_touched(&self) -> u64 {
        self.last_touched.load(Ordering::SeqCst)
    }

    pub fn is_flushed(&self) -> bool {
        self.session().with_session_manager(|sm| sm.is_flushed())
    }

    /// Computes this session's canonical display title matching `SessionPool::list()`:
    /// explicit trimmed `SessionInfo.name` -> chronological first user message across all JSONL entries -> `"新对话"`.
    pub fn display_title(&self) -> String {
        let session = self.session();
        if let Some(name) = session.session_name() {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
        let first_text = session.with_session_manager(|sm| {
            sm.get_entries().into_iter().find_map(|entry| {
                let pi_session::SessionEntry::Message(m) = entry else {
                    return None;
                };
                let AgentMessage::User(u) = m.message else {
                    return None;
                };
                let raw = match u.content {
                    pi_ai::UserContent::Text(s) => s,
                    pi_ai::UserContent::Blocks(blocks) => blocks
                        .into_iter()
                        .filter_map(|b| match b {
                            pi_ai::UserContentBlock::Text(tc) => Some(tc.text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                };
                (!raw.is_empty() && !raw.trim().is_empty())
                    .then(|| raw.chars().take(160).collect::<String>())
            })
        });
        first_text.unwrap_or_else(|| "新对话".to_owned())
    }

    /// A retained session handle or any of these four conditions prevents LRU eviction:
    /// 1. Model, tool, or compaction is running (`!is_idle() || is_streaming() || is_compacting()`).
    /// 2. `SessionInbox` has `Pending` or `Delivering` items.
    /// 3. `ServerUiBridge` or `BitLockerSessionBridge` has pending unanswered requests.
    /// 4. `SessionEvents` has >= 1 active SSE subscriber.
    pub async fn is_pinned(&self) -> bool {
        if self.active_leases.load(Ordering::SeqCst) > 0 {
            return true;
        }
        let session = self.session();
        if !session.is_idle() || session.is_streaming() || session.is_compacting() {
            return true;
        }
        if self.ui.has_pending() || self.bitlocker.has_pending() {
            return true;
        }
        if self.events.active_subscriber_count() > 0 {
            return true;
        }
        self.inbox.has_active_or_pending().await
    }

    pub async fn compact(&self, instructions: Option<String>) -> Result<(), CompactSessionError> {
        let session = self.session();
        if !session.is_idle() || session.is_streaming() || session.is_compacting() {
            return Err(CompactSessionError::Busy(
                "会话正在执行，请等待完成后再压缩".to_owned(),
            ));
        }
        match session.compact(instructions).await {
            Ok(_) => {
                self.events
                    .publish("session", &json!({ "type": "session_tree" }));
                Ok(())
            }
            Err(err) if err.contains("Already compacted") => {
                Err(CompactSessionError::AlreadyCompacted(
                    "会话已压缩，继续对话后可再次压缩".to_owned(),
                ))
            }
            Err(err) if err.contains("Nothing to compact") => Err(
                CompactSessionError::NothingToCompact("当前没有可压缩的较早内容".to_owned()),
            ),
            Err(err) => Err(CompactSessionError::Failed(err)),
        }
    }

    /// Builds the full `GET /api/sessions/:id` snapshot, decorated and sanitized via `browser_json_value`.
    pub async fn full_snapshot(&self) -> Result<Value, InboxError> {
        let pre_seq = self.events.current_sequence();
        let event_cursor = format!("{}:{pre_seq}", self.events.epoch());
        let session = self.session();
        let state = session.state();
        let messages: Vec<Value> = state
            .messages
            .iter()
            .filter(|msg| is_browser_visible_message(msg))
            .map(|msg| self.decorators.decorate_message_for_snapshot(msg))
            .collect();

        let (branch_entries, leaf_id, context_usage) =
            session.with_context_branch(|leaf, branch, context| {
                (branch, leaf.map(str::to_owned), context)
            });
        let usage = crate::session::usage::compute_branch_usage(&branch_entries);
        let mut entries_json = Vec::new();
        for entry in branch_entries {
            match entry {
                SessionEntry::Message(m) => {
                    if !is_browser_visible_message(&m.message) {
                        continue;
                    }
                    entries_json.push(json!({
                        "id": m.id,
                        "parentId": m.parent_id,
                        "message": self.decorators.decorate_message_for_snapshot(&m.message),
                    }));
                }
                SessionEntry::BranchSummary(bs) => {
                    let ts = parse_iso_timestamp_ms(&bs.timestamp).unwrap_or(0);
                    entries_json.push(json!({
                        "id": bs.id,
                        "parentId": bs.parent_id,
                        "message": {
                            "role": "branchSummary",
                            "parentId": bs.parent_id,
                            "summary": bs.summary,
                            "fromId": bs.from_id,
                            "timestamp": ts,
                        }
                    }));
                }
                SessionEntry::Compaction(c) => {
                    let ts = parse_iso_timestamp_ms(&c.timestamp).unwrap_or(0);
                    entries_json.push(json!({
                        "id": c.id,
                        "parentId": c.parent_id,
                        "message": {
                            "role": "compactionSummary",
                            "summary": c.summary,
                            "tokensBefore": c.tokens_before,
                            "firstKeptEntryId": c.first_kept_entry_id,
                            "timestamp": ts,
                        }
                    }));
                }
                _ => {}
            }
        }

        let queue = self.inbox.snapshot().await?;
        let raw = json!({
            "id": self.id,
            "eventCursor": event_cursor,
            "leafId": leaf_id,
            "bitlocker": self.bitlocker.snapshot(),
            "messages": messages,
            "entries": entries_json,
            "running": session.is_streaming(),
            "compacting": session.is_compacting(),
            "contextUsage": context_usage,
            "usage": usage,
            "queue": queue,
            "ui": self.ui.snapshot(),
        });
        Ok(browser_json_value(&raw))
    }

    pub async fn close(&self) {
        self.inbox.close().await;
        let unsubs: Vec<UnsubscribeFn> = {
            let mut guard = self.unsubscribers.lock().expect("unsubscribers lock");
            std::mem::take(&mut *guard)
        };
        for unsub in unsubs {
            unsub();
        }
        self.decorators.close();
        self.ui.close();
        self.bitlocker.close();
        self.events.close_all();
        let _ = self.hosted.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{SlotInboxExecutor, record_attempt_error};
    use pi_agent::AgentMessage;
    use pi_ai::{AssistantMessage, StopReason};
    use serde_json::json;

    fn make_assistant(stop_reason: StopReason, error_message: Option<&str>) -> AgentMessage {
        let mut msg: AssistantMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "api": "openai-completions",
            "provider": "deepseek",
            "model": "deepseek-flash",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}
            },
            "stopReason": "stop",
            "timestamp": 1
        }))
        .expect("assistant message fixture");
        msg.stop_reason = stop_reason;
        msg.error_message = error_message.map(str::to_owned);
        AgentMessage::Assistant(msg)
    }

    #[test]
    fn auth_failure_does_not_cross_attempts() {
        let mut previous = None;
        record_attempt_error(
            &mut previous,
            &make_assistant(StopReason::Error, Some("401 Unauthorized")),
        );
        assert!(
            previous
                .as_deref()
                .is_some_and(SlotInboxExecutor::is_auth_failure_message)
        );

        let mut current = None;
        assert_eq!(current, None); // No assistant event in the new attempt.
        record_attempt_error(&mut current, &make_assistant(StopReason::Stop, None));
        assert_eq!(current, None);
    }
}

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use cst_pilot::HostedSession;
use pi_agent::{AgentMessage, BoxFuture};
use pi_ai::{ImageContent, StopReason};
use pi_session::{
    AgentSession, AgentSessionEvent, InputSource, PromptOptions, SessionEntry,
    StreamingInputBehavior, parse_iso_timestamp_ms,
};
use serde_json::{Value, json};

use crate::discovery::SessionOwnerLock;
use crate::session::attachments::{ImageRef, ImageStore};
use crate::session::decorators::MessageDecorators;
use crate::session::events::{SessionEvents, browser_json_value};
use crate::session::inbox::{InboxError, InboxExecutor, SessionInbox};
use crate::session::interactions::{BitLockerSessionBridge, ServerUiBridge};

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

struct SlotInboxExecutor {
    session_id: String,
    hosted: Arc<HostedSession>,
    decorators: MessageDecorators,
    images: ImageStore,
    global_events: SessionEvents,
    invalid_auth: Arc<Mutex<BTreeSet<String>>>,
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

    fn check_and_mark_auth_error(&self, error_text: Option<&str>) {
        let session = self.hosted.session();
        let provider = session.model().provider;
        let last_assistant_err = session.state().messages.iter().rev().find_map(|m| match m {
            AgentMessage::Assistant(a) if a.stop_reason == StopReason::Error => {
                a.error_message.clone()
            }
            _ => None,
        });
        let combined = error_text
            .map(str::to_owned)
            .or(last_assistant_err)
            .unwrap_or_default();
        if !combined.is_empty() && Self::is_auth_failure_message(&combined) {
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
        let images_store = self.images.clone();
        let global_events = self.global_events.clone();
        let invalid_auth = self.invalid_auth.clone();
        Box::pin(async move {
            let exec = SlotInboxExecutor {
                session_id,
                hosted: hosted.clone(),
                decorators: decorators.clone(),
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
            let res = decorators
                .track_input(&message_id, false, move || async move {
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
                .await;

            match res {
                Ok(()) => {
                    exec.check_and_mark_auth_error(None);
                    Ok(())
                }
                Err(err) => {
                    exec.check_and_mark_auth_error(Some(&err));
                    // If the user message was already persisted into the session, treat model error as `delivered`.
                    if decorators.has_input_message(&message_id) {
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
        let images_store = self.images.clone();
        let global_events = self.global_events.clone();
        let invalid_auth = self.invalid_auth.clone();
        Box::pin(async move {
            let exec = SlotInboxExecutor {
                session_id,
                hosted: hosted.clone(),
                decorators: decorators.clone(),
                images: images_store,
                global_events,
                invalid_auth,
            };
            let image_blocks = exec.load_images(&images).await?;
            let session_clone = hosted.session().clone();
            decorators
                .track_input(&message_id, true, move || async move {
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
    }

    fn was_accepted(&self, id: &str) -> bool {
        self.decorators.has_input_message(id)
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
        on_flushed: Arc<dyn Fn(&str) + Send + Sync>,
        initial_touch: u64,
    ) -> Result<Arc<Self>, String> {
        let session = hosted.session().clone();
        bitlocker.bind_session(session.clone());
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
        let global_for_session = global_events;
        let hosted_for_session = hosted.clone();
        let inbox_for_session = inbox.clone();
        let id_for_session = id.clone();

        let unsub = session.subscribe(Arc::new(move |event| {
            let decorated = decorators_for_session.decorate_event(&event);
            events_for_session.publish("session", &decorated);

            let is_now_flushed = hosted_for_session
                .session()
                .with_session_manager(|sm| sm.is_flushed());
            if is_now_flushed {
                on_flushed(&id_for_session);
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

            if matches!(event, AgentSessionEvent::AgentEnd { .. }) {
                global_for_session.publish(
                    "state",
                    &json!({
                        "type": "usage_changed",
                        "sessionId": id_for_session,
                    }),
                );
                if hosted_for_session.session().model().provider == "cstoa" {
                    hosted_for_session.auth.invalidate_account();
                    global_for_session.publish(
                        "state",
                        &json!({
                            "type": "quota_changed",
                            "providerId": "cstoa",
                        }),
                    );
                }
            }

            if matches!(
                event,
                AgentSessionEvent::MessageStart { .. }
                    | AgentSessionEvent::MessageEnd { .. }
                    | AgentSessionEvent::AgentStart
                    | AgentSessionEvent::AgentEnd { .. }
                    | AgentSessionEvent::SessionInfoChanged { .. }
            ) {
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
            Ok(_) => Ok(()),
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
        let session = self.session();
        let state = session.state();
        let messages: Vec<Value> = state
            .messages
            .iter()
            .map(|msg| self.decorators.decorate_message_for_snapshot(msg))
            .collect();

        let branch_entries = session.with_session_manager(|sm| sm.get_branch(None));
        let mut entries_json = Vec::new();
        for entry in branch_entries {
            match entry {
                SessionEntry::Message(m) => {
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
                        "message": {
                            "role": "branchSummary",
                            "parentId": bs.parent_id,
                            "summary": bs.summary,
                            "fromId": bs.from_id,
                            "timestamp": ts,
                        }
                    }));
                }
                _ => {}
            }
        }

        let context_usage = session.get_context_usage();
        let queue = self.inbox.snapshot().await?;
        let raw = json!({
            "id": self.id,
            "bitlocker": self.bitlocker.snapshot(),
            "messages": messages,
            "entries": entries_json,
            "running": session.is_streaming(),
            "contextUsage": context_usage,
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

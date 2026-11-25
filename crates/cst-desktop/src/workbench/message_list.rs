use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use chrono::{DateTime, Local};
use cst_client::tool_presenter::{ToolCallInput, ToolExecutionMeta, ToolResultInput};
use cst_client::{
    ClientError, ClientHandle, ComposerDraft, DraftKey, OutboxItem, OutboxItemState,
    SessionViewSnapshot,
};
use gpui::{
    EntityInputHandler as _, Image, ImageSource, KeyDownEvent, ObjectFit, ScrollEvent,
    ScrollHandle, StyledImage as _,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::base::text::TextViewStyle;
use gpui_kit::component::input::{Enter, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, Focusable as _,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, TestSupportExt, Window, div, img, px,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::accessibility::A11yElementExt as _;
use crate::theme::motion::{DesktopTooltipExt as _, IconPressExt as _};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, motion,
    radius, scale, space,
};
use crate::tools::bitlocker_card::{
    PendingBitLockerPrompt, SecretInput, SecretInputEvent, find_pending_bitlocker_request,
};
use crate::tools::normal_card::{render_normal_tool_card_with_args, render_normal_tool_row_inert};
use crate::tools::{SpecialToolDispatchProps, render_special_tool_component};
use crate::workbench::image_viewer::{
    LightboxImageTarget, gpui_image_from_bytes, gpui_image_from_handle,
};
use crate::workbench::markdown_view::{
    CopyFeedbackTracker, MarkdownStreamStore, build_markdown_style, render_copyable_text_block,
    render_copyable_text_block_inert,
};
use crate::workbench::turn_segments::{
    ConversationPart, ImageContentPart, SpecialToolKind, SplitTurnOutput, ToolCallPart, Turn,
    TurnEntry, TurnPart, TurnSegment, TurnSegmentKind, build_turn_segments_with_results,
    extract_message_text, extract_user_images, format_duration_zh, format_live_duration_zh,
    group_turns_and_results, is_standalone_tool, process_segment_label,
    render_running_light_label_with_scale, segment_is_running, split_turn,
};

pub const DEFAULT_VISIBLE_TURN_WINDOW: usize = 30;

#[derive(Debug, Clone)]
pub enum MessageListEvent {
    OpenLightbox(LightboxImageTarget),
    SwitchToSession(String),
    FocusComposer,
    RequestEditOutboxItem {
        session_id: String,
        client_message_id: String,
    },
    MissingCredentials {
        provider: String,
    },
    ErrorMessage(String),
    TopNotice(String),
}

struct InlineBubbleEdit {
    session_id: String,
    turn_key: String,
    user_entry_id: String,
    original_text: String,
    user_images: Vec<ImageContentPart>,
    textarea: Entity<TextareaState>,
    _subscription: Subscription,
}

/// Estimates inline-editing user bubble dimensions `(bubble_width_px, text_height_px)` from live text
/// so the bubble adapts smoothly while keeping its right-middle edge anchored.
fn estimate_inline_bubble_size(text: &str, has_images: bool) -> (f32, f32) {
    const MAX_BUBBLE_W: f32 = 560.0;
    const H_PAD: f32 = 32.0; // `px(space::LG)` = 16 * 2
    const MAX_INNER_W: f32 = MAX_BUBBLE_W - H_PAD;
    const LINE_H: f32 = 24.0; // `scale::BODY` line height

    let mut max_line_w: f32 = 0.0;
    let mut wrapped_lines: usize = 0;

    for raw_line in text.split('\n') {
        let mut line_w: f32 = 0.0;
        for ch in raw_line.chars() {
            line_w += if !ch.is_ascii() {
                16.2
            } else if matches!(
                ch,
                'i' | 'l' | '1' | '.' | ',' | ':' | ';' | '\'' | '!' | ' '
            ) {
                5.8
            } else if ch.is_ascii_uppercase() || matches!(ch, 'W' | 'M' | 'm' | 'w') {
                11.0
            } else {
                9.2
            };
        }
        max_line_w = max_line_w.max(line_w.min(MAX_INNER_W));
        let visual_lines = ((line_w + 10.0) / MAX_INNER_W).ceil().max(1.0) as usize;
        wrapped_lines += visual_lines;
    }

    let min_w = if has_images { 168.0 } else { 72.0 };
    let target_w = (max_line_w + 14.0 + H_PAD).clamp(min_w, MAX_BUBBLE_W);
    let clamped_lines = wrapped_lines.clamp(1, 14);
    let text_h = (clamped_lines as f32) * LINE_H;
    (target_w, text_h)
}

pub(crate) fn is_real_entry_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with("entry-") && !id.starts_with("stream-")
}

pub(crate) fn latest_forkable_entry_id(session: &SessionViewSnapshot) -> Option<String> {
    session.entries.iter().rev().find_map(|entry| {
        let message = entry.get("message").unwrap_or(entry);
        let id = entry.get("id")?.as_str()?;
        (is_real_entry_id(id)
            && message["role"] == "assistant"
            && message["stopReason"]
                .as_str()
                .is_some_and(|reason| reason != "toolUse"))
        .then(|| id.to_owned())
    })
}

struct PendingSecretInput {
    entity: Entity<SecretInput>,
    request_id: String,
    seen: bool,
    busy: bool,
    _subscription: Subscription,
}

type ProcessExpansionKey = (String, (usize, usize));
type GroupedTurns = (Vec<Turn>, BTreeMap<String, Value>);

struct GroupedTurnCache {
    // Keeping a Weak prevents address reuse and makes Arc::make_mut change identity
    // even if the store has the only strong reference. It doesn't pin the JSON.
    source: Weak<Vec<Value>>,
    grouped: Arc<GroupedTurns>,
    prepared_base: Arc<Vec<Arc<PreparedTurn>>>,
    generation: u64,
}

struct PreparedTurn {
    turn: Turn,
    split: Option<SplitTurnOutput>,
    segments: Vec<TurnSegment>,
}

impl PreparedTurn {
    fn from_turn(turn: Turn, tool_results: &BTreeMap<String, Value>) -> Self {
        if turn.summary.is_some() || turn.compaction.is_some() || turn.assistants.is_empty() {
            return Self {
                turn,
                split: None,
                segments: Vec::new(),
            };
        }
        let split = split_turn(&turn.assistants);
        let segments = build_turn_segments_with_results(split.process.clone(), tool_results);
        Self {
            turn,
            split: Some(split),
            segments,
        }
    }
}

struct PreparedSessionData {
    turns: Vec<Arc<PreparedTurn>>,
    tool_results: Arc<BTreeMap<String, Value>>,
    messages_generation: u64,
}

struct PreparedSessionCache {
    session_id: String,
    revision: u64,
    running: bool,
    messages_generation: u64,
    data: Arc<PreparedSessionData>,
}

impl GroupedTurnCache {
    #[cfg_attr(not(test), allow(dead_code))]
    fn for_messages(cache: &mut Option<Self>, messages: &Arc<Vec<Value>>) -> Arc<GroupedTurns> {
        let mut dummy_gen = cache.as_ref().map_or(0, |c| c.generation);
        let (grouped, _, _) = Self::sync_messages(cache, &mut dummy_gen, messages);
        grouped
    }

    fn sync_messages(
        cache: &mut Option<Self>,
        next_gen: &mut u64,
        messages: &Arc<Vec<Value>>,
    ) -> (Arc<GroupedTurns>, Arc<Vec<Arc<PreparedTurn>>>, u64) {
        if let Some(cached) = cache
            && std::ptr::eq(cached.source.as_ptr(), Arc::as_ptr(messages))
        {
            return (
                Arc::clone(&cached.grouped),
                Arc::clone(&cached.prepared_base),
                cached.generation,
            );
        }
        let grouped = Arc::new(group_turns_and_results(messages));
        let (turns, tool_results) = grouped.as_ref();
        let prepared_base = Arc::new(
            turns
                .iter()
                .cloned()
                .map(|turn| Arc::new(PreparedTurn::from_turn(turn, tool_results)))
                .collect(),
        );
        *next_gen = next_gen.wrapping_add(1).max(1);
        let generation = *next_gen;
        *cache = Some(Self {
            source: Arc::downgrade(messages),
            grouped: Arc::clone(&grouped),
            prepared_base: Arc::clone(&prepared_base),
            generation,
        });
        (grouped, prepared_base, generation)
    }
}

fn prepare_session_turns(
    grouped_cache: &mut Option<GroupedTurnCache>,
    next_messages_gen: &mut u64,
    prepared_cache: &mut Option<PreparedSessionCache>,
    session: &SessionViewSnapshot,
) -> Arc<PreparedSessionData> {
    let source_entries = if !session.entries.is_empty() {
        &session.entries
    } else {
        &session.messages
    };
    let (grouped, prepared_base, messages_gen) =
        GroupedTurnCache::sync_messages(grouped_cache, next_messages_gen, source_entries);

    if let Some(cached) = prepared_cache
        && cached.session_id == session.id
        && cached.revision == session.revision
        && cached.running == session.running
        && cached.messages_generation == messages_gen
    {
        return Arc::clone(&cached.data);
    }

    let (_, base_tool_results) = grouped.as_ref();
    let mut prepared_turns: Vec<Arc<PreparedTurn>> = prepared_base.as_ref().clone();

    if session.running
        && let Some(ref streaming) = session.streaming_assistant
    {
        let stream_entry = TurnEntry::from_json(streaming, source_entries.len());
        if stream_entry.role == "assistant" {
            let stream_id = streaming
                .get("id")
                .or_else(|| streaming.get("message").and_then(|m| m.get("id")))
                .and_then(Value::as_str);
            let mut updated_last = false;
            if let Some(last_arc) = prepared_turns.last()
                && last_arc.turn.summary.is_none()
                && last_arc.turn.compaction.is_none()
            {
                let mut tail_turn = last_arc.turn.clone();
                if let Some(sid_str) = stream_id
                    && let Some(last_asst) = tail_turn.assistants.last_mut()
                    && last_asst.id.as_deref() == Some(sid_str)
                {
                    *last_asst = stream_entry.clone();
                } else {
                    tail_turn.assistants.push(stream_entry.clone());
                }
                *prepared_turns.last_mut().expect("non-empty turns") =
                    Arc::new(PreparedTurn::from_turn(tail_turn, base_tool_results));
                updated_last = true;
            }
            if !updated_last {
                let tail_turn = Turn {
                    user: None,
                    assistants: vec![stream_entry],
                    summary: None,
                    compaction: None,
                };
                prepared_turns.push(Arc::new(PreparedTurn::from_turn(
                    tail_turn,
                    base_tool_results,
                )));
            }
        }
    }

    let data = Arc::new(PreparedSessionData {
        turns: prepared_turns,
        tool_results: Arc::new(base_tool_results.clone()),
        messages_generation: messages_gen,
    });
    *prepared_cache = Some(PreparedSessionCache {
        session_id: session.id.clone(),
        revision: session.revision,
        running: session.running,
        messages_generation: messages_gen,
        data: Arc::clone(&data),
    });
    data
}

fn active_prepared_turn_index(turns: &[Arc<PreparedTurn>], running: bool) -> Option<usize> {
    if !running || turns.is_empty() {
        return None;
    }
    let mut idx = turns.len();
    while idx > 0 {
        idx -= 1;
        if turns[idx].turn.summary.is_none() && turns[idx].turn.compaction.is_none() {
            return Some(idx);
        }
    }
    None
}

pub struct MessageListView {
    client: ClientHandle,
    session_id: Option<String>,
    last_observed_revision: u64,
    scroll_handle: ScrollHandle,
    auto_follow_bottom: bool,
    action_in_flight: bool,
    visible_turn_limit: usize,
    segment_expansion_overrides: BTreeMap<ProcessExpansionKey, bool>,
    expanded_thinking: BTreeSet<String>,
    expanded_compactions: BTreeSet<String>,
    expanded_error_details: BTreeSet<String>,
    tool_card_expansion: BTreeMap<String, bool>,
    expanded_tool_raw: BTreeSet<String>,
    expanded_special_details: BTreeSet<(String, String)>,
    hovered_warnings: BTreeSet<String>,
    secret_inputs: BTreeMap<String, PendingSecretInput>,
    markdown_store: MarkdownStreamStore,
    copy_tracker: CopyFeedbackTracker,
    turn_started_instants: BTreeMap<String, Instant>,
    attachment_cache: BTreeMap<String, Arc<Image>>,
    attachment_loading: BTreeSet<String>,
    attachment_generation: u64,
    grouped_turn_cache: Option<GroupedTurnCache>,
    next_messages_generation: u64,
    prepared_session_cache: Option<PreparedSessionCache>,
    revealed_session_id: Option<String>,
    suppress_immediate_reveal: bool,
    content_reveal: motion::SurfaceReveal,
    inline_edit: Option<InlineBubbleEdit>,
}

impl EventEmitter<MessageListEvent> for MessageListView {}

impl MessageListView {
    pub fn new(client: ClientHandle) -> Self {
        Self {
            client,
            session_id: None,
            last_observed_revision: 0,
            scroll_handle: ScrollHandle::new(),
            auto_follow_bottom: true,
            action_in_flight: false,
            visible_turn_limit: DEFAULT_VISIBLE_TURN_WINDOW,
            segment_expansion_overrides: BTreeMap::new(),
            expanded_thinking: BTreeSet::new(),
            expanded_compactions: BTreeSet::new(),
            expanded_error_details: BTreeSet::new(),
            tool_card_expansion: BTreeMap::new(),
            expanded_tool_raw: BTreeSet::new(),
            expanded_special_details: BTreeSet::new(),
            hovered_warnings: BTreeSet::new(),
            secret_inputs: BTreeMap::new(),
            markdown_store: MarkdownStreamStore::default(),
            copy_tracker: CopyFeedbackTracker::default(),
            turn_started_instants: BTreeMap::new(),
            attachment_cache: BTreeMap::new(),
            attachment_loading: BTreeSet::new(),
            attachment_generation: 0,
            grouped_turn_cache: None,
            next_messages_generation: 0,
            prepared_session_cache: None,
            revealed_session_id: None,
            suppress_immediate_reveal: false,
            content_reveal: motion::SurfaceReveal::default(),
            inline_edit: None,
        }
    }

    pub fn dismiss_inline_edit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.inline_edit.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    pub fn inline_edit_textarea(&self) -> Option<Entity<TextareaState>> {
        self.inline_edit.as_ref().map(|e| e.textarea.clone())
    }

    fn set_process_segment_expanded(
        &mut self,
        key: &ProcessExpansionKey,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        self.auto_follow_bottom = false;
        let prev = self
            .segment_expansion_overrides
            .insert(key.clone(), expanded);
        if prev != Some(expanded) {
            cx.notify();
        }
    }

    pub fn set_session_id(&mut self, next_id: Option<String>, cx: &mut Context<Self>) {
        if self.session_id == next_id {
            return;
        }
        let was_in_session = self.session_id.is_some();
        self.session_id = next_id;
        self.revealed_session_id = None;
        self.inline_edit = None;
        // Switching Session A -> Session B always plays the local message-surface reveal;
        // entering from Home/other routes lets the route transition lead if data is already cached,
        // while async loads still glide in once settled.
        self.suppress_immediate_reveal = !was_in_session;
        self.content_reveal.clear();
        self.last_observed_revision = 0;
        self.auto_follow_bottom = true;
        self.visible_turn_limit = DEFAULT_VISIBLE_TURN_WINDOW;
        self.segment_expansion_overrides.clear();
        self.expanded_thinking.clear();
        self.expanded_compactions.clear();
        self.expanded_error_details.clear();
        self.tool_card_expansion.clear();
        self.expanded_tool_raw.clear();
        self.expanded_special_details.clear();
        self.hovered_warnings.clear();
        self.retain_seen_secret_inputs(false, cx);
        self.copy_tracker = CopyFeedbackTracker::default();
        self.markdown_store.clear();
        self.turn_started_instants.clear();
        self.grouped_turn_cache = None;
        self.prepared_session_cache = None;
        self.attachment_cache.clear();
        self.attachment_loading.clear();
        self.attachment_generation = self.attachment_generation.wrapping_add(1);
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    pub fn secret_input_for_call(&self, call_id: &str) -> Option<&Entity<SecretInput>> {
        self.secret_inputs.get(call_id).map(|input| &input.entity)
    }

    fn retain_seen_secret_inputs(&mut self, retain_seen: bool, cx: &mut Context<Self>) {
        self.secret_inputs.retain(|_, input| {
            if retain_seen && std::mem::take(&mut input.seen) {
                true
            } else {
                input.entity.update(cx, |secret, cx| secret.clear(cx));
                false
            }
        });
    }

    fn render_password_input(
        &mut self,
        session_id: &str,
        call_id: &str,
        pending: &PendingBitLockerPrompt,
        cx: &mut Context<Self>,
    ) -> Entity<SecretInput> {
        if let Some(input) = self.secret_inputs.get_mut(call_id)
            && input.request_id == pending.request_id
        {
            input.seen = true;
            return input.entity.clone();
        }
        if let Some(previous) = self.secret_inputs.remove(call_id) {
            previous.entity.update(cx, |secret, cx| secret.clear(cx));
        }

        let id = format!("bitlocker-pwd-input-{call_id}");
        let entity = cx.new(move |cx| SecretInput::with_accessibility_id(id, cx));
        let call_key = call_id.to_owned();
        let session_id = session_id.to_owned();
        let prompt = pending.clone();
        let subscription = cx.subscribe(&entity, move |this, input, event, cx| {
            let Some(state) = this.secret_inputs.get_mut(&call_key) else {
                return;
            };
            if state.entity != input || state.busy || prompt.is_expired() {
                return;
            }
            let payload = match event {
                SecretInputEvent::SubmitRequested => {
                    let payload = input.read(cx).snapshot_zeroizing_payload();
                    if payload.1 == 0 {
                        input.update(cx, |secret, cx| {
                            secret.set_error_message(Some("请输入八组六位数字恢复密码。"), cx);
                        });
                        return;
                    }
                    Some(payload)
                }
                SecretInputEvent::CancelRequested => {
                    input.update(cx, |secret, cx| secret.clear(cx));
                    None
                }
            };
            state.busy = true;
            input.update(cx, |secret, cx| secret.set_disabled(true, cx));

            let client = this.client.clone();
            let session_id = session_id.clone();
            let prompt = prompt.clone();
            let call_key = call_key.clone();
            let input_id = input.entity_id();
            cx.spawn(async move |weak, cx| {
                let res = if let Some((buf, len)) = payload {
                    client
                        .respond_bitlocker_password(session_id, prompt.request_id.clone(), buf, len)
                        .await
                } else {
                    client
                        .cancel_bitlocker_request(session_id, prompt.request_id.clone())
                        .await
                };
                let _ = weak.update(cx, |this, cx| {
                    let Some(state) = this.secret_inputs.get_mut(&call_key) else {
                        return;
                    };
                    if state.entity.entity_id() != input_id || prompt.is_expired() {
                        return;
                    }
                    let invalid_message = match &res {
                        Err(ClientError::BadRequest { code, message })
                            if code == "invalid_bitlocker_input" =>
                        {
                            Some(message.as_str())
                        }
                        _ => None,
                    };
                    state.busy = res.is_ok();
                    state.entity.update(cx, |secret, cx| {
                        if let Some(message) = invalid_message {
                            secret.set_error_message(Some(message.to_owned()), cx);
                        } else {
                            secret.clear(cx);
                        }
                        secret.set_disabled(state.busy, cx);
                    });
                    if invalid_message.is_none()
                        && let Err(err) = res
                    {
                        cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                    }
                });
            })
            .detach();
        });
        self.secret_inputs.insert(
            call_id.to_owned(),
            PendingSecretInput {
                entity: entity.clone(),
                request_id: pending.request_id.clone(),
                seen: true,
                busy: false,
                _subscription: subscription,
            },
        );
        entity
    }

    pub fn is_auto_following(&self) -> bool {
        self.auto_follow_bottom
    }

    pub fn is_action_in_flight(&self) -> bool {
        self.action_in_flight
    }

    pub fn scroll_to_bottom_and_follow(&mut self, cx: &mut Context<Self>) {
        self.auto_follow_bottom = true;
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    pub fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        self.auto_follow_bottom = false;
        self.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("scroll_wheel");
        let pixel_delta = event.delta.pixel_delta(window.rem_size());
        let offset = self.scroll_handle.offset();
        let max_offset = self.scroll_handle.max_offset();

        // In GPUI, positive `pixel_delta.y` scrolls content downward (moving viewport UP toward older messages),
        // while `offset.y` becomes more negative as viewport moves toward the bottom (`-max_offset.y`).
        if pixel_delta.y > px(1.0) {
            if self.auto_follow_bottom {
                self.auto_follow_bottom = false;
                cx.notify();
            }
        } else if pixel_delta.y < px(-1.0) {
            let scrolled_down = -offset.y - pixel_delta.y;
            if scrolled_down + px(28.0) >= max_offset.y && !self.auto_follow_bottom {
                self.auto_follow_bottom = true;
                cx.notify();
            }
        }
    }

    fn ensure_attachment_loaded(
        &mut self,
        session_id: &str,
        attachment_id: &str,
        mime_type: &str,
        cx: &mut Context<Self>,
    ) {
        let cache_key = format!("{session_id}:{attachment_id}");
        if self.attachment_cache.contains_key(&cache_key)
            || self.attachment_loading.contains(&cache_key)
        {
            return;
        }
        self.attachment_loading.insert(cache_key.clone());
        let client = self.client.clone();
        let sid = session_id.to_owned();
        let aid = attachment_id.to_owned();
        let mime = mime_type.to_owned();
        let generation = self.attachment_generation;
        cx.spawn(async move |weak, cx| {
            let res = client.get_session_attachment_bytes(sid, aid).await;
            let _ = weak.update(cx, |this, cx| {
                if generation != this.attachment_generation {
                    return;
                }
                this.attachment_loading.remove(&cache_key);
                if let Ok(bytes) = res {
                    let img_arc = gpui_image_from_bytes(&mime, bytes);
                    this.attachment_cache.insert(cache_key, img_arc);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn resolve_image_part(
        &mut self,
        session_id: &str,
        part: &ImageContentPart,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Image>> {
        if let Some(ref b64) = part.data_base64
            && !b64.is_empty()
        {
            let cache_key = inline_image_key(&part.mime_type, b64);
            if let Some(existing) = self.attachment_cache.get(&cache_key) {
                return Some(Arc::clone(existing));
            }
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
                let decoded = gpui_image_from_bytes(&part.mime_type, bytes);
                self.attachment_cache
                    .insert(cache_key, Arc::clone(&decoded));
                return Some(decoded);
            }
        }
        if let Some(ref aid) = part.attachment_id
            && !aid.is_empty()
        {
            let cache_key = format!("{session_id}:{aid}");
            if let Some(existing) = self.attachment_cache.get(&cache_key) {
                return Some(Arc::clone(existing));
            }
            self.ensure_attachment_loaded(session_id, aid, &part.mime_type, cx);
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn start_inline_edit(
        &mut self,
        session_id: String,
        turn_key: String,
        user_entry_id: String,
        user_text: String,
        user_images: Vec<ImageContentPart>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.action_in_flight || !is_real_entry_id(&user_entry_id) {
            return;
        }
        let initial = user_text.clone();
        let textarea = cx.new(|cx| {
            let mut st = TextareaState::new(window, cx)
                .soft_wrap(true)
                .auto_grow(1, 14);
            st.set_value(initial, window, cx);
            st
        });
        textarea.update(cx, |st, cx| {
            st.focus(window, cx);
        });
        let sub = cx.subscribe(&textarea, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self.inline_edit = Some(InlineBubbleEdit {
            session_id,
            turn_key,
            user_entry_id,
            original_text: user_text,
            user_images,
            textarea,
            _subscription: sub,
        });
        cx.notify();
    }

    fn submit_inline_edit(&mut self, cx: &mut Context<Self>) {
        if self.action_in_flight {
            return;
        }
        let Some(edit) = self.inline_edit.as_ref() else {
            return;
        };
        let edited_text = edit.textarea.read(cx).value().to_string();
        if edited_text.trim().is_empty() && edit.user_images.is_empty() {
            return;
        }
        if edited_text == edit.original_text {
            self.inline_edit = None;
            cx.notify();
            return;
        }
        let edit = self.inline_edit.take().expect("checked inline_edit");
        self.retry_user_turn(
            edit.session_id,
            edit.user_entry_id,
            edited_text,
            edit.user_images,
            cx,
        );
    }

    fn retry_user_turn(
        &mut self,
        session_id: String,
        user_entry_id: String,
        user_text: String,
        user_images: Vec<ImageContentPart>,
        cx: &mut Context<Self>,
    ) {
        if self.action_in_flight
            || !is_real_entry_id(&user_entry_id)
            || (user_text.trim().is_empty() && user_images.is_empty())
        {
            return;
        }
        self.action_in_flight = true;
        cx.notify();

        let active_provider = resolve_session_active_provider(&self.client, &session_id);
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let images = match load_turn_images_into_pool(&client, &session_id, &user_images).await
            {
                Ok(images) => images,
                Err(err) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.finish_action_error(&session_id, err, cx)
                    });
                    return;
                }
            };
            let nav_res = client
                .navigate_session_tree(session_id.clone(), user_entry_id, false, None)
                .await;
            match nav_res {
                Err(err) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.action_in_flight = false;
                        cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                        cx.notify();
                    });
                    return;
                }
                Ok(ref val) if val.get("cancelled").and_then(Value::as_bool) == Some(true) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.action_in_flight = false;
                        cx.notify();
                    });
                    return;
                }
                Ok(_) => {}
            }

            let _ = client.reload_session(session_id.clone()).await;
            let payload = ComposerDraft {
                cursor_char_offset: user_text.chars().count(),
                text: user_text,
                images,
                ..ComposerDraft::default()
            };
            let submit_res = client
                .submit_direct_payload(session_id.clone(), payload)
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.action_in_flight = false;
                if this.session_id.as_deref() != Some(&session_id) {
                    cx.notify();
                    return;
                }
                this.scroll_to_bottom_and_follow(cx);
                match submit_res {
                    Ok(_) => {}
                    Err(ClientError::CredentialsRequired { .. }) => {
                        cx.emit(MessageListEvent::MissingCredentials {
                            provider: active_provider,
                        });
                    }
                    Err(err) => {
                        cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_action_error(&mut self, session_id: &str, err: ClientError, cx: &mut Context<Self>) {
        self.action_in_flight = false;
        if self.session_id.as_deref() == Some(session_id) {
            cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
        }
        cx.notify();
    }

    fn record_copied(&mut self, id: SharedString, notice: &'static str, cx: &mut Context<Self>) {
        self.copy_tracker
            .mark_copied(id, |v| &mut v.copy_tracker, cx);
        cx.emit(MessageListEvent::TopNotice(notice.into()));
    }

    fn markdown_code_copied_callback(
        entity: &Entity<Self>,
    ) -> super::markdown_view::CodeCopiedCallback {
        let weak = entity.downgrade();
        Arc::new(move |cx| {
            if let Some(ent) = weak.upgrade() {
                ent.update(cx, |_, cx| {
                    cx.emit(MessageListEvent::TopNotice("已复制代码内容".into()));
                });
            }
        })
    }

    pub(super) fn fork_from_entry(
        &mut self,
        session_id: String,
        entry_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.action_in_flight || !is_real_entry_id(&entry_id) {
            return;
        }
        self.action_in_flight = true;
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.fork_session(session_id.clone(), entry_id).await;
            let _ = client.refresh_globals().await;
            let _ = weak.update(cx, |this, cx| {
                this.action_in_flight = false;
                if this.session_id.as_deref() != Some(&session_id) {
                    cx.notify();
                    return;
                }
                match res {
                    Ok(new_sid) => {
                        if !new_sid.is_empty() {
                            cx.emit(MessageListEvent::SwitchToSession(new_sid));
                            cx.emit(MessageListEvent::TopNotice("已从当前节点分支新会话".into()));
                        }
                    }
                    Err(err) => {
                        cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn render_user_bubble(
        &mut self,
        session_id: &str,
        session_running: bool,
        turn_key: &str,
        user_entry: &TurnEntry,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let text = extract_message_text(&user_entry.raw);
        let images = extract_user_images(&user_entry.raw);

        let editing_textarea = (!session_running)
            .then(|| {
                self.inline_edit.as_ref().and_then(|ed| {
                    (ed.session_id == session_id && ed.turn_key == turn_key)
                        .then(|| ed.textarea.clone())
                })
            })
            .flatten();

        let img_row_opt = (!images.is_empty()).then(|| {
            let mut img_row = div().flex().flex_wrap().gap(space::SM);
            for (idx, part) in images.iter().enumerate() {
                if let Some(img_arc) = self.resolve_image_part(session_id, part, cx) {
                    let ent_open = entity.clone();
                    let img_open = Arc::clone(&img_arc);
                    let user_img_id = SharedString::from(format!("user-msg-img-{turn_key}-{idx}"));
                    img_row = img_row.child(
                        div()
                            .id(user_img_id.clone())
                            .test_support()
                            .a11y_button(user_img_id, "预览用户消息附图")
                            .w(px(120.0))
                            .h(px(88.0))
                            .rounded(radius::ROW)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .overflow_hidden()
                            .on_click(move |_, w, cx| {
                                let prior = w.focused(cx);
                                let target = LightboxImageTarget {
                                    title: SharedString::from("用户消息附图"),
                                    dimensions: None,
                                    image: Arc::clone(&img_open),
                                    prior_focus: prior,
                                };
                                ent_open.update(cx, |_, cx| {
                                    cx.emit(MessageListEvent::OpenLightbox(target));
                                });
                            })
                            .child(
                                img(ImageSource::Image(img_arc))
                                    .size_full()
                                    .object_fit(ObjectFit::Cover),
                            ),
                    );
                }
            }
            img_row
        });

        if let Some(edit_textarea) = editing_textarea {
            let live_text = edit_textarea.read(cx).value().to_string();
            let (target_w, target_text_h) =
                estimate_inline_bubble_size(&live_text, img_row_opt.is_some());
            let anim_w = transition(
                SharedString::from(format!("inline-edit-w-{turn_key}")),
                target_w,
                Transition::new(motion::RESIZE).ease(motion::ease_out()),
                window,
                cx,
            );
            let anim_text_h = transition(
                SharedString::from(format!("inline-edit-h-{turn_key}")),
                target_text_h,
                Transition::new(motion::RESIZE).ease(motion::ease_out()),
                window,
                cx,
            );

            let ent_submit_key = entity.clone();
            let ent_cancel_key = entity.clone();
            let ta_for_ime = edit_textarea.clone();
            let ta_for_esc = edit_textarea.clone();
            let ta_for_up = edit_textarea.clone();
            let ta_for_down = edit_textarea.clone();

            let mut edit_bubble = div()
                .id(SharedString::from(format!(
                    "user-inline-edit-bubble-{turn_key}"
                )))
                .test_support()
                .w(px(anim_w))
                .max_w(px(560.0))
                .px(space::LG)
                .py(space::MD)
                .rounded(radius::ROW)
                .bg(palette.bg_surface)
                .flex()
                .flex_col()
                .justify_center()
                .gap(space::SM)
                .capture_action(move |_: &gpui_kit::component::input::MoveUp, window, cx| {
                    if crate::shell::input_nav::handle_textarea_vertical_boundary(
                        &ta_for_up, false, window, cx,
                    ) {
                        cx.stop_propagation();
                    }
                })
                .capture_action(
                    move |_: &gpui_kit::component::input::MoveDown, window, cx| {
                        if crate::shell::input_nav::handle_textarea_vertical_boundary(
                            &ta_for_down,
                            true,
                            window,
                            cx,
                        ) {
                            cx.stop_propagation();
                        }
                    },
                )
                .capture_action(move |action: &Enter, window, cx| {
                    if !ta_for_ime.read(cx).focus_handle(cx).is_focused(window) {
                        return;
                    }
                    if ta_for_ime.update(cx, |st, cx| st.marked_text_range(window, cx).is_some()) {
                        cx.stop_propagation();
                        return;
                    }
                    if action.shift && !action.secondary {
                        return;
                    }
                    cx.stop_propagation();
                    ent_submit_key.update(cx, |this, cx| this.submit_inline_edit(cx));
                })
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if event.keystroke.modifiers == gpui::Modifiers::default()
                        && event.keystroke.key == "escape"
                        && !event.is_held
                        && ta_for_esc
                            .update(cx, |st, cx| st.marked_text_range(window, cx).is_none())
                    {
                        cx.stop_propagation();
                        ent_cancel_key.update(cx, |this, cx| {
                            let _ = this.dismiss_inline_edit(cx);
                        });
                    }
                });

            if let Some(img_row) = img_row_opt {
                edit_bubble = edit_bubble.child(img_row);
            }

            edit_bubble = edit_bubble.child(
                div().w_full().h(px(anim_text_h)).child(
                    Textarea::new(&edit_textarea)
                        .accessibility_id(SharedString::from(format!(
                            "user-inline-edit-input-{turn_key}"
                        )))
                        .aria_label("编辑用户消息")
                        .xsmall()
                        .appearance(false)
                        .bordered(false)
                        .p_0()
                        .w_full()
                        .h_full()
                        .text_size(scale::BODY.font_size)
                        .line_height(scale::BODY.line_height),
                ),
            );

            let ent_cancel_btn = entity.clone();
            let ent_submit_btn = entity.clone();
            let submit_disabled =
                self.action_in_flight || (live_text.trim().is_empty() && images.is_empty());

            let edit_actions = div()
                .id(SharedString::from(format!(
                    "user-inline-edit-actions-{turn_key}"
                )))
                .test_support()
                .h(height::INLINE)
                .flex()
                .items_center()
                .justify_end()
                .gap(space::XS)
                .child(icon_action_24(
                    format!("user-inline-edit-cancel-{turn_key}"),
                    "取消编辑",
                    IconName::X,
                    palette.text_secondary,
                    false,
                    palette,
                    window,
                    cx,
                    move |_, cx| {
                        ent_cancel_btn.update(cx, |this, cx| {
                            let _ = this.dismiss_inline_edit(cx);
                        });
                    },
                ))
                .child(icon_action_24(
                    format!("user-inline-edit-submit-{turn_key}"),
                    "保存并发送",
                    IconName::Check,
                    palette.accent_ink,
                    submit_disabled,
                    palette,
                    window,
                    cx,
                    move |_, cx| {
                        ent_submit_btn.update(cx, |this, cx| {
                            this.submit_inline_edit(cx);
                        });
                    },
                ));

            // Right-middle anchored wrapper: right edge stays pinned via `items_end`/`justify_end`,
            // vertical resize expands smoothly around the bubble midpoint via `items_center`.
            return div()
                .id(SharedString::from(format!("turn-user-{turn_key}")))
                .test_support()
                .w_full()
                .flex()
                .flex_col()
                .items_end()
                .gap(space::XS)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(edit_bubble),
                )
                .child(edit_actions)
                .into_any_element();
        }

        let mut bubble = div()
            .max_w(px(560.0))
            .px(space::LG)
            .py(space::MD)
            .rounded(radius::ROW)
            .bg(palette.bg_surface)
            .flex()
            .flex_col()
            .gap(space::SM);

        if let Some(img_row) = img_row_opt {
            bubble = bubble.child(img_row);
        }

        if !text.trim().is_empty() {
            bubble = bubble.child(
                apply_scale(div(), scale::BODY)
                    .pb(space::MICRO)
                    .text_color(palette.text_primary)
                    .child(SharedString::from(text.clone())),
            );
        }

        let copy_id = SharedString::from(format!("user-copy-{turn_key}"));
        let is_copied = self.copy_tracker.is_copied(copy_id.as_ref());
        let text_copy = text.clone();
        let copy_id_click = copy_id.clone();
        let ent_copy = entity.clone();

        let real_user_eid = user_entry
            .id
            .as_deref()
            .filter(|id| is_real_entry_id(id))
            .map(str::to_owned);
        let mutating_disabled = session_running || self.action_in_flight || real_user_eid.is_none();
        let retry_disabled = mutating_disabled || (text.trim().is_empty() && images.is_empty());

        let sid_edit = session_id.to_owned();
        let tk_edit = turn_key.to_owned();
        let eid_edit = real_user_eid.clone().unwrap_or_default();
        let text_edit = text.clone();
        let imgs_edit = images.clone();
        let ent_edit = entity.clone();

        let sid_retry = session_id.to_owned();
        let eid_retry = real_user_eid.unwrap_or_default();
        let text_retry = text;
        let imgs_retry = images;
        let ent_retry = entity.clone();

        // Bottom-right `24×24px` icon-only action bar (`SHcR1` / `b0aK5`)
        let copy_icon_id = SharedString::from(format!("user-copy-btn-{turn_key}"));
        let action_bar = div()
            .id(SharedString::from(format!("user-actions-{turn_key}")))
            .test_support()
            .h(height::INLINE)
            .flex()
            .items_center()
            .justify_end()
            .gap(space::XS)
            .child(icon_action_24_element(
                copy_icon_id.clone(),
                "复制消息",
                motion::animated_copy_icon(
                    copy_icon_id,
                    is_copied,
                    px(14.0),
                    if is_copied {
                        palette.status_success
                    } else {
                        palette.text_secondary
                    },
                    window,
                    cx,
                ),
                false,
                palette,
                window,
                cx,
                move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text_copy.clone()));
                    ent_copy.update(cx, |this, cx| {
                        this.record_copied(copy_id_click.clone(), "已复制消息内容", cx);
                    });
                },
            ))
            .child(icon_action_24(
                format!("user-edit-btn-{turn_key}"),
                "修改消息",
                IconName::Pencil,
                palette.text_secondary,
                mutating_disabled,
                palette,
                window,
                cx,
                move |w, cx| {
                    let s = sid_edit.clone();
                    let tk = tk_edit.clone();
                    let e = eid_edit.clone();
                    let t = text_edit.clone();
                    let im = imgs_edit.clone();
                    ent_edit.update(cx, |this, cx| {
                        this.start_inline_edit(s, tk, e, t, im, w, cx);
                    });
                },
            ))
            .child(icon_action_24(
                format!("user-retry-btn-{turn_key}"),
                "重新生成",
                IconName::RotateCcw,
                palette.text_secondary,
                retry_disabled,
                palette,
                window,
                cx,
                move |_, cx| {
                    let s = sid_retry.clone();
                    let e = eid_retry.clone();
                    let t = text_retry.clone();
                    let im = imgs_retry.clone();
                    ent_retry.update(cx, |this, cx| {
                        this.retry_user_turn(s, e, t, im, cx);
                    });
                },
            ));

        div()
            .id(SharedString::from(format!("turn-user-{turn_key}")))
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .items_end()
            .gap(space::XS)
            .child(bubble)
            .child(action_bar)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_outbox_bubble(
        &self,
        session_id: &str,
        item: &OutboxItem,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui_kit::AnyElement {
        let mid = item.client_message_id.clone();
        let in_flight = item.state.is_in_flight();

        let mut bubble = div()
            .max_w(px(560.0))
            .px(space::LG)
            .py(space::MD)
            .rounded(radius::ROW)
            .bg(palette.bg_surface)
            .opacity(if in_flight { 0.76 } else { 1.0 })
            .flex()
            .flex_col()
            .gap(space::SM);

        if !item.images.is_empty() {
            let mut row = div().flex().flex_wrap().gap(space::SM);
            for (idx, handle) in item.images.iter().enumerate() {
                let gpui_img = gpui_image_from_handle(handle);
                let ent_open = entity.clone();
                let img_open = Arc::clone(&gpui_img);
                let dims = Some(handle.dimensions());
                let ob_img_id = SharedString::from(format!("outbox-img-{mid}-{idx}"));
                row = row.child(
                    div()
                        .id(ob_img_id.clone())
                        .test_support()
                        .a11y_button(ob_img_id, "预览待发消息附图")
                        .w(px(80.0))
                        .h(px(64.0))
                        .rounded(radius::ROW)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .overflow_hidden()
                        .on_click(move |_, w, cx| {
                            let prior = w.focused(cx);
                            ent_open.update(cx, |_, cx| {
                                cx.emit(MessageListEvent::OpenLightbox(LightboxImageTarget {
                                    title: SharedString::from("待发消息附图"),
                                    dimensions: dims,
                                    image: Arc::clone(&img_open),
                                    prior_focus: prior,
                                }));
                            });
                        })
                        .child(
                            img(ImageSource::Image(gpui_img))
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        ),
                );
            }
            bubble = bubble.child(row);
        }

        if !item.text.trim().is_empty() {
            bubble = bubble.child(
                apply_scale(div(), scale::BODY)
                    .pb(space::MICRO)
                    .text_color(palette.text_primary)
                    .child(SharedString::from(item.text.clone())),
            );
        }

        let needs_local_intervention = matches!(
            item.state,
            OutboxItemState::PausedStorageFull { .. }
                | OutboxItemState::Failed { .. }
                | OutboxItemState::Uncertain { .. }
        );

        let bubble_row = if needs_local_intervention {
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(space::SM)
                .child(
                    Icon::new(IconName::CircleAlert)
                        .size(px(16.0))
                        .text_color(palette.status_danger),
                )
                .child(bubble)
        } else {
            div().flex().items_center().justify_end().child(bubble)
        };

        let mut outer = div()
            .id(SharedString::from(format!("outbox-bubble-{mid}")))
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .items_end()
            .gap(space::XS)
            .child(bubble_row);

        // Per plan §3.3.4: settled local failure renders ONLY 3 icon-only buttons (`retry` / `edit` / `remove`) below bubble!
        if needs_local_intervention {
            let sid_retry = session_id.to_owned();
            let mid_retry = mid.clone();
            let ent_retry = entity.clone();

            let sid_edit = session_id.to_owned();
            let mid_edit = mid.clone();
            let ent_edit = entity.clone();

            let sid_rm = session_id.to_owned();
            let mid_rm = mid.clone();
            let ent_rm = entity.clone();

            outer = outer.child(
                div()
                    .id(SharedString::from(format!("outbox-actions-{mid}")))
                    .test_support()
                    .h(height::INLINE)
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(space::XS)
                    .child(icon_action_24(
                        format!("outbox-retry-{mid}"),
                        "重试发送",
                        IconName::RotateCcw,
                        palette.text_secondary,
                        false,
                        palette,
                        window,
                        cx,
                        move |_, cx| {
                            let s = sid_retry.clone();
                            let m = mid_retry.clone();
                            ent_retry.update(cx, |this, cx| {
                                let c = this.client.clone();
                                cx.spawn(async move |weak, cx| {
                                    if let Err(err) = c.retry_outbox_item(Some(s), m).await {
                                        let _ = weak.update(cx, |_, cx| {
                                            cx.emit(MessageListEvent::ErrorMessage(
                                                err.to_string(),
                                            ));
                                        });
                                    }
                                })
                                .detach();
                            });
                        },
                    ))
                    .child(icon_action_24(
                        format!("outbox-edit-{mid}"),
                        "修改消息",
                        IconName::Pencil,
                        palette.text_secondary,
                        false,
                        palette,
                        window,
                        cx,
                        move |_, cx| {
                            let s = sid_edit.clone();
                            let m = mid_edit.clone();
                            ent_edit.update(cx, |_, cx| {
                                cx.emit(MessageListEvent::RequestEditOutboxItem {
                                    session_id: s,
                                    client_message_id: m,
                                });
                            });
                        },
                    ))
                    .child(icon_action_24(
                        format!("outbox-remove-{mid}"),
                        "移除",
                        IconName::Trash,
                        palette.status_danger,
                        false,
                        palette,
                        window,
                        cx,
                        move |_, cx| {
                            ent_rm.update(cx, |this, cx| {
                                if let Err(err) =
                                    this.client.remove_outbox_item(Some(&sid_rm), &mid_rm)
                                {
                                    cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                                }
                                cx.notify();
                            });
                        },
                    )),
            );
        }

        outer.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_normal_tool_row(
        &mut self,
        call: &ToolCallPart,
        tool_result_val: Option<&Value>,
        result_revision: u64,
        live: bool,
        running_light_allowed: bool,
        started_at: Instant,
        mono_family: SharedString,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let has_result = tool_result_val.is_some();
        let is_error = tool_result_val
            .and_then(|v| v.get("isError"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let card_view = tool_result_val.map(|res_val| {
            self.client.with_presenter_cache(|cache| {
                if let Some(hit) = cache.get_cached_normal(&call.id, result_revision) {
                    return hit;
                }
                let input = to_tool_call_input(call);
                let parsed_res = ToolResultInput::from_message_json(res_val);
                cache.get_or_map_normal(&input, &parsed_res, result_revision)
            })
        });

        let duration_ms = tool_result_val
            .and_then(|v| {
                v.get("durationMs")
                    .or_else(|| v.get("details").and_then(|d| d.get("durationMs")))
            })
            .and_then(Value::as_u64);

        let standalone = is_standalone_tool(call, tool_result_val);
        let card_key = format!("tool-card-{}", call.id);
        let is_open = self
            .tool_card_expansion
            .get(&card_key)
            .copied()
            .unwrap_or(standalone);
        let raw_key = format!("tool-raw-{}", call.id);
        let is_raw_open = self.expanded_tool_raw.contains(&raw_key);

        let ent_card = entity.clone();
        let ent_raw = entity.clone();
        let ent_lb = entity.clone();
        let copy_ref = &self.copy_tracker;

        render_normal_tool_card_with_args(
            &call.id,
            &call.name,
            Some(&call.arguments),
            duration_ms,
            has_result,
            is_error,
            live,
            running_light_allowed,
            Some(started_at),
            card_view.as_deref(),
            is_open,
            is_raw_open,
            mono_family,
            palette,
            &|cid| copy_ref.is_copied(cid),
            Rc::new(move |key, _, cx| {
                ent_card.update(cx, |this, cx| {
                    this.auto_follow_bottom = false;
                    let expanded = this.tool_card_expansion.entry(key).or_insert(standalone);
                    *expanded = !*expanded;
                    cx.notify();
                });
            }),
            Rc::new(move |key, _, cx| {
                ent_raw.update(cx, |this, cx| {
                    this.auto_follow_bottom = false;
                    if !this.expanded_tool_raw.remove(&key) {
                        this.expanded_tool_raw.insert(key);
                    }
                    cx.notify();
                });
            }),
            Rc::new(move |target, _, cx| {
                ent_lb.update(cx, |_, cx| {
                    cx.emit(MessageListEvent::OpenLightbox(target));
                });
            }),
            |cid, _, cx| {
                cx.entity().update(cx, |this: &mut Self, cx| {
                    let notice = if cid.as_ref().contains("cmd") {
                        "已复制命令"
                    } else {
                        "已复制输出内容"
                    };
                    this.record_copied(cid, notice, cx);
                });
            },
            window,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_special_tool_surface(
        &mut self,
        session: &SessionViewSnapshot,
        entry_id: &str,
        kind: SpecialToolKind,
        call: &ToolCallPart,
        tool_result_val: Option<&Value>,
        result_revision: u64,
        live: bool,
        running_light_allowed: bool,
        started_at: Instant,
        mono_family: SharedString,
        transparency_enabled: bool,
        scale_factor: f32,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let cache_rev = if tool_result_val.is_some() {
            result_revision
        } else {
            0
        };
        let Some(special) = self.client.with_presenter_cache(|cache| {
            if let Some(hit) = cache.get_cached_special(&call.id, cache_rev, live) {
                return Some(hit);
            }
            let input = to_tool_call_input(call);
            let parsed_res = tool_result_val.map(ToolResultInput::from_message_json);
            cache.get_or_map_special(&input, parsed_res.as_ref(), cache_rev, live)
        }) else {
            return div().into_any_element();
        };

        let pending_bitlocker = (kind == SpecialToolKind::BitLocker)
            .then(|| {
                find_pending_bitlocker_request(
                    session.bitlocker.as_ref(),
                    &call.id,
                    tool_result_val.is_some(),
                )
            })
            .flatten();
        let secret_input = pending_bitlocker
            .as_ref()
            .filter(|pending| pending.kind == "password")
            .map(|pending| self.render_password_input(&session.id, &call.id, pending, cx));

        let details_key = (entry_id.to_owned(), call.id.clone());
        let details_expanded = self.expanded_special_details.contains(&details_key);
        let warning_hovered = self
            .hovered_warnings
            .contains(&format!("web-search-warn-{}", call.id));

        let ent_toggle = entity.clone();
        let ent_hover = entity.clone();
        let ent_runbook = entity.clone();
        let sid_runbook = session.id.clone();
        let copy_ref = &self.copy_tracker;

        render_special_tool_component(
            SpecialToolDispatchProps {
                session_id: &session.id,
                entry_id,
                call_id: &call.id,
                view: &special,
                pending_bitlocker,
                secret_input,
                details_expanded,
                warning_hovered,
                transparency_enabled,
                scale_factor,
                running_light_allowed,
                started_at: Some(started_at),
                animate_enter: live,
            },
            self.client.clone(),
            mono_family,
            palette,
            &|cid| copy_ref.is_copied(cid),
            Rc::new(move |_, cx| {
                ent_toggle.update(cx, |this, cx| {
                    this.auto_follow_bottom = false;
                    if !this.expanded_special_details.remove(&details_key) {
                        this.expanded_special_details.insert(details_key.clone());
                    }
                    cx.notify();
                });
            }),
            Rc::new(move |key, hovered, _, cx| {
                ent_hover.update(cx, |this, cx| {
                    if hovered {
                        this.hovered_warnings.insert(key);
                    } else {
                        this.hovered_warnings.remove(&key);
                    }
                    cx.notify();
                });
            }),
            Rc::new(move |e_id, c_id, _, cx| {
                let s_id = sid_runbook.clone();
                ent_runbook.update(cx, |this, cx| {
                    let c = this.client.clone();
                    cx.spawn(async move |weak, cx| {
                        if let Err(err) = c.open_session_runbook(s_id, e_id, c_id).await {
                            let _ = weak.update(cx, |_, cx| {
                                cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                            });
                        }
                    })
                    .detach();
                });
            }),
            |cid, _, cx| {
                cx.entity().update(cx, |this: &mut Self, cx| {
                    let notice =
                        if cid.as_ref().contains("bitlocker") || cid.as_ref().contains("key") {
                            "已复制密钥 ID"
                        } else if cid.as_ref().contains("runbook") {
                            "已复制命令"
                        } else {
                            "已复制内容"
                        };
                    this.record_copied(cid, notice, cx);
                });
            },
            |msg, cx| {
                cx.emit(MessageListEvent::ErrorMessage(msg));
            },
            window,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_turn_view(
        &mut self,
        session: &SessionViewSnapshot,
        turn_idx: usize,
        prepared_turn: &PreparedTurn,
        tool_results: &BTreeMap<String, Value>,
        messages_generation: u64,
        is_live_turn: bool,
        mono_family: SharedString,
        markdown_style: &TextViewStyle,
        reduce_motion: bool,
        running_light_allowed: bool,
        transparency_enabled: bool,
        scale_factor: f32,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let turn = &prepared_turn.turn;
        let turn_key = turn
            .user
            .as_ref()
            .map(TurnEntry::stable_key)
            .or_else(|| turn.summary.as_ref().map(TurnEntry::stable_key))
            .or_else(|| turn.compaction.as_ref().map(TurnEntry::stable_key))
            .or_else(|| turn.assistants.first().map(TurnEntry::stable_key))
            .unwrap_or_else(|| format!("idx-{turn_idx}"));

        // Standalone `compactionSummary` Assistant reply bubble (`ln9GA` / `z9Bj5e`, collapsed by default)
        if let Some(ref comp_entry) = turn.compaction {
            let summary_raw = extract_message_text(&comp_entry.raw);
            let summary_text = if summary_raw.trim().is_empty() {
                "已完成上下文压缩，较早对话已整理为摘要。".to_owned()
            } else {
                summary_raw
            };
            let ts_label = (comp_entry.timestamp_ms > 0)
                .then(|| format_timestamp_hhmm(comp_entry.timestamp_ms))
                .flatten();

            let comp_key = format!("compaction:{turn_key}");
            let is_open = self.expanded_compactions.contains(&comp_key);
            let ent_toggle = entity.clone();
            let toggle_key = comp_key.clone();
            let hdr_id = SharedString::from(format!("compaction-hdr-{turn_key}"));
            let (hdr_hovered, hdr_hs) =
                motion::PointerFlag::keyed(format!("hover-compaction-hdr-{turn_key}"), window, cx);
            let hdr_hover_t = motion::animated_state_progress(
                format!("compaction-hdr-{turn_key}"),
                hdr_hovered,
                window,
                cx,
            );
            let hdr_fg =
                motion::lerp_hsla(palette.text_secondary, palette.text_primary, hdr_hover_t);

            // Matches normal tool row (`BuK2W` / `normal_card.rs`):
            // `28px` frameless row, `16×16` `circle-check` (`$status-success`), `$font-ui`
            // `$text-secondary` -> hover `$text-primary`, right-aligned `$font-caption` time + chevron.
            let header = apply_scale(div(), scale::UI)
                .id(hdr_id.clone())
                .test_support()
                .a11y_toggle_button(hdr_id, "上下文已压缩", is_open)
                .aria_expanded(is_open)
                .w_full()
                .h(height::COMPACT)
                .px(px(0.0))
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .text_color(hdr_fg)
                .on_hover(move |active, w, cx| {
                    if *active {
                        motion::PointerFlag::enter(&hdr_hs, w, cx);
                    } else {
                        motion::PointerFlag::set(&hdr_hs, false, cx);
                    }
                })
                .on_click(move |_, _, cx| {
                    let k = toggle_key.clone();
                    ent_toggle.update(cx, |this, cx| {
                        this.auto_follow_bottom = false;
                        if !this.expanded_compactions.remove(&k) {
                            this.expanded_compactions.insert(k);
                        }
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .min_w_0()
                        .child(
                            Icon::new(IconName::CircleCheck)
                                .size(px(16.0))
                                .text_color(palette.status_success),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(cjk_inline_label("上下文已压缩")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .flex_shrink_0()
                        .children(ts_label.map(|hhmm| {
                            apply_scale(div(), scale::CAPTION)
                                .font_family(mono_family.clone())
                                .text_color(palette.text_muted)
                                .child(SharedString::from(hhmm))
                        }))
                        .child(motion::animated_chevron(
                            format!("compaction-chev-{turn_key}"),
                            is_open,
                            false,
                            px(16.0),
                            hdr_fg,
                            window,
                            cx,
                        )),
                );

            let mut container = div()
                .id(SharedString::from(format!("turn-compaction-{turn_key}")))
                .test_support()
                .w_full()
                .flex()
                .flex_col()
                .child(header);

            let copy_id = SharedString::from(format!("compaction-copy-{turn_key}"));
            let is_copied = self.copy_tracker.is_copied(copy_id.as_ref());
            let copy_text = summary_text.clone();
            let copy_id_click = copy_id.clone();
            let ent_copy = entity.clone();

            if let Some(animated_body) = motion::animated_collapse_with_exit(
                format!("compaction-body-{turn_key}"),
                is_open,
                window,
                cx,
                |interactive, window, cx| {
                    let copy_btn_id = SharedString::from(format!("compaction-copy-btn-{turn_key}"));
                    let copy_btn = if interactive {
                        div()
                            .id(copy_btn_id.clone())
                            .test_support()
                            .a11y_button(copy_btn_id.clone(), "复制压缩摘要")
                            .desktop_tooltip(
                                SharedString::from(format!("tooltip-compaction-copy-{turn_key}")),
                                "复制压缩摘要",
                            )
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .flex_shrink_0()
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                                ent_copy.update(cx, |this, cx| {
                                    this.record_copied(copy_id_click.clone(), "已复制压缩摘要", cx);
                                });
                            })
                            .child(motion::animated_copy_icon(
                                copy_btn_id,
                                is_copied,
                                px(14.0),
                                if is_copied {
                                    palette.status_success
                                } else {
                                    palette.text_secondary
                                },
                                window,
                                cx,
                            ))
                            .into_any_element()
                    } else {
                        div()
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .flex_shrink_0()
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::Copy)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            )
                            .into_any_element()
                    };

                    // Matches normal tool expanded body (`VwO1J` / `normal_card.rs`):
                    // `16px` left gutter with centered `2px` `$border-subtle` vertical guide rail,
                    // `8px` gap (`24px` total indent), and `$bg-surface` `4px` radius `12px` padding
                    // summary block + `28×28` top-right copy button.
                    let guide_rail = div()
                        .w(px(16.0))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .pt(px(2.0))
                        .pb(px(4.0))
                        .child(
                            div()
                                .w(px(2.0))
                                .h_full()
                                .rounded(radius::FULL)
                                .bg(palette.border_subtle),
                        );

                    let right_col = div()
                        .flex_1()
                        .min_w_0()
                        .pt(space::MICRO)
                        .pb(space::MICRO)
                        .child(
                            div()
                                .w_full()
                                .p(space::MD)
                                .rounded(radius::CONTROL)
                                .bg(palette.bg_surface)
                                .flex()
                                .items_start()
                                .justify_between()
                                .gap(space::MD)
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .flex_1()
                                        .min_w_0()
                                        .text_color(palette.text_secondary)
                                        .child(SharedString::from(summary_text.clone())),
                                )
                                .child(copy_btn),
                        );

                    div()
                        .w_full()
                        .flex()
                        .gap(space::SM)
                        .child(guide_rail)
                        .child(right_col)
                        .into_any_element()
                },
            ) {
                container = container.child(animated_body);
            }

            return container.into_any_element();
        }

        // Standalone `branchSummary` turn
        if let Some(ref sum_entry) = turn.summary {
            let sum_text = extract_message_text(&sum_entry.raw);
            let blk_id = SharedString::from(format!("branch-summary-{turn_key}"));
            let copied = self.copy_tracker.is_copied(blk_id.as_ref());
            return div()
                .id(SharedString::from(format!("turn-summary-{turn_key}")))
                .test_support()
                .w_full()
                .flex()
                .flex_col()
                .gap(space::XS)
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .text_color(palette.text_secondary)
                        .child("分支总结"),
                )
                .child(render_copyable_text_block(
                    blk_id,
                    sum_text,
                    false,
                    mono_family,
                    copied,
                    palette,
                    |cid, _, cx| {
                        cx.entity().update(cx, |this: &mut Self, cx| {
                            this.record_copied(cid, "已复制分支总结", cx);
                        });
                    },
                    window,
                    cx,
                ))
                .into_any_element();
        }

        let mut column = div()
            .id(SharedString::from(format!("turn-row-{turn_key}")))
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::LG);

        if let Some(ref user_entry) = turn.user {
            column = column.child(self.render_user_bubble(
                &session.id,
                session.running,
                &turn_key,
                user_entry,
                palette,
                entity,
                window,
                cx,
            ));
        }

        if turn.assistants.is_empty() && !is_live_turn {
            return column.into_any_element();
        }

        let empty_split;
        let split = match prepared_turn.split.as_ref() {
            Some(s) => s,
            None => {
                empty_split = split_turn(&turn.assistants);
                &empty_split
            }
        };
        let empty_segments;
        let segments: &[TurnSegment] = if prepared_turn.split.is_some() {
            &prepared_turn.segments
        } else {
            empty_segments = build_turn_segments_with_results(split.process.clone(), tool_results);
            &empty_segments
        };

        // Track live elapsed instant for `RunningLight` sweep
        let started_instant = *self
            .turn_started_instants
            .entry(turn_key.clone())
            .or_insert_with(Instant::now);

        let live_elapsed_ms = if is_live_turn {
            session
                .active_turn_started_ms
                .map(|start_ms| now_epoch_ms().saturating_sub(start_ms))
                .unwrap_or_else(|| started_instant.elapsed().as_millis() as u64)
        } else {
            split.total_duration_ms
        };

        let total_segments = segments.len();
        let has_final_text = !split.final_text.trim().is_empty();
        let mut assistant_stack = div().w_full().flex().flex_col().gap(space::MD);

        for (seg_idx, segment) in segments.iter().enumerate() {
            match segment.kind {
                TurnSegmentKind::Process => {
                    let sub_label =
                        process_segment_label(&segment.parts, tool_results, is_live_turn);
                    if seg_idx > 0 && sub_label.is_none() {
                        continue;
                    }
                    if seg_idx == 0
                        && !is_live_turn
                        && segment.parts.is_empty()
                        && split.total_duration_ms == 0
                    {
                        continue;
                    }

                    let segment_id = format!("{turn_key}:{}", segment.key);
                    let expansion_key = segment
                        .expansion_anchor()
                        .map(|anchor| (turn_key.clone(), anchor));
                    let has_expandable_parts = expansion_key.is_some();
                    // Parity with web `ProcessSegment`: live process segments expand by default
                    // so progress streams visibly and can be collapsed manually at any time;
                    // once settled, untouched segments auto-collapse cleanly.
                    let seg_expanded = expansion_key.as_ref().is_some_and(|key| {
                        self.segment_expansion_overrides
                            .get(key)
                            .copied()
                            .unwrap_or(is_live_turn)
                    });

                    let base_header = (seg_idx == 0).then(|| {
                        if is_live_turn {
                            format!("工作中 {}", format_live_duration_zh(live_elapsed_ms))
                        } else {
                            format!("已工作 {}", format_duration_zh(split.total_duration_ms))
                        }
                    });
                    let header_text = match (&base_header, &sub_label) {
                        (Some(base), Some(sub)) => format!("{base} · {sub}"),
                        (Some(base), None) => base.clone(),
                        (None, Some(sub)) => sub.clone(),
                        (None, None) => "处理过程".to_owned(),
                    };

                    let header_id = SharedString::from(format!("process-hdr-{segment_id}"));
                    let (hdr_hovered, hdr_hs) = motion::PointerFlag::keyed(
                        format!("hover-process-hdr-{segment_id}"),
                        window,
                        cx,
                    );
                    let hdr_hover_t = motion::animated_state_progress(
                        format!("process-hdr-{segment_id}"),
                        hdr_hovered && has_expandable_parts,
                        window,
                        cx,
                    );
                    let hdr_fg = motion::lerp_hsla(
                        palette.text_secondary,
                        palette.text_primary,
                        hdr_hover_t,
                    );
                    let mut header = div()
                        .id(header_id.clone())
                        .test_support()
                        .w_full()
                        .h(height::CONTROL)
                        .border_b_1()
                        .border_color(palette.bg_surface)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .text_color(hdr_fg);

                    if let Some(click_key) = expansion_key {
                        let expand_key = click_key.clone();
                        let collapse_key = click_key.clone();
                        let expand_view = entity.clone();
                        let collapse_view = entity.clone();
                        let current_expanded = seg_expanded;
                        header = header
                            .a11y_toggle_button(header_id, header_text.clone(), seg_expanded)
                            .aria_expanded(seg_expanded)
                            .on_hover(move |active, w, cx| {
                                if *active {
                                    motion::PointerFlag::enter(&hdr_hs, w, cx);
                                } else {
                                    motion::PointerFlag::set(&hdr_hs, false, cx);
                                }
                            })
                            .on_a11y_action(gpui::AccessibleAction::Expand, move |_, _, cx| {
                                expand_view.update(cx, |this, cx| {
                                    this.set_process_segment_expanded(&expand_key, true, cx);
                                });
                            })
                            .on_a11y_action(gpui::AccessibleAction::Collapse, move |_, _, cx| {
                                collapse_view.update(cx, |this, cx| {
                                    this.set_process_segment_expanded(&collapse_key, false, cx);
                                });
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_process_segment_expanded(
                                    &click_key,
                                    !current_expanded,
                                    cx,
                                );
                            }));
                    }

                    let segment_active = is_live_turn
                        && (seg_idx == 0
                            || segment_is_running(
                                &segment.parts,
                                tool_results,
                                is_live_turn,
                                seg_idx + 1 == total_segments,
                                has_final_text,
                            ));
                    let sweep_live = segment_active && running_light_allowed;
                    if let Some(base) = base_header {
                        let mut label_row =
                            div()
                                .flex()
                                .items_center()
                                .child(motion::animated_status_label(
                                    format!("process-hdr-base-{segment_id}"),
                                    SharedString::from(base),
                                    sweep_live,
                                    started_instant,
                                    hdr_fg,
                                    palette.text_primary,
                                    scale::BODY,
                                    window,
                                    cx,
                                ));
                        if let Some(sub) = sub_label {
                            label_row = label_row.child(motion::animated_status_label(
                                format!("process-hdr-sub-{segment_id}"),
                                SharedString::from(format!(" · {sub}")),
                                sweep_live,
                                started_instant,
                                hdr_fg,
                                palette.text_primary,
                                scale::BODY,
                                window,
                                cx,
                            ));
                        }
                        header = header.child(label_row);
                    } else {
                        header = header.child(motion::animated_status_label(
                            format!("process-hdr-label-{segment_id}"),
                            SharedString::from(header_text),
                            sweep_live,
                            started_instant,
                            hdr_fg,
                            palette.text_primary,
                            scale::BODY,
                            window,
                            cx,
                        ));
                    }

                    if has_expandable_parts {
                        header = header.child(motion::animated_chevron(
                            format!("process-hdr-{segment_id}"),
                            seg_expanded,
                            false,
                            px(16.0),
                            hdr_fg,
                            window,
                            cx,
                        ));
                    }

                    let mut seg_box = div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::SM)
                        .child(header);

                    if has_expandable_parts
                        && let Some(animated_body) = motion::animated_collapse_with_exit(
                            format!("process-body-{segment_id}"),
                            seg_expanded,
                            window,
                            cx,
                            |interactive, window, cx| {
                                let mut expanded_col =
                                    div().w_full().flex().flex_col().gap(space::SM);
                                if interactive {
                                    let part_count = segment.parts.len();
                                    for (part_idx, tp) in segment.parts.iter().enumerate() {
                                        expanded_col =
                                            expanded_col.child(self.render_process_part(
                                                &turn_key,
                                                &segment_id,
                                                part_idx,
                                                part_idx + 1 == part_count && !has_final_text,
                                                tp,
                                                tool_results,
                                                messages_generation,
                                                is_live_turn,
                                                reduce_motion,
                                                running_light_allowed,
                                                started_instant,
                                                mono_family.clone(),
                                                markdown_style,
                                                palette,
                                                entity,
                                                window,
                                                cx,
                                            ));
                                    }
                                } else {
                                    for tp in &segment.parts {
                                        expanded_col =
                                            expanded_col.child(render_process_part_inert(
                                                tp,
                                                tool_results,
                                                is_live_turn,
                                                mono_family.clone(),
                                                palette,
                                            ));
                                    }
                                }
                                expanded_col.into_any_element()
                            },
                        )
                    {
                        seg_box = seg_box.child(animated_body);
                    }

                    assistant_stack = assistant_stack.child(seg_box);
                }
                TurnSegmentKind::Visible => {
                    for (v_idx, tp) in segment.parts.iter().enumerate() {
                        match &tp.part {
                            ConversationPart::Text(txt) if !txt.trim().is_empty() => {
                                let stream_key =
                                    format!("mid:{turn_key}:{}:{seg_idx}:{v_idx}", tp.entry_key);
                                let md_entity =
                                    self.markdown_store.sync_entry(&stream_key, txt, cx);
                                assistant_stack = assistant_stack.child(
                                    super::markdown_view::render_assistant_markdown_with_copy_notice(
                                        &md_entity,
                                        is_live_turn && tp.is_final_entry,
                                        reduce_motion,
                                        palette,
                                        markdown_style.clone(),
                                        Some(Self::markdown_code_copied_callback(entity)),
                                    ),
                                );
                            }
                            ConversationPart::Special { kind, call } => {
                                assistant_stack =
                                    assistant_stack.child(self.render_special_tool_surface(
                                        session,
                                        &tp.entry_key,
                                        *kind,
                                        call,
                                        tool_results.get(&call.id),
                                        messages_generation,
                                        is_live_turn,
                                        running_light_allowed,
                                        started_instant,
                                        mono_family.clone(),
                                        transparency_enabled,
                                        scale_factor,
                                        palette,
                                        entity,
                                        window,
                                        cx,
                                    ));
                            }
                            ConversationPart::Image(img_part) => {
                                if let Some(img_arc) =
                                    self.resolve_image_part(&session.id, img_part, cx)
                                {
                                    let ent_open = entity.clone();
                                    let img_open = Arc::clone(&img_arc);
                                    let asst_img_id = SharedString::from(format!(
                                        "assistant-img-{turn_key}-{v_idx}"
                                    ));
                                    assistant_stack = assistant_stack.child(
                                        div()
                                            .id(asst_img_id.clone())
                                            .test_support()
                                            .a11y_button(asst_img_id, "预览诊断结果截图")
                                            .max_w(px(420.0))
                                            .rounded(radius::ROW)
                                            .border_1()
                                            .border_color(palette.border_subtle)
                                            .overflow_hidden()
                                            .on_click(move |_, w, cx| {
                                                let prior = w.focused(cx);
                                                ent_open.update(cx, |_, cx| {
                                                    cx.emit(MessageListEvent::OpenLightbox(
                                                        LightboxImageTarget {
                                                            title: SharedString::from(
                                                                "诊断结果截图",
                                                            ),
                                                            dimensions: None,
                                                            image: Arc::clone(&img_open),
                                                            prior_focus: prior,
                                                        },
                                                    ));
                                                });
                                            })
                                            .child(
                                                img(ImageSource::Image(img_arc))
                                                    .max_h(px(280.0))
                                                    .object_fit(ObjectFit::Contain),
                                            ),
                                    );
                                }
                            }
                            ConversationPart::Tools(calls) => {
                                let mut visible_tools =
                                    div().w_full().flex().flex_col().gap(space::SM);
                                for call in calls {
                                    visible_tools =
                                        visible_tools.child(self.render_normal_tool_row(
                                            call,
                                            tool_results.get(&call.id),
                                            messages_generation,
                                            is_live_turn,
                                            running_light_allowed,
                                            started_instant,
                                            mono_family.clone(),
                                            palette,
                                            entity,
                                            window,
                                            cx,
                                        ));
                                }
                                assistant_stack = assistant_stack.child(visible_tools);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Final assistant Markdown prose (`16px/24px`) backed by stable `Entity<TextViewState>`
        if !split.final_text.trim().is_empty() {
            let stream_key = split
                .final_entry_id
                .clone()
                .unwrap_or_else(|| format!("final:{turn_key}"));
            let md_entity = self
                .markdown_store
                .sync_entry(&stream_key, &split.final_text, cx);
            assistant_stack = assistant_stack.child(
                super::markdown_view::render_assistant_markdown_with_copy_notice(
                    &md_entity,
                    is_live_turn,
                    reduce_motion,
                    palette,
                    markdown_style.clone(),
                    Some(Self::markdown_code_copied_callback(entity)),
                ),
            );
        }

        // Standard assistant error prose (`{服务名} {故障现象}，{处理指引}。` + single collapsed `详情` block)
        if let Some(ref err_pres) = split.assistant_error {
            let mut err_col = div()
                .id(SharedString::from(format!("assistant-error-{turn_key}")))
                .test_support()
                .w_full()
                .flex()
                .flex_col()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::BODY)
                        .text_color(palette.text_primary)
                        .child(SharedString::from(err_pres.headline.clone())),
                );

            if let Some(ref raw_detail) = err_pres.raw_detail {
                let det_key = format!("err-detail:{turn_key}");
                let det_open = self.expanded_error_details.contains(&det_key);
                let ent_det = entity.clone();
                let k_det = det_key.clone();
                let det_btn_id = SharedString::from(format!("assistant-err-detail-btn-{turn_key}"));
                err_col = err_col.child(
                    apply_scale(div(), scale::CAPTION)
                        .id(det_btn_id.clone())
                        .test_support()
                        .a11y_toggle_button(det_btn_id, "错误详情", det_open)
                        .h(px(22.0))
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .text_color(palette.text_secondary)
                        .hover(|s| s.text_color(palette.text_primary))
                        .on_click(move |_, _, cx| {
                            let k = k_det.clone();
                            ent_det.update(cx, |this, cx| {
                                this.auto_follow_bottom = false;
                                if !this.expanded_error_details.remove(&k) {
                                    this.expanded_error_details.insert(k);
                                }
                                cx.notify();
                            });
                        })
                        .child(cjk_inline_label("详情"))
                        .child(motion::animated_chevron(
                            format!("assistant-err-det-{turn_key}"),
                            det_open,
                            false,
                            px(14.0),
                            palette.text_secondary,
                            window,
                            cx,
                        )),
                );
                if let Some(animated_err) = motion::animated_collapse_with_exit(
                    format!("assistant-err-det-body-{turn_key}"),
                    det_open,
                    window,
                    cx,
                    |interactive, window, cx| {
                        if !interactive {
                            return render_copyable_text_block_inert(
                                raw_detail.clone(),
                                true,
                                mono_family.clone(),
                                palette,
                            )
                            .into_any_element();
                        }
                        let blk_id = SharedString::from(format!("assistant-err-raw-{turn_key}"));
                        let copied = self.copy_tracker.is_copied(blk_id.as_ref());
                        render_copyable_text_block(
                            blk_id,
                            raw_detail.clone(),
                            true,
                            mono_family.clone(),
                            copied,
                            palette,
                            |cid, _, cx| {
                                cx.entity().update(cx, |this: &mut Self, cx| {
                                    this.record_copied(cid, "已复制错误详情", cx);
                                });
                            },
                            window,
                            cx,
                        )
                        .into_any_element()
                    },
                ) {
                    err_col = err_col.child(animated_err);
                }
            }
            assistant_stack = assistant_stack.child(err_col);
        }

        // Manual stop marker (`已停止生成`)
        if split.was_aborted {
            assistant_stack = assistant_stack.child(
                apply_scale(div(), scale::UI)
                    .id(SharedString::from(format!("assistant-aborted-{turn_key}")))
                    .test_support()
                    .text_color(palette.text_muted)
                    .child("已停止生成"),
            );
        }

        // Persistent bottom `24px` action bar (`VS7mw`): disabled (`opacity: 0.35`) while generating,
        // unlocked with right-aligned `HH:mm` timestamp once turn finishes.
        if is_live_turn || !turn.assistants.is_empty() {
            let copy_text = if !split.final_text.trim().is_empty() {
                split.final_text.clone()
            } else if let Some(ref e) = split.assistant_error {
                e.headline.clone()
            } else {
                String::new()
            };
            let copy_id = SharedString::from(format!("assistant-copy-{turn_key}"));
            let is_copied = !is_live_turn && self.copy_tracker.is_copied(copy_id.as_ref());
            let copy_id_click = copy_id.clone();
            let ent_copy = entity.clone();

            let user_text_for_turn = turn
                .user
                .as_ref()
                .map(|u| extract_message_text(&u.raw))
                .unwrap_or_default();
            let user_images_for_turn = turn
                .user
                .as_ref()
                .map(|u| extract_user_images(&u.raw))
                .unwrap_or_default();
            let real_user_eid = turn
                .user
                .as_ref()
                .and_then(|u| u.id.as_deref())
                .filter(|id| is_real_entry_id(id))
                .map(str::to_owned);
            let real_fork_eid = split
                .final_entry_id
                .as_deref()
                .filter(|id| is_real_entry_id(id))
                .map(str::to_owned)
                .or_else(|| {
                    turn.assistants
                        .last()
                        .and_then(|a| a.id.as_deref())
                        .filter(|id| is_real_entry_id(id))
                        .map(str::to_owned)
                });

            let mutating_busy = session.running || self.action_in_flight;
            let fork_disabled = mutating_busy || real_fork_eid.is_none();
            let retry_disabled = mutating_busy
                || real_user_eid.is_none()
                || (user_text_for_turn.trim().is_empty() && user_images_for_turn.is_empty());

            let sid_fork = session.id.clone();
            let fork_eid = real_fork_eid.unwrap_or_default();
            let ent_fork = entity.clone();

            let sid_retry = session.id.clone();
            let eid_retry = real_user_eid.unwrap_or_default();
            let ut_retry = user_text_for_turn;
            let uimg_retry = user_images_for_turn;
            let ent_retry = entity.clone();

            let ts_label_opt = (!is_live_turn)
                .then(|| {
                    turn.assistants
                        .last()
                        .map(|a| a.timestamp_ms)
                        .filter(|&ts| ts > 0)
                        .and_then(format_timestamp_hhmm)
                })
                .flatten();

            let copy_icon_id = SharedString::from(format!("assistant-copy-btn-{turn_key}"));
            let left_btns = div()
                .flex()
                .items_center()
                .gap(space::XS)
                .child(icon_action_24_element(
                    copy_icon_id.clone(),
                    "复制回答",
                    motion::animated_copy_icon(
                        copy_icon_id,
                        is_copied,
                        px(14.0),
                        if is_copied {
                            palette.status_success
                        } else {
                            palette.text_secondary
                        },
                        window,
                        cx,
                    ),
                    is_live_turn,
                    palette,
                    window,
                    cx,
                    move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        ent_copy.update(cx, |this, cx| {
                            this.record_copied(copy_id_click.clone(), "已复制回答内容", cx);
                        });
                    },
                ))
                .child(icon_action_24(
                    format!("assistant-fork-btn-{turn_key}"),
                    "分支会话",
                    IconName::GitFork,
                    palette.text_secondary,
                    fork_disabled,
                    palette,
                    window,
                    cx,
                    move |_, cx| {
                        let s = sid_fork.clone();
                        let e = fork_eid.clone();
                        ent_fork.update(cx, |this, cx| {
                            this.fork_from_entry(s, e, cx);
                        });
                    },
                ))
                .child(icon_action_24(
                    format!("assistant-retry-btn-{turn_key}"),
                    "重新生成",
                    IconName::RotateCcw,
                    palette.text_secondary,
                    retry_disabled,
                    palette,
                    window,
                    cx,
                    move |_, cx| {
                        let s = sid_retry.clone();
                        let e = eid_retry.clone();
                        let t = ut_retry.clone();
                        let im = uimg_retry.clone();
                        ent_retry.update(cx, |this, cx| {
                            this.retry_user_turn(s, e, t, im, cx);
                        });
                    },
                ));

            let assistant_actions = div()
                .id(SharedString::from(format!("assistant-actions-{turn_key}")))
                .test_support()
                .w_full()
                .h(height::COMPACT)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(left_btns)
                .children(ts_label_opt.map(|hhmm| {
                    apply_scale(div(), scale::UI)
                        .font_family(mono_family.clone())
                        .text_color(palette.text_secondary)
                        .child(cjk_inline_label(hhmm))
                }));

            assistant_stack = assistant_stack.child(assistant_actions);
        }

        column.child(assistant_stack).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_process_part(
        &mut self,
        turn_key: &str,
        seg_key: &str,
        part_idx: usize,
        is_trailing_live_part: bool,
        tp: &TurnPart,
        tool_results: &BTreeMap<String, Value>,
        messages_generation: u64,
        live: bool,
        reduce_motion: bool,
        running_light_allowed: bool,
        started_at: Instant,
        mono_family: SharedString,
        markdown_style: &TextViewStyle,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        match &tp.part {
            ConversationPart::Thinking(t) => {
                let think_key = format!("{seg_key}:think:{part_idx}");
                let open = self.expanded_thinking.contains(&think_key);
                let ent_th = entity.clone();
                let k_th = think_key.clone();
                let thinking_live = live && tp.is_final_entry && is_trailing_live_part;
                let think_label = if thinking_live {
                    "正在思考"
                } else {
                    "已思考"
                };

                let think_hdr_id = SharedString::from(format!("thinking-hdr-{think_key}"));
                // Collapsed: `"已思考"` / `"正在思考"` + `gap(8px)` + `14px` arrow; hover brightens text without bg block
                let hdr = apply_scale(div(), scale::UI)
                    .id(think_hdr_id.clone())
                    .test_support()
                    .a11y_toggle_button(think_hdr_id, think_label, open)
                    .h(height::INLINE)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .text_color(palette.text_secondary)
                    .hover(|s| s.text_color(palette.text_primary))
                    .on_click(move |_, _, cx| {
                        let k = k_th.clone();
                        ent_th.update(cx, |this, cx| {
                            this.auto_follow_bottom = false;
                            if !this.expanded_thinking.remove(&k) {
                                this.expanded_thinking.insert(k);
                            }
                            cx.notify();
                        });
                    })
                    .child(render_running_light_label_with_scale(
                        think_label,
                        thinking_live && running_light_allowed,
                        started_at,
                        palette.text_secondary,
                        palette.text_primary,
                        scale::UI,
                        window,
                    ))
                    .child(motion::animated_chevron(
                        format!("thinking-hdr-{think_key}"),
                        open,
                        false,
                        px(14.0),
                        palette.text_secondary,
                        window,
                        cx,
                    ));

                let mut col = div().w_full().flex().flex_col().gap(space::XS).child(hdr);
                if let Some(animated_think) = motion::animated_collapse_with_exit(
                    format!("thinking-body-collapse-{think_key}"),
                    open,
                    window,
                    cx,
                    |interactive, window, cx| {
                        let content = if t.redacted && t.thinking.trim().is_empty() {
                            "思考内容已按模型策略省略。".to_owned()
                        } else {
                            t.thinking.clone()
                        };
                        if !interactive {
                            return render_copyable_text_block_inert(
                                content,
                                false,
                                mono_family,
                                palette,
                            )
                            .into_any_element();
                        }
                        let blk_id = SharedString::from(format!("thinking-body-{think_key}"));
                        let copied = self.copy_tracker.is_copied(blk_id.as_ref());
                        render_copyable_text_block(
                            blk_id,
                            content,
                            false,
                            mono_family,
                            copied,
                            palette,
                            |cid, _, cx| {
                                cx.entity().update(cx, |this: &mut Self, cx| {
                                    this.record_copied(cid, "已复制思考内容", cx);
                                });
                            },
                            window,
                            cx,
                        )
                        .into_any_element()
                    },
                ) {
                    col = col.child(animated_think);
                }
                col.into_any_element()
            }
            ConversationPart::Text(txt) => {
                let stream_key = format!("proc:{turn_key}:{seg_key}:{}:{part_idx}", tp.entry_key);
                let md_entity = self.markdown_store.sync_entry(&stream_key, txt, cx);
                super::markdown_view::render_assistant_markdown_with_copy_notice(
                    &md_entity,
                    live && tp.is_final_entry,
                    reduce_motion,
                    palette,
                    markdown_style.clone(),
                    Some(Self::markdown_code_copied_callback(entity)),
                )
                .into_any_element()
            }
            ConversationPart::Tools(calls) => {
                let mut group_col = div().w_full().flex().flex_col().gap(space::XS);
                for call in calls {
                    group_col = group_col.child(self.render_normal_tool_row(
                        call,
                        tool_results.get(&call.id),
                        messages_generation,
                        live,
                        running_light_allowed,
                        started_at,
                        mono_family.clone(),
                        palette,
                        entity,
                        window,
                        cx,
                    ));
                }
                group_col.into_any_element()
            }
            _ => div().into_any_element(),
        }
    }
}

impl Render for MessageListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();
        let mono_family = theme_global.mono_font_family;
        let reduce_motion = cx.reduce_motion();

        let snap = self.client.snapshot();
        let Some(ref sid) = self.session_id.clone() else {
            return div().size_full().into_any_element();
        };

        let Some(session) = snap.loaded_sessions.get(sid).cloned() else {
            self.grouped_turn_cache = None;
            self.prepared_session_cache = None;
            // Session data was not ready immediately; animate once it finishes loading.
            self.suppress_immediate_reveal = false;
            self.retain_seen_secret_inputs(false, cx);
            return div()
                .id("message-list-loading")
                .size_full()
                .into_any_element();
        };
        if session.running && self.inline_edit.is_some() {
            self.inline_edit = None;
        }

        if self.revealed_session_id.as_deref() != Some(sid.as_str()) {
            self.revealed_session_id = Some(sid.clone());
            if self.suppress_immediate_reveal {
                self.suppress_immediate_reveal = false;
                self.content_reveal.clear();
            } else {
                self.content_reveal
                    .start(std::time::Duration::from_millis(180), 6.0, 0.01, cx);
            }
        }
        let (reveal_op, reveal_dy) = self.content_reveal.sample(window, cx);

        let revision_changed = session.revision != self.last_observed_revision;
        if revision_changed {
            self.last_observed_revision = session.revision;
        }

        // Cache prepared turns (grouping + split_turn + segments) across animation frames AND
        // incremental streaming deltas so 60-120fps frames skip cloning and reparsing history.
        let prepared = prepare_session_turns(
            &mut self.grouped_turn_cache,
            &mut self.next_messages_generation,
            &mut self.prepared_session_cache,
            &session,
        );
        let turns = &prepared.turns;
        let tool_results = prepared.tool_results.as_ref();
        let messages_generation = prepared.messages_generation;
        let live_turn_idx = active_prepared_turn_index(turns, session.running);
        if self.auto_follow_bottom && (revision_changed || live_turn_idx.is_some()) {
            self.scroll_handle.scroll_to_bottom();
        } else if !self.auto_follow_bottom {
            self.scroll_handle.cancel_scroll_to_bottom();
        }
        let running_light_allowed = !reduce_motion
            && snap.channel_mode != cst_client::ChannelModeDto::WebActive
            && !crate::background::has_active_hardware_diagnostic(&snap);

        let markdown_style = build_markdown_style(&palette);

        // Tail sliding window (`DEFAULT_VISIBLE_TURN_WINDOW = 30`)
        let total_turns = turns.len();
        let window_start = total_turns.saturating_sub(self.visible_turn_limit);
        let hidden_earlier_count = window_start;

        let outbox_items = self.client.with_outbox(|ob| ob.session_items(sid).to_vec());

        let entity = cx.entity();

        let mut list_col = div()
            .relative()
            .top(reveal_dy)
            .opacity(reveal_op)
            .w_full()
            .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
            .mx_auto()
            .py(space::GROUP)
            .flex()
            .flex_col()
            .gap(space::GROUP);

        if hidden_earlier_count > 0 {
            let ent_more = entity.clone();
            let batch = hidden_earlier_count.min(DEFAULT_VISIBLE_TURN_WINDOW);
            let earlier_label = format!("显示更早的消息（还有 {hidden_earlier_count} 轮）");
            list_col = list_col.child(
                div().w_full().flex().justify_center().child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id("show-earlier-turns-btn")
                        .test_support()
                        .a11y_button("show-earlier-turns-btn", earlier_label.clone())
                        .h(height::CONTROL)
                        .px(space::MD)
                        .rounded(radius::FULL)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .bg(palette.bg_surface)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, _, cx| {
                            #[cfg(feature = "test-support")]
                            crate::ux_probe::mark_input("show_earlier");
                            ent_more.update(cx, |this, cx| {
                                this.visible_turn_limit += batch;
                                this.scroll_to_top(cx);
                                cx.notify();
                            });
                        })
                        .child(cjk_inline_label(earlier_label)),
                ),
            );
        }

        for (idx, prepared_turn) in turns.iter().enumerate().skip(window_start) {
            let is_live = live_turn_idx == Some(idx);
            list_col = list_col.child(self.render_turn_view(
                &session,
                idx,
                prepared_turn,
                tool_results,
                messages_generation,
                is_live,
                mono_family.clone(),
                &markdown_style,
                reduce_motion,
                running_light_allowed,
                transparency_enabled,
                scale_factor,
                &palette,
                &entity,
                window,
                cx,
            ));
        }

        self.retain_seen_secret_inputs(true, cx);

        for ob_item in &outbox_items {
            list_col = list_col
                .child(self.render_outbox_bubble(sid, ob_item, &palette, &entity, window, cx));
        }

        if session.compacting {
            let started_instant = *self
                .turn_started_instants
                .entry(format!("compacting:{sid}"))
                .or_insert_with(Instant::now);
            list_col = list_col.child(
                div()
                    .id("session-compacting-status")
                    .test_support()
                    .w_full()
                    .h(height::COMPACT)
                    .px(px(0.0))
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(motion::animated_spinner(
                        format!("compacting-spin-{sid}"),
                        px(16.0),
                        palette.accent_ink,
                        running_light_allowed,
                        window,
                        cx,
                    ))
                    .child(render_running_light_label_with_scale(
                        "正在压缩上下文…",
                        running_light_allowed,
                        started_instant,
                        palette.text_secondary,
                        palette.text_primary,
                        scale::UI,
                        window,
                    )),
            );
        }

        let show_scroll_bottom_btn =
            !self.auto_follow_bottom && self.scroll_handle.max_offset().y > px(36.0);
        #[cfg(feature = "test-support")]
        if crate::ux_probe::is_enabled() {
            crate::ux_probe::record_scroll(
                f32::from(self.scroll_handle.offset().y),
                f32::from(self.scroll_handle.max_offset().y),
                self.auto_follow_bottom,
                show_scroll_bottom_btn,
            );
            if let Some(layer) = crate::ux_probe::bounds_probe_layer("message_col", None) {
                list_col = list_col.relative().child(layer);
            }
        }

        let mut outer = div()
            .id("session-message-list-viewport")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .child(
                div()
                    .id("session-message-scroll-area")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .on_scroll(cx.listener(Self::on_scroll_wheel))
                    .child(div().w_full().px(space::GROUP).child(list_col)),
            );
        #[cfg(feature = "test-support")]
        if let Some(layer) = crate::ux_probe::bounds_probe_layer("scroll_area", None) {
            outer = outer.child(layer);
        }

        let scroll_btn_progress = motion::surface_presence(
            "scroll-to-bottom-presence",
            show_scroll_bottom_btn,
            motion::POPOVER_OPEN,
            window,
            cx,
        );
        if show_scroll_bottom_btn || scroll_btn_progress > 0.001 {
            let ent_btm = entity.clone();
            let (btn_pressed, press_state) =
                motion::icon_press_state("scroll-to-bottom-btn", window, cx);
            let pill = div()
                .h(height::FIELD)
                .px(space::LG)
                .rounded(radius::ROW)
                .bg(palette.text_primary)
                .shadow(motion::scale_shadows(
                    palette.craft_lift_shadows(),
                    scroll_btn_progress.powi(3),
                ))
                .opacity(0.01 + 0.99 * scroll_btn_progress)
                .mb(px(-8.0 * (1.0 - scroll_btn_progress)))
                .flex()
                .items_center()
                .gap(space::SM)
                .child(motion::animated_icon_press(
                    "scroll-to-bottom-btn",
                    show_scroll_bottom_btn && btn_pressed,
                    Icon::new(IconName::ArrowDown)
                        .size(px(16.0))
                        .text_color(palette.bg_workspace),
                    window,
                    cx,
                ))
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .text_color(palette.bg_workspace)
                        .child(cjk_inline_label("回到底部")),
                );
            let pill = if show_scroll_bottom_btn {
                pill.id("scroll-to-bottom-btn")
                    .test_support()
                    .a11y_button("scroll-to-bottom-btn", "回到底部")
                    .hover(|s| s.opacity(0.92))
                    .on_icon_press(press_state)
                    .on_click(move |_, _, cx| {
                        ent_btm.update(cx, |this, cx| {
                            this.scroll_to_bottom_and_follow(cx);
                        });
                    })
                    .into_any_element()
            } else {
                pill.into_any_element()
            };
            outer = outer.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(space::LG)
                    .flex()
                    .justify_center()
                    .child(pill),
            );
        }

        outer.into_any_element()
    }
}

fn render_process_part_inert(
    tp: &TurnPart,
    tool_results: &BTreeMap<String, Value>,
    live: bool,
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> gpui::AnyElement {
    match &tp.part {
        ConversationPart::Thinking(_) => apply_scale(div(), scale::UI)
            .h(height::INLINE)
            .flex()
            .items_center()
            .gap(space::SM)
            .text_color(palette.text_secondary)
            .child(cjk_inline_label("已思考"))
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(14.0))
                    .text_color(palette.text_secondary),
            )
            .into_any_element(),
        ConversationPart::Text(txt) => apply_scale(div(), scale::UI)
            .text_color(palette.text_secondary)
            .child(SharedString::from(txt.clone()))
            .into_any_element(),
        ConversationPart::Tools(calls) => {
            let mut group_col = div().w_full().flex().flex_col().gap(space::XS);
            for call in calls {
                let tool_result_val = tool_results.get(&call.id);
                let duration_ms = tool_result_val
                    .and_then(|v| {
                        v.get("durationMs")
                            .or_else(|| v.get("details").and_then(|d| d.get("durationMs")))
                    })
                    .and_then(Value::as_u64);
                let is_err = tool_result_val
                    .and_then(|v| v.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                group_col = group_col.child(render_normal_tool_row_inert(
                    &call.name,
                    Some(&call.arguments),
                    duration_ms,
                    tool_result_val.is_some(),
                    is_err,
                    live,
                    mono_family.clone(),
                    palette,
                ));
            }
            group_col.into_any_element()
        }
        _ => div().into_any_element(),
    }
}

fn inline_image_key(mime_type: &str, base64: &str) -> String {
    format!("inline:{mime_type}:{:x}", Sha256::digest(base64.as_bytes()))
}

fn format_timestamp_hhmm(ts_ms: u64) -> Option<String> {
    let dt = DateTime::from_timestamp_millis(ts_ms as i64)?;
    Some(dt.with_timezone(&Local).format("%H:%M").to_string())
}

#[allow(clippy::too_many_arguments)]
fn icon_action_24_element(
    id: impl Into<SharedString>,
    label: &'static str,
    icon: Icon,
    disabled: bool,
    palette: &DesktopPalette,
    window: &mut Window,
    cx: &mut App,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id_shared = id.into();
    if disabled {
        return div()
            .id(id_shared.clone())
            .test_support()
            .a11y_button(id_shared.clone(), label)
            .desktop_tooltip(id_shared, label)
            .w(height::INLINE)
            .h(height::INLINE)
            .rounded(radius::CONTROL)
            .opacity(0.35)
            .flex()
            .items_center()
            .justify_center()
            .child(icon);
    }

    let (pressed, press_state) = motion::icon_press_state(id_shared.as_ref(), window, cx);
    let (hovered, hs) = motion::PointerFlag::keyed(
        gpui_kit::ElementId::Name(SharedString::from(format!("hover-{id_shared}"))),
        window,
        cx,
    );
    let hover_t = motion::animated_state_progress(id_shared.clone(), hovered, window, cx);
    let press_t = motion::animated_press_progress(id_shared.clone(), pressed, window, cx);
    let bg = motion::lerp_hsla(
        motion::lerp_hsla(
            gpui_kit::transparent_black(),
            palette.bg_item_hover,
            hover_t,
        ),
        palette.bg_active_press,
        press_t,
    );

    div()
        .id(id_shared.clone())
        .test_support()
        .a11y_button(id_shared.clone(), label)
        .desktop_tooltip_with_hover(
            SharedString::from(format!("tooltip-{id_shared}")),
            label.into(),
            false,
            Some(hs),
        )
        .w(height::INLINE)
        .h(height::INLINE)
        .rounded(radius::CONTROL)
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .on_icon_press(press_state)
        .on_click(move |_, w, cx| on_click(w, cx))
        .child(motion::animated_icon_press(
            id_shared, pressed, icon, window, cx,
        ))
}

#[allow(clippy::too_many_arguments)]
fn icon_action_24(
    id: impl Into<SharedString>,
    label: &'static str,
    icon: IconName,
    icon_color: gpui_kit::Hsla,
    disabled: bool,
    palette: &DesktopPalette,
    window: &mut Window,
    cx: &mut App,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    icon_action_24_element(
        id,
        label,
        Icon::new(icon).size(px(14.0)).text_color(icon_color),
        disabled,
        palette,
        window,
        cx,
        on_click,
    )
}

fn to_tool_call_input(call: &ToolCallPart) -> ToolCallInput {
    ToolCallInput {
        id: call.id.clone(),
        name: call.name.clone(),
        arguments: call.arguments.as_object().cloned().unwrap_or_default(),
        execution: Some(ToolExecutionMeta::default()),
    }
}

fn resolve_session_active_provider(client: &ClientHandle, session_id: &str) -> String {
    let draft_prov = client.with_workspace(|ws| {
        ws.draft(&DraftKey::Session(session_id.to_owned()))
            .and_then(|d| d.selected_provider.clone())
    });
    if let Some(p) = draft_prov.filter(|s| !s.is_empty()) {
        return p;
    }
    let snap = client.snapshot();
    if let Some(ref models) = snap.models {
        if let Some(ref sel) = models.selected
            && !sel.provider.is_empty()
        {
            return sel.provider.clone();
        }
        if let Some(ref def) = models.default_model
            && !def.provider.is_empty()
        {
            return def.provider.clone();
        }
    }
    "cstoa".to_owned()
}

async fn load_turn_images_into_pool(
    client: &ClientHandle,
    session_id: &str,
    parts: &[ImageContentPart],
) -> Result<Vec<cst_client::SharedImageHandle>, ClientError> {
    let pool = client.with_workspace(|ws| ws.image_pool().clone());
    let mut draft = ComposerDraft::default();
    let invalid_image = || ClientError::BadRequest {
        code: "invalid_image".into(),
        message: "无法读取历史消息的附图。".into(),
    };
    for part in parts {
        let bytes = if let Some(b64) = part.data_base64.as_deref().filter(|s| !s.is_empty()) {
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|_| invalid_image())?
        } else if let Some(id) = part.attachment_id.as_deref().filter(|s| !s.is_empty()) {
            client
                .get_session_attachment_bytes(session_id.to_owned(), id.to_owned())
                .await?
        } else {
            return Err(invalid_image());
        };
        draft.attach_image(pool.admit_image(bytes, None)?)?;
    }
    Ok(draft.images)
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_images_with_a_shared_prefix_have_distinct_cache_keys() {
        let prefix = "a".repeat(32);
        assert_ne!(
            inline_image_key("image/png", &format!("{prefix}1234")),
            inline_image_key("image/png", &format!("{prefix}5678")),
        );
        let _ = inline_image_key("image/png", "aé易✋aé易✋aé易✋aé易✋");
    }

    #[test]
    fn grouped_turn_cache_reuses_only_the_same_message_allocation() {
        let user =
            |id: &str| serde_json::json!({"id": id, "message": {"role": "user", "content": id}});
        let mut messages = Arc::new(vec![user("first")]);
        let mut cache = None;
        let first = GroupedTurnCache::for_messages(&mut cache, &messages);
        assert!(Arc::ptr_eq(
            &first,
            &GroupedTurnCache::for_messages(&mut cache, &messages)
        ));

        Arc::make_mut(&mut messages)[0] = user("updated");
        let updated = GroupedTurnCache::for_messages(&mut cache, &messages);
        assert!(!Arc::ptr_eq(&first, &updated));
        assert_eq!(
            updated.0[0].user.as_ref().unwrap().id.as_deref(),
            Some("updated")
        );

        let source = cache.as_ref().unwrap().source.clone();
        drop(messages);
        assert!(
            source.upgrade().is_none(),
            "the cache must not pin message history"
        );
        let new_session = Arc::new(vec![user("next")]);
        let next = GroupedTurnCache::for_messages(&mut cache, &new_session);
        assert_eq!(next.0[0].user.as_ref().unwrap().id.as_deref(), Some("next"));
    }

    #[test]
    fn prepare_session_turns_caches_animation_frames_and_reuses_history_during_streaming() {
        let messages = Arc::new(vec![
            serde_json::json!({"id": "u1", "message": {"role": "user", "content": "第一轮"}}),
            serde_json::json!({"id": "a1", "message": {"role": "assistant", "content": [{"type": "text", "text": "已完成"}]}}),
            serde_json::json!({"id": "u2", "message": {"role": "user", "content": "第二轮"}}),
        ]);
        let mut snap = SessionViewSnapshot {
            id: "sess-1".into(),
            revision: 10,
            event_cursor: None,
            leaf_id: None,
            running: true,
            compacting: false,
            messages: Arc::clone(&messages),
            entries: Arc::new(Vec::new()),
            context_usage: None,
            usage: None,
            queue: None,
            ui: None,
            bitlocker: None,
            streaming_assistant: Some(serde_json::json!({
                "id": "a2",
                "role": "assistant",
                "content": [{"type": "text", "text": "正在"}]
            })),
            active_tools: Arc::new(BTreeMap::new()),
            active_turn_started_ms: Some(1000),
            estimated_bytes: 256,
        };

        let mut grouped_cache = None;
        let mut next_gen = 0u64;
        let mut prepared_cache = None;

        // 1. Same revision (animation frame): returns the exact same PreparedSessionData allocation.
        let p1 = prepare_session_turns(
            &mut grouped_cache,
            &mut next_gen,
            &mut prepared_cache,
            &snap,
        );
        let p2 = prepare_session_turns(
            &mut grouped_cache,
            &mut next_gen,
            &mut prepared_cache,
            &snap,
        );
        assert!(Arc::ptr_eq(&p1, &p2));
        assert_eq!(p1.turns.len(), 2);

        // 2. Streaming delta (revision bumps, `messages` Arc unchanged):
        //    - `messages_generation` stays stable (tool caches stay warm)
        //    - Historical turn 0 reuses the exact same `Arc<PreparedTurn>`
        //    - Only active tail turn 1 updates its text.
        snap.revision = 11;
        snap.streaming_assistant = Some(serde_json::json!({
            "id": "a2",
            "role": "assistant",
            "content": [{"type": "text", "text": "正在采集整机概览"}]
        }));
        let p3 = prepare_session_turns(
            &mut grouped_cache,
            &mut next_gen,
            &mut prepared_cache,
            &snap,
        );
        assert!(!Arc::ptr_eq(&p1, &p3));
        assert_eq!(p1.messages_generation, p3.messages_generation);
        assert!(Arc::ptr_eq(&p1.turns[0], &p3.turns[0]));
        assert!(!Arc::ptr_eq(&p1.turns[1], &p3.turns[1]));
        assert_eq!(
            p3.turns[1].split.as_ref().unwrap().final_text,
            "正在采集整机概览"
        );
    }

    #[test]
    fn prepare_session_turns_prefers_entries_and_preserves_compaction_summary() {
        let entries = Arc::new(vec![
            serde_json::json!({"id": "u1", "message": {"role": "user", "content": "压缩前提问", "timestamp": 1000}}),
            serde_json::json!({"id": "a1", "message": {"role": "assistant", "content": [{"type": "text", "text": "压缩前回复"}], "timestamp": 1100}}),
            serde_json::json!({"id": "c1", "message": {"role": "compactionSummary", "summary": "已完成历史总结", "tokensBefore": 148000, "timestamp": 1200}}),
            serde_json::json!({"id": "u2", "message": {"role": "user", "content": "压缩后继续", "timestamp": 1300}}),
        ]);
        let snap = SessionViewSnapshot {
            id: "sess-compact".into(),
            revision: 1,
            event_cursor: None,
            leaf_id: Some("u2".into()),
            running: false,
            compacting: false,
            messages: Arc::new(vec![
                serde_json::json!({"role": "compactionSummary", "summary": "已完成历史总结", "tokensBefore": 148000, "timestamp": 1200}),
                serde_json::json!({"role": "user", "content": "压缩后继续", "timestamp": 1300}),
            ]),
            entries,
            context_usage: None,
            usage: None,
            queue: None,
            ui: None,
            bitlocker: None,
            streaming_assistant: None,
            active_tools: Arc::new(BTreeMap::new()),
            active_turn_started_ms: None,
            estimated_bytes: 512,
        };
        let mut grouped_cache = None;
        let mut next_gen = 0u64;
        let mut prepared_cache = None;
        let prepared = prepare_session_turns(
            &mut grouped_cache,
            &mut next_gen,
            &mut prepared_cache,
            &snap,
        );
        // Should retain pre-compaction turn + standalone compaction turn + post-compaction turn.
        assert_eq!(prepared.turns.len(), 3);
        assert!(prepared.turns[0].turn.user.is_some());
        assert!(prepared.turns[1].turn.compaction.is_some());
        assert!(prepared.turns[2].turn.user.is_some());
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use chrono::{DateTime, Local};
use cst_client::tool_presenter::{ToolCallInput, ToolExecutionMeta, ToolResultInput};
use cst_client::{
    ClientError, ClientHandle, ComposerDraft, DraftKey, OutboxItem, OutboxItemState,
    SessionViewSnapshot,
};
use gpui::{Image, ImageSource, ObjectFit, ScrollEvent, ScrollHandle, StyledImage as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt, Window, div, img, px,
};
use serde_json::Value;

use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius,
    scale, space,
};
use crate::tools::bitlocker_card::{SecretInput, SecretInputEvent, find_pending_bitlocker_request};
use crate::tools::normal_card::render_normal_tool_card_with_args;
use crate::tools::{SpecialToolDispatchProps, render_special_tool_component};
use crate::workbench::image_viewer::{
    LightboxImageTarget, gpui_image_from_bytes, gpui_image_from_handle,
};
use crate::workbench::markdown_view::{
    CopyFeedbackTracker, MarkdownStreamStore, render_assistant_markdown, render_copyable_text_block,
};
use crate::workbench::turn_segments::{
    ConversationPart, ImageContentPart, SpecialToolKind, ToolCallPart, Turn, TurnEntry, TurnPart,
    TurnSegmentKind, active_turn_index, build_turn_segments, extract_message_text,
    extract_user_images, format_duration_zh, group_turns_and_results, process_segment_label,
    render_running_light_label, split_turn,
};

pub const DEFAULT_VISIBLE_TURN_WINDOW: usize = 30;

#[derive(Debug, Clone)]
pub enum MessageListEvent {
    OpenLightbox(LightboxImageTarget),
    SwitchToSession(String),
    FocusComposer,
    RequestEditUserTurn {
        session_id: String,
        parent_entry_id: Option<String>,
        user_text: String,
    },
    MissingCredentials {
        provider: String,
    },
    ErrorMessage(String),
    TopNotice(String),
}

pub struct MessageListView {
    client: ClientHandle,
    session_id: Option<String>,
    last_observed_revision: u64,
    scroll_handle: ScrollHandle,
    auto_follow_bottom: bool,
    visible_turn_limit: usize,
    expanded_segments: BTreeSet<String>,
    expanded_thinking: BTreeSet<String>,
    expanded_error_details: BTreeSet<String>,
    expanded_tool_cards: BTreeSet<String>,
    expanded_tool_raw: BTreeSet<String>,
    expanded_special_details: BTreeSet<String>,
    hovered_warnings: BTreeSet<String>,
    secret_inputs: BTreeMap<String, Entity<SecretInput>>,
    secret_input_subs: Vec<Subscription>,
    markdown_store: MarkdownStreamStore,
    copy_tracker: CopyFeedbackTracker,
    turn_started_instants: BTreeMap<String, Instant>,
    attachment_cache: BTreeMap<String, Arc<Image>>,
    attachment_loading: BTreeSet<String>,
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
            visible_turn_limit: DEFAULT_VISIBLE_TURN_WINDOW,
            expanded_segments: BTreeSet::new(),
            expanded_thinking: BTreeSet::new(),
            expanded_error_details: BTreeSet::new(),
            expanded_tool_cards: BTreeSet::new(),
            expanded_tool_raw: BTreeSet::new(),
            expanded_special_details: BTreeSet::new(),
            hovered_warnings: BTreeSet::new(),
            secret_inputs: BTreeMap::new(),
            secret_input_subs: Vec::new(),
            markdown_store: MarkdownStreamStore::default(),
            copy_tracker: CopyFeedbackTracker::default(),
            turn_started_instants: BTreeMap::new(),
            attachment_cache: BTreeMap::new(),
            attachment_loading: BTreeSet::new(),
        }
    }

    pub fn set_session_id(&mut self, next_id: Option<String>, cx: &mut Context<Self>) {
        if self.session_id == next_id {
            return;
        }
        self.session_id = next_id;
        self.last_observed_revision = 0;
        self.auto_follow_bottom = true;
        self.visible_turn_limit = DEFAULT_VISIBLE_TURN_WINDOW;
        self.expanded_segments.clear();
        self.expanded_thinking.clear();
        self.expanded_error_details.clear();
        self.expanded_tool_cards.clear();
        self.expanded_tool_raw.clear();
        self.expanded_special_details.clear();
        self.hovered_warnings.clear();
        for (_, si) in std::mem::take(&mut self.secret_inputs) {
            si.update(cx, |s, cx| s.clear(cx));
        }
        self.secret_input_subs.clear();
        self.markdown_store.clear();
        self.turn_started_instants.clear();
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    pub fn secret_input_for_call(&self, call_id: &str) -> Option<&Entity<SecretInput>> {
        self.secret_inputs.get(call_id)
    }

    pub fn is_auto_following(&self) -> bool {
        self.auto_follow_bottom
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
        cx.spawn(async move |weak, cx| {
            let res = client.get_session_attachment_bytes(sid, aid).await;
            let _ = weak.update(cx, |this, cx| {
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
            let cache_key = format!("inline:{}:{}", b64.len(), &b64[..b64.len().min(32)]);
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

    pub fn apply_edit_and_rewrite_user_turn(
        &mut self,
        session_id: String,
        parent_entry_id: Option<String>,
        user_text: String,
        cx: &mut Context<Self>,
    ) {
        let key = DraftKey::Session(session_id.clone());
        self.client.with_workspace_mut(|ws| {
            let draft = ComposerDraft {
                cursor_char_offset: user_text.chars().count(),
                text: user_text,
                ..ComposerDraft::default()
            };
            ws.restore_draft(key, draft);
        });
        cx.emit(MessageListEvent::FocusComposer);

        if let Some(target_parent_id) = parent_entry_id {
            let client = self.client.clone();
            cx.spawn(async move |weak, cx| {
                let res = client
                    .navigate_session_tree(session_id.clone(), target_parent_id, false, None)
                    .await;
                let _ = client.activate_session(Some(session_id)).await;
                if let Err(err) = res {
                    let _ = weak.update(cx, |_, cx| {
                        cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                    });
                }
            })
            .detach();
        }
    }

    fn request_edit_user_turn(
        &mut self,
        session_id: String,
        parent_entry_id: Option<String>,
        user_text: String,
        cx: &mut Context<Self>,
    ) {
        cx.emit(MessageListEvent::RequestEditUserTurn {
            session_id,
            parent_entry_id,
            user_text,
        });
    }

    fn retry_user_turn(
        &mut self,
        session_id: String,
        parent_entry_id: Option<String>,
        user_text: String,
        cx: &mut Context<Self>,
    ) {
        if user_text.trim().is_empty() {
            return;
        }
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            if let Some(pid) = parent_entry_id
                && let Err(err) = client
                    .navigate_session_tree(session_id.clone(), pid, false, None)
                    .await
            {
                let _ = weak.update(cx, |_, cx| {
                    cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                });
                return;
            }
            let key = DraftKey::Session(session_id.clone());
            client.with_workspace_mut(|ws| {
                let draft = ComposerDraft {
                    cursor_char_offset: user_text.chars().count(),
                    text: user_text,
                    ..ComposerDraft::default()
                };
                ws.restore_draft(key.clone(), draft);
            });
            match client.submit_draft(key).await {
                Ok(_) => {}
                Err(ClientError::CredentialsRequired { .. }) => {
                    let _ = weak.update(cx, |_, cx| {
                        cx.emit(MessageListEvent::MissingCredentials {
                            provider: "cstoa".into(),
                        });
                    });
                }
                Err(err) => {
                    let _ = weak.update(cx, |_, cx| {
                        cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                    });
                }
            }
        })
        .detach();
    }

    fn fork_from_entry(&mut self, session_id: String, entry_id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.fork_session(session_id, entry_id).await;
            let _ = client.refresh_globals().await;
            let _ = weak.update(cx, |_, cx| match res {
                Ok(new_sid) => {
                    if !new_sid.is_empty() {
                        cx.emit(MessageListEvent::SwitchToSession(new_sid));
                    } else {
                        cx.emit(MessageListEvent::TopNotice(
                            "已从当前位置派生新对话。".into(),
                        ));
                    }
                }
                Err(err) => {
                    cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                }
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn render_user_bubble(
        &mut self,
        session_id: &str,
        turn_key: &str,
        user_entry: &TurnEntry,
        parent_entry_id: Option<String>,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let text = extract_message_text(&user_entry.raw);
        let images = extract_user_images(&user_entry.raw);

        let mut bubble = div()
            .max_w(px(560.0))
            .px(space::LG)
            .py(space::MD)
            .rounded(radius::ROW)
            .bg(palette.bg_surface)
            .flex()
            .flex_col()
            .gap(space::SM);

        if !images.is_empty() {
            let mut img_row = div().flex().flex_wrap().gap(space::SM);
            for (idx, part) in images.iter().enumerate() {
                if let Some(img_arc) = self.resolve_image_part(session_id, part, cx) {
                    let ent_open = entity.clone();
                    let img_open = Arc::clone(&img_arc);
                    img_row = img_row.child(
                        div()
                            .id(SharedString::from(format!("user-msg-img-{turn_key}-{idx}")))
                            .test_support()
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
            bubble = bubble.child(img_row);
        }

        if !text.trim().is_empty() {
            bubble = bubble.child(
                apply_scale(div(), scale::BODY)
                    .text_color(palette.text_primary)
                    .child(SharedString::from(text.clone())),
            );
        }

        let copy_id = SharedString::from(format!("user-copy-{turn_key}"));
        let is_copied = self.copy_tracker.is_copied(copy_id.as_ref());
        let text_copy = text.clone();
        let copy_id_click = copy_id.clone();
        let ent_copy = entity.clone();

        let sid_edit = session_id.to_owned();
        let parent_edit = parent_entry_id.clone();
        let text_edit = text.clone();
        let ent_edit = entity.clone();

        let sid_retry = session_id.to_owned();
        let parent_retry = parent_entry_id;
        let text_retry = text;
        let ent_retry = entity.clone();

        // Bottom-right `24×24px` icon-only action bar (`SHcR1` / `b0aK5`)
        let action_bar = div()
            .id(SharedString::from(format!("user-actions-{turn_key}")))
            .test_support()
            .h(height::INLINE)
            .flex()
            .items_center()
            .justify_end()
            .gap(space::XS)
            .child(icon_action_24(
                format!("user-copy-btn-{turn_key}"),
                if is_copied {
                    IconName::Check
                } else {
                    IconName::Copy
                },
                if is_copied {
                    palette.status_success
                } else {
                    palette.text_secondary
                },
                false,
                palette,
                move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text_copy.clone()));
                    ent_copy.update(cx, |this, cx| {
                        this.copy_tracker.mark_copied(
                            copy_id_click.clone(),
                            |v| &mut v.copy_tracker,
                            cx,
                        );
                    });
                },
            ))
            .child(icon_action_24(
                format!("user-edit-btn-{turn_key}"),
                IconName::Pencil,
                palette.text_secondary,
                false,
                palette,
                move |_, cx| {
                    let s = sid_edit.clone();
                    let p = parent_edit.clone();
                    let t = text_edit.clone();
                    ent_edit.update(cx, |this, cx| {
                        this.request_edit_user_turn(s, p, t, cx);
                    });
                },
            ))
            .child(icon_action_24(
                format!("user-retry-btn-{turn_key}"),
                IconName::RotateCcw,
                palette.text_secondary,
                false,
                palette,
                move |_, cx| {
                    let s = sid_retry.clone();
                    let p = parent_retry.clone();
                    let t = text_retry.clone();
                    ent_retry.update(cx, |this, cx| {
                        this.retry_user_turn(s, p, t, cx);
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

    fn render_outbox_bubble(
        &self,
        session_id: &str,
        item: &OutboxItem,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
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
                row = row.child(
                    div()
                        .id(SharedString::from(format!("outbox-img-{mid}-{idx}")))
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
                    Icon::new(IconName::TriangleAlert)
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
                        IconName::RotateCcw,
                        palette.text_secondary,
                        false,
                        palette,
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
                        IconName::Pencil,
                        palette.text_secondary,
                        false,
                        palette,
                        move |_, cx| {
                            ent_edit.update(cx, |this, cx| {
                                match this.client.edit_outbox_item(
                                    Some(&sid_edit),
                                    &mid_edit,
                                    false,
                                ) {
                                    Ok(_) => cx.emit(MessageListEvent::FocusComposer),
                                    Err(err) => {
                                        cx.emit(MessageListEvent::ErrorMessage(err.to_string()));
                                    }
                                }
                                cx.notify();
                            });
                        },
                    ))
                    .child(icon_action_24(
                        format!("outbox-remove-{mid}"),
                        IconName::Trash,
                        palette.status_danger,
                        false,
                        palette,
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
        mono_family: SharedString,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let input = to_tool_call_input(call);
        let parsed_res = tool_result_val.map(ToolResultInput::from_message_json);
        let empty_res = ToolResultInput::default();
        let card_view = self.client.with_presenter_cache(|cache| {
            cache.get_or_map_normal(
                &input,
                parsed_res.as_ref().unwrap_or(&empty_res),
                result_revision,
            )
        });

        let duration_ms = tool_result_val
            .and_then(|v| {
                v.get("durationMs")
                    .or_else(|| v.get("details").and_then(|d| d.get("durationMs")))
            })
            .and_then(Value::as_u64);

        let card_key = format!("tool-card-{}", call.id);
        let is_open = self.expanded_tool_cards.contains(&card_key);
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
            parsed_res.is_some(),
            parsed_res.as_ref().is_some_and(|r| r.is_error),
            live,
            &card_view,
            is_open,
            is_raw_open,
            mono_family,
            palette,
            &|cid| copy_ref.is_copied(cid),
            Rc::new(move |key, _, cx| {
                ent_card.update(cx, |this, cx| {
                    if !this.expanded_tool_cards.remove(&key) {
                        this.expanded_tool_cards.insert(key);
                    }
                    cx.notify();
                });
            }),
            Rc::new(move |key, _, cx| {
                ent_raw.update(cx, |this, cx| {
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
                    this.copy_tracker
                        .mark_copied(cid, |v| &mut v.copy_tracker, cx);
                });
            },
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
        mono_family: SharedString,
        transparency_enabled: bool,
        scale_factor: f32,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let input = to_tool_call_input(call);
        let parsed_res = tool_result_val.map(ToolResultInput::from_message_json);
        let Some(special) = self.client.with_presenter_cache(|cache| {
            cache.get_or_map_special(&input, parsed_res.as_ref(), result_revision, live)
        }) else {
            return div().into_any_element();
        };

        let has_result = tool_result_val.is_some();
        let secret_input = if kind == SpecialToolKind::BitLocker {
            let pending_opt =
                find_pending_bitlocker_request(session.bitlocker.as_ref(), &call.id, has_result);
            if let Some(ref p) = pending_opt
                && p.kind == "password"
            {
                let call_id_owned = call.id.clone();
                let sid_owned = session.id.clone();
                let rid_owned = p.request_id.clone();
                Some(
                    self.secret_inputs
                        .entry(call.id.clone())
                        .or_insert_with(|| {
                            let si = cx.new(SecretInput::new);
                            let sub = cx.subscribe(&si, move |this, si_ent, ev, cx| {
                                match ev {
                                    SecretInputEvent::SubmitRequested => {
                                        let (buf, len) =
                                            si_ent.read(cx).snapshot_zeroizing_payload();
                                        if len == 0 {
                                            si_ent.update(cx, |s, cx| {
                                                s.set_error_message(
                                                    Some("请输入八组六位数字恢复密码。"),
                                                    cx,
                                                );
                                            });
                                            return;
                                        }
                                        let c = this.client.clone();
                                        let s_id = sid_owned.clone();
                                        let r_id = rid_owned.clone();
                                        let si_done = si_ent.clone();
                                        cx.spawn(async move |weak, cx| {
                                            let res = c
                                                .respond_bitlocker_password(s_id, r_id, buf, len)
                                                .await;
                                            let _ = weak.update(cx, |_, cx| match res {
                                                Ok(_) => {
                                                    si_done.update(cx, |s, cx| s.clear(cx));
                                                }
                                                Err(ClientError::BadRequest {
                                                    ref code,
                                                    ref message,
                                                }) if code == "invalid_bitlocker_input" => {
                                                    let msg = message.clone();
                                                    si_done.update(cx, |s, cx| {
                                                        s.set_error_message(Some(msg), cx);
                                                    });
                                                }
                                                Err(err) => {
                                                    si_done.update(cx, |s, cx| s.clear(cx));
                                                    cx.emit(MessageListEvent::ErrorMessage(
                                                        err.to_string(),
                                                    ));
                                                }
                                            });
                                        })
                                        .detach();
                                    }
                                    SecretInputEvent::CancelRequested => {
                                        let c = this.client.clone();
                                        let s_id = sid_owned.clone();
                                        let r_id = rid_owned.clone();
                                        cx.spawn(async move |weak, cx| {
                                            if let Err(err) =
                                                c.cancel_bitlocker_request(s_id, r_id).await
                                            {
                                                let _ = weak.update(cx, |_, cx| {
                                                    cx.emit(MessageListEvent::ErrorMessage(
                                                        err.to_string(),
                                                    ));
                                                });
                                            }
                                        })
                                        .detach();
                                    }
                                }
                                let _ = &call_id_owned;
                            });
                            self.secret_input_subs.push(sub);
                            si
                        })
                        .clone(),
                )
            } else {
                if let Some(removed) = self.secret_inputs.remove(&call.id) {
                    removed.update(cx, |s, cx| s.clear(cx));
                }
                None
            }
        } else {
            None
        };

        let details_expanded = match kind {
            SpecialToolKind::SysOverview => self
                .expanded_special_details
                .contains(&format!("sys-overview-details-{}", call.id)),
            SpecialToolKind::WebSearch => self
                .expanded_special_details
                .contains(&format!("web-search-expand-{}", call.id)),
            SpecialToolKind::BitLocker => self
                .expanded_special_details
                .contains(&format!("bitlocker-details-{}", call.id)),
            SpecialToolKind::Runbook => false,
        };
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
                has_result,
                view: &special,
                bitlocker_snapshot: session.bitlocker.as_ref(),
                secret_input,
                details_expanded,
                warning_hovered,
                transparency_enabled,
                scale_factor,
            },
            self.client.clone(),
            mono_family,
            palette,
            &|cid| copy_ref.is_copied(cid),
            Rc::new(move |key, _, cx| {
                ent_toggle.update(cx, |this, cx| {
                    if !this.expanded_special_details.remove(&key) {
                        this.expanded_special_details.insert(key);
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
                    this.copy_tracker
                        .mark_copied(cid, |v| &mut v.copy_tracker, cx);
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
        turn: &Turn,
        parent_entry_id: Option<String>,
        tool_results: &BTreeMap<String, Value>,
        is_live_turn: bool,
        mono_family: SharedString,
        reduce_motion: bool,
        transparency_enabled: bool,
        scale_factor: f32,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let turn_key = turn
            .user
            .as_ref()
            .map(TurnEntry::stable_key)
            .or_else(|| turn.summary.as_ref().map(TurnEntry::stable_key))
            .or_else(|| turn.assistants.first().map(TurnEntry::stable_key))
            .unwrap_or_else(|| format!("idx-{turn_idx}"));

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
                            this.copy_tracker
                                .mark_copied(cid, |v| &mut v.copy_tracker, cx);
                        });
                    },
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
                &turn_key,
                user_entry,
                parent_entry_id.clone(),
                palette,
                entity,
                cx,
            ));
        }

        if turn.assistants.is_empty() && !is_live_turn {
            return column.into_any_element();
        }

        let split = split_turn(&turn.assistants);
        let segments = build_turn_segments(split.process);

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

                    let seg_state_key = format!("{turn_key}:{}", segment.key);
                    let seg_expanded = self.expanded_segments.contains(&seg_state_key);
                    let has_expandable_parts = !segment.parts.is_empty();

                    let header_text = if seg_idx == 0 {
                        let base = if is_live_turn {
                            format!("工作中 {}", format_duration_zh(live_elapsed_ms))
                        } else {
                            format!("已工作 {}", format_duration_zh(split.total_duration_ms))
                        };
                        match sub_label {
                            Some(sub) => format!("{base} · {sub}"),
                            None => base,
                        }
                    } else {
                        sub_label.unwrap_or_else(|| "处理过程".to_owned())
                    };

                    let ent_seg = entity.clone();
                    let k_seg = seg_state_key.clone();
                    let mut seg_hdr = div()
                        .id(SharedString::from(format!("process-hdr-{seg_state_key}")))
                        .test_support()
                        .h(height::COMPACT)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .text_color(palette.text_secondary)
                        .hover(|s| s.text_color(palette.text_primary));

                    if has_expandable_parts {
                        seg_hdr = seg_hdr.on_click(move |_, _, cx| {
                            let k = k_seg.clone();
                            ent_seg.update(cx, |this, cx| {
                                if !this.expanded_segments.remove(&k) {
                                    this.expanded_segments.insert(k);
                                }
                                cx.notify();
                            });
                        });
                    }

                    seg_hdr = seg_hdr.child(render_running_light_label(
                        &header_text,
                        is_live_turn && seg_idx == 0,
                        started_instant,
                        reduce_motion,
                        palette,
                        window,
                    ));

                    if has_expandable_parts {
                        seg_hdr = seg_hdr.child(
                            Icon::new(if seg_expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(14.0))
                            .text_color(palette.text_secondary),
                        );
                    }

                    let mut seg_box = div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::SM)
                        .child(seg_hdr);

                    if seg_expanded && has_expandable_parts {
                        let mut expanded_col = div().w_full().flex().flex_col().gap(space::SM);

                        for (part_idx, tp) in segment.parts.iter().enumerate() {
                            expanded_col = expanded_col.child(self.render_process_part(
                                session,
                                &seg_state_key,
                                part_idx,
                                tp,
                                tool_results,
                                is_live_turn,
                                mono_family.clone(),
                                palette,
                                entity,
                                cx,
                            ));
                        }
                        seg_box = seg_box.child(expanded_col);
                    }

                    assistant_stack = assistant_stack.child(seg_box);
                }
                TurnSegmentKind::Visible => {
                    for (v_idx, tp) in segment.parts.iter().enumerate() {
                        match &tp.part {
                            ConversationPart::Special { kind, call } => {
                                assistant_stack =
                                    assistant_stack.child(self.render_special_tool_surface(
                                        session,
                                        &tp.entry_key,
                                        *kind,
                                        call,
                                        tool_results.get(&call.id),
                                        session.revision,
                                        is_live_turn,
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
                                    assistant_stack = assistant_stack.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "assistant-img-{turn_key}-{v_idx}"
                                            )))
                                            .test_support()
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
            assistant_stack =
                assistant_stack.child(render_assistant_markdown(&md_entity, is_live_turn, palette));
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
                err_col = err_col.child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id(SharedString::from(format!(
                            "assistant-err-detail-btn-{turn_key}"
                        )))
                        .test_support()
                        .flex()
                        .items_center()
                        .gap(space::MICRO)
                        .text_color(palette.text_muted)
                        .on_click(move |_, _, cx| {
                            let k = k_det.clone();
                            ent_det.update(cx, |this, cx| {
                                if !this.expanded_error_details.remove(&k) {
                                    this.expanded_error_details.insert(k);
                                }
                                cx.notify();
                            });
                        })
                        .child(cjk_inline_label("详情"))
                        .child(
                            Icon::new(if det_open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(11.0))
                            .text_color(palette.text_muted),
                        ),
                );
                if det_open {
                    let blk_id = SharedString::from(format!("assistant-err-raw-{turn_key}"));
                    let copied = self.copy_tracker.is_copied(blk_id.as_ref());
                    err_col = err_col.child(render_copyable_text_block(
                        blk_id,
                        raw_detail.clone(),
                        true,
                        mono_family.clone(),
                        copied,
                        palette,
                        |cid, _, cx| {
                            cx.entity().update(cx, |this: &mut Self, cx| {
                                this.copy_tracker
                                    .mark_copied(cid, |v| &mut v.copy_tracker, cx);
                            });
                        },
                        cx,
                    ));
                }
            }
            assistant_stack = assistant_stack.child(err_col);
        }

        // Manual stop marker (`已停止生成`)
        if split.was_aborted {
            assistant_stack = assistant_stack.child(
                apply_scale(div(), scale::CAPTION)
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
            let user_entry_id = turn.user.as_ref().and_then(|u| u.id.clone());

            let sid_edit = session.id.clone();
            let parent_edit = parent_entry_id.clone();
            let ut_edit = user_text_for_turn.clone();
            let ent_edit = entity.clone();

            let sid_fork = session.id.clone();
            let fork_eid = user_entry_id
                .or_else(|| split.final_entry_id.clone())
                .unwrap_or_default();
            let ent_fork = entity.clone();

            let sid_retry = session.id.clone();
            let parent_retry = parent_entry_id;
            let ut_retry = user_text_for_turn;
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

            let left_btns = div()
                .flex()
                .items_center()
                .gap(space::XS)
                .opacity(if is_live_turn { 0.35 } else { 1.0 })
                .child(icon_action_24(
                    format!("assistant-copy-btn-{turn_key}"),
                    if is_copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    },
                    if is_copied {
                        palette.status_success
                    } else {
                        palette.text_secondary
                    },
                    is_live_turn,
                    palette,
                    move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        ent_copy.update(cx, |this, cx| {
                            this.copy_tracker.mark_copied(
                                copy_id_click.clone(),
                                |v| &mut v.copy_tracker,
                                cx,
                            );
                        });
                    },
                ))
                .child(icon_action_24(
                    format!("assistant-edit-btn-{turn_key}"),
                    IconName::Pencil,
                    palette.text_secondary,
                    is_live_turn,
                    palette,
                    move |_, cx| {
                        let s = sid_edit.clone();
                        let p = parent_edit.clone();
                        let t = ut_edit.clone();
                        ent_edit.update(cx, |this, cx| {
                            this.request_edit_user_turn(s, p, t, cx);
                        });
                    },
                ))
                .child(icon_action_24(
                    format!("assistant-fork-btn-{turn_key}"),
                    IconName::GitFork,
                    palette.text_secondary,
                    is_live_turn,
                    palette,
                    move |_, cx| {
                        if fork_eid.is_empty() {
                            return;
                        }
                        let s = sid_fork.clone();
                        let e = fork_eid.clone();
                        ent_fork.update(cx, |this, cx| {
                            this.fork_from_entry(s, e, cx);
                        });
                    },
                ))
                .child(icon_action_24(
                    format!("assistant-retry-btn-{turn_key}"),
                    IconName::RotateCcw,
                    palette.text_secondary,
                    is_live_turn,
                    palette,
                    move |_, cx| {
                        let s = sid_retry.clone();
                        let p = parent_retry.clone();
                        let t = ut_retry.clone();
                        ent_retry.update(cx, |this, cx| {
                            this.retry_user_turn(s, p, t, cx);
                        });
                    },
                ));

            let assistant_actions = div()
                .id(SharedString::from(format!("assistant-actions-{turn_key}")))
                .test_support()
                .w_full()
                .h(height::INLINE)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(left_btns)
                .children(ts_label_opt.map(|hhmm| {
                    apply_scale(div(), scale::CAPTION)
                        .font_family(mono_family.clone())
                        .text_color(palette.text_muted)
                        .child(SharedString::from(hhmm))
                }));

            assistant_stack = assistant_stack.child(assistant_actions);
        }

        column.child(assistant_stack).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_process_part(
        &mut self,
        session: &SessionViewSnapshot,
        seg_key: &str,
        part_idx: usize,
        tp: &TurnPart,
        tool_results: &BTreeMap<String, Value>,
        live: bool,
        mono_family: SharedString,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        match &tp.part {
            ConversationPart::Thinking(t) => {
                let think_key = format!("{seg_key}:think:{part_idx}");
                let open = self.expanded_thinking.contains(&think_key);
                let ent_th = entity.clone();
                let k_th = think_key.clone();

                // Collapsed: `"已思考"` + `gap(8px)` + `14px` arrow; hover brightens text without bg block
                let hdr = apply_scale(div(), scale::UI)
                    .id(SharedString::from(format!("thinking-hdr-{think_key}")))
                    .test_support()
                    .h(height::INLINE)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .text_color(palette.text_secondary)
                    .hover(|s| s.text_color(palette.text_primary))
                    .on_click(move |_, _, cx| {
                        let k = k_th.clone();
                        ent_th.update(cx, |this, cx| {
                            if !this.expanded_thinking.remove(&k) {
                                this.expanded_thinking.insert(k);
                            }
                            cx.notify();
                        });
                    })
                    .child(cjk_inline_label("已思考"))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(14.0))
                        .text_color(palette.text_secondary),
                    );

                let mut col = div().w_full().flex().flex_col().gap(space::XS).child(hdr);
                if open {
                    let content = if t.redacted && t.thinking.trim().is_empty() {
                        "思考内容已按模型策略省略。".to_owned()
                    } else {
                        t.thinking.clone()
                    };
                    let blk_id = SharedString::from(format!("thinking-body-{think_key}"));
                    let copied = self.copy_tracker.is_copied(blk_id.as_ref());
                    col = col.child(render_copyable_text_block(
                        blk_id,
                        content,
                        false,
                        mono_family,
                        copied,
                        palette,
                        |cid, _, cx| {
                            cx.entity().update(cx, |this: &mut Self, cx| {
                                this.copy_tracker
                                    .mark_copied(cid, |v| &mut v.copy_tracker, cx);
                            });
                        },
                        cx,
                    ));
                }
                col.into_any_element()
            }
            ConversationPart::Text(txt) => apply_scale(div(), scale::UI)
                .text_color(palette.text_secondary)
                .child(SharedString::from(txt.clone()))
                .into_any_element(),
            ConversationPart::Tools(calls) => {
                let mut group_col = div().w_full().flex().flex_col().gap(space::XS);
                for call in calls {
                    group_col = group_col.child(self.render_normal_tool_row(
                        call,
                        tool_results.get(&call.id),
                        session.revision,
                        live,
                        mono_family.clone(),
                        palette,
                        entity,
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
            return div()
                .id("message-list-loading")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    apply_scale(div(), scale::UI)
                        .text_color(palette.text_muted)
                        .child("正在加载对话内容…"),
                )
                .into_any_element();
        };

        if session.revision != self.last_observed_revision {
            self.last_observed_revision = session.revision;
            if self.auto_follow_bottom {
                self.scroll_handle.scroll_to_bottom();
            }
        }

        // Merge `session.messages` with live `streaming_assistant` when running
        let mut combined_entries: Vec<Value> = session.messages.as_ref().clone();
        if session.running
            && let Some(ref streaming) = session.streaming_assistant
        {
            let stream_id = streaming
                .get("id")
                .or_else(|| streaming.get("message").and_then(|m| m.get("id")))
                .and_then(Value::as_str);
            let replaced = if let Some(sid_str) = stream_id {
                if let Some(last) = combined_entries.last_mut() {
                    let last_id = last
                        .get("id")
                        .or_else(|| last.get("message").and_then(|m| m.get("id")))
                        .and_then(Value::as_str);
                    if last_id == Some(sid_str) {
                        *last = streaming.clone();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            if !replaced {
                combined_entries.push(streaming.clone());
            }
        }

        let (turns, tool_results) = group_turns_and_results(&combined_entries);
        let live_turn_idx = active_turn_index(&turns, session.running);

        // Tail sliding window (`DEFAULT_VISIBLE_TURN_WINDOW = 30`)
        let total_turns = turns.len();
        let window_start = total_turns.saturating_sub(self.visible_turn_limit);
        let hidden_earlier_count = window_start;

        let outbox_items = self.client.with_outbox(|ob| ob.session_items(sid).to_vec());

        let entity = cx.entity();

        let mut list_col = div()
            .w_full()
            .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
            .mx_auto()
            .px(space::GROUP)
            .py(space::GROUP)
            .flex()
            .flex_col()
            .gap(space::GROUP);

        if hidden_earlier_count > 0 {
            let ent_more = entity.clone();
            let batch = hidden_earlier_count.min(DEFAULT_VISIBLE_TURN_WINDOW);
            list_col = list_col.child(
                div().w_full().flex().justify_center().child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id("show-earlier-turns-btn")
                        .test_support()
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
                            ent_more.update(cx, |this, cx| {
                                this.visible_turn_limit += batch;
                                this.scroll_handle.scroll_to_top_of_item(batch);
                                cx.notify();
                            });
                        })
                        .child(cjk_inline_label(format!(
                            "显示更早的消息（还有 {hidden_earlier_count} 轮）"
                        ))),
                ),
            );
        }

        for (idx, turn) in turns.iter().enumerate().skip(window_start) {
            let parent_entry_id = if idx > 0 {
                turns[idx - 1]
                    .assistants
                    .last()
                    .and_then(|a| a.id.clone())
                    .or_else(|| turns[idx - 1].user.as_ref().and_then(|u| u.id.clone()))
            } else {
                None
            };
            let is_live = live_turn_idx == Some(idx);
            list_col = list_col.child(self.render_turn_view(
                &session,
                idx,
                turn,
                parent_entry_id,
                &tool_results,
                is_live,
                mono_family.clone(),
                reduce_motion,
                transparency_enabled,
                scale_factor,
                &palette,
                &entity,
                window,
                cx,
            ));
        }

        for ob_item in &outbox_items {
            list_col = list_col.child(self.render_outbox_bubble(sid, ob_item, &palette, &entity));
        }

        let show_scroll_bottom_btn =
            !self.auto_follow_bottom && self.scroll_handle.max_offset().y > px(36.0);

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
                    .child(list_col),
            );

        if show_scroll_bottom_btn {
            let ent_btm = entity.clone();
            outer = outer.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(space::LG)
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .id("scroll-to-bottom-btn")
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::FULL)
                            .bg(palette.bg_elevated)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .shadow(palette.craft_lift_shadows())
                            .flex()
                            .items_center()
                            .gap(space::TIGHT)
                            .hover(|s| s.bg(palette.bg_surface))
                            .on_click(move |_, _, cx| {
                                ent_btm.update(cx, |this, cx| {
                                    this.scroll_to_bottom_and_follow(cx);
                                });
                            })
                            .child(
                                Icon::new(IconName::ArrowDown)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("回到底部")),
                            ),
                    ),
            );
        }

        outer.into_any_element()
    }
}

fn format_timestamp_hhmm(ts_ms: u64) -> Option<String> {
    let dt = DateTime::from_timestamp_millis(ts_ms as i64)?;
    Some(dt.with_timezone(&Local).format("%H:%M").to_string())
}

fn icon_action_24(
    id: impl Into<SharedString>,
    icon: IconName,
    icon_color: gpui_kit::Hsla,
    disabled: bool,
    palette: &DesktopPalette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let mut btn = div()
        .id(id.into())
        .test_support()
        .w(height::INLINE)
        .h(height::INLINE)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_center();
    if !disabled {
        btn = btn
            .hover(|s| s.bg(palette.bg_item_hover))
            .on_click(move |_, w, cx| on_click(w, cx));
    }
    btn.child(Icon::new(icon).size(px(14.0)).text_color(icon_color))
}

fn to_tool_call_input(call: &ToolCallPart) -> ToolCallInput {
    ToolCallInput {
        id: call.id.clone(),
        name: call.name.clone(),
        arguments: call.arguments.as_object().cloned().unwrap_or_default(),
        execution: Some(ToolExecutionMeta::default()),
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

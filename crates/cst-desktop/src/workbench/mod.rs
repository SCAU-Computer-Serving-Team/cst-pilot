pub mod composer;
pub mod image_viewer;
pub mod markdown_view;
pub mod message_list;
pub mod model_picker;
pub mod queue_bar;
pub mod turn_segments;

use cst_client::{ClientHandle, DraftKey};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Subscription,
    TestSupportExt, Window, div, px,
};

use crate::background::{SkyPreset, render_sky_background};
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
};
use crate::workbench::composer::{ComposerHostEvent, ComposerView, SlashNavigationTarget};
use crate::workbench::image_viewer::{LightboxImageTarget, render_image_lightbox_modal};
use crate::workbench::message_list::{MessageListEvent, MessageListView};

#[derive(Debug, Clone)]
pub enum WorkbenchEvent {
    ActivatedSession(String),
    MissingCredentials { provider: String },
    OpenSlashRoute(SlashNavigationTarget),
    OpenAccountPage,
    WorkspaceError(String),
    TopNotice(String),
}

pub struct WorkbenchView {
    client: ClientHandle,
    active_session_id: Option<String>,
    sky_time: f32,
    home_composer: Entity<ComposerView>,
    session_composer: Entity<ComposerView>,
    message_list: Entity<MessageListView>,
    lightbox: Option<LightboxImageTarget>,
    lightbox_trap_focus: FocusHandle,
    lightbox_close_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<WorkbenchEvent> for WorkbenchView {}

impl WorkbenchView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home_composer =
            cx.new(|cx| ComposerView::new(client.clone(), DraftKey::NewSession, window, cx));
        let initial_sid = client.with_workspace(|ws| ws.active_session_id().map(str::to_owned));
        let initial_session_key = DraftKey::Session(initial_sid.clone().unwrap_or_default());
        let session_composer =
            cx.new(|cx| ComposerView::new(client.clone(), initial_session_key, window, cx));
        let message_list = cx.new(|_| MessageListView::new(client.clone()));
        if initial_sid.is_some() {
            let sid_clone = initial_sid.clone();
            message_list.update(cx, |ml, cx| ml.set_session_id(sid_clone, cx));
        }

        let lightbox_trap_focus = cx.focus_handle();
        let lightbox_close_focus = cx.focus_handle();

        let sub_home = cx.subscribe_in(&home_composer, window, Self::on_composer_event);
        let sub_sess = cx.subscribe_in(&session_composer, window, Self::on_composer_event);
        let sub_list = cx.subscribe_in(&message_list, window, Self::on_message_list_event);

        Self {
            client,
            active_session_id: initial_sid,
            sky_time: 0.0,
            home_composer,
            session_composer,
            message_list,
            lightbox: None,
            lightbox_trap_focus,
            lightbox_close_focus,
            _subscriptions: vec![sub_home, sub_sess, sub_list],
        }
    }

    pub fn home_composer(&self) -> &Entity<ComposerView> {
        &self.home_composer
    }

    pub fn session_composer(&self) -> &Entity<ComposerView> {
        &self.session_composer
    }

    pub fn message_list(&self) -> &Entity<MessageListView> {
        &self.message_list
    }

    pub fn has_lightbox(&self) -> bool {
        self.lightbox.is_some()
    }

    pub fn sync_route_and_sky(
        &mut self,
        session_id: Option<String>,
        sky_time: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sky_time = sky_time;
        if self.active_session_id != session_id {
            self.active_session_id = session_id.clone();
            self.message_list
                .update(cx, |ml, cx| ml.set_session_id(session_id.clone(), cx));
            if let Some(sid) = session_id {
                self.session_composer.update(cx, |comp, cx| {
                    comp.set_draft_key(DraftKey::Session(sid), window, cx)
                });
            }
        }
    }

    pub fn prefill_session_composer(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owned = text.to_owned();
        self.session_composer.update(cx, |comp, cx| {
            comp.textarea_state().update(cx, |st, cx| {
                st.set_value(owned, window, cx);
                st.focus_handle(cx).focus(window, cx);
            });
            cx.notify();
        });
    }

    /// Dismisses the single topmost open workbench surface (`lightbox` -> active `ComposerView` popovers)
    /// and restores focus within 1 frame.
    pub fn dismiss_topmost_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(target) = self.lightbox.take() {
            if let Some(prev) = target.prior_focus {
                prev.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if self.active_session_id.is_some() {
            self.session_composer
                .update(cx, |comp, cx| comp.dismiss_topmost_popover(window, cx))
        } else {
            self.home_composer
                .update(cx, |comp, cx| comp.dismiss_topmost_popover(window, cx))
        }
    }

    fn open_lightbox(
        &mut self,
        target: LightboxImageTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lightbox = Some(target);
        self.lightbox_close_focus.focus(window, cx);
        cx.notify();
    }

    fn on_composer_event(
        &mut self,
        _composer: &Entity<ComposerView>,
        event: &ComposerHostEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerHostEvent::SubmittedToSession { session_id } => {
                if self.active_session_id.as_deref() != Some(session_id.as_str()) {
                    self.sync_route_and_sky(Some(session_id.clone()), self.sky_time, window, cx);
                    cx.emit(WorkbenchEvent::ActivatedSession(session_id.clone()));
                } else {
                    self.message_list
                        .update(cx, |ml, cx| ml.scroll_to_bottom_and_follow(cx));
                }
                cx.notify();
            }
            ComposerHostEvent::MissingCredentials { provider } => {
                cx.emit(WorkbenchEvent::MissingCredentials {
                    provider: provider.clone(),
                });
            }
            ComposerHostEvent::NavigateSlashRoute(target) => {
                if *target == SlashNavigationTarget::ForkLatest {
                    if let Some(ref sid) = self.active_session_id {
                        let snap = self.client.snapshot();
                        let latest_user_eid = snap.loaded_sessions.get(sid).and_then(|s| {
                            s.messages.iter().rev().find_map(|m| {
                                let msg = m.get("message").unwrap_or(m);
                                (msg.get("role").and_then(serde_json::Value::as_str)
                                    == Some("user"))
                                .then(|| {
                                    m.get("id")
                                        .or_else(|| msg.get("id"))
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned)
                                })
                                .flatten()
                            })
                        });
                        if let Some(eid) = latest_user_eid {
                            let sid_owned = sid.clone();
                            let client = self.client.clone();
                            cx.spawn(async move |weak, cx| {
                                let res = client.fork_session(sid_owned, eid).await;
                                let _ = client.refresh_globals().await;
                                let _ = weak.update(cx, |_, cx| match res {
                                    Ok(new_sid) => {
                                        if !new_sid.is_empty() {
                                            cx.emit(WorkbenchEvent::ActivatedSession(new_sid));
                                        }
                                    }
                                    Err(e) => {
                                        cx.emit(WorkbenchEvent::WorkspaceError(e.to_string()))
                                    }
                                });
                            })
                            .detach();
                        }
                    }
                    return;
                }
                cx.emit(WorkbenchEvent::OpenSlashRoute(*target));
            }
            ComposerHostEvent::OpenAccountPage => {
                cx.emit(WorkbenchEvent::OpenAccountPage);
            }
            ComposerHostEvent::OpenLightbox(target) => {
                self.open_lightbox(target.clone(), window, cx);
            }
            ComposerHostEvent::ErrorMessage(msg) => {
                cx.emit(WorkbenchEvent::WorkspaceError(msg.clone()));
            }
            ComposerHostEvent::TopNotice(msg) => {
                cx.emit(WorkbenchEvent::TopNotice(msg.clone()));
            }
        }
    }

    fn on_message_list_event(
        &mut self,
        _list: &Entity<MessageListView>,
        event: &MessageListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            MessageListEvent::OpenLightbox(target) => {
                self.open_lightbox(target.clone(), window, cx);
            }
            MessageListEvent::SwitchToSession(sid) => {
                self.sync_route_and_sky(Some(sid.clone()), self.sky_time, window, cx);
                cx.emit(WorkbenchEvent::ActivatedSession(sid.clone()));
            }
            MessageListEvent::FocusComposer => {
                let fh = self.session_composer.read(cx).focus_handle(cx);
                fh.focus(window, cx);
                cx.notify();
            }
            MessageListEvent::MissingCredentials { provider } => {
                cx.emit(WorkbenchEvent::MissingCredentials {
                    provider: provider.clone(),
                });
            }
            MessageListEvent::ErrorMessage(msg) => {
                cx.emit(WorkbenchEvent::WorkspaceError(msg.clone()));
            }
            MessageListEvent::TopNotice(msg) => {
                cx.emit(WorkbenchEvent::TopNotice(msg.clone()));
            }
        }
    }
}

impl Render for WorkbenchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();

        let snap = self.client.snapshot();
        let entity = cx.entity();

        let body = if let Some(ref sid) = self.active_session_id {
            let session_title = snap
                .sessions
                .iter()
                .find(|s| &s.id == sid)
                .map(|s| {
                    if s.title.trim().is_empty() {
                        "新对话".to_owned()
                    } else {
                        s.title.clone()
                    }
                })
                .unwrap_or_else(|| "当前对话".to_owned());

            let ent_tree = entity.clone();
            let ent_exp = entity.clone();

            // `44px` (`height::HEADER`) session header bar (`HUkqs`)
            let session_header = div()
                .id("session-header-hukqs")
                .test_support()
                .w_full()
                .h(height::HEADER)
                .px(space::GROUP)
                .border_b_1()
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(
                    apply_scale(div(), scale::BODY_MEDIUM)
                        .min_w_0()
                        .truncate()
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label(session_title)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .child(
                            apply_scale(div(), scale::CAPTION_MEDIUM)
                                .id("session-header-tree-btn")
                                .test_support()
                                .h(height::COMPACT)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .flex()
                                .items_center()
                                .gap(space::XS)
                                .text_color(palette.text_secondary)
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(move |_, _, cx| {
                                    ent_tree.update(cx, |_, cx| {
                                        cx.emit(WorkbenchEvent::OpenSlashRoute(
                                            SlashNavigationTarget::Tree,
                                        ));
                                    });
                                })
                                .child(
                                    Icon::new(IconName::GitBranch)
                                        .size(px(13.0))
                                        .text_color(palette.text_secondary),
                                )
                                .child(cjk_inline_label("分支树")),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION_MEDIUM)
                                .id("session-header-export-btn")
                                .test_support()
                                .h(height::COMPACT)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .flex()
                                .items_center()
                                .gap(space::XS)
                                .text_color(palette.text_secondary)
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(move |_, _, cx| {
                                    ent_exp.update(cx, |_, cx| {
                                        cx.emit(WorkbenchEvent::OpenSlashRoute(
                                            SlashNavigationTarget::Export,
                                        ));
                                    });
                                })
                                .child(
                                    Icon::new(IconName::Download)
                                        .size(px(13.0))
                                        .text_color(palette.text_secondary),
                                )
                                .child(cjk_inline_label("导出")),
                        ),
                );

            div()
                .id("workspace-session-view")
                .test_support()
                .size_full()
                .bg(palette.bg_workspace)
                .flex()
                .flex_col()
                .child(session_header)
                .child(self.message_list.clone())
                .child(
                    div()
                        .w_full()
                        .px(space::GROUP)
                        .pb(space::LG)
                        .pt(space::XS)
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .w_full()
                                .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                                .child(self.session_composer.clone()),
                        ),
                )
                .into_any_element()
        } else {
            let home_pending = self.client.with_outbox(|ob| ob.home_item().is_some());

            // Home welcome canvas (`p2HWHP` / `PuezS` with `blue-hour.glsl` sky + `600px` centered composer)
            div()
                .id("workspace-home-view")
                .test_support()
                .relative()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(render_sky_background(
                            SkyPreset::home_for_theme(palette.mode),
                            self.sky_time,
                            1.0,
                        )),
                )
                .child(
                    div()
                        .relative()
                        .size_full()
                        .px(space::GROUP)
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w_full()
                                .max_w(px(600.0))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(space::GROUP)
                                .child(
                                    apply_scale(div(), scale::HERO)
                                        .text_color(palette.text_primary)
                                        .child("有什么需要诊断或修复？"),
                                )
                                .child(self.home_composer.clone())
                                .children(home_pending.then(|| {
                                    apply_scale(div(), scale::CAPTION)
                                        .text_color(palette.text_secondary)
                                        .child("正在创建新对话并提交消息…")
                                })),
                        ),
                )
                .into_any_element()
        };

        let mut root = div().relative().size_full().child(body);

        if let Some(ref target) = self.lightbox {
            let ent_close = entity.clone();
            root = root.child(render_image_lightbox_modal(
                target,
                &palette,
                transparency_enabled,
                scale_factor,
                &self.lightbox_trap_focus,
                &self.lightbox_close_focus,
                move |w, cx| {
                    ent_close.update(cx, |this, cx| {
                        let _ = this.dismiss_topmost_surface(w, cx);
                    });
                },
            ));
        }

        root
    }
}

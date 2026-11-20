pub mod composer;
pub mod image_viewer;
pub mod markdown_view;
pub mod message_list;
pub mod model_picker;
pub mod queue_bar;
pub mod turn_segments;

use cst_client::{ClientHandle, DraftKey};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::Icon;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, PathBuilder, Render, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt, Window, canvas, div, linear_color_stop, linear_gradient, point,
    px, rgba,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{
    FrostedGlassSpec, SkyPreset, render_frosted_surface, render_sky_background_rounded,
};
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

#[derive(Debug, Clone)]
struct PendingEditDraftConfirm {
    session_id: String,
    parent_entry_id: Option<String>,
    user_text: String,
    prior_focus: Option<FocusHandle>,
}

pub struct WorkbenchView {
    client: ClientHandle,
    active_session_id: Option<String>,
    sky_time: f32,
    home_composer: Entity<ComposerView>,
    session_composer: Entity<ComposerView>,
    message_list: Entity<MessageListView>,
    header_menu_open: bool,
    header_more_focus: FocusHandle,
    header_menu_prior_focus: Option<FocusHandle>,
    pending_edit_confirm: Option<PendingEditDraftConfirm>,
    edit_confirm_trap_focus: FocusHandle,
    edit_confirm_keep_focus: FocusHandle,
    edit_confirm_overwrite_focus: FocusHandle,
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

        let header_more_focus = cx.focus_handle();
        let edit_confirm_trap_focus = cx.focus_handle();
        let edit_confirm_keep_focus = cx.focus_handle();
        let edit_confirm_overwrite_focus = cx.focus_handle();
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
            header_menu_open: false,
            header_more_focus,
            header_menu_prior_focus: None,
            pending_edit_confirm: None,
            edit_confirm_trap_focus,
            edit_confirm_keep_focus,
            edit_confirm_overwrite_focus,
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
            self.header_menu_open = false;
            self.header_menu_prior_focus = None;
            self.pending_edit_confirm = None;
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

    /// Dismisses the single topmost open workbench surface (`HUkqs` header menu -> draft-overwrite confirm -> `lightbox` -> active `ComposerView` popovers)
    /// and restores focus within 1 frame.
    pub fn dismiss_topmost_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.header_menu_open {
            self.header_menu_open = false;
            if let Some(prev) = self.header_menu_prior_focus.take() {
                prev.focus(window, cx);
            } else {
                self.header_more_focus.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if let Some(pending) = self.pending_edit_confirm.take() {
            if let Some(prev) = pending.prior_focus {
                prev.focus(window, cx);
            }
            cx.notify();
            return true;
        }
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
        self.header_menu_open = false;
        self.lightbox = Some(target);
        self.lightbox_close_focus.focus(window, cx);
        cx.notify();
    }

    fn execute_edit_backfill(
        &mut self,
        session_id: String,
        parent_entry_id: Option<String>,
        user_text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.message_list.update(cx, |ml, cx| {
            ml.apply_edit_and_rewrite_user_turn(session_id, parent_entry_id, user_text, cx);
        });
        self.session_composer.update(cx, |comp, cx| {
            comp.reconcile_external_draft_changes(window, cx);
            comp.focus_handle(cx).focus(window, cx);
            cx.notify();
        });
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
                self.session_composer.update(cx, |comp, cx| {
                    comp.reconcile_external_draft_changes(window, cx);
                    comp.focus_handle(cx).focus(window, cx);
                    cx.notify();
                });
            }
            MessageListEvent::RequestEditUserTurn {
                session_id,
                parent_entry_id,
                user_text,
            } => {
                let has_draft = self.session_composer.read(cx).has_unsent_draft_content(cx);
                if has_draft {
                    self.header_menu_open = false;
                    self.pending_edit_confirm = Some(PendingEditDraftConfirm {
                        session_id: session_id.clone(),
                        parent_entry_id: parent_entry_id.clone(),
                        user_text: user_text.clone(),
                        prior_focus: window.focused(cx),
                    });
                    self.edit_confirm_keep_focus.focus(window, cx);
                    cx.notify();
                } else {
                    self.execute_edit_backfill(
                        session_id.clone(),
                        parent_entry_id.clone(),
                        user_text.clone(),
                        window,
                        cx,
                    );
                }
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

            let ent_more = entity.clone();

            // `44px` (`height::HEADER`) session header bar (`HUkqs`)
            let session_header = div()
                .id("session-header-hukqs")
                .test_support()
                .a11y_region("session-header-hukqs", format!("对话标题：{session_title}"))
                .w_full()
                .h(height::HEADER)
                .pl(space::GROUP)
                .pr(space::LG)
                .border_b_1()
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::MD)
                        .min_w_0()
                        .max_w(px(880.0))
                        .child(
                            Icon::new(IconName::MessageSquare)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(
                            apply_scale(div(), scale::BODY_MEDIUM)
                                .min_w_0()
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(session_title)),
                        ),
                )
                .child(
                    div()
                        .id("session-header-more-btn")
                        .test_support()
                        .a11y_toggle_button(
                            "session-header-more-btn",
                            "会话更多操作",
                            self.header_menu_open,
                        )
                        .track_focus(&self.header_more_focus)
                        .tab_index(0)
                        .w(height::COMPACT)
                        .h(height::COMPACT)
                        .flex_shrink_0()
                        .rounded(radius::CONTROL)
                        .bg(if self.header_menu_open {
                            palette.bg_item_active
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| {
                            ent_more.update(cx, |this, cx| {
                                this.header_menu_open = !this.header_menu_open;
                                if this.header_menu_open {
                                    this.header_menu_prior_focus = w.focused(cx);
                                }
                                cx.notify();
                            });
                        })
                        .child(
                            Icon::new(IconName::EllipsisVertical)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        ),
                );

            let mut session_view = div()
                .id("workspace-session-view")
                .test_support()
                .a11y_region("workspace-session-view", "会话工作台")
                .relative()
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
                        .pb(space::MD)
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .w_full()
                                .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                                .child(self.session_composer.clone()),
                        ),
                );

            if self.header_menu_open {
                let ent_backdrop = entity.clone();
                let ent_tree = entity.clone();
                let ent_exp = entity.clone();

                let menu_card = render_frosted_surface(
                    "session-header-more-menu",
                    FrostedGlassSpec::popover_h2p2z(&palette),
                    palette.bg_elevated,
                    transparency_enabled,
                    scale_factor,
                )
                .test_support()
                .role(gpui::Role::Menu)
                .accessibility_id("session-header-more-menu")
                .aria_label("会话更多菜单")
                .w(px(200.0))
                .p(space::XS)
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette.border_hairline)
                .flex()
                .flex_col()
                .gap(space::MICRO)
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    apply_scale(div(), scale::UI)
                        .id("session-header-tree-btn")
                        .test_support()
                        .a11y_menuitem("session-header-tree-btn", "分支树")
                        .relative()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .text_color(palette.text_primary)
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, _, cx| {
                            ent_tree.update(cx, |this, cx| {
                                this.header_menu_open = false;
                                cx.emit(WorkbenchEvent::OpenSlashRoute(
                                    SlashNavigationTarget::Tree,
                                ));
                                cx.notify();
                            });
                        })
                        .child(
                            Icon::new(IconName::GitBranch)
                                .size(px(15.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("分支树")),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .id("session-header-export-btn")
                        .test_support()
                        .a11y_menuitem("session-header-export-btn", "导出会话")
                        .relative()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .text_color(palette.text_primary)
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, _, cx| {
                            ent_exp.update(cx, |this, cx| {
                                this.header_menu_open = false;
                                cx.emit(WorkbenchEvent::OpenSlashRoute(
                                    SlashNavigationTarget::Export,
                                ));
                                cx.notify();
                            });
                        })
                        .child(
                            Icon::new(IconName::FileText)
                                .size(px(15.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("导出会话")),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .my(space::MICRO)
                        .h(px(1.0))
                        .bg(palette.border_subtle),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .id("session-header-share-disabled")
                        .test_support()
                        .relative()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .opacity(0.45)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .text_color(palette.text_secondary)
                        .child(
                            Icon::new(IconName::ExternalLink)
                                .size(px(15.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("分享")),
                );

                session_view = session_view
                    .child(
                        div()
                            .id("session-header-menu-backdrop")
                            .absolute()
                            .top(height::HEADER)
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .on_click(move |_, w, cx| {
                                ent_backdrop.update(cx, |this, cx| {
                                    let _ = this.dismiss_topmost_surface(w, cx);
                                });
                            }),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(40.0))
                            .right(space::LG)
                            .child(menu_card),
                    );
            }

            session_view.into_any_element()
        } else {
            let home_pending = self.client.with_outbox(|ob| ob.home_item().is_some());

            // Home welcome canvas (`p2HWHP` / `PuezS` with `blue-hour.glsl` rounded sky + `600px` centered composer)
            div()
                .id("workspace-home-view")
                .test_support()
                .a11y_region("workspace-home-view", "工作台欢迎主页")
                .relative()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(render_sky_background_rounded(
                            SkyPreset::home_for_theme(palette.mode),
                            self.sky_time,
                            1.0,
                            [8.0, 8.0, 8.0, 8.0],
                            scale_factor,
                        )),
                )
                .child(
                    div()
                        .relative()
                        .size_full()
                        .px(space::GROUP)
                        .pb(space::SM)
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
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(200.0))
                                        .relative()
                                        .flex()
                                        .justify_center()
                                        .child(render_home_brand_arcs())
                                        .child(
                                            apply_scale(div(), scale::HERO)
                                                .absolute()
                                                .top(px(125.0))
                                                .left_0()
                                                .right_0()
                                                .flex()
                                                .justify_center()
                                                .text_color(palette.text_welcome)
                                                .child("我该如何协助您？"),
                                        ),
                                )
                                .child(self.home_composer.clone())
                                .children(home_pending.then(|| {
                                    apply_scale(div(), scale::CAPTION)
                                        .pt(space::MD)
                                        .text_color(rgba(0xFFFF_FFBF))
                                        .child("正在创建新对话并提交消息…")
                                })),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(17.0))
                        .flex()
                        .justify_center()
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .text_color(palette.text_secondary)
                                .child("@cst-pilot"),
                        ),
                )
                .into_any_element()
        };

        let mut root = div().relative().size_full().child(body);

        if self.pending_edit_confirm.is_some() {
            let ent_scrim = entity.clone();
            let ent_keep = entity.clone();
            let ent_overwrite = entity.clone();

            let scrim = render_frosted_surface(
                "overwrite-draft-confirm-scrim",
                FrostedGlassSpec::scrim_dz0fp(&palette),
                palette.bg_scrim,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_click(move |_, w, cx| {
                ent_scrim.update(cx, |this, cx| {
                    let _ = this.dismiss_topmost_surface(w, cx);
                });
            });

            let card = render_frosted_surface(
                "overwrite-draft-confirm-card",
                FrostedGlassSpec::modal_k3x72(&palette, 8.0),
                palette.bg_modal,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .a11y_dialog("overwrite-draft-confirm-card", "覆盖当前输入框草稿")
            .w(layout::MODAL_STANDARD_WIDTH)
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_subtle)
            .p(space::GROUP)
            .flex()
            .flex_col()
            .gap(space::LG)
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("覆盖当前输入框草稿？")),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_secondary)
                            .child("回填历史消息将替换输入框内尚未发送的内容。"),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("overwrite-draft-keep-btn")
                            .test_support()
                            .a11y_button("overwrite-draft-keep-btn", "保留原草稿")
                            .track_focus(&self.edit_confirm_keep_focus)
                            .tab_index(0)
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_elevated)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(move |_, w, cx| {
                                ent_keep.update(cx, |this, cx| {
                                    let _ = this.dismiss_topmost_surface(w, cx);
                                });
                            })
                            .child(cjk_inline_label("保留原草稿")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("overwrite-draft-confirm-btn")
                            .test_support()
                            .a11y_button("overwrite-draft-confirm-btn", "覆盖草稿")
                            .track_focus(&self.edit_confirm_overwrite_focus)
                            .tab_index(0)
                            .h(height::CONTROL)
                            .px(space::LG)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .on_click(move |_, w, cx| {
                                ent_overwrite.update(cx, |this, cx| {
                                    if let Some(pending) = this.pending_edit_confirm.take() {
                                        this.execute_edit_backfill(
                                            pending.session_id,
                                            pending.parent_entry_id,
                                            pending.user_text,
                                            w,
                                            cx,
                                        );
                                    }
                                });
                            })
                            .child(cjk_inline_label("覆盖草稿")),
                    ),
            );

            root = root.child(scrim.child(div().tab_stop(false).child(card).focus_trap(
                "overwrite-draft-confirm-trap",
                &self.edit_confirm_trap_focus,
            )));
        }

        if let Some(ref target) = self.lightbox {
            let ent_close = entity.clone();
            root = root.child(render_image_lightbox_modal(
                target,
                &palette,
                transparency_enabled,
                scale_factor,
                window.viewport_size(),
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

/// Renders the `280×280` concentric brand arc graphic (`cst-pilot-desktop.pen` 「01 · 工作台」 `lbYEI`:
/// outer `280×280` `tnt5a` & inner `244×244` `Nc5f1`, `3.5px` stroke, `45°..315°` sweep, white vertical fade).
fn render_home_brand_arcs() -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(280.0))
        .flex()
        .justify_center()
        .child(
            canvas(
                |_, _, _| (),
                |bounds, _, window, _cx| {
                    let cx_f = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
                    let cy_f = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
                    let stroke_bg = linear_gradient(
                        0.0,
                        linear_color_stop(rgba(0xFFFF_FF00), 0.49),
                        linear_color_stop(rgba(0xFFFF_FFFF), 1.0),
                    );
                    let start_rad = 45.0_f32.to_radians();
                    let sweep_rad = 270.0_f32.to_radians();
                    let steps = 180usize;

                    for diameter in [280.0_f32, 244.0_f32] {
                        let r = (diameter - 3.5) * 0.5;
                        let mut builder = PathBuilder::stroke(px(3.5));
                        for i in 0..=steps {
                            let theta = start_rad + sweep_rad * (i as f32 / steps as f32);
                            let pt = point(px(cx_f + r * theta.cos()), px(cy_f + r * theta.sin()));
                            if i == 0 {
                                builder.move_to(pt);
                            } else {
                                builder.line_to(pt);
                            }
                        }
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, stroke_bg);
                        }
                    }
                },
            )
            .w(px(280.0))
            .h(px(280.0)),
        )
}

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use chrono::{DateTime, Local, NaiveDate};
use cst_client::{ClientError, ClientHandle, SessionSummary};
use gpui::KeyDownEvent;
use gpui::{MouseButton, Pixels, Size, size};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::base::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Escape, Input, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, TestSupportExt,
    Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius,
    render_subpage_header_xwjof, scale, space,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDayGroup {
    Today,
    LastWeek,
    LastMonth,
    Earlier,
}

impl SessionDayGroup {
    pub fn title(self) -> &'static str {
        match self {
            Self::Today => "今天",
            Self::LastWeek => "最近一周",
            Self::LastMonth => "最近一个月",
            Self::Earlier => "更早",
        }
    }
}

#[derive(Debug, Clone)]
pub enum VirtualSessionRow {
    GroupHeader(SessionDayGroup),
    Session(SessionSummary),
}

/// Groups sessions by natural calendar day relative to `today` in local timezone, omitting empty groups.
pub fn group_sessions_by_natural_day(
    sessions: &[SessionSummary],
    today: NaiveDate,
) -> Vec<VirtualSessionRow> {
    let mut g_today = Vec::new();
    let mut g_week = Vec::new();
    let mut g_month = Vec::new();
    let mut g_earlier = Vec::new();

    for sess in sessions {
        let bucket = sess
            .updated_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Local).date_naive())
            .map(|date| {
                let days = today.signed_duration_since(date).num_days();
                if days <= 0 {
                    SessionDayGroup::Today
                } else if days <= 7 {
                    SessionDayGroup::LastWeek
                } else if days <= 30 {
                    SessionDayGroup::LastMonth
                } else {
                    SessionDayGroup::Earlier
                }
            })
            .unwrap_or(SessionDayGroup::Today);

        match bucket {
            SessionDayGroup::Today => g_today.push(sess.clone()),
            SessionDayGroup::LastWeek => g_week.push(sess.clone()),
            SessionDayGroup::LastMonth => g_month.push(sess.clone()),
            SessionDayGroup::Earlier => g_earlier.push(sess.clone()),
        }
    }

    let mut out = Vec::new();
    for (grp, list) in [
        (SessionDayGroup::Today, g_today),
        (SessionDayGroup::LastWeek, g_week),
        (SessionDayGroup::LastMonth, g_month),
        (SessionDayGroup::Earlier, g_earlier),
    ] {
        if !list.is_empty() {
            out.push(VirtualSessionRow::GroupHeader(grp));
            for item in list {
                out.push(VirtualSessionRow::Session(item));
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
struct ContextMenuTarget {
    session_id: String,
    current_title: String,
    anchor_top_px: f32,
}

#[derive(Debug, Clone)]
pub enum AllSessionsEvent {
    OpenSession(String),
    SessionDeleted(String),
    Notice(String),
    ErrorMessage(String),
}

pub struct AllSessionsView {
    client: ClientHandle,
    scroll_handle: VirtualListScrollHandle,
    batch_mode: bool,
    selected_ids: BTreeSet<String>,
    optimistically_hidden_ids: BTreeSet<String>,
    optimistic_titles: BTreeMap<String, String>,
    context_menu: Option<ContextMenuTarget>,
    renaming_session_id: Option<String>,
    rename_input: Option<Entity<InputState>>,
    focus: FocusHandle,
    rename_trap_focus: FocusHandle,
    rename_prior_focus: Option<FocusHandle>,
    cached_rows: Vec<VirtualSessionRow>,
    cached_sizes: Rc<Vec<Size<Pixels>>>,
}

impl EventEmitter<AllSessionsEvent> for AllSessionsView {}

impl AllSessionsView {
    pub fn new(client: ClientHandle, cx: &mut Context<Self>) -> Self {
        Self {
            client,
            scroll_handle: VirtualListScrollHandle::new(),
            batch_mode: false,
            selected_ids: BTreeSet::new(),
            optimistically_hidden_ids: BTreeSet::new(),
            optimistic_titles: BTreeMap::new(),
            context_menu: None,
            renaming_session_id: None,
            rename_input: None,
            focus: cx.focus_handle(),
            rename_trap_focus: cx.focus_handle(),
            rename_prior_focus: None,
            cached_rows: Vec::new(),
            cached_sizes: Rc::new(Vec::new()),
        }
    }

    pub fn is_batch_mode(&self) -> bool {
        self.batch_mode || self.context_menu.is_some() || self.renaming_session_id.is_some()
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    pub(crate) fn has_rename_modal(&self) -> bool {
        self.renaming_session_id.is_some()
    }

    pub(crate) fn reset_surface(&mut self, cx: &mut Context<Self>) {
        self.batch_mode = false;
        self.selected_ids.clear();
        self.context_menu = None;
        self.renaming_session_id = None;
        self.rename_input = None;
        self.rename_prior_focus = None;
        cx.notify();
    }

    fn close_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.renaming_session_id = None;
        self.rename_input = None;
        self.rename_prior_focus
            .take()
            .unwrap_or_else(|| self.focus.clone())
            .focus(window, cx);
        cx.notify();
    }

    pub fn dismiss_batch_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.has_rename_modal() {
            self.close_rename(window, cx);
        } else if self.context_menu.take().is_some() {
            self.focus.focus(window, cx);
        } else if self.batch_mode {
            self.batch_mode = false;
            self.selected_ids.clear();
            self.focus.focus(window, cx);
        } else {
            return false;
        }
        cx.notify();
        true
    }

    pub(crate) fn visible_sessions(&self) -> Vec<SessionSummary> {
        self.client
            .snapshot()
            .sessions
            .iter()
            .filter(|s| !self.optimistically_hidden_ids.contains(&s.id))
            .cloned()
            .map(|mut s| {
                if let Some(title) = self.optimistic_titles.get(&s.id) {
                    s.title.clone_from(title);
                }
                s
            })
            .collect()
    }

    fn prune_selection(&mut self, visible: &[SessionSummary]) {
        let ids: BTreeSet<_> = visible.iter().map(|s| s.id.as_str()).collect();
        self.selected_ids.retain(|id| ids.contains(id.as_str()));
    }

    fn toggle_select_all(&mut self, cx: &mut Context<Self>) {
        let visible_ids: Vec<String> = self.visible_sessions().into_iter().map(|s| s.id).collect();
        if visible_ids.is_empty() {
            return;
        }
        if visible_ids.iter().all(|id| self.selected_ids.contains(id)) {
            self.selected_ids.clear();
        } else {
            self.selected_ids = visible_ids.into_iter().collect();
        }
        cx.notify();
    }

    fn execute_batch_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prune_selection(&self.visible_sessions());
        if self.selected_ids.is_empty() {
            return;
        }
        let targets = std::mem::take(&mut self.selected_ids).into_iter().collect();
        self.batch_mode = false;
        self.focus.focus(window, cx);
        self.delete_sessions_optimistic(targets, cx);
    }

    pub(crate) fn delete_sessions_optimistic(
        &mut self,
        mut targets: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        let snap = self.client.snapshot();
        targets.retain(|id| {
            snap.sessions.iter().any(|s| &s.id == id)
                && !self.optimistic_titles.contains_key(id)
                && self.optimistically_hidden_ids.insert(id.clone())
        });
        if targets.is_empty() {
            cx.notify();
            return;
        }
        self.selected_ids
            .retain(|id| !self.optimistically_hidden_ids.contains(id));
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let mut failure = None;
            for sid in &targets {
                match client.delete_session(sid.clone()).await {
                    Ok(_) => {
                        let _ = weak.update(cx, |_, cx| {
                            cx.emit(AllSessionsEvent::SessionDeleted(sid.clone()))
                        });
                    }
                    Err(err) => {
                        failure = Some(match err {
                            ClientError::Conflict { message, .. } => message,
                            other => other.to_string(),
                        })
                    }
                }
            }

            let refreshed = client.refresh_globals().await;
            let _ = weak.update(cx, |this, cx| {
                // Only settle this batch. SSE may already have removed rows belonging to another batch.
                for id in &targets {
                    this.optimistically_hidden_ids.remove(id);
                }
                if let Some(msg) = failure {
                    cx.emit(AllSessionsEvent::Notice(format!("部分会话未能删除：{msg}")));
                } else if let Err(err) = refreshed {
                    cx.emit(AllSessionsEvent::ErrorMessage(err.to_string()));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_rename_modal(
        &mut self,
        session_id: String,
        current_title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        if !self.can_rename(&session_id) {
            cx.notify();
            return;
        }
        self.rename_prior_focus = window
            .focused(cx)
            .filter(|f| self.focus.contains(f, window));
        self.renaming_session_id = Some(session_id);
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("输入会话标题…");
            state.set_value(current_title, window, cx);
            state.focus(window, cx);
            state
        });
        self.rename_input = Some(input);
        cx.notify();
    }

    fn submit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sid), Some(input)) =
            (self.renaming_session_id.clone(), self.rename_input.as_ref())
        else {
            return;
        };
        if super::input_is_composing(input, window, cx) {
            return;
        }
        let title = input.read(cx).value();
        if self.rename_session(sid, title.trim().to_owned(), cx) {
            self.close_rename(window, cx);
        }
    }

    pub(crate) fn can_rename(&self, sid: &str) -> bool {
        !self.optimistically_hidden_ids.contains(sid)
            && !self.optimistic_titles.contains_key(sid)
            && self.client.snapshot().sessions.iter().any(|s| s.id == sid)
    }

    pub(crate) fn rename_session(
        &mut self,
        sid: String,
        name: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_rename(&sid) {
            cx.emit(AllSessionsEvent::ErrorMessage(
                "会话已移除或正在更新。".into(),
            ));
            return false;
        }
        if name.is_empty() || name.chars().count() > 160 {
            let msg = if name.is_empty() {
                "会话标题不能为空。"
            } else {
                "会话标题不能超过 160 个字符。"
            };
            cx.emit(AllSessionsEvent::ErrorMessage(msg.into()));
            return false;
        }

        self.optimistic_titles.insert(sid.clone(), name.clone());
        cx.notify();
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.rename_session(sid.clone(), name).await;
            let refresh = client.refresh_globals().await;
            let _ = weak.update(cx, |this, cx| {
                // The snapshot is authoritative after refresh (including server normalization
                // and edits made from another view). Never retain a completed title override.
                this.optimistic_titles.remove(&sid);
                if let Some(err) = res.err().or_else(|| refresh.err()) {
                    cx.emit(AllSessionsEvent::ErrorMessage(err.to_string()));
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    fn row_anchor_top_px(&self, row_idx: usize) -> f32 {
        let mut y = 0.0_f32;
        for sz in self.cached_sizes.iter().take(row_idx + 1) {
            y += f32::from(sz.height);
        }
        (24.0 + y).clamp(52.0, 520.0)
    }

    fn render_virtual_range(
        &mut self,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let entity: Entity<Self> = cx.entity();
        let batch_mode = self.batch_mode;

        let mut elements = Vec::with_capacity(range.len());
        for idx in range {
            let Some(row) = self.cached_rows.get(idx).cloned() else {
                continue;
            };
            match row {
                VirtualSessionRow::GroupHeader(group) => {
                    elements.push(
                        apply_scale(div(), scale::UI)
                            .id(SharedString::from(format!(
                                "all-sessions-group-{:?}",
                                group
                            )))
                            .test_support()
                            .w_full()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .child(cjk_inline_label(group.title()))
                            .into_any_element(),
                    );
                }
                VirtualSessionRow::Session(sess) => {
                    let sid = sess.id.clone();
                    let is_checked = self.selected_ids.contains(&sid);
                    let title_str = if sess.title.trim().is_empty() {
                        "新对话".to_owned()
                    } else {
                        sess.title.clone()
                    };
                    let updated_text = sess
                        .updated_at
                        .as_deref()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| {
                            dt.with_timezone(&Local)
                                .format("%Y/%m/%d %H:%M")
                                .to_string()
                        })
                        .unwrap_or_default();

                    let ent_click = entity.clone();
                    let sid_click = sid.clone();
                    let ent_rclick = entity.clone();
                    let sid_rclick = sid.clone();
                    let title_rclick = title_str.clone();
                    let ent_more = entity.clone();
                    let sid_more = sid.clone();
                    let title_more = title_str.clone();
                    let anchor_y = self.row_anchor_top_px(idx);

                    let mut left_row = div()
                        .flex()
                        .items_center()
                        .gap(space::MD)
                        .min_w_0()
                        .flex_1();

                    if batch_mode {
                        left_row = left_row.child(
                            div()
                                .w(px(16.0))
                                .h(px(16.0))
                                .rounded(radius::CONTROL)
                                .border_1()
                                .border_color(if is_checked {
                                    palette.accent_solid
                                } else {
                                    palette.border_strong
                                })
                                .bg(if is_checked {
                                    palette.accent_solid
                                } else {
                                    palette.bg_workspace
                                })
                                .flex()
                                .items_center()
                                .justify_center()
                                .flex_shrink_0()
                                .children(is_checked.then(|| {
                                    Icon::new(IconName::Check)
                                        .size(px(12.0))
                                        .text_color(palette.text_on_accent)
                                })),
                        );
                    }

                    let title_label = SharedString::from(title_str.clone());
                    left_row = left_row.child(
                        apply_scale(div(), scale::BODY)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label(title_str)),
                    );

                    let mut right_slot = div().flex().items_center().gap(space::SM).flex_shrink_0();
                    if !updated_text.is_empty() {
                        right_slot = right_slot.child(
                            apply_scale(div(), scale::CAPTION)
                                .font_family(mono_family.clone())
                                .text_color(palette.text_muted)
                                .child(SharedString::from(updated_text)),
                        );
                    }
                    if !batch_mode {
                        let more_id = SharedString::from(format!("all-sessions-more-btn-{sid}"));
                        right_slot = right_slot.child(
                            div()
                                .id(more_id.clone())
                                .test_support()
                                .a11y_button(more_id, format!("会话操作：{title_label}"))
                                .w(height::CONTROL)
                                .h(height::CONTROL)
                                .rounded(radius::CONTROL)
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    let s_id = sid_more.clone();
                                    let t_cur = title_more.clone();
                                    ent_more.update(cx, |this, cx| {
                                        let closing = this
                                            .context_menu
                                            .as_ref()
                                            .is_some_and(|m| m.session_id == s_id);
                                        if closing {
                                            this.context_menu = None;
                                        } else {
                                            this.context_menu = Some(ContextMenuTarget {
                                                session_id: s_id,
                                                current_title: t_cur,
                                                anchor_top_px: anchor_y,
                                            });
                                        }
                                        cx.notify();
                                    });
                                })
                                .child(
                                    Icon::new(IconName::Ellipsis)
                                        .size(px(16.0))
                                        .text_color(palette.text_secondary),
                                ),
                        );
                    }

                    let row_id = SharedString::from(format!("all-sessions-row-{sid}"));
                    let mut row_el = div()
                        .id(row_id.clone())
                        .test_support()
                        .w_full()
                        .h(height::HEADER)
                        .pl(space::MD)
                        .pr(space::SM)
                        .rounded(radius::CONTROL)
                        .bg(if batch_mode && is_checked {
                            palette.bg_item_active
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::MD)
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                            cx.stop_propagation();
                            let s_id = sid_rclick.clone();
                            let t_cur = title_rclick.clone();
                            ent_rclick.update(cx, |this, cx| {
                                if !this.batch_mode {
                                    this.context_menu = Some(ContextMenuTarget {
                                        session_id: s_id,
                                        current_title: t_cur,
                                        anchor_top_px: anchor_y,
                                    });
                                    cx.notify();
                                }
                            });
                        })
                        .on_click(move |_, _, cx| {
                            let target = sid_click.clone();
                            ent_click.update(cx, |this, cx| {
                                this.context_menu = None;
                                if this.batch_mode {
                                    if !this.selected_ids.remove(&target) {
                                        this.selected_ids.insert(target);
                                    }
                                    cx.notify();
                                } else {
                                    cx.emit(AllSessionsEvent::OpenSession(target));
                                }
                            });
                        });
                    if batch_mode {
                        row_el = row_el.a11y_checkbox(row_id, title_label, is_checked);
                    } else {
                        row_el = row_el.a11y_listitem(row_id, title_label, false);
                    }

                    elements.push(row_el.child(left_row).child(right_slot).into_any_element());
                }
            }
        }
        elements
    }

    fn render_context_menu_overlay(
        &self,
        menu: &ContextMenuTarget,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let sid_rename = menu.session_id.clone();
        let title_rename = menu.current_title.clone();
        let sid_manage = menu.session_id.clone();
        let sid_delete = menu.session_id.clone();

        div()
            .id("all-sessions-menu-backdrop")
            .test_support()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .justify_center()
            .px(space::GROUP)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.context_menu = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                    .h_full()
                    .child(
                        render_frosted_surface(
                            "all-sessions-context-menu-uwt8w",
                            FrostedGlassSpec::popover_h2p2z(palette),
                            palette.bg_elevated,
                            transparency_enabled,
                            scale_factor,
                        )
                        .test_support()
                        .role(gpui_kit::Role::Menu)
                        .accessibility_id("all-sessions-context-menu")
                        .aria_label("会话操作菜单")
                        .absolute()
                        .top(px(menu.anchor_top_px))
                        .right(space::SM)
                        .w(px(200.0))
                        .p(space::XS)
                        .rounded(radius::ROW)
                        .border_1()
                        .border_color(palette.border_hairline)
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .flex()
                        .flex_col()
                        .gap(space::MICRO)
                        .child(
                            apply_scale(div(), scale::UI)
                                .id("all-sessions-ctx-rename")
                                .test_support()
                                .a11y_menuitem("all-sessions-ctx-rename", "重命名")
                                .relative()
                                .h(height::CONTROL)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .hover(|s| s.bg(palette.bg_item_active))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.open_rename_modal(
                                        sid_rename.clone(),
                                        title_rename.clone(),
                                        w,
                                        cx,
                                    );
                                }))
                                .child(
                                    Icon::new(IconName::Pencil)
                                        .size(px(16.0))
                                        .text_color(palette.text_secondary),
                                )
                                .child(cjk_inline_label("重命名")),
                        )
                        .child(
                            apply_scale(div(), scale::UI)
                                .id("all-sessions-ctx-manage")
                                .test_support()
                                .a11y_menuitem("all-sessions-ctx-manage", "管理")
                                .relative()
                                .h(height::CONTROL)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .hover(|s| s.bg(palette.bg_item_active))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.context_menu = None;
                                    this.batch_mode = true;
                                    this.selected_ids.clear();
                                    this.selected_ids.insert(sid_manage.clone());
                                    cx.notify();
                                }))
                                .child(
                                    Icon::new(IconName::ListChecks)
                                        .size(px(16.0))
                                        .text_color(palette.text_secondary),
                                )
                                .child(cjk_inline_label("管理")),
                        )
                        .child(
                            div()
                                .relative()
                                .px(space::XS)
                                .py(space::MICRO)
                                .child(div().w_full().h(px(1.0)).bg(palette.border_subtle)),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("all-sessions-ctx-delete")
                                .test_support()
                                .a11y_menuitem("all-sessions-ctx-delete", "删除")
                                .relative()
                                .h(height::CONTROL)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .text_color(palette.status_danger)
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .hover(|s| s.bg(palette.status_danger_subtle))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_sessions_optimistic(vec![sid_delete.clone()], cx);
                                }))
                                .child(
                                    Icon::new(IconName::Trash)
                                        .size(px(16.0))
                                        .text_color(palette.status_danger),
                                )
                                .child(cjk_inline_label("删除")),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_rename_modal(
        &self,
        inp: &Entity<InputState>,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let modal_card = render_frosted_surface(
            "all-sessions-rename-modal",
            FrostedGlassSpec::modal_k3x72(palette, 8.0),
            palette.bg_modal,
            transparency_enabled,
            scale_factor,
        )
        .test_support()
        .a11y_dialog("all-sessions-rename-modal", "重命名会话")
        .w(px(400.0))
        .p(px(20.0))
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
            if let Some(input) = &this.rename_input {
                super::consume_composing_escape(input, window, cx);
            }
        }))
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
            if ev.keystroke.key == "enter"
                && !ev.is_held
                && this.rename_input.as_ref().is_some_and(|input| {
                    use gpui_kit::Focusable as _;
                    input.read(cx).focus_handle(cx).is_focused(window)
                })
            {
                this.submit_rename(window, cx);
                cx.stop_propagation();
            }
        }))
        .flex()
        .flex_col()
        .gap(space::LG)
        .child(
            apply_scale(div(), scale::BODY_SEMIBOLD)
                .relative()
                .text_color(palette.text_primary)
                .child(cjk_inline_label("重命名会话")),
        )
        .child(
            div().relative().child(
                Input::new(inp)
                    .accessibility_id("all-sessions-rename-input")
                    .aria_label("会话标题")
                    .h(height::FIELD)
                    .cleanable(true),
            ),
        )
        .child(
            div()
                .relative()
                .flex()
                .justify_end()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("all-sessions-rename-cancel-btn")
                        .test_support()
                        .tab_index(0)
                        .a11y_button("all-sessions-rename-cancel-btn", "取消")
                        .h(height::CONTROL)
                        .px(space::MD)
                        .rounded(radius::CONTROL)
                        .border_1()
                        .border_color(palette.border_strong)
                        .bg(palette.bg_workspace)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_rename(window, cx);
                        }))
                        .child(cjk_inline_label("取消")),
                )
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("all-sessions-rename-save-btn")
                        .test_support()
                        .tab_index(0)
                        .a11y_button("all-sessions-rename-save-btn", "保存")
                        .h(height::CONTROL)
                        .px(space::LG)
                        .rounded(radius::CONTROL)
                        .bg(palette.accent_solid)
                        .text_color(palette.text_on_accent)
                        .flex()
                        .items_center()
                        .hover(|s| s.bg(palette.accent_hover))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_rename(window, cx);
                        }))
                        .child(cjk_inline_label("保存")),
                ),
        );

        let wrapped_modal = div()
            .tab_stop(false)
            .focus_trap("all-sessions-rename-trap", &self.rename_trap_focus)
            .child(modal_card);

        render_frosted_surface(
            "all-sessions-rename-scrim",
            FrostedGlassSpec::scrim_dz0fp(palette),
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
        .child(wrapped_modal)
        .into_any_element()
    }
}

impl Render for AllSessionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();

        let visible = self.visible_sessions();
        self.prune_selection(&visible);
        if self
            .context_menu
            .as_ref()
            .is_some_and(|menu| !visible.iter().any(|s| s.id == menu.session_id))
        {
            self.context_menu = None;
        }
        let today = Local::now().date_naive();
        self.cached_rows = group_sessions_by_natural_day(&visible, today);
        self.cached_sizes = Rc::new(
            self.cached_rows
                .iter()
                .map(|r| match r {
                    VirtualSessionRow::GroupHeader(_) => size(px(816.0), height::CONTROL),
                    VirtualSessionRow::Session(_) => size(px(816.0), height::HEADER),
                })
                .collect(),
        );

        let entity = cx.entity();
        let all_selected =
            !visible.is_empty() && visible.iter().all(|s| self.selected_ids.contains(&s.id));
        let selected_count = self.selected_ids.len();

        let header_trailing = if self.batch_mode {
            apply_scale(div(), scale::UI_MEDIUM)
                .id("all-sessions-exit-batch-btn")
                .test_support()
                .a11y_button("all-sessions-exit-batch-btn", "完成批量管理")
                .h(height::CONTROL)
                .px(space::MD)
                .rounded(radius::CONTROL)
                .bg(palette.bg_item_active)
                .text_color(palette.text_primary)
                .flex()
                .items_center()
                .hover(|s| s.bg(palette.bg_item_active))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.batch_mode = false;
                    this.selected_ids.clear();
                    cx.notify();
                }))
                .child(cjk_inline_label("完成"))
                .into_any_element()
        } else {
            apply_scale(div(), scale::UI_MEDIUM)
                .id("all-sessions-enter-batch-btn")
                .test_support()
                .a11y_button("all-sessions-manage-btn", "管理会话")
                .h(height::CONTROL)
                .px(space::MD)
                .rounded(radius::CONTROL)
                .text_color(palette.text_secondary)
                .flex()
                .items_center()
                .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.context_menu = None;
                    this.batch_mode = true;
                    this.selected_ids.clear();
                    cx.notify();
                }))
                .child(cjk_inline_label("管理"))
                .into_any_element()
        };

        let batch_bar_opt = self.batch_mode.then(|| {
            div()
                .id("all-sessions-batch-bar")
                .test_support()
                .w_full()
                .h(height::CONTROL)
                .px(space::MD)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    apply_scale(div(), scale::UI)
                        .text_color(palette.text_secondary)
                        .child(cjk_inline_label(format!("已选择 {selected_count} 个"))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("all-sessions-select-all-btn")
                                .test_support()
                                .a11y_button(
                                    "all-sessions-select-all-btn",
                                    if all_selected {
                                        "取消全选"
                                    } else {
                                        "全选"
                                    },
                                )
                                .h(height::CONTROL)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .text_color(palette.text_secondary)
                                .flex()
                                .items_center()
                                .hover(|s| {
                                    s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_select_all(cx)))
                                .child(cjk_inline_label(if all_selected {
                                    "取消全选"
                                } else {
                                    "全选"
                                })),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("all-sessions-batch-delete-btn")
                                .test_support()
                                .a11y_button("all-sessions-batch-delete-btn", "批量删除所选会话")
                                .h(height::CONTROL)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .text_color(if selected_count == 0 {
                                    palette.text_muted
                                } else {
                                    palette.status_danger
                                })
                                .flex()
                                .items_center()
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.execute_batch_delete(window, cx)
                                }))
                                .child(cjk_inline_label("删除")),
                        ),
                )
        });

        let body_el = if self.cached_rows.is_empty() {
            div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    apply_scale(div(), scale::UI)
                        .text_color(palette.text_muted)
                        .child("暂无历史会话"),
                )
                .into_any_element()
        } else {
            v_virtual_list(
                entity,
                "all-sessions-virtual-list",
                Rc::clone(&self.cached_sizes),
                Self::render_virtual_range,
            )
            .track_scroll(&self.scroll_handle)
            .flex_1()
            .w_full()
            .into_any_element()
        };

        let menu_overlay = self.context_menu.clone().map(|m| {
            self.render_context_menu_overlay(&m, &palette, transparency_enabled, scale_factor, cx)
        });

        let rename_overlay = if self.renaming_session_id.is_some() {
            self.rename_input.clone().map(|inp| {
                self.render_rename_modal(&inp, &palette, transparency_enabled, scale_factor, cx)
            })
        } else {
            None
        };

        div()
            .id("all-sessions-view")
            .test_support()
            .a11y_region("all-sessions-view", "全部会话")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.bg_workspace)
            .child(render_subpage_header_xwjof(
                "all-sessions-subpage-header",
                IconName::RotateCcwClock,
                "全部会话",
                Some(header_trailing),
                &palette,
            ))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .px(space::GROUP)
                    .py(space::GROUP)
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap(space::SM)
                            .children(batch_bar_opt)
                            .child(body_el),
                    )
                    .children(menu_overlay),
            )
            .children(rename_overlay)
    }
}

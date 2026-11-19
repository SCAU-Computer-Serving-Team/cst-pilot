use std::collections::BTreeSet;
use std::rc::Rc;

use chrono::{DateTime, Local, NaiveDate};
use cst_client::{ClientError, ClientHandle, SessionSummary};
use gpui::{Pixels, Size, size};
use gpui_kit::assets::IconName;
use gpui_kit::base::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::component::Icon;
use gpui_kit::{
    Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
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
pub enum AllSessionsEvent {
    OpenSession(String),
    ActiveSessionDeletedSwitchHome,
    Notice(String),
    ErrorMessage(String),
}

pub struct AllSessionsView {
    client: ClientHandle,
    scroll_handle: VirtualListScrollHandle,
    batch_mode: bool,
    selected_ids: BTreeSet<String>,
    optimistically_hidden_ids: BTreeSet<String>,
    cached_rows: Vec<VirtualSessionRow>,
    cached_sizes: Rc<Vec<Size<Pixels>>>,
}

impl EventEmitter<AllSessionsEvent> for AllSessionsView {}

impl AllSessionsView {
    pub fn new(client: ClientHandle) -> Self {
        Self {
            client,
            scroll_handle: VirtualListScrollHandle::new(),
            batch_mode: false,
            selected_ids: BTreeSet::new(),
            optimistically_hidden_ids: BTreeSet::new(),
            cached_rows: Vec::new(),
            cached_sizes: Rc::new(Vec::new()),
        }
    }

    pub fn is_batch_mode(&self) -> bool {
        self.batch_mode
    }

    pub fn dismiss_batch_mode(&mut self, cx: &mut Context<Self>) -> bool {
        if self.batch_mode {
            self.batch_mode = false;
            self.selected_ids.clear();
            cx.notify();
            true
        } else {
            false
        }
    }

    fn visible_sessions(&self) -> Vec<SessionSummary> {
        let snap = self.client.snapshot();
        snap.sessions
            .iter()
            .filter(|s| !self.optimistically_hidden_ids.contains(&s.id))
            .cloned()
            .collect()
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

    fn execute_batch_delete(&mut self, cx: &mut Context<Self>) {
        let targets: Vec<String> = self.selected_ids.iter().cloned().collect();
        if targets.is_empty() {
            return;
        }
        self.selected_ids.clear();
        self.batch_mode = false;

        // 1. Immediate optimistic removal; if active session is among targets, switch to Home first!
        let active_sid = self
            .client
            .with_workspace(|ws| ws.active_session_id().map(str::to_owned));
        if let Some(ref cur) = active_sid
            && targets.iter().any(|t| t == cur)
        {
            cx.emit(AllSessionsEvent::ActiveSessionDeletedSwitchHome);
        }

        for id in &targets {
            self.optimistically_hidden_ids.insert(id.clone());
        }
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let mut rolled_back_ids = Vec::new();
            let mut conflict_msg: Option<String> = None;

            for sid in targets {
                match client.delete_session(sid.clone()).await {
                    Ok(_) => {}
                    Err(ClientError::Conflict { message, .. }) => {
                        rolled_back_ids.push(sid);
                        conflict_msg = Some(message);
                    }
                    Err(err) => {
                        rolled_back_ids.push(sid);
                        if conflict_msg.is_none() {
                            conflict_msg = Some(err.to_string());
                        }
                    }
                }
            }

            let _ = client.refresh_globals().await;

            let _ = weak.update(cx, |this, cx| {
                // Clean up confirmed deletions from `optimistically_hidden_ids`, and restore rolled-back rows!
                let current_ids: BTreeSet<String> = this
                    .client
                    .snapshot()
                    .sessions
                    .iter()
                    .map(|s| s.id.clone())
                    .collect();
                this.optimistically_hidden_ids
                    .retain(|id| current_ids.contains(id) && !rolled_back_ids.contains(id));

                if let Some(msg) = conflict_msg {
                    cx.emit(AllSessionsEvent::Notice(format!(
                        "部分运行中或占用的会话未能删除，已自动恢复：{msg}"
                    )));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_virtual_range(
        &mut self,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let palette = DesktopThemeGlobal::current(cx).palette;
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
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(SharedString::from(format!(
                                "all-sessions-group-{:?}",
                                group
                            )))
                            .test_support()
                            .w_full()
                            .h(height::CONTROL)
                            .pt(space::SM)
                            .px(space::SM)
                            .text_color(palette.text_muted)
                            .flex()
                            .items_center()
                            .child(cjk_inline_label(group.title()))
                            .into_any_element(),
                    );
                }
                VirtualSessionRow::Session(sess) => {
                    let sid = sess.id.clone();
                    let is_checked = self.selected_ids.contains(&sid);
                    let title = if sess.title.trim().is_empty() {
                        SharedString::from("新对话")
                    } else {
                        SharedString::from(sess.title.clone())
                    };
                    let updated_text = sess
                        .updated_at
                        .as_deref()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&Local).format("%m-%d %H:%M").to_string())
                        .unwrap_or_default();

                    let ent_click = entity.clone();
                    let sid_click = sid.clone();

                    let leading = if batch_mode {
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
                                palette.bg_elevated
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .children(is_checked.then(|| {
                                Icon::new(IconName::Check)
                                    .size(px(11.0))
                                    .text_color(palette.text_on_accent)
                            }))
                    } else if sess.running {
                        div()
                            .w(px(8.0))
                            .h(px(8.0))
                            .rounded(radius::FULL)
                            .bg(palette.accent_solid)
                            .flex_shrink_0()
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .child(
                                Icon::new(IconName::MessageSquare)
                                    .size(px(14.0))
                                    .text_color(palette.text_muted),
                            )
                    };

                    elements.push(
                        apply_scale(div(), scale::UI)
                            .id(SharedString::from(format!("all-sessions-row-{sid}")))
                            .test_support()
                            .w_full()
                            .h(height::HEADER)
                            .px(space::MD)
                            .rounded(radius::ROW)
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
                            .on_click(move |_, _, cx| {
                                let target = sid_click.clone();
                                ent_click.update(cx, |this, cx| {
                                    if this.batch_mode {
                                        if !this.selected_ids.remove(&target) {
                                            this.selected_ids.insert(target);
                                        }
                                        cx.notify();
                                    } else {
                                        cx.emit(AllSessionsEvent::OpenSession(target));
                                    }
                                });
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(space::MD)
                                    .min_w_0()
                                    .flex_1()
                                    .child(leading)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(palette.text_primary)
                                            .child(cjk_inline_label(title)),
                                    ),
                            )
                            .children((!updated_text.is_empty()).then(|| {
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_muted)
                                    .flex_shrink_0()
                                    .child(SharedString::from(updated_text))
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        elements
    }
}

impl Render for AllSessionsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = DesktopThemeGlobal::current(cx).palette;
        let visible = self.visible_sessions();
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

        let batch_controls = if self.batch_mode {
            div()
                .flex()
                .items_center()
                .gap(space::MD)
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("all-sessions-select-all-btn")
                        .test_support()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
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
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(if self.selected_ids.is_empty() {
                            palette.text_muted
                        } else {
                            palette.status_danger
                        })
                        .flex()
                        .items_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(|this, _, _, cx| this.execute_batch_delete(cx)))
                        .child(cjk_inline_label("删除")),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .id("all-sessions-exit-batch-btn")
                        .test_support()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let _ = this.dismiss_batch_mode(cx);
                        }))
                        .child(cjk_inline_label("完成")),
                )
        } else {
            div().child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("all-sessions-enter-batch-btn")
                    .test_support()
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .bg(palette.bg_surface)
                    .text_color(palette.text_secondary)
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.batch_mode = true;
                        this.selected_ids.clear();
                        cx.notify();
                    }))
                    .child(cjk_inline_label("管理")),
            )
        };

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

        div()
            .id("all-sessions-view")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .px(space::GROUP)
            .py(space::GROUP)
            .child(
                div()
                    .w_full()
                    .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap(space::LG)
                    .child(
                        div()
                            .w_full()
                            .h(height::HEADER)
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .child(
                                apply_scale(div(), scale::TITLE)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("全部会话")),
                            )
                            .child(batch_controls),
                    )
                    .child(body_el),
            )
    }
}

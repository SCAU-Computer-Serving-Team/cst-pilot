use std::collections::BTreeSet;
use std::rc::Rc;

use cst_client::SessionSummary;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
};

pub const MAX_RECENT_SIDEBAR_SESSIONS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellRoute {
    Home,
    Session,
    AllSessions,
    Settings,
    Login,
    Account,
}

impl ShellRoute {
    pub fn uses_sky_shader(self) -> bool {
        matches!(self, Self::Home | Self::Login)
    }
}

pub type SidebarActionFn = Rc<dyn Fn(&mut Window, &mut App)>;
pub type SidebarSessionFn = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub struct SidebarCallbacks {
    pub on_collapse: SidebarActionFn,
    pub on_new_session: SidebarActionFn,
    pub on_open_all_sessions: SidebarActionFn,
    pub on_open_dashboard: SidebarActionFn,
    pub on_select_session: SidebarSessionFn,
    pub on_toggle_batch_mode: SidebarActionFn,
    pub on_toggle_select_all: SidebarActionFn,
    pub on_batch_delete: SidebarActionFn,
    pub on_toggle_session_checked: SidebarSessionFn,
    pub on_toggle_account_menu: SidebarActionFn,
    pub on_open_settings: SidebarActionFn,
}

/// Full-height `232px` sidebar (`cst-pilot-desktop.pen` 「01 · 工作台」 master `L081M1`).
#[allow(clippy::too_many_arguments)]
pub fn render_sidebar_l081m1(
    route: ShellRoute,
    active_session_id: Option<&str>,
    sessions: &[SessionSummary],
    batch_mode: bool,
    selected_ids: &BTreeSet<String>,
    account_label: SharedString,
    account_initial: SharedString,
    collapse_btn_focus: &FocusHandle,
    account_btn_focus: &FocusHandle,
    palette: &DesktopPalette,
    cb: SidebarCallbacks,
) -> impl IntoElement {
    let recent: &[SessionSummary] = if sessions.len() > MAX_RECENT_SIDEBAR_SESSIONS {
        &sessions[..MAX_RECENT_SIDEBAR_SESSIONS]
    } else {
        sessions
    };

    let mut recent_list = div()
        .id("sidebar-recent-scroll")
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(space::MICRO);

    if recent.is_empty() {
        recent_list = recent_list.child(
            apply_scale(div(), scale::CAPTION)
                .px(space::SM)
                .py(space::MD)
                .text_color(palette.text_muted)
                .child("暂无历史对话"),
        );
    } else {
        for sess in recent {
            let sid = sess.id.clone();
            let is_active =
                matches!(route, ShellRoute::Session) && active_session_id == Some(sid.as_str());
            let is_checked = selected_ids.contains(&sid);
            let title_text = if sess.title.trim().is_empty() {
                SharedString::from("新对话")
            } else {
                SharedString::from(sess.title.clone())
            };

            let row_bg = if (batch_mode && is_checked) || (!batch_mode && is_active) {
                palette.bg_item_active
            } else {
                gpui_kit::transparent_black()
            };

            let on_select = Rc::clone(&cb.on_select_session);
            let on_check = Rc::clone(&cb.on_toggle_session_checked);
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
                    .when_some(is_checked.then_some(()), |d, ()| {
                        d.child(
                            Icon::new(IconName::Check)
                                .size(px(11.0))
                                .text_color(palette.text_on_accent),
                        )
                    })
            } else if sess.running {
                div()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded(radius::FULL)
                    .bg(palette.accent_solid)
                    .flex_shrink_0()
            } else {
                div().w(px(0.0)).h(px(0.0))
            };

            recent_list = recent_list.child(
                apply_scale(div(), scale::UI)
                    .id(SharedString::from(format!("sidebar-session-row-{sid}")))
                    .test_support()
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::ROW)
                    .bg(row_bg)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, window, cx| {
                        if batch_mode {
                            (on_check)(sid_click.clone(), window, cx);
                        } else {
                            (on_select)(sid_click.clone(), window, cx);
                        }
                    })
                    .child(leading)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(cjk_inline_label(title_text)),
                    ),
            );
        }
    }

    let on_collapse = Rc::clone(&cb.on_collapse);
    let on_new = Rc::clone(&cb.on_new_session);
    let on_all = Rc::clone(&cb.on_open_all_sessions);
    let on_dash = Rc::clone(&cb.on_open_dashboard);
    let on_batch_toggle = Rc::clone(&cb.on_toggle_batch_mode);
    let on_sel_all = Rc::clone(&cb.on_toggle_select_all);
    let on_del = Rc::clone(&cb.on_batch_delete);
    let on_acc = Rc::clone(&cb.on_toggle_account_menu);
    let on_settings = Rc::clone(&cb.on_open_settings);

    div()
        .id("sidebar-l081m1")
        .test_support()
        .w(layout::SIDEBAR_WIDTH)
        .h_full()
        .flex_shrink_0()
        .bg(palette.bg_shell)
        .flex()
        .flex_col()
        // 1. Header (`40px`)
        .child(
            div()
                .h(layout::TOPBAR_HEIGHT)
                .px(space::MD)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .text_color(palette.text_primary)
                        .child("CST Pilot"),
                )
                .child(
                    div()
                        .id("sidebar-collapse-btn")
                        .test_support()
                        .track_focus(collapse_btn_focus)
                        .tab_index(0)
                        .w(height::COMPACT)
                        .h(height::COMPACT)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, window, cx| (on_collapse)(window, cx))
                        .child(
                            Icon::new(IconName::PanelLeft)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        ),
                ),
        )
        // 2. Primary navigation actions
        .child(
            div()
                .px(space::SM)
                .py(space::XS)
                .flex()
                .flex_col()
                .gap(space::MICRO)
                .child(sidebar_nav_item(
                    "sidebar-new-session-btn",
                    IconName::Plus,
                    "新建对话",
                    matches!(route, ShellRoute::Home),
                    palette,
                    move |w, cx| (on_new)(w, cx),
                ))
                .child(sidebar_nav_item(
                    "sidebar-all-sessions-btn",
                    IconName::MessageSquare,
                    "全部会话",
                    matches!(route, ShellRoute::AllSessions),
                    palette,
                    move |w, cx| (on_all)(w, cx),
                ))
                .child(sidebar_nav_item(
                    "sidebar-dashboard-btn",
                    IconName::LayoutDashboard,
                    "仪表盘",
                    false,
                    palette,
                    move |w, cx| (on_dash)(w, cx),
                )),
        )
        // 3. Recent sessions header + batch actions
        .child(
            div()
                .px(space::MD)
                .pt(space::MD)
                .pb(space::XS)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .text_color(palette.text_muted)
                        .child("最近"),
                )
                .child(if batch_mode {
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .id("sidebar-batch-select-all-btn")
                                .test_support()
                                .text_color(palette.text_secondary)
                                .on_click(move |_, w, cx| (on_sel_all)(w, cx))
                                .child("全选"),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .id("sidebar-batch-delete-btn")
                                .test_support()
                                .text_color(palette.status_danger)
                                .on_click(move |_, w, cx| (on_del)(w, cx))
                                .child("删除"),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .id("sidebar-batch-done-btn")
                                .test_support()
                                .text_color(palette.accent_solid)
                                .on_click(move |_, w, cx| (on_batch_toggle)(w, cx))
                                .child("完成"),
                        )
                } else {
                    div().child(
                        apply_scale(div(), scale::CAPTION)
                            .id("sidebar-batch-manage-btn")
                            .test_support()
                            .text_color(palette.text_muted)
                            .hover(|s| s.text_color(palette.text_primary))
                            .on_click(move |_, w, cx| (on_batch_toggle)(w, cx))
                            .child("管理"),
                    )
                }),
        )
        // 4. Scrollable recent 20 list
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .px(space::SM)
                .flex()
                .flex_col()
                .child(recent_list),
        )
        // 5. Bottom account + settings bar
        .child(
            div()
                .p(space::SM)
                .border_t_1()
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .gap(space::XS)
                .child(
                    apply_scale(div(), scale::UI)
                        .id("sidebar-account-btn")
                        .test_support()
                        .track_focus(account_btn_focus)
                        .tab_index(0)
                        .flex_1()
                        .min_w_0()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::ROW)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| (on_acc)(w, cx))
                        .child(
                            apply_scale(div(), scale::CAPTION_MEDIUM)
                                .w(px(22.0))
                                .h(px(22.0))
                                .rounded(radius::FULL)
                                .bg(palette.accent_subtle)
                                .text_color(palette.accent_solid)
                                .flex()
                                .items_center()
                                .justify_center()
                                .flex_shrink_0()
                                .child(account_initial),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(account_label)),
                        ),
                )
                .child(
                    div()
                        .id("sidebar-settings-btn")
                        .test_support()
                        .tab_index(0)
                        .w(height::CONTROL)
                        .h(height::CONTROL)
                        .rounded(radius::ROW)
                        .bg(if matches!(route, ShellRoute::Settings) {
                            palette.bg_item_active
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, w, cx| (on_settings)(w, cx))
                        .child(
                            Icon::new(IconName::Settings)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        ),
                ),
        )
}

fn sidebar_nav_item(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    palette: &DesktopPalette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    apply_scale(div(), scale::UI)
        .id(id)
        .test_support()
        .tab_index(0)
        .h(height::CONTROL)
        .px(space::SM)
        .rounded(radius::ROW)
        .bg(if active {
            palette.bg_item_active
        } else {
            gpui_kit::transparent_black()
        })
        .text_color(palette.text_primary)
        .flex()
        .items_center()
        .gap(space::SM)
        .hover(|s| s.bg(palette.bg_item_hover))
        .on_click(move |_, w, cx| on_click(w, cx))
        .child(
            Icon::new(icon)
                .size(px(15.0))
                .text_color(palette.text_secondary),
        )
        .child(cjk_inline_label(label))
}

trait DivCondExt: Sized {
    fn when_some<T>(self, opt: Option<T>, f: impl FnOnce(Self, T) -> Self) -> Self {
        match opt {
            Some(v) => f(self, v),
            None => self,
        }
    }
}

impl<E> DivCondExt for E {}

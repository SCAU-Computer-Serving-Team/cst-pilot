use std::collections::BTreeSet;
use std::rc::Rc;

use cst_client::SessionSummary;
use gpui_kit::assets::IconName;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    App, Entity, FocusHandle, Focusable as _, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, SharedString, StatefulInteractiveElement, Styled, TestSupportExt,
    Window, WindowControlArea, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::motion::{DesktopTooltipExt as _, IconPressExt as _};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, motion, radius, scale, space,
};

#[doc(hidden)]
pub use self::render_sidebar as render_sidebar_l081m1;

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
pub type SidebarSessionMenuFn = Rc<dyn Fn(String, Point<Pixels>, &mut Window, &mut App)>;

pub struct SidebarCallbacks {
    pub on_collapse: SidebarActionFn,
    pub on_new_session: SidebarActionFn,
    pub on_clear_search: SidebarActionFn,
    pub on_open_palette: SidebarActionFn,
    pub on_open_all_sessions: SidebarActionFn,
    pub on_open_dashboard: SidebarActionFn,
    pub on_select_session: SidebarSessionFn,
    pub on_open_session_menu: SidebarSessionMenuFn,
    pub on_toggle_select_all: SidebarActionFn,
    pub on_batch_delete: SidebarActionFn,
    pub on_toggle_session_checked: SidebarSessionFn,
    pub on_toggle_account_menu: SidebarActionFn,
    pub on_open_settings: SidebarActionFn,
}

/// Computes the filtered recent sidebar sessions (sorted by `updated_at` desc, filtered by `search_query`, capped at `20`).
pub fn filter_recent_sidebar_sessions(
    sessions: &[SessionSummary],
    search_query: &str,
) -> Vec<SessionSummary> {
    let mut sorted: Vec<SessionSummary> = sessions.to_vec();
    sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let q = search_query.trim().to_lowercase();
    if !q.is_empty() {
        sorted.retain(|s| {
            let title = if s.title.trim().is_empty() {
                "新对话"
            } else {
                s.title.as_str()
            };
            title.to_lowercase().contains(&q)
        });
    }
    if sorted.len() > MAX_RECENT_SIDEBAR_SESSIONS {
        sorted.truncate(MAX_RECENT_SIDEBAR_SESSIONS);
    }
    sorted
}

/// Full-height `248px` sidebar (`cst-pilot-desktop.pen` 「01 · 工作台」 master `L081M1` / `h5AY3`).
#[allow(clippy::too_many_arguments)]
pub fn render_sidebar(
    route: ShellRoute,
    active_session_id: Option<&str>,
    sessions: &[SessionSummary],
    window: &mut Window,
    cx: &mut App,
    search_input: &Entity<InputState>,
    search_query: &str,
    batch_mode: bool,
    selected_ids: &BTreeSet<String>,
    account_label: SharedString,
    account_initial: SharedString,
    account_menu_open: bool,
    collapse_btn_focus: &FocusHandle,
    account_btn_focus: &FocusHandle,
    settings_btn_focus: &FocusHandle,
    palette: &DesktopPalette,
    cb: SidebarCallbacks,
) -> impl IntoElement {
    let recent = filter_recent_sidebar_sessions(sessions, search_query);
    let has_search_query = !search_query.trim().is_empty();
    let row_height = px(36.0);

    let mut recent_list = div()
        .id("sidebar-recent-scroll")
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(space::TIGHT);

    if recent.is_empty() {
        recent_list = recent_list.child(
            apply_scale(div(), scale::BODY)
                .px(space::MD)
                .py(space::MD)
                .text_color(palette.text_muted)
                .child(if has_search_query {
                    "无匹配会话"
                } else {
                    "暂无历史对话"
                }),
        );
    } else {
        let flip_scope = SharedString::from(format!(
            "{active_session_id:?}:{}",
            search_query.trim().to_lowercase()
        ));
        for (index, sess) in recent.iter().enumerate() {
            let sid = sess.id.clone();
            let is_active =
                matches!(route, ShellRoute::Session) && active_session_id == Some(sid.as_str());
            let is_checked = selected_ids.contains(&sid);
            let is_selected = if batch_mode { is_checked } else { is_active };

            let title_text = if sess.title.trim().is_empty() {
                SharedString::from("新对话")
            } else {
                SharedString::from(sess.title.clone())
            };

            let row_bg = if is_selected {
                palette.bg_item_active
            } else {
                gpui_kit::transparent_black()
            };
            let row_hover_bg = if is_selected {
                palette.bg_item_active
            } else {
                palette.bg_item_hover
            };
            let row_scale = if is_selected {
                scale::BODY_MEDIUM
            } else {
                scale::BODY
            };
            let row_text_color = if is_selected {
                palette.text_primary
            } else {
                palette.text_secondary
            };

            let on_select = Rc::clone(&cb.on_select_session);
            let on_check = Rc::clone(&cb.on_toggle_session_checked);
            let on_menu_rclick = Rc::clone(&cb.on_open_session_menu);
            let on_menu_btn = Rc::clone(&cb.on_open_session_menu);
            let sid_click = sid.clone();
            let sid_rclick = sid.clone();
            let sid_menu = sid.clone();
            let row_group = SharedString::from(format!("sidebar-row-grp-{sid}"));

            let row_a11y_id = SharedString::from(format!("sidebar-session-row-{sid}"));
            let flip_y = motion::flip_row_offset_y(
                "sidebar-recent",
                &sid,
                &flip_scope,
                (row_height + space::TIGHT) * index as f32,
                window,
                cx,
            );
            let mut row = apply_scale(div(), row_scale)
                .id(row_a11y_id.clone())
                .test_support()
                .relative()
                .top(flip_y)
                .group(row_group.clone())
                .h(row_height)
                .px(space::MD)
                .rounded(radius::CONTROL)
                .bg(row_bg)
                .text_color(row_text_color)
                .flex()
                .items_center()
                .gap(space::MD)
                .hover(move |s| s.bg(row_hover_bg).text_color(palette.text_primary))
                .on_mouse_down(gpui::MouseButton::Right, move |ev, window, cx| {
                    (on_menu_rclick)(sid_rclick.clone(), ev.position, window, cx);
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    if batch_mode {
                        (on_check)(sid_click.clone(), window, cx);
                    } else {
                        (on_select)(sid_click.clone(), window, cx);
                    }
                });

            let slot_w = transition(
                SharedString::from(format!("sb-shift-{sid}")),
                if batch_mode { 24.0 } else { 0.0 },
                Transition::new(if batch_mode {
                    motion::NORMAL
                } else {
                    motion::SURFACE_CLOSE
                })
                .ease(motion::ease_out()),
                window,
                cx,
            );
            let slot_ratio = (slot_w / 24.0).clamp(0.0, 1.0);
            let chk_mark = (batch_mode || slot_w > 0.5).then(|| {
                motion::animated_checkbox_mark(
                    format!("sb-chk-{sid}"),
                    batch_mode && is_checked,
                    px(11.0),
                    palette.text_on_accent,
                    window,
                    cx,
                )
            });

            if batch_mode {
                row = row.a11y_checkbox(row_a11y_id, title_text.clone(), is_checked);
            } else {
                row = row.a11y_listitem(row_a11y_id, title_text.clone(), is_active);
            }

            if let Some(mark_opt) = chk_mark {
                row = row.child(
                    div()
                        .w(px(slot_w))
                        .h(px(16.0))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .opacity(slot_ratio)
                        .child(
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
                                .children(mark_opt),
                        ),
                );
            }

            let menu_btn_id = SharedString::from(format!("sidebar-row-menu-btn-{sid}"));
            let menu_btn_label = SharedString::from(format!("会话操作：{title_text}"));
            let focus = window.use_keyed_state(menu_btn_id.clone(), cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            });
            row = row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(cjk_inline_label(title_text)),
                )
                .child(
                    div()
                        .id(menu_btn_id.clone())
                        .test_support()
                        .a11y_button(menu_btn_id, menu_btn_label)
                        .track_focus(focus.read(cx))
                        .w(px(20.0))
                        .h(px(20.0))
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .flex_shrink_0()
                        .hover(|s| s.bg(palette.bg_surface))
                        .on_click(move |ev, window, cx| {
                            (on_menu_btn)(sid_menu.clone(), ev.position(), window, cx);
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.0)
                                .group_hover(row_group, |s| s.opacity(1.0))
                                .child(
                                    Icon::new(IconName::Ellipsis)
                                        .size(px(14.0))
                                        .text_color(palette.text_muted),
                                ),
                        ),
                );

            recent_list = recent_list.child(row);
        }
    }

    let on_collapse = Rc::clone(&cb.on_collapse);
    let on_new = Rc::clone(&cb.on_new_session);
    let on_clear = Rc::clone(&cb.on_clear_search);
    let on_palette = Rc::clone(&cb.on_open_palette);
    let on_all = Rc::clone(&cb.on_open_all_sessions);
    let on_dash = Rc::clone(&cb.on_open_dashboard);
    let on_sel_all = Rc::clone(&cb.on_toggle_select_all);
    let on_del = Rc::clone(&cb.on_batch_delete);
    let on_acc = Rc::clone(&cb.on_toggle_account_menu);
    let on_settings = Rc::clone(&cb.on_open_settings);
    let search_focus_ent = search_input.clone();

    let search_trailing = if has_search_query {
        div()
            .id("sidebar-search-clear-btn")
            .test_support()
            .a11y_button("sidebar-search-clear-btn", "清空搜索")
            .w(px(20.0))
            .h(px(20.0))
            .rounded(radius::CONTROL)
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .hover(|s| s.bg(palette.bg_item_hover))
            .on_click(move |_, w, cx| {
                (on_clear)(w, cx);
                cx.stop_propagation();
            })
            .child(
                Icon::new(IconName::X)
                    .size(px(13.0))
                    .text_color(palette.text_muted),
            )
            .into_any_element()
    } else {
        apply_scale(div(), scale::CAPTION)
            .id("sidebar-search-kbd-btn")
            .test_support()
            .a11y_button("sidebar-search-kbd-btn", "打开命令面板")
            .text_color(palette.text_muted)
            .flex_shrink_0()
            .hover(|s| s.text_color(palette.text_primary))
            .on_click(move |_, w, cx| {
                (on_palette)(w, cx);
                cx.stop_propagation();
            })
            .child(cjk_inline_label("Ctrl+K"))
            .into_any_element()
    };

    let search_box = div()
        .id("sidebar-search-box")
        .test_support()
        .h(row_height)
        .px(space::MD)
        .rounded(radius::CONTROL)
        .border_1()
        .border_color(palette.border_hairline)
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(space::SM)
        .on_click(move |_, window, cx| {
            search_focus_ent.focus_handle(cx).focus(window, cx);
        })
        .child(
            Icon::new(IconName::Search)
                .size(px(18.0))
                .text_color(palette.text_secondary),
        )
        .child(
            apply_scale(div(), scale::BODY).flex_1().min_w_0().child(
                Input::new(search_input)
                    .accessibility_id("sidebar-search-box")
                    .aria_label("搜索会话")
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .p_0(),
            ),
        )
        .child(search_trailing);

    let has_checked_items = !selected_ids.is_empty();
    let account_btn_label = SharedString::from(format!("账号菜单：{account_label}"));

    div()
        .id("sidebar-l081m1")
        .test_support()
        .a11y_nav("sidebar-l081m1", "侧栏导航")
        .w(layout::SIDEBAR_WIDTH)
        .h_full()
        .flex_shrink_0()
        .px(space::LG)
        .pb(space::MD)
        .bg(palette.bg_shell)
        .flex()
        .flex_col()
        .justify_between()
        // Top + middle scrollable workspace navigation (`b50Q7Y` + `pQpX5`)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .gap(space::SM)
                // 1. Header `b50Q7Y` (`40px`, draggable title + collapse & new-session buttons)
                .child(
                    div()
                        .h(layout::TOPBAR_HEIGHT)
                        .pl(space::SM)
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .id("sidebar-header-drag-region")
                                .window_control_area(WindowControlArea::Drag)
                                .flex_1()
                                .h_full()
                                .flex()
                                .items_center()
                                .child(
                                    apply_scale(div(), scale::BODY_SEMIBOLD)
                                        .text_color(palette.text_primary)
                                        .child(cjk_inline_label("CST Pilot")),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(space::TIGHT)
                                .child({
                                    let (pressed, press_state) = motion::icon_press_state(
                                        "sidebar-collapse-btn",
                                        window,
                                        cx,
                                    );
                                    let (hovered, hs) = motion::PointerFlag::keyed(
                                        "hover-sidebar-collapse-btn",
                                        window,
                                        cx,
                                    );
                                    let hover_t = motion::animated_state_progress(
                                        "sidebar-collapse-btn",
                                        hovered,
                                        window,
                                        cx,
                                    );
                                    let press_t = motion::animated_press_progress(
                                        "sidebar-collapse-btn",
                                        pressed,
                                        window,
                                        cx,
                                    );
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
                                        .id("sidebar-collapse-btn")
                                        .test_support()
                                        .a11y_button("sidebar-collapse-btn", "收起侧栏")
                                        .desktop_tooltip_with_hover(
                                            "tooltip-sidebar-collapse-btn".into(),
                                            "收起侧栏".into(),
                                            false,
                                            Some(hs),
                                        )
                                        .track_focus(collapse_btn_focus)
                                        .tab_index(0)
                                        .w(height::CONTROL)
                                        .h(height::CONTROL)
                                        .rounded(radius::CONTROL)
                                        .bg(bg)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .on_icon_press(press_state)
                                        .on_click(move |_, window, cx| (on_collapse)(window, cx))
                                        .child(motion::animated_icon_press(
                                            "sidebar-collapse-btn",
                                            pressed,
                                            Icon::new(IconName::PanelLeft)
                                                .size(px(18.0))
                                                .text_color(motion::lerp_hsla(
                                                    palette.text_secondary,
                                                    palette.text_primary,
                                                    hover_t,
                                                )),
                                            window,
                                            cx,
                                        ))
                                })
                                .child({
                                    let (pressed, press_state) = motion::icon_press_state(
                                        "sidebar-new-session-btn",
                                        window,
                                        cx,
                                    );
                                    let (hovered, hs) = motion::PointerFlag::keyed(
                                        "hover-sidebar-new-session-btn",
                                        window,
                                        cx,
                                    );
                                    let hover_t = motion::animated_state_progress(
                                        "sidebar-new-session-btn",
                                        hovered,
                                        window,
                                        cx,
                                    );
                                    let press_t = motion::animated_press_progress(
                                        "sidebar-new-session-btn",
                                        pressed,
                                        window,
                                        cx,
                                    );
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
                                        .id("sidebar-new-session-btn")
                                        .test_support()
                                        .a11y_button("sidebar-new-session-btn", "新建对话")
                                        .desktop_tooltip_with_hover(
                                            "tooltip-sidebar-new-session-btn".into(),
                                            "新建对话".into(),
                                            false,
                                            Some(hs),
                                        )
                                        .tab_index(0)
                                        .w(height::CONTROL)
                                        .h(height::CONTROL)
                                        .rounded(radius::CONTROL)
                                        .bg(bg)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .on_icon_press(press_state)
                                        .on_click(move |_, window, cx| (on_new)(window, cx))
                                        .child(motion::animated_icon_press(
                                            "sidebar-new-session-btn",
                                            pressed,
                                            Icon::new(IconName::SquarePen)
                                                .size(px(18.0))
                                                .text_color(motion::lerp_hsla(
                                                    palette.text_secondary,
                                                    palette.text_primary,
                                                    hover_t,
                                                )),
                                            window,
                                            cx,
                                        ))
                                }),
                        ),
                )
                // 2. Navigation & recent sessions body `pQpX5` (`gap: 16px`)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::LG)
                        // 2a. Search box `oq4G4` (`36px`)
                        .child(search_box)
                        // 2b. Primary navigation `WKJAy` (`gap: 6px`, Dashboard -> All Sessions)
                        .child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .flex_col()
                                .gap(space::TIGHT)
                                .child(sidebar_nav_item(
                                    "sidebar-dashboard-btn",
                                    IconName::LayoutDashboard,
                                    "仪表盘",
                                    false,
                                    palette,
                                    move |w, cx| (on_dash)(w, cx),
                                ))
                                .child(sidebar_nav_item(
                                    "sidebar-all-sessions-btn",
                                    IconName::RotateCcwClock,
                                    "全部会话",
                                    !batch_mode && matches!(route, ShellRoute::AllSessions),
                                    palette,
                                    move |w, cx| (on_all)(w, cx),
                                )),
                        )
                        // 2c. Recent sessions group `CzUNm` (`28px` header + `8px` gap + scrollable list)
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(space::SM)
                                .child(
                                    div()
                                        .h(height::COMPACT)
                                        .px(space::MD)
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            apply_scale(div(), scale::BODY_MEDIUM)
                                                .text_color(palette.text_muted)
                                                .child(cjk_inline_label("最近")),
                                        )
                                        .when_some(batch_mode.then_some(()), |hdr, ()| {
                                            hdr.child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(space::SM)
                                                    .child(
                                                        apply_scale(div(), scale::BODY)
                                                            .id("sidebar-batch-select-all-btn")
                                                            .test_support()
                                                            .a11y_button(
                                                                "sidebar-batch-select-all-btn",
                                                                "全选会话",
                                                            )
                                                            .text_color(palette.text_secondary)
                                                            .hover(|s| {
                                                                s.text_color(palette.text_primary)
                                                            })
                                                            .on_click(move |_, w, cx| {
                                                                (on_sel_all)(w, cx)
                                                            })
                                                            .child(cjk_inline_label("全选")),
                                                    )
                                                    .when_some(
                                                        has_checked_items.then_some(()),
                                                        |d, ()| {
                                                            d.child(
                                                                apply_scale(div(), scale::BODY)
                                                                    .id("sidebar-batch-delete-btn")
                                                                    .test_support()
                                                                    .a11y_button(
                                                                        "sidebar-batch-delete-btn",
                                                                        "批量删除所选会话",
                                                                    )
                                                                    .text_color(
                                                                        palette.status_danger,
                                                                    )
                                                                    .on_click(move |_, w, cx| {
                                                                        (on_del)(w, cx)
                                                                    })
                                                                    .child(cjk_inline_label(
                                                                        "删除",
                                                                    )),
                                                            )
                                                        },
                                                    ),
                                            )
                                        }),
                                )
                                .child(recent_list),
                        ),
                ),
        )
        // 3. Bottom account + settings menu trigger bar `fMdf9`
        .child(
            div().pt(space::SM).flex_shrink_0().child(
                div()
                    .h(row_height)
                    .pr(space::MICRO)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::XS)
                    .child(
                        apply_scale(div(), scale::BODY_MEDIUM)
                            .id("sidebar-account-btn")
                            .test_support()
                            .a11y_button("sidebar-account-btn", account_btn_label)
                            .track_focus(account_btn_focus)
                            .tab_index(0)
                            .flex_1()
                            .min_w_0()
                            .h(row_height)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .gap(space::MD)
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(move |_, w, cx| (on_acc)(w, cx))
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .w(height::INLINE)
                                    .h(height::INLINE)
                                    .rounded(radius::FULL)
                                    .bg(palette.bg_item_active)
                                    .text_color(palette.text_primary)
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
                    .child({
                        let (pressed, press_state) =
                            motion::icon_press_state("sidebar-settings-btn", window, cx);
                        let (hovered, hs) =
                            motion::PointerFlag::keyed("hover-sidebar-settings-btn", window, cx);
                        let hover_t = motion::animated_state_progress(
                            "sidebar-settings-btn",
                            hovered,
                            window,
                            cx,
                        );
                        let press_t = motion::animated_press_progress(
                            "sidebar-settings-btn",
                            pressed,
                            window,
                            cx,
                        );
                        let base_bg = if account_menu_open || matches!(route, ShellRoute::Settings)
                        {
                            palette.bg_item_active
                        } else {
                            gpui_kit::transparent_black()
                        };
                        let bg = motion::lerp_hsla(
                            motion::lerp_hsla(base_bg, palette.bg_item_hover, hover_t),
                            palette.bg_active_press,
                            press_t,
                        );
                        div()
                            .id("sidebar-settings-btn")
                            .test_support()
                            .a11y_button("sidebar-settings-btn", "账号与设置菜单")
                            .desktop_tooltip_with_hover(
                                "tooltip-sidebar-settings-btn".into(),
                                "账号与设置菜单".into(),
                                false,
                                Some(hs),
                            )
                            .track_focus(settings_btn_focus)
                            .tab_index(0)
                            .w(height::CONTROL)
                            .h(height::CONTROL)
                            .rounded(radius::CONTROL)
                            .bg(bg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_icon_press(press_state)
                            .on_click(move |_, w, cx| (on_settings)(w, cx))
                            .child(motion::animated_icon_press(
                                "sidebar-settings-btn",
                                pressed,
                                Icon::new(IconName::Settings)
                                    .size(px(18.0))
                                    .text_color(palette.text_secondary),
                                window,
                                cx,
                            ))
                    }),
            ),
        )
}

/// Visual-only non-interactive sidebar snapshot used exclusively during the `150ms` collapse exit
/// so content slides and fades smoothly while all interactive, focus, and a11y nodes unmount on frame 1.
#[allow(clippy::too_many_arguments)]
pub fn render_sidebar_exiting(
    route: ShellRoute,
    active_session_id: Option<&str>,
    sessions: &[SessionSummary],
    search_query: &str,
    batch_mode: bool,
    selected_ids: &BTreeSet<String>,
    account_label: SharedString,
    account_initial: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let recent = filter_recent_sidebar_sessions(sessions, search_query);
    let has_search_query = !search_query.trim().is_empty();
    let row_height = px(36.0);

    let mut recent_list = div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(space::TIGHT);

    if recent.is_empty() {
        recent_list = recent_list.child(
            apply_scale(div(), scale::BODY)
                .px(space::MD)
                .py(space::MD)
                .text_color(palette.text_muted)
                .child(if has_search_query {
                    "无匹配会话"
                } else {
                    "暂无历史对话"
                }),
        );
    } else {
        for sess in &recent {
            let is_active =
                matches!(route, ShellRoute::Session) && active_session_id == Some(sess.id.as_str());
            let is_checked = selected_ids.contains(&sess.id);
            let is_selected = if batch_mode { is_checked } else { is_active };
            let title_text = if sess.title.trim().is_empty() {
                SharedString::from("新对话")
            } else {
                SharedString::from(sess.title.clone())
            };
            let row_bg = if is_selected {
                palette.bg_item_active
            } else {
                gpui_kit::transparent_black()
            };
            let row_scale = if is_selected {
                scale::BODY_MEDIUM
            } else {
                scale::BODY
            };
            let row_text_color = if is_selected {
                palette.text_primary
            } else {
                palette.text_secondary
            };

            let mut row = apply_scale(div(), row_scale)
                .h(row_height)
                .px(space::MD)
                .rounded(radius::CONTROL)
                .bg(row_bg)
                .text_color(row_text_color)
                .flex()
                .items_center()
                .gap(space::MD);
            if batch_mode {
                row = row.child(
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
                        .flex_shrink_0(),
                );
            }
            recent_list = recent_list.child(
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(cjk_inline_label(title_text)),
                ),
            );
        }
    }

    let nav_row = |icon: IconName, label: &'static str, active: bool| {
        apply_scale(
            div(),
            if active {
                scale::BODY_MEDIUM
            } else {
                scale::BODY
            },
        )
        .h(row_height)
        .px(space::MD)
        .rounded(radius::CONTROL)
        .bg(if active {
            palette.bg_item_active
        } else {
            gpui_kit::transparent_black()
        })
        .text_color(palette.text_primary)
        .flex()
        .items_center()
        .gap(space::MD)
        .child(
            Icon::new(icon)
                .size(px(18.0))
                .text_color(palette.text_secondary),
        )
        .child(cjk_inline_label(label))
    };

    div()
        .w(layout::SIDEBAR_WIDTH)
        .h_full()
        .flex_shrink_0()
        .px(space::LG)
        .pb(space::MD)
        .bg(palette.bg_shell)
        .flex()
        .flex_col()
        .justify_between()
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .flex_col()
                .gap(space::SM)
                .child(
                    div()
                        .h(layout::TOPBAR_HEIGHT)
                        .pl(space::SM)
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label("CST Pilot")),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(space::TIGHT)
                                .child(
                                    div()
                                        .w(height::CONTROL)
                                        .h(height::CONTROL)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::new(IconName::PanelLeft)
                                                .size(px(18.0))
                                                .text_color(palette.text_secondary),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(height::CONTROL)
                                        .h(height::CONTROL)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::new(IconName::SquarePen)
                                                .size(px(18.0))
                                                .text_color(palette.text_secondary),
                                        ),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::LG)
                        .child(
                            div()
                                .h(row_height)
                                .px(space::MD)
                                .rounded(radius::CONTROL)
                                .border_1()
                                .border_color(palette.border_hairline)
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .child(
                                    Icon::new(IconName::Search)
                                        .size(px(18.0))
                                        .text_color(palette.text_secondary),
                                )
                                .child(
                                    apply_scale(div(), scale::BODY)
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(if has_search_query {
                                            palette.text_primary
                                        } else {
                                            palette.text_muted
                                        })
                                        .child(if has_search_query {
                                            SharedString::from(search_query.to_owned())
                                        } else {
                                            SharedString::from("搜索")
                                        }),
                                )
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .text_color(palette.text_muted)
                                        .flex_shrink_0()
                                        .child(cjk_inline_label("Ctrl+K")),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .flex_col()
                                .gap(space::TIGHT)
                                .child(nav_row(IconName::LayoutDashboard, "仪表盘", false))
                                .child(nav_row(
                                    IconName::RotateCcwClock,
                                    "全部会话",
                                    !batch_mode && matches!(route, ShellRoute::AllSessions),
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(space::SM)
                                .child(
                                    div()
                                        .h(height::COMPACT)
                                        .px(space::MD)
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .child(
                                            apply_scale(div(), scale::BODY_MEDIUM)
                                                .text_color(palette.text_muted)
                                                .child(cjk_inline_label("最近")),
                                        ),
                                )
                                .child(recent_list),
                        ),
                ),
        )
        .child(
            div().pt(space::SM).flex_shrink_0().child(
                div()
                    .h(row_height)
                    .pr(space::MICRO)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::XS)
                    .child(
                        apply_scale(div(), scale::BODY_MEDIUM)
                            .flex_1()
                            .min_w_0()
                            .h(row_height)
                            .px(space::MD)
                            .flex()
                            .items_center()
                            .gap(space::MD)
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .w(height::INLINE)
                                    .h(height::INLINE)
                                    .rounded(radius::FULL)
                                    .bg(palette.bg_item_active)
                                    .text_color(palette.text_primary)
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
                            .w(height::CONTROL)
                            .h(height::CONTROL)
                            .rounded(radius::CONTROL)
                            .bg(if matches!(route, ShellRoute::Settings) {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::Settings)
                                    .size(px(18.0))
                                    .text_color(palette.text_secondary),
                            ),
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
    apply_scale(
        div(),
        if active {
            scale::BODY_MEDIUM
        } else {
            scale::BODY
        },
    )
    .id(id)
    .test_support()
    .a11y_button(id, label)
    .tab_index(0)
    .h(px(36.0))
    .px(space::MD)
    .rounded(radius::CONTROL)
    .bg(if active {
        palette.bg_item_active
    } else {
        gpui_kit::transparent_black()
    })
    .text_color(palette.text_primary)
    .flex()
    .items_center()
    .gap(space::MD)
    .hover(|s| {
        s.bg(if active {
            palette.bg_item_active
        } else {
            palette.bg_item_hover
        })
    })
    .on_click(move |_, w, cx| on_click(w, cx))
    .child(
        Icon::new(icon)
            .size(px(18.0))
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

use std::rc::Rc;

use cst_client::WorkspaceView;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, WindowControlArea, div, px,
};

use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
};

pub type TopbarActionFn = Rc<dyn Fn(&mut Window, &mut App)>;
pub type TopbarViewFn = Rc<dyn Fn(WorkspaceView, &mut Window, &mut App)>;

pub struct TopbarCallbacks {
    pub on_expand_sidebar: TopbarActionFn,
    pub on_toggle_recent_drawer: TopbarActionFn,
    pub on_new_session: TopbarActionFn,
    pub on_open_dashboard: TopbarActionFn,
    pub on_select_view: TopbarViewFn,
    pub on_open_web: TopbarActionFn,
    pub on_toggle_account_menu: TopbarActionFn,
    pub on_open_settings: TopbarActionFn,
    pub on_minimize_window: TopbarActionFn,
    pub on_zoom_window: TopbarActionFn,
    pub on_request_close: TopbarActionFn,
}

/// Dual-mode `40px` topbar:
/// - Expanded sidebar mode: `cst-pilot-desktop.pen` 「01 · 工作台」 master `GElS8`
/// - Collapsed sidebar mode: `cst-pilot-desktop.pen` 「01 · 工作台」 master `MKlBE` (`1280×40`)
#[allow(clippy::too_many_arguments)]
pub fn render_topbar(
    sidebar_collapsed: bool,
    active_view: WorkspaceView,
    is_maximized: bool,
    update_available: bool,
    account_initial: SharedString,
    expand_btn_focus: &FocusHandle,
    recent_btn_focus: &FocusHandle,
    account_btn_focus: &FocusHandle,
    palette: &DesktopPalette,
    cb: TopbarCallbacks,
) -> impl IntoElement {
    let bar_id = if sidebar_collapsed {
        "topbar-mklbe"
    } else {
        "topbar-gels8"
    };

    let left_cluster = if sidebar_collapsed {
        let on_expand = Rc::clone(&cb.on_expand_sidebar);
        let on_recent = Rc::clone(&cb.on_toggle_recent_drawer);
        let on_new = Rc::clone(&cb.on_new_session);
        let on_dash = Rc::clone(&cb.on_open_dashboard);

        div()
            .pl(space::MD)
            .flex()
            .items_center()
            .gap(space::XS)
            .child(
                div()
                    .id("topbar-expand-sidebar-btn")
                    .test_support()
                    .track_focus(expand_btn_focus)
                    .tab_index(0)
                    .w(height::COMPACT)
                    .h(height::COMPACT)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, w, cx| (on_expand)(w, cx))
                    .child(
                        Icon::new(IconName::PanelLeft)
                            .size(px(16.0))
                            .text_color(palette.text_secondary),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .px(space::XS)
                    .text_color(palette.text_primary)
                    .child("CST Pilot"),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("topbar-recent-drawer-btn")
                    .test_support()
                    .track_focus(recent_btn_focus)
                    .tab_index(0)
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .text_color(palette.text_secondary)
                    .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                    .on_click(move |_, w, cx| (on_recent)(w, cx))
                    .child(cjk_inline_label("最近会话"))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(13.0))
                            .text_color(palette.text_muted),
                    ),
            )
            .child(topbar_icon_btn(
                "topbar-new-session-btn",
                IconName::Plus,
                palette,
                move |w, cx| (on_new)(w, cx),
            ))
            .child(topbar_icon_btn(
                "topbar-dashboard-btn",
                IconName::LayoutDashboard,
                palette,
                move |w, cx| (on_dash)(w, cx),
            ))
    } else {
        div().pl(space::SM)
    };

    let on_view_gui = Rc::clone(&cb.on_select_view);
    let on_view_tui = Rc::clone(&cb.on_select_view);
    let view_segmented = div()
        .id("topbar-view-segmented")
        .test_support()
        .h(height::COMPACT)
        .p(space::MICRO)
        .rounded(radius::ROW)
        .bg(palette.bg_track_recess)
        .flex()
        .items_center()
        .gap(space::MICRO)
        .child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id("topbar-view-gui-btn")
                .test_support()
                .h(height::INLINE)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .bg(if active_view == WorkspaceView::Gui {
                    palette.bg_elevated
                } else {
                    gpui_kit::transparent_black()
                })
                .text_color(if active_view == WorkspaceView::Gui {
                    palette.text_primary
                } else {
                    palette.text_secondary
                })
                .flex()
                .items_center()
                .justify_center()
                .on_click(move |_, w, cx| (on_view_gui)(WorkspaceView::Gui, w, cx))
                .child(cjk_inline_label("图形")),
        )
        .child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id("topbar-view-tui-btn")
                .test_support()
                .h(height::INLINE)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .bg(if active_view == WorkspaceView::Tui {
                    palette.bg_elevated
                } else {
                    gpui_kit::transparent_black()
                })
                .text_color(if active_view == WorkspaceView::Tui {
                    palette.text_primary
                } else {
                    palette.text_secondary
                })
                .flex()
                .items_center()
                .justify_center()
                .on_click(move |_, w, cx| (on_view_tui)(WorkspaceView::Tui, w, cx))
                .child(cjk_inline_label("终端")),
        );

    let on_web = Rc::clone(&cb.on_open_web);
    let web_btn = apply_scale(div(), scale::CAPTION_MEDIUM)
        .id("topbar-open-web-btn")
        .test_support()
        .tab_index(0)
        .h(height::COMPACT)
        .px(space::SM)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .gap(space::XS)
        .text_color(palette.text_secondary)
        .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
        .on_click(move |_, w, cx| (on_web)(w, cx))
        .child(
            Icon::new(IconName::Globe)
                .size(px(14.0))
                .text_color(palette.text_secondary),
        )
        .child(cjk_inline_label("网页"));

    let mut right_cluster = div()
        .flex()
        .items_center()
        .gap(space::XS)
        .child(view_segmented)
        .child(web_btn);

    if update_available {
        let on_settings_upd = Rc::clone(&cb.on_open_settings);
        right_cluster = right_cluster.child(
            div()
                .id("topbar-update-badge-btn")
                .test_support()
                .w(height::COMPACT)
                .h(height::COMPACT)
                .rounded(radius::CONTROL)
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(palette.bg_item_hover))
                .on_click(move |_, w, cx| (on_settings_upd)(w, cx))
                .child(
                    Icon::new(IconName::Bell)
                        .size(px(15.0))
                        .text_color(palette.accent_solid),
                ),
        );
    }

    if sidebar_collapsed {
        let on_acc = Rc::clone(&cb.on_toggle_account_menu);
        let on_set = Rc::clone(&cb.on_open_settings);
        right_cluster = right_cluster
            .child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id("topbar-account-btn")
                    .test_support()
                    .track_focus(account_btn_focus)
                    .tab_index(0)
                    .w(height::COMPACT)
                    .h(height::COMPACT)
                    .rounded(radius::FULL)
                    .bg(palette.accent_subtle)
                    .text_color(palette.accent_solid)
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_click(move |_, w, cx| (on_acc)(w, cx))
                    .child(account_initial),
            )
            .child(topbar_icon_btn(
                "topbar-settings-btn",
                IconName::Settings,
                palette,
                move |w, cx| (on_set)(w, cx),
            ));
    }

    let on_min = Rc::clone(&cb.on_minimize_window);
    let on_zoom = Rc::clone(&cb.on_zoom_window);
    let on_close = Rc::clone(&cb.on_request_close);

    // Native caption buttons (`40×40px` hit targets on right edge)
    let window_caption_controls = div()
        .flex()
        .items_center()
        .h_full()
        .child(
            div()
                .id("window-btn-minimize")
                .test_support()
                .window_control_area(WindowControlArea::Min)
                .w(px(40.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(palette.bg_item_hover))
                .on_click(move |_, w, cx| (on_min)(w, cx))
                .child(
                    Icon::new(IconName::Minus)
                        .size(px(14.0))
                        .text_color(palette.text_secondary),
                ),
        )
        .child(
            div()
                .id("window-btn-maximize")
                .test_support()
                .window_control_area(WindowControlArea::Max)
                .w(px(40.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(palette.bg_item_hover))
                .on_click(move |_, w, cx| (on_zoom)(w, cx))
                .child(
                    Icon::new(if is_maximized {
                        IconName::Minimize2
                    } else {
                        IconName::Maximize2
                    })
                    .size(px(13.0))
                    .text_color(palette.text_secondary),
                ),
        )
        .child(
            div()
                .id("window-btn-close")
                .test_support()
                .w(px(42.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| {
                    s.bg(palette.status_danger)
                        .text_color(palette.text_on_accent)
                })
                .on_click(move |_, w, cx| (on_close)(w, cx))
                .child(
                    Icon::new(IconName::X)
                        .size(px(15.0))
                        .text_color(palette.text_secondary),
                ),
        );

    div()
        .id(bar_id)
        .test_support()
        .w_full()
        .h(layout::TOPBAR_HEIGHT)
        .flex_shrink_0()
        .bg(palette.bg_shell)
        .flex()
        .items_center()
        .child(left_cluster)
        // Middle draggable blank area (`WindowControlArea::Drag`)
        .child(
            div()
                .id("topbar-drag-region")
                .window_control_area(WindowControlArea::Drag)
                .flex_1()
                .h_full(),
        )
        .child(right_cluster)
        .child(window_caption_controls)
}

fn topbar_icon_btn(
    id: &'static str,
    icon: IconName,
    palette: &DesktopPalette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .tab_index(0)
        .w(height::COMPACT)
        .h(height::COMPACT)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(palette.bg_item_hover))
        .on_click(move |_, w, cx| on_click(w, cx))
        .child(
            Icon::new(icon)
                .size(px(15.0))
                .text_color(palette.text_secondary),
        )
}

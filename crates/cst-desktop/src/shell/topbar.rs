use std::rc::Rc;

use cst_client::WorkspaceView;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, WindowControlArea,
    div, px,
};

use crate::accessibility::A11yElementExt as _;
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
            .pl(space::GROUP)
            .flex()
            .items_center()
            .gap(space::XS)
            .child(
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label("CST Pilot")),
            )
            .child(
                div()
                    .id("topbar-expand-sidebar-btn")
                    .test_support()
                    .a11y_button("topbar-expand-sidebar-btn", "展开侧栏")
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
            .child(div().w(px(1.0)).h(px(14.0)).bg(palette.border_strong))
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("topbar-recent-drawer-btn")
                    .test_support()
                    .a11y_button("topbar-recent-drawer-btn", "最近会话抽屉")
                    .track_focus(recent_btn_focus)
                    .tab_index(0)
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .gap(space::TIGHT)
                    .text_color(palette.text_primary)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, w, cx| (on_recent)(w, cx))
                    .child(
                        Icon::new(IconName::RotateCcwClock)
                            .size(px(14.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label("最近会话"))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(12.0))
                            .text_color(palette.text_secondary),
                    ),
            )
            .child(topbar_icon_btn(
                "topbar-new-session-btn",
                "新建对话",
                IconName::SquarePen,
                palette.text_primary,
                palette,
                move |w, cx| (on_new)(w, cx),
            ))
            .child(topbar_icon_btn(
                "topbar-dashboard-btn",
                "仪表盘",
                IconName::LayoutDashboard,
                palette.text_secondary,
                palette,
                move |w, cx| (on_dash)(w, cx),
            ))
    } else {
        div().pl(space::MD)
    };

    let on_view_gui = Rc::clone(&cb.on_select_view);
    let on_view_tui = Rc::clone(&cb.on_select_view);
    let gui_active = active_view == WorkspaceView::Gui;
    let tui_active = active_view == WorkspaceView::Tui;

    let view_segmented = div()
        .id("topbar-view-segmented")
        .test_support()
        .a11y_region("topbar-view-segmented", "工作区视图切换")
        .h(height::COMPACT)
        .p(space::MICRO)
        .rounded(px(6.0))
        .bg(palette.bg_surface)
        .flex()
        .items_center()
        .gap(space::MICRO)
        .child(view_segment_item(
            "topbar-view-gui-btn",
            IconName::Monitor,
            "图形",
            gui_active,
            palette,
            move |w, cx| (on_view_gui)(WorkspaceView::Gui, w, cx),
        ))
        .child(view_segment_item(
            "topbar-view-tui-btn",
            IconName::SquareTerminal,
            "终端",
            tui_active,
            palette,
            move |w, cx| (on_view_tui)(WorkspaceView::Tui, w, cx),
        ));

    let on_web = Rc::clone(&cb.on_open_web);
    let web_btn = apply_scale(div(), scale::UI)
        .id("topbar-open-web-btn")
        .test_support()
        .a11y_button("topbar-open-web-btn", "打开网页端")
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
        .child(Icon::new(IconName::Globe).size(px(14.0)))
        .child(cjk_inline_label("网页"))
        .child(
            Icon::new(IconName::ArrowUpRight)
                .size(px(12.0))
                .text_color(palette.text_muted),
        );

    let right_gap = if sidebar_collapsed {
        space::XS
    } else {
        space::TIGHT
    };

    let mut right_cluster = div()
        .h_full()
        .flex()
        .items_center()
        .gap(right_gap)
        .child(view_segmented)
        .child(web_btn);

    if sidebar_collapsed {
        let on_acc = Rc::clone(&cb.on_toggle_account_menu);
        let on_set = Rc::clone(&cb.on_open_settings);
        right_cluster = right_cluster
            .child(
                div()
                    .id("topbar-account-btn")
                    .test_support()
                    .a11y_button("topbar-account-btn", "账号菜单")
                    .track_focus(account_btn_focus)
                    .tab_index(0)
                    .w(height::COMPACT)
                    .h(height::COMPACT)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, w, cx| (on_acc)(w, cx))
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .w(px(20.0))
                            .h(px(20.0))
                            .rounded(radius::FULL)
                            .bg(palette.bg_item_active)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(account_initial),
                    ),
            )
            .child(topbar_icon_btn(
                "topbar-settings-btn",
                "打开设置",
                IconName::Settings,
                palette.text_secondary,
                palette,
                move |w, cx| (on_set)(w, cx),
            ));
    }

    right_cluster = right_cluster
        .child(div().w(px(1.0)).h(px(14.0)).bg(palette.border_strong))
        .child(render_window_caption_controls(
            is_maximized,
            palette,
            Rc::clone(&cb.on_minimize_window),
            Rc::clone(&cb.on_zoom_window),
            Rc::clone(&cb.on_request_close),
        ));

    div()
        .id(bar_id)
        .test_support()
        .a11y_region(bar_id, "顶部工具栏")
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
}

/// Native caption buttons (`120×40px` master `T1U1Xf`, three `40×40px` hit targets on right edge).
pub fn render_window_caption_controls(
    is_maximized: bool,
    palette: &DesktopPalette,
    on_min: TopbarActionFn,
    on_zoom: TopbarActionFn,
    on_close: TopbarActionFn,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .h_full()
        .child(
            div()
                .id("window-btn-minimize")
                .test_support()
                .a11y_button("window-btn-minimize", "最小化窗口")
                .window_control_area(WindowControlArea::Min)
                .w(px(40.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(palette.text_secondary)
                .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                .on_click(move |_, w, cx| (on_min)(w, cx))
                .child(Icon::new(IconName::Minus).size(px(14.0))),
        )
        .child(
            div()
                .id("window-btn-maximize")
                .test_support()
                .a11y_button(
                    "window-btn-maximize",
                    if is_maximized {
                        "还原窗口"
                    } else {
                        "最大化窗口"
                    },
                )
                .window_control_area(WindowControlArea::Max)
                .w(px(40.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(palette.text_secondary)
                .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                .on_click(move |_, w, cx| (on_zoom)(w, cx))
                .child(
                    Icon::new(if is_maximized {
                        IconName::Copy
                    } else {
                        IconName::Square
                    })
                    .size(px(12.0)),
                ),
        )
        .child(
            div()
                .id("window-btn-close")
                .test_support()
                .a11y_button("window-btn-close", "关闭窗口")
                .window_control_area(WindowControlArea::Close)
                .w(px(40.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(palette.text_secondary)
                .hover(|s| {
                    s.bg(palette.status_danger)
                        .text_color(palette.text_on_accent)
                })
                .on_click(move |_, w, cx| (on_close)(w, cx))
                .child(Icon::new(IconName::X).size(px(14.0))),
        )
}

fn view_segment_item(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    palette: &DesktopPalette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let base = apply_scale(div(), if active { scale::UI_MEDIUM } else { scale::UI })
        .id(id)
        .test_support()
        .a11y_tab(id, label, active)
        .h(height::INLINE)
        .px(space::SM)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_center()
        .gap(space::XS)
        .on_click(move |_, w, cx| on_click(w, cx));

    let styled = if active {
        base.bg(palette.bg_elevated)
            .border_1()
            .border_color(palette.border_subtle)
            .text_color(palette.text_primary)
    } else {
        base.bg(gpui_kit::transparent_black())
            .text_color(palette.text_secondary)
            .hover(|s| s.text_color(palette.text_primary))
    };

    styled
        .child(Icon::new(icon).size(px(14.0)))
        .child(cjk_inline_label(label))
}

fn topbar_icon_btn(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    icon_color: Hsla,
    palette: &DesktopPalette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .a11y_button(id, label)
        .tab_index(0)
        .w(height::COMPACT)
        .h(height::COMPACT)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(palette.bg_item_hover))
        .on_click(move |_, w, cx| on_click(w, cx))
        .child(Icon::new(icon).size(px(16.0)).text_color(icon_color))
}

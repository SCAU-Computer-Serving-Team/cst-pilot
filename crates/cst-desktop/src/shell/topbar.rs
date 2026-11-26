use std::cell::Cell;
use std::rc::Rc;

use cst_client::WorkspaceView;
use gpui_kit::assets::IconName;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, WindowControlArea,
    div, linear_color_stop, linear_gradient, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::motion::{DesktopTooltipExt as _, IconPressExt as _};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, motion, radius, scale, space,
};

pub type TopbarActionFn = Rc<dyn Fn(&mut Window, &mut App)>;
pub type TopbarViewFn = Rc<dyn Fn(WorkspaceView, &mut Window, &mut App)>;

pub struct TopbarCallbacks {
    pub on_expand_sidebar: TopbarActionFn,
    pub on_toggle_recent_drawer: TopbarActionFn,
    pub on_toggle_session_more: TopbarActionFn,
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

/// Maps an already-eased `[0.0, 1.0]` progress into a staggered sub-window `[start, end]`.
fn stagger_progress(t: f32, start: f32, end: f32) -> f32 {
    if end <= start {
        return t.clamp(0.0, 1.0);
    }
    ((t - start) / (end - start)).clamp(0.0, 1.0)
}

/// Exact horizontal span (`28px` avatar + `4px` gap + `28px` settings) occupied by the
/// collapsed-sidebar right compensation buttons inside their sliding slot.
const RIGHT_COMPENSATION_WIDTH_PX: f32 = 60.0;

/// Truncates a collapsed-topbar session title to at most 5 visible (non-whitespace) characters.
/// Returns `(display_text, truncated)` so callers render the vertical right-half feather mask
/// over the 5th character only when the title exceeds 5 visible characters.
pub(crate) fn prepare_collapsed_session_title(raw: &str) -> (String, bool) {
    let cleaned = raw.replace(['\r', '\n'], " ");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return ("新对话".to_owned(), false);
    }
    let mut out = String::new();
    let mut visible_count = 0usize;
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            // Preserve a single separating space between visible characters without counting it as a character.
            if visible_count > 0 && visible_count < 5 && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        if visible_count >= 5 {
            return (out.trim_end().to_owned(), true);
        }
        out.push(ch);
        visible_count += 1;
        if visible_count == 5 {
            let has_more_visible = chars.any(|c| !c.is_whitespace());
            return (out, has_more_visible);
        }
    }
    (out.trim_end().to_owned(), false)
}

/// Estimates inline pixel width for `scale::UI_MEDIUM` (`14px`) text as a zero-flicker initial seed
/// before `on_children_prepainted` reports the exact shaped glyph width.
fn estimate_ui_medium_width_px(text: &str) -> f32 {
    let mut width = 0.0_f32;
    for ch in text.chars() {
        width += if ch.is_ascii_whitespace() {
            4.5
        } else if ch.is_ascii() {
            8.2
        } else {
            14.2
        };
    }
    width.max(14.0)
}

/// Estimates the right-half feather mask width for the 5th character (`scale::UI_MEDIUM`, `14px`).
fn fifth_char_feather_width_px(display_text: &str) -> f32 {
    match display_text.chars().last() {
        Some(ch) if ch.is_ascii_whitespace() => 6.0,
        Some(ch) if ch.is_ascii() => 6.5,
        _ => 9.0,
    }
}

/// Alpha-composites `fg` over opaque `bg` so feather gradient stops always match the live button fill
/// across default, hover, active-open, and press states in both light and dark themes.
fn composite_over(bg: Hsla, fg: Hsla) -> Hsla {
    if fg.a <= 0.001 {
        return bg;
    }
    if fg.a >= 0.999 {
        return fg;
    }
    let mut out = motion::lerp_hsla(bg, Hsla { a: 1.0, ..fg }, fg.a);
    out.a = 1.0;
    out
}

/// Dual-mode `40px` topbar:
/// - Expanded sidebar mode: `cst-pilot-desktop.pen` 「01 · 工作台」 master `GElS8`
/// - Collapsed sidebar mode: `cst-pilot-desktop.pen` 「01 · 工作台」 master `MKlBE` (`1280×40`)
#[allow(clippy::too_many_arguments)]
pub fn render_topbar(
    sidebar_collapsed: bool,
    collapsed_progress: f32,
    recent_drawer_open: bool,
    show_session_more: bool,
    session_more_open: bool,
    collapsed_session_title: Option<&str>,
    recent_btn_anchor_x: Rc<Cell<f32>>,
    session_more_anchor_x: Rc<Cell<f32>>,
    active_view: WorkspaceView,
    is_maximized: bool,
    account_initial: SharedString,
    expand_btn_focus: &FocusHandle,
    recent_btn_focus: &FocusHandle,
    session_more_btn_focus: &FocusHandle,
    account_btn_focus: &FocusHandle,
    settings_btn_focus: &FocusHandle,
    palette: &DesktopPalette,
    cb: TopbarCallbacks,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let bar_id = if sidebar_collapsed {
        "topbar-mklbe"
    } else {
        "topbar-gels8"
    };
    let collapsed_t = collapsed_progress.clamp(0.0, 1.0);
    let (recent_display_title, recent_title_truncated) = match collapsed_session_title {
        Some(raw) => prepare_collapsed_session_title(raw),
        None => ("最近会话".to_owned(), false),
    };

    let left_cluster = if sidebar_collapsed {
        let on_expand = Rc::clone(&cb.on_expand_sidebar);
        let on_recent = Rc::clone(&cb.on_toggle_recent_drawer);
        let on_more = Rc::clone(&cb.on_toggle_session_more);
        let on_new = Rc::clone(&cb.on_new_session);
        let on_dash = Rc::clone(&cb.on_open_dashboard);

        let p_brand = stagger_progress(collapsed_t, 0.0, 0.70);
        let p_div = stagger_progress(collapsed_t, 0.06, 0.74);
        let p_recent = stagger_progress(collapsed_t, 0.00, 0.72);
        let p_more = stagger_progress(collapsed_t, 0.08, 0.80);
        let p_new = stagger_progress(collapsed_t, 0.14, 0.86);
        let p_dash = stagger_progress(collapsed_t, 0.28, 1.00);

        #[derive(Clone, Default)]
        struct TitleMeasure {
            text_and_width: Rc<std::cell::RefCell<(String, f32)>>,
        }
        let measure_key = gpui_kit::ElementId::NamedChild(
            std::sync::Arc::new(gpui_kit::ElementId::View(window.current_view())),
            "topbar-recent-title-measure".into(),
        );
        let title_measure = window.use_keyed_state(measure_key, cx, |_, _| TitleMeasure::default());
        let measure_cell = title_measure.read(cx).text_and_width.clone();
        let target_title_w = {
            let borrowed = measure_cell.borrow();
            if borrowed.0 == recent_display_title && borrowed.1 > 0.0 {
                borrowed.1
            } else {
                estimate_ui_medium_width_px(&recent_display_title)
            }
        };
        let anim_title_w = transition(
            "topbar-recent-title-w",
            target_title_w,
            Transition::new(motion::RESIZE).ease(motion::ease_out()),
            window,
            cx,
        );
        let measure_capture = measure_cell;
        let measured_label = recent_display_title.clone();
        let anchor_x_capture = recent_btn_anchor_x;
        let more_anchor_capture = session_more_anchor_x;

        div()
            .pl(space::GROUP)
            .h_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(space::XS)
            .child(
                div()
                    .relative()
                    .left(px(-16.0 * (1.0 - p_brand)))
                    .opacity(0.01 + 0.99 * p_brand)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("CST Pilot")),
                    )
                    .child({
                        let (pressed, press_state) =
                            motion::icon_press_state("topbar-expand-sidebar-btn", window, cx);
                        let (hovered, hs) = motion::PointerFlag::keyed(
                            "hover-topbar-expand-sidebar-btn",
                            window,
                            cx,
                        );
                        let hover_t = motion::animated_state_progress(
                            "topbar-expand-sidebar-btn",
                            hovered,
                            window,
                            cx,
                        );
                        let press_t = motion::animated_press_progress(
                            "topbar-expand-sidebar-btn",
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
                        let fg = motion::lerp_hsla(
                            palette.text_secondary,
                            palette.text_primary,
                            hover_t,
                        );
                        div()
                            .id("topbar-expand-sidebar-btn")
                            .test_support()
                            .a11y_button("topbar-expand-sidebar-btn", "展开侧栏")
                            .desktop_tooltip_with_hover(
                                "tooltip-topbar-expand-sidebar-btn".into(),
                                "展开侧栏".into(),
                                false,
                                Some(hs),
                            )
                            .track_focus(expand_btn_focus)
                            .tab_index(0)
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .rounded(radius::CONTROL)
                            .bg(bg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_icon_press(press_state)
                            .on_click(move |_, w, cx| (on_expand)(w, cx))
                            .child(motion::animated_icon_press(
                                "topbar-expand-sidebar-btn",
                                pressed,
                                Icon::new(IconName::PanelLeft).size(px(16.0)).text_color(fg),
                                window,
                                cx,
                            ))
                    }),
            )
            .child(
                div()
                    .relative()
                    .left(px(-16.0 * (1.0 - p_div)))
                    .mx(space::MICRO)
                    .w(px(1.0))
                    .h(px(14.0))
                    .bg(palette.border_strong)
                    .opacity(0.01 + 0.99 * p_div),
            )
            .child({
                let mut nav_group = div()
                    .on_children_prepainted(move |bounds, _, _| {
                        if let Some(first) = bounds.first() {
                            anchor_x_capture.set(f32::from(first.origin.x));
                        }
                        if show_session_more && let Some(second) = bounds.get(1) {
                            more_anchor_capture.set(f32::from(second.origin.x));
                        }
                    })
                    .overflow_hidden()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child({
                        let (pressed, press_state) =
                            motion::icon_press_state("topbar-recent-drawer-btn", window, cx);
                        let (hovered, hs) = motion::PointerFlag::keyed(
                            "hover-topbar-recent-drawer-btn",
                            window,
                            cx,
                        );
                        let hover_t = motion::animated_state_progress(
                            "topbar-recent-drawer-btn",
                            hovered,
                            window,
                            cx,
                        );
                        let press_t = motion::animated_press_progress(
                            "topbar-recent-drawer-btn",
                            pressed,
                            window,
                            cx,
                        );
                        let open_t = motion::animated_state_progress(
                            "topbar-recent-drawer-open",
                            recent_drawer_open,
                            window,
                            cx,
                        );
                        let idle_or_hover = motion::lerp_hsla(
                            gpui_kit::transparent_black(),
                            palette.bg_item_hover,
                            hover_t,
                        );
                        let resting_bg =
                            motion::lerp_hsla(idle_or_hover, palette.bg_item_active, open_t);
                        let bg = if recent_drawer_open {
                            resting_bg
                        } else {
                            motion::lerp_hsla(resting_bg, palette.bg_active_press, press_t)
                        };
                        let icon_fg = motion::lerp_hsla(
                            palette.text_secondary,
                            palette.text_primary,
                            hover_t,
                        );
                        let surface_bg = composite_over(palette.bg_shell, bg);
                        let feather_w = fifth_char_feather_width_px(&recent_display_title);
                        let feather_overlay = recent_title_truncated.then(|| {
                            let mut transparent_stop = surface_bg;
                            transparent_stop.a = 0.0;
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right_0()
                                .w(px(feather_w))
                                .bg(linear_gradient(
                                    90.0,
                                    linear_color_stop(transparent_stop, 0.0),
                                    linear_color_stop(surface_bg, 0.92),
                                ))
                        });
                        let title_slot = div()
                            .relative()
                            .w(px(anim_title_w))
                            .h(px(22.0))
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .on_children_prepainted(move |bounds, window, _| {
                                        if let Some(child_bounds) = bounds.first() {
                                            let w = f32::from(child_bounds.size.width).ceil();
                                            let mut cell = measure_capture.borrow_mut();
                                            if cell.0 != measured_label || (cell.1 - w).abs() > 0.5
                                            {
                                                cell.0.clone_from(&measured_label);
                                                cell.1 = w;
                                                window.request_animation_frame();
                                            }
                                        }
                                    })
                                    .flex()
                                    .items_center()
                                    .flex_shrink_0()
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .whitespace_nowrap()
                                            .child(cjk_inline_label(recent_display_title.clone())),
                                    ),
                            )
                            .children(feather_overlay);

                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("topbar-recent-drawer-btn")
                            .test_support()
                            .a11y_button("topbar-recent-drawer-btn", "最近会话抽屉")
                            .track_focus(recent_btn_focus)
                            .tab_index(0)
                            .relative()
                            .left(px(-16.0 * (1.0 - p_recent)))
                            .opacity(0.01 + 0.99 * p_recent)
                            .h(height::COMPACT)
                            .pl(space::SM)
                            .pr(space::TIGHT)
                            .rounded(radius::CONTROL)
                            .bg(bg)
                            .flex()
                            .items_center()
                            .gap(space::TIGHT)
                            .text_color(palette.text_primary)
                            .on_hover(move |active, w, cx| {
                                if *active {
                                    motion::PointerFlag::enter(&hs, w, cx);
                                } else {
                                    motion::PointerFlag::set(&hs, false, cx);
                                }
                            })
                            .on_icon_press(press_state)
                            .on_click(move |_, w, cx| (on_recent)(w, cx))
                            .child(
                                Icon::new(IconName::MessageSquare)
                                    .size(px(16.0))
                                    .text_color(icon_fg),
                            )
                            .child(title_slot)
                            .child(motion::animated_chevron(
                                "topbar-recent-drawer",
                                recent_drawer_open,
                                true,
                                px(12.0),
                                icon_fg,
                                window,
                                cx,
                            ))
                    });

                if show_session_more {
                    let (more_pressed, more_press_state) =
                        motion::icon_press_state("topbar-session-more-btn", window, cx);
                    let (more_hovered, more_hs) =
                        motion::PointerFlag::keyed("hover-topbar-session-more-btn", window, cx);
                    let more_hover_t = motion::animated_state_progress(
                        "topbar-session-more-btn",
                        more_hovered,
                        window,
                        cx,
                    );
                    let more_press_t = motion::animated_press_progress(
                        "topbar-session-more-btn",
                        more_pressed,
                        window,
                        cx,
                    );
                    let more_bg = motion::lerp_hsla(
                        motion::lerp_hsla(
                            if session_more_open {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            },
                            palette.bg_item_hover,
                            more_hover_t,
                        ),
                        palette.bg_active_press,
                        more_press_t,
                    );
                    let more_fg = motion::lerp_hsla(
                        palette.text_secondary,
                        palette.text_primary,
                        more_hover_t.max(if session_more_open { 1.0 } else { 0.0 }),
                    );
                    nav_group = nav_group.child(
                        div()
                            .relative()
                            .left(px(-16.0 * (1.0 - p_more)))
                            .opacity(0.01 + 0.99 * p_more)
                            .child(
                                div()
                                    .id("topbar-session-more-btn")
                                    .test_support()
                                    .a11y_toggle_button(
                                        "topbar-session-more-btn",
                                        "会话更多操作",
                                        session_more_open,
                                    )
                                    .desktop_tooltip_with_hover(
                                        "tooltip-topbar-session-more-btn".into(),
                                        "会话更多操作".into(),
                                        false,
                                        Some(more_hs),
                                    )
                                    .track_focus(session_more_btn_focus)
                                    .tab_index(0)
                                    .w(height::COMPACT)
                                    .h(height::COMPACT)
                                    .rounded(radius::CONTROL)
                                    .bg(more_bg)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .on_icon_press(more_press_state)
                                    .on_click(move |_, w, cx| (on_more)(w, cx))
                                    .child(motion::animated_icon_press(
                                        "topbar-session-more-btn",
                                        more_pressed,
                                        Icon::new(IconName::EllipsisVertical)
                                            .size(px(16.0))
                                            .text_color(more_fg),
                                        window,
                                        cx,
                                    )),
                            ),
                    );
                }

                nav_group
                    .child(
                        div()
                            .relative()
                            .left(px(-16.0 * (1.0 - p_new)))
                            .opacity(0.01 + 0.99 * p_new)
                            .child(topbar_icon_btn(
                                "topbar-new-session-btn",
                                "新建对话",
                                IconName::SquarePen,
                                palette.text_secondary,
                                palette,
                                window,
                                cx,
                                move |w, cx| (on_new)(w, cx),
                            )),
                    )
                    .child(
                        div()
                            .relative()
                            .left(px(-16.0 * (1.0 - p_dash)))
                            .opacity(0.01 + 0.99 * p_dash)
                            .child(topbar_icon_btn(
                                "topbar-dashboard-btn",
                                "仪表盘",
                                IconName::LayoutDashboard,
                                palette.text_secondary,
                                palette,
                                window,
                                cx,
                                move |w, cx| (on_dash)(w, cx),
                            )),
                    )
            })
    } else if collapsed_t > 0.01 {
        // Non-interactive rightward exit mirror while sidebar expands from the left and squeezes icons out.
        let exit_dx = px(18.0 * (1.0 - collapsed_t));
        let exit_opacity = 0.01 + 0.99 * collapsed_t;
        let mut exit_row = div()
            .relative()
            .left(exit_dx)
            .opacity(exit_opacity)
            .flex()
            .items_center()
            .gap(space::XS)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("CST Pilot")),
                    )
                    .child(
                        div()
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::PanelLeft)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
            .child(
                div()
                    .mx(space::MICRO)
                    .w(px(1.0))
                    .h(px(14.0))
                    .bg(palette.border_strong),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .h(height::COMPACT)
                    .pl(space::SM)
                    .pr(space::TIGHT)
                    .flex()
                    .items_center()
                    .gap(space::TIGHT)
                    .text_color(palette.text_primary)
                    .child(
                        Icon::new(IconName::MessageSquare)
                            .size(px(16.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label(recent_display_title))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(12.0))
                            .text_color(palette.text_secondary),
                    ),
            );
        if show_session_more {
            exit_row = exit_row.child(
                div()
                    .w(height::COMPACT)
                    .h(height::COMPACT)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::EllipsisVertical)
                            .size(px(16.0))
                            .text_color(palette.text_secondary),
                    ),
            );
        }
        div()
            .pl(space::GROUP)
            .h_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .child(
                exit_row
                    .child(
                        div()
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::SquarePen)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            ),
                    )
                    .child(
                        div()
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::LayoutDashboard)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
    } else {
        div().pl(space::MD)
    };

    let on_view_gui = Rc::clone(&cb.on_select_view);
    let on_view_tui = Rc::clone(&cb.on_select_view);
    let gui_active = active_view == WorkspaceView::Gui;
    let tui_active = active_view == WorkspaceView::Tui;

    #[derive(Clone, Default)]
    struct SegBounds {
        rects: Rc<[Cell<(f32, f32)>; 2]>,
    }
    let seg_key = gpui_kit::ElementId::NamedChild(
        std::sync::Arc::new(gpui_kit::ElementId::View(window.current_view())),
        "topbar-seg-bounds".into(),
    );
    let seg_bounds = window.use_keyed_state(seg_key, cx, |_, _| SegBounds::default());
    let seg_rects = seg_bounds.read(cx).rects.clone();
    let gui_rect = seg_rects[0].get();
    let tui_rect = seg_rects[1].get();
    let measured = gui_rect.1 > 0.0 && tui_rect.1 > 0.0;
    let target_rect = if tui_active { tui_rect } else { gui_rect };
    let seg_rects_capture = seg_rects;

    let mut view_segmented = div()
        .on_children_prepainted(move |bounds, window, _| {
            // GPUI reports direct children in insertion order, including the
            // absolute pill slot. Measure only the two buttons.
            let [_, gui, tui] = bounds.as_slice() else {
                return;
            };
            let x0 = f32::from(space::MICRO);
            let x1 = x0 + f32::from(tui.origin.x - gui.origin.x);
            let gui = (x0, f32::from(gui.size.width));
            let tui = (x1, f32::from(tui.size.width));
            let old_gui = seg_rects_capture[0].replace(gui);
            let old_tui = seg_rects_capture[1].replace(tui);
            if old_gui != gui || old_tui != tui {
                window.request_animation_frame();
            }
        })
        .id("topbar-view-segmented")
        .test_support()
        .a11y_region("topbar-view-segmented", "工作区视图切换")
        .relative()
        .h(height::COMPACT)
        .p(space::MICRO)
        .rounded(px(6.0))
        .bg(palette.bg_surface)
        .flex()
        .items_center()
        .gap(space::MICRO);

    if measured {
        let pill_x = transition(
            "topbar-seg-pill-x",
            target_rect.0,
            Transition::new(motion::NORMAL).ease(motion::ease_in_out()),
            window,
            cx,
        );
        let pill_w = transition(
            "topbar-seg-pill-w",
            target_rect.1,
            Transition::new(motion::NORMAL).ease(motion::ease_in_out()),
            window,
            cx,
        );
        view_segmented = view_segmented.child(
            div()
                .absolute()
                .top(space::MICRO)
                .bottom(space::MICRO)
                .left(px(pill_x))
                .w(px(pill_w))
                .rounded(radius::CONTROL)
                .bg(palette.bg_elevated)
                .border_1()
                .border_color(palette.border_subtle),
        );
    } else {
        view_segmented = view_segmented.child(div().absolute());
    }

    view_segmented = view_segmented
        .child(view_segment_item(
            "topbar-view-gui-btn",
            IconName::Monitor,
            "图形",
            gui_active,
            !measured && gui_active,
            palette,
            move |w, cx| (on_view_gui)(WorkspaceView::Gui, w, cx),
        ))
        .child(view_segment_item(
            "topbar-view-tui-btn",
            IconName::SquareTerminal,
            "终端",
            tui_active,
            !measured && tui_active,
            palette,
            move |w, cx| (on_view_tui)(WorkspaceView::Tui, w, cx),
        ));

    let on_web = Rc::clone(&cb.on_open_web);
    let (web_pressed, web_press_state) =
        motion::icon_press_state("topbar-open-web-btn", window, cx);
    let (web_hovered, web_hs) = motion::PointerFlag::keyed("hover-topbar-open-web-btn", window, cx);
    let web_hover_t =
        motion::animated_state_progress("topbar-open-web-btn", web_hovered, window, cx);
    let web_press_t =
        motion::animated_press_progress("topbar-open-web-btn", web_pressed, window, cx);
    let web_bg = motion::lerp_hsla(
        motion::lerp_hsla(
            gpui_kit::transparent_black(),
            palette.bg_item_hover,
            web_hover_t,
        ),
        palette.bg_active_press,
        web_press_t,
    );
    let web_fg = motion::lerp_hsla(palette.text_secondary, palette.text_primary, web_hover_t);
    let web_btn = apply_scale(div(), scale::UI)
        .id("topbar-open-web-btn")
        .test_support()
        .a11y_button("topbar-open-web-btn", "打开网页端")
        .tab_index(0)
        .h(height::COMPACT)
        .px(space::SM)
        .rounded(radius::CONTROL)
        .bg(web_bg)
        .flex()
        .items_center()
        .gap(space::XS)
        .text_color(web_fg)
        .on_hover(move |active, w, cx| {
            if *active {
                motion::PointerFlag::enter(&web_hs, w, cx);
            } else {
                motion::PointerFlag::set(&web_hs, false, cx);
            }
        })
        .on_icon_press(web_press_state)
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

    let slot_t = collapsed_t.clamp(0.0, 1.0);
    let slot_w = px(RIGHT_COMPENSATION_WIDTH_PX * slot_t);
    let slot_gap = px(f32::from(space::XS) * slot_t);

    let mut controls_group = div().h_full().flex().items_center().child(
        div()
            .flex()
            .items_center()
            .gap(right_gap)
            .child(view_segmented)
            .child(web_btn),
    );

    if sidebar_collapsed {
        let on_acc = Rc::clone(&cb.on_toggle_account_menu);
        let on_set = Rc::clone(&cb.on_open_settings);
        let (acc_pressed, acc_press_state) =
            motion::icon_press_state("topbar-account-btn", window, cx);
        let (acc_hovered, acc_hs) =
            motion::PointerFlag::keyed("hover-topbar-account-btn", window, cx);
        let acc_hover_t =
            motion::animated_state_progress("topbar-account-btn", acc_hovered, window, cx);
        let acc_press_t =
            motion::animated_press_progress("topbar-account-btn", acc_pressed, window, cx);
        let acc_bg = motion::lerp_hsla(
            motion::lerp_hsla(
                gpui_kit::transparent_black(),
                palette.bg_item_hover,
                acc_hover_t,
            ),
            palette.bg_active_press,
            acc_press_t,
        );
        let p_acc = stagger_progress(collapsed_t, 0.04, 0.84);
        let p_set = stagger_progress(collapsed_t, 0.18, 1.00);
        controls_group = controls_group.child(
            div()
                .ml(slot_gap)
                .w(slot_w)
                .h_full()
                .overflow_hidden()
                .flex()
                .items_center()
                .flex_shrink_0()
                .child(
                    div()
                        .w(px(RIGHT_COMPENSATION_WIDTH_PX))
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .flex_shrink_0()
                        .child(
                            div()
                                .id("topbar-account-btn")
                                .test_support()
                                .a11y_button("topbar-account-btn", "账号菜单")
                                .desktop_tooltip_with_hover(
                                    "tooltip-topbar-account-btn".into(),
                                    "账号菜单".into(),
                                    false,
                                    Some(acc_hs),
                                )
                                .track_focus(account_btn_focus)
                                .tab_index(0)
                                .relative()
                                .left(px(-16.0 * (1.0 - p_acc)))
                                .opacity(0.01 + 0.99 * p_acc)
                                .w(height::COMPACT)
                                .h(height::COMPACT)
                                .rounded(radius::CONTROL)
                                .bg(acc_bg)
                                .flex()
                                .items_center()
                                .justify_center()
                                .on_icon_press(acc_press_state)
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
                        .child({
                            let (set_pressed, set_press_state) =
                                motion::icon_press_state("topbar-settings-btn", window, cx);
                            let (set_hovered, set_hs) =
                                motion::PointerFlag::keyed("hover-topbar-settings-btn", window, cx);
                            let set_hover_t = motion::animated_state_progress(
                                "topbar-settings-btn",
                                set_hovered,
                                window,
                                cx,
                            );
                            let set_press_t = motion::animated_press_progress(
                                "topbar-settings-btn",
                                set_pressed,
                                window,
                                cx,
                            );
                            let set_bg = motion::lerp_hsla(
                                motion::lerp_hsla(
                                    gpui_kit::transparent_black(),
                                    palette.bg_item_hover,
                                    set_hover_t,
                                ),
                                palette.bg_active_press,
                                set_press_t,
                            );
                            div()
                                .id("topbar-settings-btn")
                                .test_support()
                                .a11y_button("topbar-settings-btn", "账号与设置菜单")
                                .desktop_tooltip_with_hover(
                                    "tooltip-topbar-settings-btn".into(),
                                    "账号与设置菜单".into(),
                                    false,
                                    Some(set_hs),
                                )
                                .track_focus(settings_btn_focus)
                                .tab_index(0)
                                .relative()
                                .left(px(-16.0 * (1.0 - p_set)))
                                .opacity(0.01 + 0.99 * p_set)
                                .w(height::COMPACT)
                                .h(height::COMPACT)
                                .rounded(radius::CONTROL)
                                .bg(set_bg)
                                .flex()
                                .items_center()
                                .justify_center()
                                .on_icon_press(set_press_state)
                                .on_click(move |_, w, cx| (on_set)(w, cx))
                                .child(motion::animated_icon_press(
                                    "topbar-settings-btn",
                                    set_pressed,
                                    Icon::new(IconName::Settings)
                                        .size(px(16.0))
                                        .text_color(palette.text_secondary),
                                    window,
                                    cx,
                                ))
                        }),
                ),
        );
    } else if collapsed_t > 0.01 {
        // Inert rightward exit while slot squeezes closed and shared controls glide back right.
        let exit_dx = px(16.0 * (1.0 - collapsed_t));
        let exit_op = 0.01 + 0.99 * collapsed_t;
        controls_group = controls_group.child(
            div()
                .ml(slot_gap)
                .w(slot_w)
                .h_full()
                .overflow_hidden()
                .flex()
                .items_center()
                .flex_shrink_0()
                .child(
                    div()
                        .w(px(RIGHT_COMPENSATION_WIDTH_PX))
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .flex_shrink_0()
                        .child(
                            div()
                                .relative()
                                .left(exit_dx)
                                .opacity(exit_op)
                                .w(height::COMPACT)
                                .h(height::COMPACT)
                                .flex()
                                .items_center()
                                .justify_center()
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
                        .child(
                            div()
                                .relative()
                                .left(exit_dx)
                                .opacity(exit_op)
                                .w(height::COMPACT)
                                .h(height::COMPACT)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::Settings)
                                        .size(px(16.0))
                                        .text_color(palette.text_secondary),
                                ),
                        ),
                ),
        );
    }

    let mut right_cluster = div()
        .relative()
        .h_full()
        .flex()
        .items_center()
        .gap(right_gap)
        .child(controls_group);

    right_cluster = right_cluster
        .child(div().w(px(1.0)).h(px(14.0)).bg(palette.border_strong))
        .child(render_window_caption_controls(
            is_maximized,
            palette,
            Rc::clone(&cb.on_minimize_window),
            Rc::clone(&cb.on_zoom_window),
            Rc::clone(&cb.on_request_close),
            window,
            cx,
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
#[allow(clippy::too_many_arguments)]
pub fn render_window_caption_controls(
    is_maximized: bool,
    palette: &DesktopPalette,
    on_min: TopbarActionFn,
    on_zoom: TopbarActionFn,
    on_close: TopbarActionFn,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let min_btn = caption_control_btn(
        "window-btn-minimize",
        "最小化窗口",
        WindowControlArea::Min,
        IconName::Minus,
        px(14.0),
        palette.bg_item_hover,
        palette.bg_active_press,
        palette.text_secondary,
        palette.text_primary,
        on_min,
        window,
        cx,
    );
    let max_btn = caption_control_btn(
        "window-btn-maximize",
        if is_maximized {
            "还原窗口"
        } else {
            "最大化窗口"
        },
        WindowControlArea::Max,
        if is_maximized {
            IconName::Copy
        } else {
            IconName::Square
        },
        px(12.0),
        palette.bg_item_hover,
        palette.bg_active_press,
        palette.text_secondary,
        palette.text_primary,
        on_zoom,
        window,
        cx,
    );
    let close_btn = caption_control_btn(
        "window-btn-close",
        "关闭窗口",
        WindowControlArea::Close,
        IconName::X,
        px(14.0),
        palette.status_danger,
        palette.status_danger_hover,
        palette.text_secondary,
        palette.text_on_accent,
        on_close,
        window,
        cx,
    );

    div()
        .flex()
        .items_center()
        .h_full()
        .child(min_btn)
        .child(max_btn)
        .child(close_btn)
}

#[allow(clippy::too_many_arguments)]
fn caption_control_btn(
    id: &'static str,
    label: &'static str,
    area: WindowControlArea,
    icon: IconName,
    icon_size: gpui_kit::Pixels,
    hover_bg: Hsla,
    press_bg: Hsla,
    base_fg: Hsla,
    hover_fg: Hsla,
    on_click: TopbarActionFn,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let (pressed, press_state) = motion::icon_press_state(id, window, cx);
    let (hovered, hs) = motion::PointerFlag::keyed(
        gpui_kit::ElementId::Name(SharedString::from(format!("hover-{id}"))),
        window,
        cx,
    );
    let hover_t = motion::animated_state_progress(id, hovered, window, cx);
    let press_t = motion::animated_press_progress(id, pressed, window, cx);
    let bg = motion::lerp_hsla(
        motion::lerp_hsla(gpui_kit::transparent_black(), hover_bg, hover_t),
        press_bg,
        press_t,
    );
    let fg = motion::lerp_hsla(base_fg, hover_fg, hover_t.max(press_t));

    div()
        .id(id)
        .test_support()
        .a11y_button(id, label)
        .tab_index(0)
        .window_control_area(area)
        .w(px(40.0))
        .h_full()
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .text_color(fg)
        .on_hover(move |active, w, cx| {
            if *active {
                motion::PointerFlag::enter(&hs, w, cx);
            } else {
                motion::PointerFlag::set(&hs, false, cx);
            }
        })
        .on_icon_press(press_state)
        .on_click(move |_, w, cx| (on_click)(w, cx))
        .child(motion::animated_icon_press(
            id,
            pressed,
            Icon::new(icon).size(icon_size).text_color(fg),
            window,
            cx,
        ))
}

fn view_segment_item(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    paint_static_pill: bool,
    palette: &DesktopPalette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let base = apply_scale(div(), if active { scale::UI_MEDIUM } else { scale::UI })
        .id(id)
        .test_support()
        .a11y_tab(id, label, active)
        .relative()
        .h(height::INLINE)
        .px(space::SM)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_center()
        .gap(space::XS)
        .on_click(move |_, w, cx| on_click(w, cx));

    let styled = if active {
        if paint_static_pill {
            base.bg(palette.bg_elevated)
                .border_1()
                .border_color(palette.border_subtle)
                .text_color(palette.text_primary)
        } else {
            base.bg(gpui_kit::transparent_black())
                .text_color(palette.text_primary)
        }
    } else {
        base.bg(gpui_kit::transparent_black())
            .text_color(palette.text_secondary)
            .hover(|s| s.text_color(palette.text_primary))
    };

    styled
        .child(Icon::new(icon).size(px(14.0)))
        .child(cjk_inline_label(label))
}

#[allow(clippy::too_many_arguments)]
fn topbar_icon_btn(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    icon_color: Hsla,
    palette: &DesktopPalette,
    window: &mut Window,
    cx: &mut App,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (pressed, press_state) = motion::icon_press_state(id, window, cx);
    let (hovered, hs) = motion::PointerFlag::keyed(
        gpui_kit::ElementId::Name(SharedString::from(format!("hover-{id}"))),
        window,
        cx,
    );
    let hover_t = motion::animated_state_progress(id, hovered, window, cx);
    let press_t = motion::animated_press_progress(id, pressed, window, cx);
    let bg = motion::lerp_hsla(
        motion::lerp_hsla(
            gpui_kit::transparent_black(),
            palette.bg_item_hover,
            hover_t,
        ),
        palette.bg_active_press,
        press_t,
    );

    let fg = motion::lerp_hsla(icon_color, palette.text_primary, hover_t);

    div()
        .id(id)
        .test_support()
        .a11y_button(id, label)
        .desktop_tooltip_with_hover(
            SharedString::from(format!("tooltip-{id}")),
            label.into(),
            false,
            Some(hs),
        )
        .tab_index(0)
        .w(height::COMPACT)
        .h(height::COMPACT)
        .rounded(radius::CONTROL)
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .on_icon_press(press_state)
        .on_click(move |_, w, cx| on_click(w, cx))
        .child(motion::animated_icon_press(
            id,
            pressed,
            Icon::new(icon).size(px(16.0)).text_color(fg),
            window,
            cx,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_session_title_caps_at_five_chars_with_feather_flag() {
        assert_eq!(
            prepare_collapsed_session_title("   "),
            ("新对话".to_owned(), false)
        );
        assert_eq!(
            prepare_collapsed_session_title("你好"),
            ("你好".to_owned(), false)
        );
        assert_eq!(
            prepare_collapsed_session_title("五个字标题"),
            ("五个字标题".to_owned(), false)
        );
        assert_eq!(
            prepare_collapsed_session_title("C 盘空间与硬盘信息"),
            ("C 盘空间与".to_owned(), true)
        );
        assert_eq!(
            prepare_collapsed_session_title("六个字长标题"),
            ("六个字长标".to_owned(), true)
        );
    }
}

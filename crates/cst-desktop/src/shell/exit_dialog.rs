use std::rc::Rc;

use cst_client::{ExitCheckReport, KeyboardFocusStyleDto};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::{ElementExt as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_scrim, render_frosted_surface};
use crate::theme::kbd_focus::{DialogActionNav, KbdTone};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, motion, radius, scale, space,
};

/// Formats a single concise Chinese warning line for [`ExitCheckReport`] matching `pFooK`.
pub fn summarize_exit_message(report: &ExitCheckReport) -> &'static str {
    let has_tasks = report.running_sessions > 0
        || report.pending_server_inbox > 0
        || report.active_exports > 0
        || report.unresolved_local_outbox > 0;
    let has_drafts = report.unsent_drafts > 0;
    match (has_tasks, has_drafts) {
        (true, true) => "有未完成任务和草稿，退出将中断并清空。",
        (true, false) => "有运行中的任务，退出将立即中断。",
        (false, true) => "有未发送的草稿，退出后将清空。",
        (false, false) => "退出后当前会话进度仍会保留。",
    }
}

/// `384px` exit confirmation dialog (`cst-pilot-desktop.pen` 「01 · 工作台」 frame `xXtdv`, card `pFooK`).
/// Default initial focus sits on the safe `"取消"` button.
#[allow(clippy::too_many_arguments)]
pub fn render_exit_confirm_dialog(
    report: &ExitCheckReport,
    exiting_in_progress: bool,
    scrim_progress: f32,
    kbd_style: KeyboardFocusStyleDto,
    nav: DialogActionNav,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    trap_focus: &FocusHandle,
    cancel_focus: &FocusHandle,
    confirm_focus: &FocusHandle,
    on_cancel: impl Fn(&mut Window, &mut App) + 'static,
    on_confirm_exit: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let on_cancel = Rc::new(on_cancel);
    let on_confirm_exit = Rc::new(on_confirm_exit);
    let summary_text = summarize_exit_message(report);
    let focuses = [cancel_focus.clone(), confirm_focus.clone()];
    let kbd_active = nav.is_keyboard_active(cx);
    nav.clear_tracker(cx);
    let tracker_cancel = nav.tracker(cx);
    let tracker_confirm = nav.tracker(cx);
    let nav_move = nav.clone();
    let nav_key = nav.clone();
    let nav_click_cancel = nav.clone();
    let nav_click_confirm = nav.clone();
    let kbd_overlay = nav.render_overlay(
        kbd_style,
        &focuses,
        [KbdTone::Neutral, KbdTone::Danger],
        false,
        palette,
        window,
        cx,
    );

    let scrim = render_frosted_scrim(
        "exit-dialog-scrim",
        FrostedGlassSpec::scrim(palette).with_opacity(scrim_progress),
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
    .on_mouse_move(move |ev, window, cx| nav_move.handle_mouse_move(ev, window, cx));

    let card = render_frosted_surface(
        "exit-dialog-card-pfook",
        FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .a11y_dialog("exit-confirm-dialog", "确认退出 CST Pilot")
    .w(layout::MODAL_STANDARD_WIDTH)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_subtle)
    .p(px(20.0))
    .flex()
    .flex_col()
    .gap(space::MD)
    .child(
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .size(px(16.0))
                            .text_color(palette.status_danger),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("退出 CST Pilot？")),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_secondary)
                    .child(summary_text),
            ),
    )
    .child(
        div()
            .relative()
            .flex()
            .items_center()
            .justify_end()
            .gap(space::SM)
            .child({
                let cb = Rc::clone(&on_cancel);
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("exit-dialog-cancel-btn")
                    .test_support()
                    .a11y_button("exit-dialog-cancel-btn", "取消")
                    .track_focus(cancel_focus)
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
                    .when(!kbd_active, |el| el.hover(|s| s.bg(palette.bg_item_hover)))
                    .on_click(move |_, window, cx| {
                        nav_click_cancel.clear_keyboard(window, cx);
                        if !exiting_in_progress {
                            cb(window, cx);
                        }
                    })
                    .child(cjk_inline_label("取消"))
                    .on_prepaint(move |bounds, _, _| tracker_cancel.set(0, bounds))
            })
            .child({
                let cb = Rc::clone(&on_confirm_exit);
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("exit-dialog-confirm-btn")
                    .test_support()
                    .a11y_button(
                        "exit-dialog-confirm-btn",
                        if exiting_in_progress {
                            "正在退出"
                        } else {
                            "退出"
                        },
                    )
                    .track_focus(confirm_focus)
                    .tab_index(0)
                    .h(height::CONTROL)
                    .px(space::LG)
                    .rounded(radius::CONTROL)
                    .bg(palette.status_danger)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(if exiting_in_progress { 0.65 } else { 1.0 })
                    .when(!kbd_active, |el| {
                        el.hover(|s| s.bg(palette.status_danger_hover))
                    })
                    .on_click(move |_, window, cx| {
                        nav_click_confirm.clear_keyboard(window, cx);
                        if !exiting_in_progress {
                            cb(window, cx);
                        }
                    })
                    .child(cjk_inline_label(if exiting_in_progress {
                        "正在退出…"
                    } else {
                        "退出"
                    }))
                    .on_prepaint(move |bounds, _, _| tracker_confirm.set(1, bounds))
            })
            .child(kbd_overlay),
    );

    let key_cancel = Rc::clone(&on_cancel);
    let key_confirm = Rc::clone(&on_confirm_exit);
    scrim.child(
        div()
            .tab_stop(false)
            .on_key_down(move |ev, window, cx| {
                let on_l = |w: &mut Window, cx: &mut App| {
                    if !exiting_in_progress {
                        key_cancel(w, cx);
                    }
                };
                let on_r = |w: &mut Window, cx: &mut App| {
                    if !exiting_in_progress {
                        key_confirm(w, cx);
                    }
                };
                if nav_key.handle_key_down(ev, &focuses, false, &on_l, &on_r, window, cx) {
                    cx.stop_propagation();
                }
            })
            .child(motion::apply_modal_motion(
                card,
                scrim_progress,
                scale_factor,
                palette.craft_stack_shadows(),
            ))
            .focus_trap("exit-dialog-focus-trap", trap_focus),
    )
}

/// Visual-only inert exit snapshot for [`render_exit_confirm_dialog`] during `150ms` close fade-out.
pub fn render_exit_confirm_dialog_inert(
    report: &ExitCheckReport,
    exiting_in_progress: bool,
    scrim_progress: f32,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
) -> impl IntoElement {
    let summary_text = summarize_exit_message(report);
    let card = render_frosted_surface(
        "exit-dialog-card-pfook-exit",
        FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .w(layout::MODAL_STANDARD_WIDTH)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_subtle)
    .p(px(20.0))
    .flex()
    .flex_col()
    .gap(space::MD)
    .child(
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .size(px(16.0))
                            .text_color(palette.status_danger),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("退出 CST Pilot？")),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_secondary)
                    .child(summary_text),
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
                    .child(cjk_inline_label("取消")),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .h(height::CONTROL)
                    .px(space::LG)
                    .rounded(radius::CONTROL)
                    .bg(palette.status_danger)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(if exiting_in_progress { 0.65 } else { 1.0 })
                    .child(cjk_inline_label(if exiting_in_progress {
                        "正在退出…"
                    } else {
                        "退出"
                    })),
            ),
    );

    render_frosted_scrim(
        "exit-dialog-scrim-exit",
        FrostedGlassSpec::scrim(palette).with_opacity(scrim_progress),
        palette.bg_scrim,
        transparency_enabled,
        scale_factor,
    )
    .absolute()
    .top_0()
    .left_0()
    .right_0()
    .bottom_0()
    .flex()
    .items_center()
    .justify_center()
    .child(motion::apply_modal_motion(
        card,
        scrim_progress,
        scale_factor,
        palette.craft_stack_shadows(),
    ))
}

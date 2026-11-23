use std::rc::Rc;

use cst_client::ExitCheckReport;
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
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
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    trap_focus: &FocusHandle,
    cancel_focus: &FocusHandle,
    confirm_focus: &FocusHandle,
    on_cancel: impl Fn(&mut Window, &mut App) + 'static,
    on_confirm_exit: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_cancel = Rc::new(on_cancel);
    let on_confirm_exit = Rc::new(on_confirm_exit);
    let summary_text = summarize_exit_message(report);

    let scrim = render_frosted_surface(
        "exit-dialog-scrim",
        FrostedGlassSpec::scrim(palette).with_opacity(scrim_progress),
        palette.bg_scrim,
        transparency_enabled,
        scale_factor,
    )
    .opacity(0.01 + 0.99 * scrim_progress)
    .test_support()
    .absolute()
    .top_0()
    .left_0()
    .right_0()
    .bottom_0()
    .flex()
    .items_center()
    .justify_center()
    .occlude();

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
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, window, cx| {
                        if !exiting_in_progress {
                            cb(window, cx);
                        }
                    })
                    .child(cjk_inline_label("取消"))
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
                    .hover(|s| s.bg(palette.status_danger_hover))
                    .on_click(move |_, window, cx| {
                        if !exiting_in_progress {
                            cb(window, cx);
                        }
                    })
                    .child(cjk_inline_label(if exiting_in_progress {
                        "正在退出…"
                    } else {
                        "退出"
                    }))
            }),
    );

    scrim.child(
        div()
            .tab_stop(false)
            .child(motion::animate_modal_enter(
                "exit-dialog-anim",
                card,
                palette.craft_stack_shadows(),
            ))
            .focus_trap("exit-dialog-focus-trap", trap_focus),
    )
}

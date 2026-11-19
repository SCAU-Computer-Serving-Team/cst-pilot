use std::rc::Rc;

use cst_client::ExitCheckReport;
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, TestSupportExt, Window, div, px,
};

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
};

/// Formats the human-readable Chinese summary lines for non-zero unfinished items in [`ExitCheckReport`].
pub fn summarize_exit_report(report: &ExitCheckReport) -> Vec<String> {
    let mut items = Vec::new();
    if report.running_sessions > 0 {
        items.push(format!(
            "{} 个会话正在执行诊断或生成回复",
            report.running_sessions
        ));
    }
    if report.pending_server_inbox > 0 {
        items.push(format!(
            "{} 条排队消息尚未投递完成",
            report.pending_server_inbox
        ));
    }
    if report.active_exports > 0 {
        items.push(format!(
            "{} 项会话导出任务正在后台生成",
            report.active_exports
        ));
    }
    if report.unresolved_local_outbox > 0 {
        items.push(format!(
            "{} 条本机待发消息尚未确认结果",
            report.unresolved_local_outbox
        ));
    }
    if report.unsent_drafts > 0 {
        items.push(format!("{} 个输入框存有未发送草稿", report.unsent_drafts));
    }
    items
}

/// `384px` exit confirmation dialog (`cst-pilot-desktop.pen` 「01 · 工作台」 frame `xXtdv`, card `pFooK`).
/// Default initial focus sits on the safe `"取消"` button; `"退出"` uses solid danger red (`$status-danger`).
#[allow(clippy::too_many_arguments)]
pub fn render_exit_confirm_dialog(
    report: &ExitCheckReport,
    exiting_in_progress: bool,
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
    let bullets = summarize_exit_report(report);

    let scrim = render_frosted_surface(
        "exit-dialog-scrim",
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
    .occlude();

    let mut bullet_list = div().flex().flex_col().gap(space::XS);
    for line in bullets {
        bullet_list = bullet_list.child(
            apply_scale(div(), scale::UI)
                .text_color(palette.text_secondary)
                .child(format!("· {line}")),
        );
    }

    let card = render_frosted_surface(
        "exit-dialog-card-pfook",
        FrostedGlassSpec::modal_k3x72(palette, 8.0),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .w(layout::MODAL_STANDARD_WIDTH)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_subtle)
    .p(space::GROUP)
    .flex()
    .flex_col()
    .gap(space::LG)
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
                        Icon::new(IconName::TriangleAlert)
                            .size(px(18.0))
                            .text_color(palette.status_warning),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("确认退出 CST Pilot？")),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_secondary)
                    .child("退出将停止当前正在运行的后台任务并清空内存中的未发草稿："),
            )
            .child(bullet_list),
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
                    .track_focus(confirm_focus)
                    .tab_index(0)
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .bg(palette.status_danger)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(if exiting_in_progress { 0.65 } else { 1.0 })
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
            .child(card)
            .focus_trap("exit-dialog-focus-trap", trap_focus),
    )
}

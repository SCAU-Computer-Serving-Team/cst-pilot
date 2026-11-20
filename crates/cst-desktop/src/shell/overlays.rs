use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    A11yElementExt as _, DesktopPalette, apply_scale, cjk_inline_label, height, layout, radius,
    scale, space,
};

/// `WebActive` takeover lock overlay (`cst-pilot-desktop.pen` 「01 · 工作台」 frame `sYHwZ`, card `WO6NQ`, `384px` wide).
pub fn render_web_active_lock_overlay(
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    trap_focus: &FocusHandle,
    switch_back_focus: &FocusHandle,
    on_open_browser: impl Fn(&mut Window, &mut App) + 'static,
    on_switch_back: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_open_browser = Rc::new(on_open_browser);
    let on_switch_back = Rc::new(on_switch_back);

    let scrim = render_frosted_surface(
        "web-active-scrim",
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

    let card = render_frosted_surface(
        "web-active-card-wo6nq",
        FrostedGlassSpec::modal_k3x72(palette, 8.0),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .a11y_dialog("web-active-lock-overlay", "正在网页端使用")
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
                        Icon::new(IconName::Globe)
                            .size(px(18.0))
                            .text_color(palette.accent_solid),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("正在网页端使用")),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(palette.text_secondary)
                    .child("当前会话已切换至浏览器继续操作。为防止双端并发修改冲突，桌面端工作区已暂停编辑，会话列表与配置保持实时同步。"),
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
                let cb = Rc::clone(&on_open_browser);
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("web-active-open-browser-btn")
                    .test_support()
                    .a11y_button("web-active-open-browser-btn", "打开网页")
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
                    .on_click(move |_, window, cx| cb(window, cx))
                    .child(cjk_inline_label("打开网页"))
            })
            .child({
                let cb = Rc::clone(&on_switch_back);
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("web-active-switch-back-btn")
                    .test_support()
                    .a11y_button("web-active-switch-back-btn", "切回桌面端")
                    .track_focus(switch_back_focus)
                    .tab_index(0)
                    .h(height::CONTROL)
                    .px(space::LG)
                    .rounded(radius::CONTROL)
                    .bg(palette.accent_solid)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(palette.accent_hover))
                    .on_click(move |_, window, cx| cb(window, cx))
                    .child(cjk_inline_label("切回桌面端"))
            }),
    );

    scrim.child(
        div()
            .tab_stop(false)
            .child(card)
            .focus_trap("web-active-focus-trap", trap_focus),
    )
}

/// Workspace top-center `40px` rectangular error bar (`X2gAh`:
/// `top(20px)`, `h(40px)`, `px(16px)`, `rounded(8px)`, `CircleAlert` `16px` `$status-danger` + single-line reason, `H2p2z` frosted glass).
pub fn render_workspace_error_bar(
    message: SharedString,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
) -> impl IntoElement {
    div()
        .absolute()
        .top(px(20.0))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            render_frosted_surface(
                "workspace-top-error-bar",
                FrostedGlassSpec::popover_h2p2z(palette),
                palette.bg_elevated,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .a11y_status("workspace-top-error-bar", format!("错误：{message}"))
            .h(height::FIELD)
            .max_w(px(640.0))
            .px(space::LG)
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_hairline)
            .flex()
            .items_center()
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .min_w_0()
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .size(px(16.0))
                            .text_color(palette.status_danger),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_primary)
                            .truncate()
                            .child(cjk_inline_label(message)),
                    ),
            ),
        )
}

/// Non-blocking top notice banner (`h1BufF`:
/// `top(20px)`, `h(40px)`, `px(16px)`, `rounded(8px)`, `CircleCheck` `16px` `$status-success` + message, `H2p2z` frosted glass).
pub fn render_top_notice_banner(
    message: SharedString,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
) -> impl IntoElement {
    div()
        .absolute()
        .top(px(20.0))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            render_frosted_surface(
                "top-notice-banner",
                FrostedGlassSpec::popover_h2p2z(palette),
                palette.bg_elevated,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .a11y_status("top-notice-banner", format!("提示：{message}"))
            .h(height::FIELD)
            .max_w(px(480.0))
            .px(space::LG)
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_hairline)
            .flex()
            .items_center()
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .min_w_0()
                    .child(
                        Icon::new(IconName::CircleCheck)
                            .size(px(16.0))
                            .text_color(palette.status_success),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_primary)
                            .truncate()
                            .child(cjk_inline_label(message)),
                    ),
            ),
        )
}

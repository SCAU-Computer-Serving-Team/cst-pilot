use std::rc::Rc;

use cst_client::KeyboardFocusStyleDto;
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::{ElementExt as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_scrim, render_frosted_surface};
use crate::theme::kbd_focus::{DialogActionNav, KbdTone};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, motion, radius, scale, space,
};

#[derive(Debug, Clone)]
pub struct MissingKeyModalState {
    pub provider_label: Option<String>,
    pub return_session_id: Option<String>,
    pub prior_focus: Option<FocusHandle>,
}

/// Renders the `384px` Missing Model Credentials prompt modal
/// (`cst-pilot-desktop.pen` 「01 · 工作台」 `ywnbk` -> `e7fEty`, bottom `"取消 / 去配置"`).
/// Preserves composer draft & images and remembers `return_session_id` without auto-resending.
#[allow(clippy::too_many_arguments)]
pub fn render_missing_key_dialog(
    state: &MissingKeyModalState,
    scrim_progress: f32,
    kbd_style: KeyboardFocusStyleDto,
    nav: DialogActionNav,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    trap_focus: &FocusHandle,
    cancel_focus: &FocusHandle,
    on_cancel: impl Fn(&mut Window, &mut App) + 'static,
    on_configure: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let on_cancel = Rc::new(on_cancel);
    let on_configure = Rc::new(on_configure);
    let configure_focus = nav.fallback_focus(1, cx);
    let focuses = [cancel_focus.clone(), configure_focus.clone()];
    let kbd_active = nav.is_keyboard_active(cx);
    nav.clear_tracker(cx);
    let tracker_cancel = nav.tracker(cx);
    let tracker_cfg = nav.tracker(cx);
    let nav_move = nav.clone();
    let nav_key = nav.clone();
    let nav_click_cancel = nav.clone();
    let nav_click_cfg = nav.clone();
    let kbd_overlay = nav.render_overlay(
        kbd_style,
        &focuses,
        [KbdTone::Neutral, KbdTone::Accent],
        false,
        palette,
        window,
        cx,
    );

    let clean_prov = state.provider_label.as_deref().filter(|s| !s.is_empty());
    let title_text = match clean_prov {
        Some(prov) => SharedString::from(format!("缺少 {prov} 凭据")),
        None => SharedString::from("缺少提供商凭据"),
    };
    let body_text = match clean_prov {
        Some(prov) => SharedString::from(format!(
            "当前所选模型所属的 {prov} 尚未配置有效凭据。已保留输入框内的草稿与附图，配置完成后可返回原会话继续发送。"
        )),
        None => SharedString::from(
            "当前尚未配置任何可用的提供商凭据。前往「设置 · 提供商」或登录页完成配置即可开始诊断。",
        ),
    };

    let click_cancel = Rc::clone(&on_cancel);
    let click_cfg = Rc::clone(&on_configure);
    render_frosted_scrim(
        "missing-key-scrim-ywnbk",
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
    .on_mouse_move(move |ev, window, cx| nav_move.handle_mouse_move(ev, window, cx))
    .child(
        div()
            .tab_stop(false)
            .focus_trap("missing-key-focus-trap", trap_focus)
            .on_key_down(move |ev, window, cx| {
                if nav_key.handle_key_down(
                    ev,
                    &focuses,
                    false,
                    &*on_cancel,
                    &*on_configure,
                    window,
                    cx,
                ) {
                    cx.stop_propagation();
                }
            })
            .child(motion::apply_modal_motion(
                render_frosted_surface(
                    "missing-key-dialog-e7fety",
                    FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
                    palette.bg_modal,
                    transparency_enabled,
                    scale_factor,
                )
                .test_support()
                .a11y_dialog("missing-key-dialog", title_text.clone())
                .w(layout::MODAL_STANDARD_WIDTH)
                .p(px(20.0))
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette.border_subtle)
                .flex()
                .flex_col()
                .gap(space::MD)
                .child(
                    div()
                        .relative()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .size(px(16.0))
                                .text_color(palette.status_warning),
                        )
                        .child(
                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                .min_w_0()
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(title_text)),
                        ),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .relative()
                        .text_color(palette.text_secondary)
                        .child(body_text),
                )
                .child(
                    div()
                        .relative()
                        .flex()
                        .justify_end()
                        .gap(space::SM)
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("missing-key-cancel-btn")
                                .test_support()
                                .tab_index(0)
                                .track_focus(cancel_focus)
                                .a11y_button("missing-key-cancel-btn", "取消")
                                .h(height::CONTROL)
                                .px(space::MD)
                                .rounded(radius::CONTROL)
                                .border_1()
                                .border_color(palette.border_strong)
                                .bg(palette.bg_workspace)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .when(!kbd_active, |el| el.hover(|s| s.bg(palette.bg_item_hover)))
                                .on_click(move |_, w, cx| {
                                    nav_click_cancel.clear_keyboard(w, cx);
                                    click_cancel(w, cx);
                                })
                                .child(cjk_inline_label("取消"))
                                .on_prepaint(move |bounds, _, _| tracker_cancel.set(0, bounds)),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("missing-key-configure-btn")
                                .test_support()
                                .tab_index(0)
                                .track_focus(&configure_focus)
                                .a11y_button("missing-key-configure-btn", "去配置")
                                .h(height::CONTROL)
                                .px(space::LG)
                                .rounded(radius::CONTROL)
                                .bg(palette.accent_solid)
                                .text_color(palette.text_on_accent)
                                .flex()
                                .items_center()
                                .when(!kbd_active, |el| el.hover(|s| s.bg(palette.accent_hover)))
                                .on_click(move |_, w, cx| {
                                    nav_click_cfg.clear_keyboard(w, cx);
                                    click_cfg(w, cx);
                                })
                                .child(cjk_inline_label("去配置"))
                                .on_prepaint(move |bounds, _, _| tracker_cfg.set(1, bounds)),
                        )
                        .child(kbd_overlay),
                ),
                scrim_progress,
                scale_factor,
                palette.craft_stack_shadows(),
            )),
    )
}

/// Visual-only inert exit snapshot for [`render_missing_key_dialog`] during `150ms` close fade-out.
pub fn render_missing_key_dialog_inert(
    state: &MissingKeyModalState,
    scrim_progress: f32,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
) -> impl IntoElement {
    let clean_prov = state.provider_label.as_deref().filter(|s| !s.is_empty());
    let title_text = match clean_prov {
        Some(prov) => SharedString::from(format!("缺少 {prov} 凭据")),
        None => SharedString::from("缺少提供商凭据"),
    };
    let body_text = match clean_prov {
        Some(prov) => SharedString::from(format!(
            "当前所选模型所属的 {prov} 尚未配置有效凭据。已保留输入框内的草稿与附图，配置完成后可返回原会话继续发送。"
        )),
        None => SharedString::from(
            "当前尚未配置任何可用的提供商凭据。前往「设置 · 提供商」或登录页完成配置即可开始诊断。",
        ),
    };

    let card = render_frosted_surface(
        "missing-key-dialog-e7fety-exit",
        FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .w(layout::MODAL_STANDARD_WIDTH)
    .p(px(20.0))
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_subtle)
    .flex()
    .flex_col()
    .gap(space::MD)
    .child(
        div()
            .relative()
            .flex()
            .items_center()
            .gap(space::SM)
            .child(
                Icon::new(IconName::TriangleAlert)
                    .size(px(16.0))
                    .text_color(palette.status_warning),
            )
            .child(
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .min_w_0()
                    .truncate()
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(title_text)),
            ),
    )
    .child(
        apply_scale(div(), scale::UI)
            .relative()
            .text_color(palette.text_secondary)
            .child(body_text),
    )
    .child(
        div()
            .relative()
            .flex()
            .justify_end()
            .gap(space::SM)
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(palette.bg_workspace)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .child(cjk_inline_label("取消")),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .h(height::CONTROL)
                    .px(space::LG)
                    .rounded(radius::CONTROL)
                    .bg(palette.accent_solid)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .child(cjk_inline_label("去配置")),
            ),
    );

    render_frosted_scrim(
        "missing-key-scrim-ywnbk-exit",
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

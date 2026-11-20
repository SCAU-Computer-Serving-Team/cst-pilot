use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
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
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    trap_focus: &FocusHandle,
    cancel_focus: &FocusHandle,
    on_cancel: impl Fn(&mut Window, &mut App) + 'static,
    on_configure: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let clean_prov = state.provider_label.as_deref().filter(|s| !s.is_empty());
    let title_text = match clean_prov {
        Some(prov) => SharedString::from(format!("缺少 {prov} 服务凭据")),
        None => SharedString::from("缺少模型服务凭据"),
    };
    let body_text = match clean_prov {
        Some(prov) => SharedString::from(format!(
            "当前所选模型所属的 {prov} 尚未配置有效凭据。已保留输入框内的草稿与附图，配置完成后可返回原会话继续发送。"
        )),
        None => SharedString::from(
            "当前尚未配置任何可用的模型服务凭据。前往「设置 · 模型服务」或登录页完成配置即可开始诊断。",
        ),
    };

    render_frosted_surface(
        "missing-key-scrim-ywnbk",
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
    .occlude()
    .child(
        div()
            .tab_stop(false)
            .focus_trap("missing-key-focus-trap", trap_focus)
            .child(
                render_frosted_surface(
                    "missing-key-dialog-e7fety",
                    FrostedGlassSpec::modal_k3x72(palette, 8.0),
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
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(move |_, w, cx| on_cancel(w, cx))
                                .child(cjk_inline_label("取消")),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("missing-key-configure-btn")
                                .test_support()
                                .tab_index(0)
                                .a11y_button("missing-key-configure-btn", "去配置")
                                .h(height::CONTROL)
                                .px(space::LG)
                                .rounded(radius::CONTROL)
                                .bg(palette.accent_solid)
                                .text_color(palette.text_on_accent)
                                .flex()
                                .items_center()
                                .hover(|s| s.bg(palette.accent_hover))
                                .on_click(move |_, w, cx| on_configure(w, cx))
                                .child(cjk_inline_label("去配置")),
                        ),
                ),
            ),
    )
}

use cst_client::{UpdatePhaseDto, UpdateStateDto};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px, relative,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
};

#[derive(Debug, Clone, Default)]
pub struct UpdateDialogState {
    pub open: bool,
    pub auto_launch_when_ready: bool,
    pub handoff_locked: bool,
    pub busy_error: Option<SharedString>,
    pub prior_focus: Option<FocusHandle>,
}

pub fn format_megabytes(bytes: u64) -> String {
    format!("{:.1} MB", (bytes as f64) / (1024.0 * 1024.0))
}

pub fn compute_update_progress_ratio(update_state: Option<&UpdateStateDto>) -> Option<f32> {
    let st = &update_state?.status;
    match st.phase {
        UpdatePhaseDto::Checking => Some(0.18),
        UpdatePhaseDto::Preparing => {
            if let (Some(dl), Some(total)) = (st.downloaded_bytes, st.archive_bytes)
                && total > 0
            {
                Some(((dl as f64) / (total as f64)).clamp(0.05, 0.99) as f32)
            } else {
                Some(0.55)
            }
        }
        UpdatePhaseDto::Ready | UpdatePhaseDto::Launching => Some(1.0),
        _ => None,
    }
}

/// Renders the `384px` Software Update confirm / download-cancel / restart-handoff modal (`r: 8px` `K3x72`):
/// - `Phase::Available` (`Pvhss` -> `kOm7J`): `16px` `$accent-ink` upgrade icon, target version, monospace archive MB, plain `releaseNotes`, `"稍后"` + `"更新并重启"`
/// - `Phase::Preparing` (`IHkDa` -> `TqiZo`): `6px` `$bg-surface` progress track, downloaded MB, `"稍后"` (background) + neutral outline `"取消"` (`s2CZVU`)
/// - `Phase::Ready` / `handoff_locked`: `"立即重启更新"`, `409 session_busy_for_update` warning, or `"正在重启完成更新…"` lock screen.
#[allow(clippy::too_many_arguments)]
pub fn render_update_dialog(
    dialog: &UpdateDialogState,
    update_state: Option<&UpdateStateDto>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    on_dismiss_later: impl Fn(&mut Window, &mut App) + 'static,
    on_start_prepare: impl Fn(&mut Window, &mut App) + 'static,
    on_cancel_prepare: impl Fn(&mut Window, &mut App) + 'static,
    on_launch_restart: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let phase = update_state
        .map(|u| u.status.phase)
        .unwrap_or(UpdatePhaseDto::Available);
    let target_ver = update_state
        .and_then(|u| u.status.version.as_deref())
        .unwrap_or("新版本");
    let archive_bytes = update_state.and_then(|u| u.status.archive_bytes);
    let downloaded_bytes = update_state.and_then(|u| u.status.downloaded_bytes);
    let notes = update_state
        .and_then(|u| u.status.release_notes.as_deref())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("包含稳定性改进与诊断能力更新。");

    if dialog.handoff_locked || phase == UpdatePhaseDto::Launching {
        return render_frosted_surface(
            "update-handoff-lock-scrim",
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
            render_frosted_surface(
                "update-handoff-lock-card",
                FrostedGlassSpec::modal_k3x72(palette, 8.0),
                palette.bg_modal,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .a11y_dialog("update-handoff-lock-card", "正在重启完成更新")
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
                        Icon::new(IconName::CircleArrowUp)
                            .size(px(16.0))
                            .text_color(palette.accent_ink),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("正在重启完成更新…")),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .relative()
                    .text_color(palette.text_secondary)
                    .child("正在交接独立升级程序校验并替换组件，窗口即将自动重启。"),
            ),
        )
        .into_any_element();
    }

    let is_preparing = matches!(phase, UpdatePhaseDto::Preparing | UpdatePhaseDto::Checking);
    let is_ready = phase == UpdatePhaseDto::Ready;
    let scrim_id = if is_preparing {
        "update-preparing-scrim-ihkda"
    } else {
        "update-available-scrim-pvhss"
    };
    let card_id = if is_preparing {
        "update-preparing-dialog-tqizo"
    } else {
        "update-available-dialog-kom7j"
    };

    let header_title = if is_preparing {
        format!("正在下载更新 {target_ver}")
    } else if is_ready {
        format!("更新包 {target_ver} 已就绪")
    } else {
        format!("发现新版本 {target_ver}")
    };
    let right_size_badge = archive_bytes.map(format_megabytes);

    let mut card = render_frosted_surface(
        card_id,
        FrostedGlassSpec::modal_k3x72(palette, 8.0),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .a11y_dialog("update-dialog-card", header_title.clone())
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
            .justify_between()
            .gap(space::SM)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .min_w_0()
                    .child(
                        Icon::new(IconName::CircleArrowUp)
                            .size(px(16.0))
                            .text_color(palette.accent_ink),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .min_w_0()
                            .truncate()
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label(header_title)),
                    ),
            )
            .children(right_size_badge.map(|sz| {
                apply_scale(div(), scale::CAPTION)
                    .font_family(mono_family.clone())
                    .text_color(palette.text_secondary)
                    .flex_shrink_0()
                    .child(SharedString::from(sz))
            })),
    );

    if is_preparing {
        let ratio = compute_update_progress_ratio(update_state).unwrap_or(0.5);
        let dl_label = match (downloaded_bytes, archive_bytes) {
            (Some(dl), Some(tot)) => {
                format!(
                    "已下载 {} / {}",
                    format_megabytes(dl),
                    format_megabytes(tot)
                )
            }
            (None, Some(tot)) => format!("正在下载签名离线包（共 {}）…", format_megabytes(tot)),
            _ => "正在下载并校验签名清单…".to_owned(),
        };

        card = card.child(
            div()
                .relative()
                .flex()
                .flex_col()
                .gap(space::XS)
                .child(
                    div()
                        .w_full()
                        .h(px(6.0))
                        .rounded(radius::FULL)
                        .bg(palette.bg_surface)
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(relative(ratio))
                                .rounded(radius::FULL)
                                .bg(palette.accent_solid),
                        ),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .font_family(mono_family)
                        .text_color(palette.text_secondary)
                        .child(SharedString::from(dl_label)),
                ),
        );
    } else {
        card = card.child(
            apply_scale(div(), scale::UI)
                .id("update-dialog-notes-scroll")
                .relative()
                .max_h(px(160.0))
                .overflow_y_scroll()
                .text_color(palette.text_secondary)
                .child(SharedString::from(notes.to_owned())),
        );
    }

    if let Some(ref busy_err) = dialog.busy_error {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .id("update-dialog-busy-error")
                .test_support()
                .relative()
                .text_color(palette.status_danger)
                .child(busy_err.clone()),
        );
    }

    let footer = if is_preparing {
        div()
            .relative()
            .flex()
            .justify_end()
            .gap(space::SM)
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("update-dialog-background-btn")
                    .test_support()
                    .a11y_button("update-dialog-background-btn", "后台继续下载")
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
                    .on_click(move |_, w, cx| on_dismiss_later(w, cx))
                    .child(cjk_inline_label("稍后")),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("update-dialog-cancel-prepare-btn")
                    .test_support()
                    .a11y_button("update-dialog-cancel-prepare-btn", "取消下载更新")
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
                    .on_click(move |_, w, cx| on_cancel_prepare(w, cx))
                    .child(cjk_inline_label("取消")),
            )
    } else if is_ready {
        div()
            .relative()
            .flex()
            .justify_end()
            .gap(space::SM)
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("update-dialog-later-btn")
                    .test_support()
                    .a11y_button("update-dialog-later-btn", "稍后")
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
                    .on_click(move |_, w, cx| on_dismiss_later(w, cx))
                    .child(cjk_inline_label("稍后")),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("update-dialog-launch-btn")
                    .test_support()
                    .a11y_button("update-dialog-launch-btn", "立即重启更新")
                    .h(height::CONTROL)
                    .px(space::LG)
                    .rounded(radius::CONTROL)
                    .bg(palette.accent_solid)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.accent_hover))
                    .on_click(move |_, w, cx| on_launch_restart(w, cx))
                    .child(cjk_inline_label("立即重启更新")),
            )
    } else {
        div()
            .relative()
            .flex()
            .justify_end()
            .gap(space::SM)
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("update-dialog-later-btn")
                    .test_support()
                    .a11y_button("update-dialog-later-btn", "稍后")
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
                    .on_click(move |_, w, cx| on_dismiss_later(w, cx))
                    .child(cjk_inline_label("稍后")),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("update-dialog-confirm-restart-btn")
                    .test_support()
                    .a11y_button("update-dialog-confirm-restart-btn", "更新并重启")
                    .h(height::CONTROL)
                    .px(space::LG)
                    .rounded(radius::CONTROL)
                    .bg(palette.accent_solid)
                    .text_color(palette.text_on_accent)
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.accent_hover))
                    .on_click(move |_, w, cx| on_start_prepare(w, cx))
                    .child(cjk_inline_label("更新并重启")),
            )
    };

    render_frosted_surface(
        scrim_id,
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
    .child(card.child(footer))
    .into_any_element()
}

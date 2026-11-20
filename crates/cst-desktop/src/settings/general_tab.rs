use std::rc::Rc;

use cst_client::{
    DefaultViewDto, DesktopSettingsDto, ThemeModeDto, UpdatePhaseDto, UpdateStateDto,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, TestSupportExt, Window, div, px, relative,
};

use crate::settings::update_dialog::{compute_update_progress_ratio, format_megabytes};
use crate::theme::{
    A11yElementExt as _, DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale,
    space,
};

pub type SelectThemeFn = Rc<dyn Fn(ThemeModeDto, &mut Window, &mut App)>;
pub type SelectDefaultViewFn = Rc<dyn Fn(DefaultViewDto, &mut Window, &mut App)>;
pub type UpdateCardActionFn = Rc<dyn Fn(&mut Window, &mut App)>;

/// Renders the `常规` settings tab (`cst-pilot-desktop.pen` 「03 · 设置」 `q8TgB`):
/// 1. Category heading `常规` (`20/28 @ 600`)
/// 2. `外观` group (`$bg-workspace` + `8px` radius + `$border-subtle`, two `72px` rows with icon segmented controls)
/// 3. `软件更新` group (`72px` `C9or7K` card with `$bg-track-recess` `keDNb` progress fill and neutral outline `s2CZVU` button).
#[allow(clippy::too_many_arguments)]
pub fn render_general_tab(
    desktop_settings: &DesktopSettingsDto,
    current_version: &str,
    update_state: Option<&UpdateStateDto>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    on_select_theme: SelectThemeFn,
    on_select_default_view: SelectDefaultViewFn,
    on_check_or_open_update: UpdateCardActionFn,
    on_cancel_update: UpdateCardActionFn,
) -> impl IntoElement {
    let phase = update_state
        .map(|u| u.status.phase)
        .unwrap_or(UpdatePhaseDto::Idle);
    let ver_str = update_state
        .map(|u| u.current_version.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(current_version);

    let progress_ratio = compute_update_progress_ratio(update_state);
    let in_flight = matches!(phase, UpdatePhaseDto::Checking | UpdatePhaseDto::Preparing);
    let unavailable = phase == UpdatePhaseDto::Unavailable;

    let status_subtitle = match phase {
        UpdatePhaseDto::Unavailable => SharedString::from("未配置编译期更新源，自动升级未启用"),
        UpdatePhaseDto::Checking => SharedString::from("正在检查签名发布清单…"),
        UpdatePhaseDto::Current => SharedString::from("当前已是最新版本"),
        UpdatePhaseDto::Available => {
            let target = update_state
                .and_then(|u| u.status.version.as_deref())
                .unwrap_or("新版本");
            SharedString::from(format!("发现新版本 {target}，可下载并重启升级"))
        }
        UpdatePhaseDto::Preparing => {
            let dl = update_state.and_then(|u| u.status.downloaded_bytes);
            let tot = update_state.and_then(|u| u.status.archive_bytes);
            match (dl, tot) {
                (Some(d), Some(t)) => SharedString::from(format!(
                    "正在下载更新包 {} / {}",
                    format_megabytes(d),
                    format_megabytes(t)
                )),
                (_, Some(t)) => {
                    SharedString::from(format!("正在下载更新包（{}）…", format_megabytes(t)))
                }
                _ => SharedString::from("正在下载并验签离线更新包…"),
            }
        }
        UpdatePhaseDto::Ready => SharedString::from("更新包校验通过，点击重启完成升级"),
        UpdatePhaseDto::Failed => {
            let msg = update_state
                .map(|u| u.status.message.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("检查或下载更新失败");
            SharedString::from(msg.to_owned())
        }
        _ => SharedString::from("手动检查签名离线升级包"),
    };

    // Neutral outline button `s2CZVU` across all states (no solid accent blue conflict!)
    let right_update_btn = if in_flight {
        apply_scale(div(), scale::UI_MEDIUM)
            .id("settings-update-cancel-btn-ohwrt")
            .test_support()
            .a11y_button("settings-update-cancel-btn", "取消更新")
            .relative()
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
            .on_click(move |_, w, cx| (on_cancel_update)(w, cx))
            .child(cjk_inline_label("取消"))
            .into_any_element()
    } else if unavailable {
        apply_scale(div(), scale::UI_MEDIUM)
            .id("settings-update-disabled-btn")
            .test_support()
            .relative()
            .h(height::CONTROL)
            .px(space::MD)
            .rounded(radius::CONTROL)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_surface)
            .text_color(palette.text_muted)
            .flex()
            .items_center()
            .child(cjk_inline_label("未启用"))
            .into_any_element()
    } else {
        let btn_label = match phase {
            UpdatePhaseDto::Available => "查看更新",
            UpdatePhaseDto::Ready => "重启更新",
            _ => "检查更新",
        };
        apply_scale(div(), scale::UI_MEDIUM)
            .id("settings-update-action-btn")
            .test_support()
            .a11y_button("settings-update-action-btn", btn_label)
            .relative()
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
            .on_click(move |_, w, cx| (on_check_or_open_update)(w, cx))
            .child(cjk_inline_label(btn_label))
            .into_any_element()
    };

    let mut update_card = div()
        .id("settings-update-card-c9or7k")
        .test_support()
        .a11y_region("settings-update-card", "软件版本与更新")
        .relative()
        .w_full()
        .h(px(72.0))
        .px(space::LG)
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_workspace)
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_between();

    // Underlying left-to-right `$bg-track-recess` download progress fill (`keDNb`)
    if let Some(ratio) = progress_ratio
        && in_flight
    {
        update_card = update_card.child(
            div()
                .id("settings-update-progress-fill-kednb")
                .test_support()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(ratio))
                .bg(palette.bg_track_recess),
        );
    }

    update_card = update_card
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .gap(space::MICRO)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            apply_scale(div(), scale::BODY)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label("软件版本与更新")),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .font_family(mono_family)
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(ver_str.to_owned())),
                        ),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(if phase == UpdatePhaseDto::Failed {
                            palette.status_danger
                        } else {
                            palette.text_secondary
                        })
                        .child(status_subtitle),
                ),
        )
        .child(right_update_btn);

    let t_sys = Rc::clone(&on_select_theme);
    let t_light = Rc::clone(&on_select_theme);
    let t_dark = Rc::clone(&on_select_theme);
    let v_gui = Rc::clone(&on_select_default_view);
    let v_tui = Rc::clone(&on_select_default_view);

    div()
        .id("settings-general-tab-q8tgb")
        .test_support()
        .a11y_region("settings-general-tab", "常规设置")
        .w_full()
        .flex()
        .flex_col()
        .gap(space::GROUP)
        .child(
            apply_scale(div(), scale::TITLE)
                .text_color(palette.text_primary)
                .child(cjk_inline_label("常规")),
        )
        // Group 1: 外观
        .child(
            div()
                .flex()
                .flex_col()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::BODY_SEMIBOLD)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("外观")),
                )
                .child(
                    div()
                        .w_full()
                        .rounded(radius::ROW)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .bg(palette.bg_workspace)
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .w_full()
                                .h(px(72.0))
                                .px(space::LG)
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    apply_scale(div(), scale::BODY)
                                        .text_color(palette.text_primary)
                                        .child(cjk_inline_label("界面主题")),
                                )
                                .child(
                                    div()
                                        .h(height::CONTROL)
                                        .p(space::XS)
                                        .rounded(radius::ROW)
                                        .bg(palette.bg_surface)
                                        .flex()
                                        .items_center()
                                        .gap(space::MICRO)
                                        .child(seg_pill(
                                            "settings-theme-system",
                                            IconName::Monitor,
                                            "跟随系统",
                                            desktop_settings.theme == ThemeModeDto::System,
                                            palette,
                                            move |w, cx| (t_sys)(ThemeModeDto::System, w, cx),
                                        ))
                                        .child(seg_pill(
                                            "settings-theme-light",
                                            IconName::Sun,
                                            "浅色",
                                            desktop_settings.theme == ThemeModeDto::Light,
                                            palette,
                                            move |w, cx| (t_light)(ThemeModeDto::Light, w, cx),
                                        ))
                                        .child(seg_pill(
                                            "settings-theme-dark",
                                            IconName::Moon,
                                            "深色",
                                            desktop_settings.theme == ThemeModeDto::Dark,
                                            palette,
                                            move |w, cx| (t_dark)(ThemeModeDto::Dark, w, cx),
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(72.0))
                                .px(space::LG)
                                .border_t_1()
                                .border_color(palette.border_subtle)
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    apply_scale(div(), scale::BODY)
                                        .text_color(palette.text_primary)
                                        .child(cjk_inline_label("默认启动视图")),
                                )
                                .child(
                                    div()
                                        .h(height::CONTROL)
                                        .p(space::XS)
                                        .rounded(radius::ROW)
                                        .bg(palette.bg_surface)
                                        .flex()
                                        .items_center()
                                        .gap(space::MICRO)
                                        .child(seg_pill(
                                            "settings-view-gui",
                                            IconName::Layers,
                                            "图形",
                                            desktop_settings.default_view == DefaultViewDto::Gui,
                                            palette,
                                            move |w, cx| (v_gui)(DefaultViewDto::Gui, w, cx),
                                        ))
                                        .child(seg_pill(
                                            "settings-view-tui",
                                            IconName::SquareTerminal,
                                            "终端",
                                            desktop_settings.default_view == DefaultViewDto::Tui,
                                            palette,
                                            move |w, cx| (v_tui)(DefaultViewDto::Tui, w, cx),
                                        )),
                                ),
                        ),
                ),
        )
        // Group 2: 软件更新 (`C9or7K`)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::BODY_SEMIBOLD)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("软件更新")),
                )
                .child(update_card),
        )
}

fn seg_pill(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    palette: &DesktopPalette,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let base = if active {
        apply_scale(div(), scale::UI_MEDIUM)
            .bg(palette.bg_elevated)
            .border_1()
            .border_color(palette.border_subtle)
            .text_color(palette.text_primary)
    } else {
        apply_scale(div(), scale::UI)
            .bg(gpui_kit::transparent_black())
            .text_color(palette.text_secondary)
    };

    base.id(id)
        .test_support()
        .a11y_tab(id, label, active)
        .h(height::INLINE)
        .px(space::MD)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .gap(space::XS)
        .on_click(move |_, w, cx| on_click(w, cx))
        .child(Icon::new(icon).size(px(14.0)).text_color(if active {
            palette.text_primary
        } else {
            palette.text_secondary
        }))
        .child(cjk_inline_label(label))
}

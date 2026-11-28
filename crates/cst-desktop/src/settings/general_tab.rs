use std::rc::Rc;

use cst_client::{
    DesktopSettingsDto, KeyboardFocusStyleDto, ThemeModeDto, UpdatePhaseDto, UpdateStateDto,
};
use gpui::{FocusHandle, KeyDownEvent, MouseMoveEvent};
use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::Icon;
use gpui_kit::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px, relative,
};

use crate::accessibility::A11yElementExt as _;
use crate::settings::update_dialog::{compute_update_progress_ratio, format_megabytes};
use crate::theme::kbd_focus::{
    KbdDynamicBoundsTracker, KbdDynamicSpec, KbdRailPlacement, render_gliding_dynamic_indicator,
    render_kbd_style_icon,
};
use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale, space};

pub type SelectThemeFn = Rc<dyn Fn(ThemeModeDto, &mut Window, &mut App)>;
pub type SelectKbdFocusStyleFn = Rc<dyn Fn(KeyboardFocusStyleDto, &mut Window, &mut App)>;
pub type UpdateCardActionFn = Rc<dyn Fn(&mut Window, &mut App)>;
pub type GeneralKeyDownFn = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;
pub type GeneralMouseMoveFn = Rc<dyn Fn(&MouseMoveEvent, &mut Window, &mut App)>;

pub struct GeneralTabKbdProps {
    pub focus_handle: FocusHandle,
    pub kbd_active: bool,
    pub row_idx: usize,
    pub tracker: KbdDynamicBoundsTracker,
    pub on_key_down: GeneralKeyDownFn,
    pub on_mouse_move: GeneralMouseMoveFn,
}

/// Renders the `常规` settings tab (`cst-pilot-desktop.pen` 「03 · 设置」 `q8TgB`):
/// Frameless full-width hairline rows (`52px`) with light-chip option groups (`28px` / `4px` radius,
/// `$bg-surface` + `$border-subtle` active, zero outer gray pill tracks), read-only `图形窗口`,
/// and `40px` separated `软件更新 当前 vX.Y.Z` hairline status row with `120×4px` progress track.
#[allow(clippy::too_many_arguments)]
pub fn render_general_tab(
    desktop_settings: &DesktopSettingsDto,
    current_version: &str,
    update_state: Option<&UpdateStateDto>,
    _mono_family: SharedString,
    palette: &DesktopPalette,
    kbd: GeneralTabKbdProps,
    window: &mut Window,
    cx: &mut App,
    on_select_theme: SelectThemeFn,
    on_select_kbd_focus_style: SelectKbdFocusStyleFn,
    on_check_or_open_update: UpdateCardActionFn,
    on_cancel_update: UpdateCardActionFn,
) -> impl IntoElement {
    kbd.tracker.clear();

    let phase = update_state
        .map(|u| u.status.phase)
        .unwrap_or(UpdatePhaseDto::Idle);
    let raw_ver = update_state
        .map(|u| u.current_version.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(current_version);
    let ver_badge = if raw_ver.starts_with('v') || raw_ver.starts_with('V') {
        format!("当前 {raw_ver}")
    } else {
        format!("当前 v{raw_ver}")
    };

    let progress_ratio = compute_update_progress_ratio(update_state);
    let in_flight = matches!(phase, UpdatePhaseDto::Checking | UpdatePhaseDto::Preparing);
    let restarting = matches!(
        phase,
        UpdatePhaseDto::Launching | UpdatePhaseDto::ExitRequired
    );
    let unavailable = phase == UpdatePhaseDto::Unavailable;
    let error_kind = update_state.and_then(|u| u.status.error_kind.as_deref());
    let manual_upgrade_required =
        phase == UpdatePhaseDto::Failed && error_kind == Some("manual_upgrade_required");

    let (status_title, status_detail): (SharedString, SharedString) = match phase {
        UpdatePhaseDto::Unavailable => {
            let detail = update_state
                .map(|u| u.status.message.trim())
                .filter(|s| !s.is_empty())
                .map(|msg| match msg {
                    "此宿主已关闭升级检查。" => "宿主已禁用",
                    "当前构建未启用在线更新。" => "未配置编译期更新源",
                    other => other,
                })
                .unwrap_or("未配置编译期更新源");
            ("未启用自动更新".into(), detail.to_owned().into())
        }
        UpdatePhaseDto::Checking => ("正在检查更新…".into(), "正在核验签名发布清单".into()),
        UpdatePhaseDto::Current => {
            let detail = update_state
                .map(|u| u.status.message.trim())
                .filter(|s| !s.is_empty() && *s != "当前已是最新版本。" && *s != "当前已是最新版本")
                .unwrap_or("");
            ("当前已是最新版本".into(), detail.to_owned().into())
        }
        UpdatePhaseDto::Available => {
            let target = update_state
                .and_then(|u| u.status.version.as_deref())
                .unwrap_or("新版本");
            (
                format!("发现新版本 {target}").into(),
                "可下载并重启升级".into(),
            )
        }
        UpdatePhaseDto::Preparing => {
            let target = update_state
                .and_then(|u| u.status.version.as_deref())
                .map(|v| {
                    if v.starts_with('v') || v.starts_with('V') {
                        format!("正在下载 {v}")
                    } else {
                        format!("正在下载 v{v}")
                    }
                })
                .unwrap_or_else(|| "正在下载更新包".to_owned());
            let dl = update_state.and_then(|u| u.status.downloaded_bytes);
            let tot = update_state.and_then(|u| u.status.archive_bytes);
            let detail = match (dl, tot, progress_ratio) {
                (Some(d), Some(t), Some(ratio)) => format!(
                    "{}% · {} / {}",
                    (ratio * 100.0).round() as u32,
                    format_megabytes(d),
                    format_megabytes(t)
                ),
                (Some(d), Some(t), None) => {
                    format!("{} / {}", format_megabytes(d), format_megabytes(t))
                }
                (_, Some(t), _) => format!("总计 {}", format_megabytes(t)),
                _ => "正在验签在线更新包…".to_owned(),
            };
            (target.into(), detail.into())
        }
        UpdatePhaseDto::Ready => ("更新包已就绪".into(), "校验通过，重启后完成升级".into()),
        UpdatePhaseDto::Launching | UpdatePhaseDto::ExitRequired => (
            "正在重启完成更新…".into(),
            "窗口即将自动重启并替换组件".into(),
        ),
        UpdatePhaseDto::Failed => {
            let title = match error_kind {
                Some("manual_upgrade_required") => "需下载完整发行包",
                Some("download_failed") => "下载更新失败",
                _ => "更新检查失败",
            };
            let msg = update_state
                .map(|u| u.status.message.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("检查或下载更新失败");
            (title.into(), msg.to_owned().into())
        }
        UpdatePhaseDto::Idle => ("检查签名更新".into(), "手动检查在线升级包".into()),
    };

    let tr_update_btn = kbd.tracker.clone();
    let kbd_active_for_update = kbd.kbd_active;
    let right_update_controls = if in_flight {
        let mut group = div().flex().items_center().gap(space::LG).flex_shrink_0();
        if let Some(ratio) = progress_ratio {
            group = group.child(
                div()
                    .w(px(120.0))
                    .h(px(4.0))
                    .rounded(radius::FULL)
                    .bg(palette.bg_track_recess)
                    .overflow_hidden()
                    .child(
                        div()
                            .id("settings-update-progress-fill-kednb")
                            .test_support()
                            .h_full()
                            .w(relative(ratio))
                            .rounded(radius::FULL)
                            .bg(palette.accent_solid),
                    ),
            );
        }
        if phase == UpdatePhaseDto::Preparing {
            group
                .child(
                    apply_scale(div(), scale::UI)
                        .id("settings-update-cancel-btn-ohwrt")
                        .test_support()
                        .a11y_button("settings-update-cancel-btn", "取消更新")
                        .h(height::COMPACT)
                        .px(px(10.0))
                        .mr(px(-10.0))
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .hover(move |s| {
                            if kbd_active_for_update {
                                s
                            } else {
                                s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                            }
                        })
                        .active(|s| s.bg(palette.bg_active_press))
                        .on_click(move |_, w, cx| (on_cancel_update)(w, cx))
                        .on_prepaint(move |bounds, _, _| tr_update_btn.set("gen-update", bounds))
                        .child(cjk_inline_label("取消")),
                )
                .into_any_element()
        } else {
            group
                .child(
                    apply_scale(div(), scale::UI)
                        .id("settings-update-checking-label")
                        .test_support()
                        .text_color(palette.text_muted)
                        .child(cjk_inline_label("检查中")),
                )
                .into_any_element()
        }
    } else if unavailable || restarting || manual_upgrade_required {
        let readonly_label = if unavailable {
            "未启用"
        } else if restarting {
            "重启中"
        } else {
            "手动换包"
        };
        apply_scale(div(), scale::UI)
            .id("settings-update-disabled-btn")
            .test_support()
            .text_color(palette.text_muted)
            .child(cjk_inline_label(readonly_label))
            .into_any_element()
    } else {
        let btn_label = match phase {
            UpdatePhaseDto::Available => "查看更新",
            UpdatePhaseDto::Ready => "重启更新",
            UpdatePhaseDto::Failed => "重试",
            _ => "检查更新",
        };
        apply_scale(div(), scale::UI_MEDIUM)
            .id("settings-update-action-btn")
            .test_support()
            .a11y_button("settings-update-action-btn", btn_label)
            .h(height::COMPACT)
            .px(px(10.0))
            .mr(px(-10.0))
            .rounded(radius::CONTROL)
            .text_color(palette.text_primary)
            .flex()
            .items_center()
            .hover(move |s| {
                if kbd_active_for_update {
                    s
                } else {
                    s.bg(palette.bg_item_hover)
                }
            })
            .active(|s| s.bg(palette.bg_active_press))
            .on_click(move |_, w, cx| (on_check_or_open_update)(w, cx))
            .on_prepaint(move |bounds, _, _| tr_update_btn.set("gen-update", bounds))
            .child(cjk_inline_label(btn_label))
            .into_any_element()
    };

    let update_row = div()
        .id("settings-update-card-c9or7k")
        .test_support()
        .a11y_region("settings-update-card", "软件更新")
        .w_full()
        .h(px(52.0))
        .border_b_1()
        .border_color(palette.border_subtle)
        .flex()
        .items_center()
        .justify_between()
        .gap(space::LG)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(space::MD)
                .child(
                    apply_scale(div(), scale::BODY)
                        .text_color(palette.text_primary)
                        .child(status_title),
                )
                .children((!status_detail.is_empty()).then(|| {
                    apply_scale(div(), scale::CAPTION)
                        .pt(px(2.0))
                        .min_w_0()
                        .truncate()
                        .text_color(if phase == UpdatePhaseDto::Failed {
                            palette.status_danger
                        } else {
                            palette.text_muted
                        })
                        .child(status_detail)
                })),
        )
        .child(right_update_controls);

    let t_sys = Rc::clone(&on_select_theme);
    let t_light = Rc::clone(&on_select_theme);
    let t_dark = Rc::clone(&on_select_theme);
    let k_brackets = Rc::clone(&on_select_kbd_focus_style);
    let k_rail = Rc::clone(&on_select_kbd_focus_style);
    let brackets_active = desktop_settings.keyboard_focus_style == KeyboardFocusStyleDto::Brackets;
    let rail_active = desktop_settings.keyboard_focus_style == KeyboardFocusStyleDto::Rail;

    let active_kbd_spec = if kbd.kbd_active {
        match kbd.row_idx {
            0 => {
                let key = match desktop_settings.theme {
                    ThemeModeDto::System => "gen-theme-system",
                    ThemeModeDto::Light => "gen-theme-light",
                    ThemeModeDto::Dark => "gen-theme-dark",
                };
                Some(KbdDynamicSpec::control(key, KbdRailPlacement::BottomCenter))
            }
            1 => {
                let key = if brackets_active {
                    "gen-kbd-brackets"
                } else {
                    "gen-kbd-rail"
                };
                Some(KbdDynamicSpec::control(key, KbdRailPlacement::BottomCenter))
            }
            2 if !unavailable => Some(KbdDynamicSpec::control(
                "gen-update",
                KbdRailPlacement::LeftOutside,
            )),
            _ => None,
        }
    } else {
        None
    };

    let on_key = kbd.on_key_down;
    let on_move = kbd.on_mouse_move;

    div()
        .id("settings-general-tab-q8tgb")
        .test_support()
        .a11y_region("settings-general-tab", "常规设置")
        .relative()
        .track_focus(&kbd.focus_handle)
        .tab_index(0)
        .on_key_down(move |ev, w, cx| (on_key)(ev, w, cx))
        .on_mouse_move(move |ev, w, cx| (on_move)(ev, w, cx))
        .w_full()
        .flex()
        .flex_col()
        // Frameless 52px hairline rows (no redundant H1 or `外观` subheading)
        .child(
            div()
                .w_full()
                .h(px(52.0))
                .border_b_1()
                .border_color(palette.border_subtle)
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
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .child(light_chip_icon(
                            "settings-theme-system",
                            "gen-theme-system",
                            IconName::Monitor,
                            "跟随系统",
                            desktop_settings.theme == ThemeModeDto::System,
                            kbd.kbd_active,
                            palette,
                            kbd.tracker.clone(),
                            move |w, cx| (t_sys)(ThemeModeDto::System, w, cx),
                        ))
                        .child(light_chip_icon(
                            "settings-theme-light",
                            "gen-theme-light",
                            IconName::Sun,
                            "浅色",
                            desktop_settings.theme == ThemeModeDto::Light,
                            kbd.kbd_active,
                            palette,
                            kbd.tracker.clone(),
                            move |w, cx| (t_light)(ThemeModeDto::Light, w, cx),
                        ))
                        .child(light_chip_icon(
                            "settings-theme-dark",
                            "gen-theme-dark",
                            IconName::Moon,
                            "深色",
                            desktop_settings.theme == ThemeModeDto::Dark,
                            kbd.kbd_active,
                            palette,
                            kbd.tracker.clone(),
                            move |w, cx| (t_dark)(ThemeModeDto::Dark, w, cx),
                        )),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(52.0))
                .border_b_1()
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    apply_scale(div(), scale::BODY)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("方向键选中样式")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .child(light_chip_custom(
                            "settings-kbd-focus-brackets",
                            "gen-kbd-brackets",
                            render_kbd_style_icon(
                                KeyboardFocusStyleDto::Brackets,
                                if brackets_active {
                                    palette.text_primary
                                } else {
                                    palette.text_secondary
                                },
                            ),
                            "四角",
                            brackets_active,
                            kbd.kbd_active,
                            palette,
                            kbd.tracker.clone(),
                            move |w, cx| (k_brackets)(KeyboardFocusStyleDto::Brackets, w, cx),
                        ))
                        .child(light_chip_custom(
                            "settings-kbd-focus-rail",
                            "gen-kbd-rail",
                            render_kbd_style_icon(
                                KeyboardFocusStyleDto::Rail,
                                if rail_active {
                                    palette.text_primary
                                } else {
                                    palette.text_secondary
                                },
                            ),
                            "游标",
                            rail_active,
                            kbd.kbd_active,
                            palette,
                            kbd.tracker.clone(),
                            move |w, cx| (k_rail)(KeyboardFocusStyleDto::Rail, w, cx),
                        )),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(52.0))
                .border_b_1()
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
                    apply_scale(div(), scale::UI)
                        .text_color(palette.text_secondary)
                        .child(cjk_inline_label("图形窗口")),
                ),
        )
        // 40px gap -> 软件更新 当前 vX.Y.Z
        .child(
            div()
                .mt(px(40.0))
                .flex()
                .flex_col()
                .gap(space::SM)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::MD)
                        .child(
                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                .text_color(palette.text_primary)
                                .child("软件更新"),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .pt(px(2.0))
                                .text_color(palette.text_muted)
                                .child(SharedString::from(ver_badge)),
                        ),
                )
                .child(update_row),
        )
        .child(render_gliding_dynamic_indicator(
            "settings-general",
            desktop_settings.keyboard_focus_style,
            active_kbd_spec,
            kbd.tracker,
            palette,
            window,
            cx,
        ))
}

#[allow(clippy::too_many_arguments)]
fn light_chip_icon(
    id: &'static str,
    track_key: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    kbd_active: bool,
    palette: &DesktopPalette,
    tracker: KbdDynamicBoundsTracker,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let icon_el = Icon::new(icon)
        .size(px(14.0))
        .text_color(if active {
            palette.text_primary
        } else {
            palette.text_secondary
        })
        .into_any_element();
    light_chip_custom(
        id, track_key, icon_el, label, active, kbd_active, palette, tracker, on_click,
    )
}

#[allow(clippy::too_many_arguments)]
fn light_chip_custom(
    id: &'static str,
    track_key: &'static str,
    icon_el: AnyElement,
    label: &'static str,
    active: bool,
    kbd_active: bool,
    palette: &DesktopPalette,
    tracker: KbdDynamicBoundsTracker,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let base = if active {
        apply_scale(div(), scale::UI_MEDIUM)
            .bg(palette.bg_surface)
            .border_1()
            .border_color(palette.border_subtle)
            .text_color(palette.text_primary)
    } else {
        apply_scale(div(), scale::UI)
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .bg(gpui_kit::transparent_black())
            .text_color(palette.text_secondary)
    };

    base.id(id)
        .test_support()
        .a11y_tab(id, label, active)
        .h(height::COMPACT)
        .px(px(10.0))
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .gap(space::TIGHT)
        .hover(move |s| {
            if active || kbd_active {
                s
            } else {
                s.bg(palette.bg_item_hover).text_color(palette.text_primary)
            }
        })
        .active(move |s| {
            if active {
                s
            } else {
                s.bg(palette.bg_active_press)
            }
        })
        .on_click(move |_, w, cx| on_click(w, cx))
        .on_prepaint(move |bounds, _, _| tracker.set(track_key, bounds))
        .child(icon_el)
        .child(cjk_inline_label(label))
}

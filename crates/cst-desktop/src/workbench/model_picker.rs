use std::f32::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use cst_client::{ModelChoiceDto, ModelsSnapshot};
use gpui::{Font, FontWeight, Hsla, Pixels, TextRun};
use gpui_kit::assets::IconName;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::Icon;
use gpui_kit::{
    App, CursorStyle, FocusHandle, InteractiveElement, IntoElement, ParentElement, PathBuilder,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, canvas, div, point,
    px, relative,
};
use serde_json::Value;

use crate::accessibility::A11yElementExt as _;
use crate::shell::open_url_in_trusted_browser;
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius,
    scale, space,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPickerPage {
    Main,
    Models,
    Thinking,
}

/// Preserves user-defined `enabled` scoped model ordering (`scoped-models.ts` parity).
pub fn resolve_scoped_models(snapshot: &ModelsSnapshot) -> Vec<ModelChoiceDto> {
    let Some(ref enabled) = snapshot.enabled else {
        return snapshot.models.clone();
    };
    let mut out = Vec::with_capacity(enabled.len());
    for pattern in enabled {
        if let Some(found) = snapshot
            .models
            .iter()
            .find(|m| format!("{}/{}", m.provider, m.id) == *pattern)
        {
            out.push(found.clone());
        }
    }
    out
}

pub fn thinking_level_label(raw: &str) -> &'static str {
    match raw.to_ascii_lowercase().as_str() {
        "off" => "Off",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" | "max" => "Max",
        _ => "Medium",
    }
}

pub fn is_quota_provider(provider: &str) -> bool {
    matches!(provider, "cstoa" | "deepseek" | "opencode-go")
}

/// Formats integer with thousands commas (`10,000`).
pub fn format_thousands_u64(n: u64) -> String {
    let raw = n.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (idx, ch) in raw.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// Formats `ProviderQuota` balance identically to `quota-format.ts`.
pub fn format_quota_balance(quota: &Value) -> Option<String> {
    if !quota
        .get("supported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let balance = quota.get("balance").and_then(Value::as_f64)?;
    if !balance.is_finite() {
        return None;
    }
    let provider = quota.get("provider").and_then(Value::as_str).unwrap_or("");
    match provider {
        "cstoa" => {
            if quota.get("unit").and_then(Value::as_str) != Some("credit") || balance < 0.0 {
                return None;
            }
            let credits = balance.round() as u64;
            Some(format!("{} 额度点", format_thousands_u64(credits)))
        }
        "deepseek" => {
            let currency = quota.get("currency").and_then(Value::as_str).unwrap_or("");
            match currency {
                "USD" => Some(format!("US$ {balance:.2}")),
                "CNY" => Some(format!("¥ {balance:.2}")),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Renders the `14×14px` context capacity ring icon (`cst-pilot-desktop.pen` 「01 · 工作台」 `r7fqB`).
pub fn render_context_ring_button(
    percent_opt: Option<f64>,
    trigger_focus: &FocusHandle,
    palette: &DesktopPalette,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let pct = percent_opt.unwrap_or(0.0).clamp(0.0, 100.0);
    let track_color = palette.border_strong;
    let arc_color = if pct >= 85.0 {
        palette.status_warning
    } else {
        palette.accent_solid
    };

    div()
        .id("composer-context-ring-btn")
        .test_support()
        .a11y_button(
            "composer-context-ring-btn",
            format!("上下文容量与额度（{pct:.0}%）"),
        )
        .track_focus(trigger_focus)
        .tab_index(0)
        .w(height::COMPACT)
        .h(height::COMPACT)
        .pt(space::MICRO)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(palette.bg_item_hover))
        .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, w, cx| on_toggle(w, cx))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _cx| {
                    let cx_f = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
                    let cy_f = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
                    let r = 5.5_f32;
                    let start_angle = -FRAC_PI_2;

                    let mut track_pts = Vec::with_capacity(48);
                    for i in 0..48 {
                        let theta = start_angle + TAU * (i as f32 / 48.0);
                        track_pts.push(point(
                            px(cx_f + r * theta.cos()),
                            px(cy_f + r * theta.sin()),
                        ));
                    }
                    let mut track_builder = PathBuilder::stroke(px(2.0));
                    track_builder.add_polygon(&track_pts, true);
                    if let Ok(path) = track_builder.build() {
                        window.paint_path(path, track_color);
                    }

                    if pct > 0.2 {
                        if pct >= 99.8 {
                            let mut full_builder = PathBuilder::stroke(px(2.0));
                            full_builder.add_polygon(&track_pts, true);
                            if let Ok(path) = full_builder.build() {
                                window.paint_path(path, arc_color);
                            }
                        } else {
                            let sweep = TAU * (pct as f32 / 100.0);
                            let steps = ((sweep * 12.0).ceil() as usize).max(2);
                            let mut arc_builder = PathBuilder::stroke(px(2.0));
                            for i in 0..=steps {
                                let theta = start_angle + sweep * (i as f32 / steps as f32);
                                let pt =
                                    point(px(cx_f + r * theta.cos()), px(cy_f + r * theta.sin()));
                                if i == 0 {
                                    arc_builder.move_to(pt);
                                } else {
                                    arc_builder.line_to(pt);
                                }
                            }
                            if let Ok(path) = arc_builder.build() {
                                window.paint_path(path, arc_color);
                            }
                        }
                    }
                },
            )
            .w(px(14.0))
            .h(px(14.0)),
        )
}

pub type SimpleUiFn = Rc<dyn Fn(&mut Window, &mut App)>;

/// Renders the `336px` context capacity & remaining quota popover (`cst-pilot-desktop.pen` 「01 · 工作台」 `h5jG6d`).
#[allow(clippy::too_many_arguments)]
pub fn render_context_quota_popover(
    interactive: bool,
    progress: f32,
    provider: &str,
    context_usage: Option<&Value>,
    usage_stats: Option<&Value>,
    quota: Option<&Value>,
    quota_loading: bool,
    _mono_family: SharedString,
    palette: &DesktopPalette,
    _transparency_enabled: bool,
    _scale_factor: f32,
    on_retry_quota: SimpleUiFn,
    on_open_account: SimpleUiFn,
) -> impl IntoElement {
    let tokens_opt = context_usage
        .and_then(|u| u.get("tokens"))
        .and_then(Value::as_u64);
    let window_tokens = context_usage
        .and_then(|u| u.get("contextWindow"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let percent_opt = context_usage
        .and_then(|u| u.get("percent"))
        .and_then(Value::as_f64);
    let cache_hit_opt = usage_stats
        .and_then(|u| u.get("cacheHitRate"))
        .and_then(Value::as_f64);

    let capacity_str = match tokens_opt {
        None => "用量未知".to_owned(),
        Some(toks) => {
            let pct_part = match percent_opt {
                Some(p) => format!("{p:.1}%"),
                None => "占比未知".to_owned(),
            };
            format!(
                "{} / {}（{pct_part}）",
                format_thousands_u64(toks),
                format_thousands_u64(window_tokens)
            )
        }
    };

    let cache_str = match cache_hit_opt {
        Some(rate) => format!("{:.1}%", rate * 100.0),
        None => "暂无缓存数据".to_owned(),
    };

    let bar_ratio = (percent_opt.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0) as f32;

    let mut card = div()
        .id(if interactive {
            "composer-context-popover-h5jg6d"
        } else {
            "composer-context-popover-h5jg6d-exit"
        })
        .relative()
        .top(px(6.0 * (1.0 - progress)))
        .opacity(0.01 + 0.99 * progress)
        .shadow(motion::scale_shadows(
            palette.flat_popover_shadows(),
            progress.powi(3),
        ))
        .w(px(336.0))
        .p(space::LG)
        .rounded(radius::ROW)
        .bg(palette.bg_glass)
        .border_1()
        .border_color(palette.border_subtle)
        .flex()
        .flex_col()
        .gap(space::SM)
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .text_color(palette.text_secondary)
                        .child("上下文容量"),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(palette.text_primary)
                        .child(SharedString::from(capacity_str)),
                ),
        )
        .child(
            div()
                .relative()
                .w_full()
                .h(px(6.0))
                .rounded(radius::FULL)
                .bg(palette.bg_surface)
                .overflow_hidden()
                .child(
                    div()
                        .w(relative(bar_ratio))
                        .h_full()
                        .rounded(radius::FULL)
                        .bg(if bar_ratio >= 0.85 {
                            palette.status_warning
                        } else {
                            palette.accent_solid
                        }),
                ),
        )
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(palette.text_secondary)
                        .child("平均缓存命中率"),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(palette.text_primary)
                        .child(SharedString::from(cache_str)),
                ),
        );

    if is_quota_provider(provider) {
        let is_external_more = provider != "cstoa";
        let prov_owned = provider.to_owned();
        let more_action: SimpleUiFn = if provider == "cstoa" {
            Rc::clone(&on_open_account)
        } else {
            Rc::new(move |_, _| {
                let url = if prov_owned == "deepseek" {
                    "https://platform.deepseek.com/"
                } else {
                    "https://opencode.ai/auth"
                };
                open_url_in_trusted_browser(url);
            })
        };

        let more_btn = apply_scale(div(), scale::CAPTION_MEDIUM)
            .id(if interactive {
                "context-popover-more-btn"
            } else {
                "context-popover-more-btn-exit"
            })
            .text_color(palette.accent_ink)
            .flex()
            .items_center()
            .gap(space::MICRO)
            .child(cjk_inline_label(if provider == "cstoa" {
                "账号信息"
            } else {
                "更多"
            }))
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(13.0))
                    .text_color(palette.accent_ink),
            );
        let more_btn = if interactive {
            let mut b = more_btn
                .test_support()
                .a11y_button(
                    "context-popover-more-btn",
                    if provider == "cstoa" {
                        "账号信息"
                    } else {
                        "更多额度信息"
                    },
                )
                .on_click(move |_, w, cx| (more_action)(w, cx));
            if is_external_more {
                b = b.cursor(CursorStyle::PointingHand);
            }
            b.into_any_element()
        } else {
            more_btn.into_any_element()
        };

        card = card
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(1.0))
                    .bg(palette.border_subtle),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child("剩余额度"),
                            )
                            .child(
                                Icon::new(IconName::Info)
                                    .size(px(13.0))
                                    .text_color(palette.text_muted),
                            ),
                    )
                    .child(more_btn),
            );

        if let Some(q) = quota {
            if let Some(bal) = format_quota_balance(q) {
                card = card.child(
                    apply_scale(div(), scale::BODY_SEMIBOLD)
                        .relative()
                        .text_color(palette.text_primary)
                        .child(SharedString::from(bal)),
                );
            }
            if provider == "opencode-go"
                && q.get("supported").and_then(Value::as_bool) == Some(true)
                && let Some(windows) = q.get("windows").and_then(Value::as_object)
            {
                for (key, label) in [
                    ("rolling", "5 小时"),
                    ("weekly", "本周"),
                    ("monthly", "本月"),
                ] {
                    if let Some(win) = windows.get(key) {
                        let status = win.get("status").and_then(Value::as_str).unwrap_or("");
                        let pct = win.get("percent").and_then(Value::as_f64).unwrap_or(0.0);
                        let rem = if status == "rate-limited" {
                            0
                        } else {
                            (100.0 - pct).round().max(0.0) as u64
                        };
                        card = card.child(
                            div()
                                .relative()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .text_color(palette.text_secondary)
                                        .child(label),
                                )
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .text_color(palette.text_primary)
                                        .child(SharedString::from(format!("剩余 {rem}%"))),
                                ),
                        );
                    }
                }
            }
            if q.get("supported").and_then(Value::as_bool) != Some(true) {
                let reason = if quota_loading {
                    "正在查询额度…".to_owned()
                } else {
                    q.get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("暂无额度数据")
                        .to_owned()
                };
                let requires_login = q
                    .get("requiresLogin")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let retry_fn = Rc::clone(&on_retry_quota);
                let mut row = div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_muted)
                            .child(SharedString::from(reason)),
                    );
                if !requires_login && !quota_loading {
                    let retry_el = apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id(if interactive {
                            "context-popover-retry-quota-btn"
                        } else {
                            "context-popover-retry-quota-btn-exit"
                        })
                        .text_color(palette.accent_solid)
                        .child("重试");
                    row = row.child(if interactive {
                        retry_el
                            .test_support()
                            .a11y_button("context-popover-retry-quota-btn", "重试查询额度")
                            .on_click(move |_, w, cx| (retry_fn)(w, cx))
                            .into_any_element()
                    } else {
                        retry_el.into_any_element()
                    });
                }
                card = card.child(row);
            }
        } else {
            card = card.child(
                apply_scale(div(), scale::CAPTION)
                    .relative()
                    .text_color(palette.text_muted)
                    .child(if quota_loading {
                        "正在查询额度…"
                    } else {
                        "暂无额度数据"
                    }),
            );
        }
    }

    if interactive {
        card.test_support()
            .occlude()
            .a11y_dialog("composer-context-popover-h5jg6d", "上下文容量与剩余额度")
            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .into_any_element()
    } else {
        card.into_any_element()
    }
}

pub type SelectModelFn = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
pub type SelectThinkingFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type ChangePickerPageFn = Rc<dyn Fn(ModelPickerPage, &mut Window, &mut App)>;

const PICKER_MIN_WIDTH: f32 = 224.0;
const PICKER_MAX_WIDTH: f32 = 320.0;
const PICKER_ROW_HEIGHT: f32 = 32.0;
const PICKER_BACK_HEIGHT: f32 = 28.0;
const PICKER_ROW_GAP: f32 = 2.0;
const PICKER_CARD_CHROME_H: f32 = 10.0; // space::XS (4px) * 2 + 1px border * 2
const PICKER_CARD_CHROME_V: f32 = 10.0; // space::XS (4px) * 2 + 1px border * 2
const PICKER_ROW_PADDING_H: f32 = 16.0; // space::SM (8px) * 2

fn shape_text_width(
    text: &str,
    font_size: Pixels,
    weight: FontWeight,
    family: &SharedString,
    window: &Window,
) -> f32 {
    // Custom model metadata may contain line breaks; shape_line only accepts one line.
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let run = TextRun {
                len: line.len(),
                font: Font {
                    family: family.clone(),
                    weight,
                    ..Default::default()
                },
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .shape_line(line.to_owned().into(), font_size, &[run], None)
                    .width,
            )
            .ceil()
        })
        .fold(0.0, f32::max)
}

fn compute_picker_height(
    page: ModelPickerPage,
    model_count: usize,
    thinking_count: usize,
    viewport_height: f32,
) -> f32 {
    let max_height = 440.0_f32.min((viewport_height * 0.6).max(120.0));
    let raw_height = match page {
        ModelPickerPage::Main => 2.0 * PICKER_ROW_HEIGHT + PICKER_ROW_GAP + PICKER_CARD_CHROME_V,
        ModelPickerPage::Models => {
            let rows = model_count.max(1) as f32;
            PICKER_BACK_HEIGHT
                + PICKER_ROW_GAP
                + rows * PICKER_ROW_HEIGHT
                + (rows - 1.0).max(0.0) * PICKER_ROW_GAP
                + PICKER_CARD_CHROME_V
        }
        ModelPickerPage::Thinking => {
            let rows = thinking_count.max(1) as f32;
            PICKER_BACK_HEIGHT
                + PICKER_ROW_GAP
                + rows * PICKER_ROW_HEIGHT
                + (rows - 1.0).max(0.0) * PICKER_ROW_GAP
                + PICKER_CARD_CHROME_V
        }
    };
    raw_height.min(max_height).ceil()
}

#[allow(clippy::too_many_arguments)]
fn measure_model_picker_dimensions(
    page: ModelPickerPage,
    scoped_models: &[ModelChoiceDto],
    current_model_label: &str,
    thinking_levels: &[String],
    current_thinking: &str,
    viewport_width: f32,
    viewport_height: f32,
    window: &Window,
    cx: &App,
) -> (f32, f32) {
    let ui_family = DesktopThemeGlobal::current(cx).ui_font_family;
    let max_width = (viewport_width - 32.0).max(180.0);
    let effective_thinking_count = if thinking_levels.is_empty() {
        4
    } else {
        thinking_levels.len()
    };

    let raw_width = match page {
        ModelPickerPage::Main => {
            let model_key_w = shape_text_width(
                "模型",
                scale::CAPTION_MEDIUM.font_size,
                scale::CAPTION_MEDIUM.weight,
                &ui_family,
                window,
            );
            let model_val_w = shape_text_width(
                current_model_label,
                scale::CAPTION_MEDIUM.font_size,
                scale::CAPTION_MEDIUM.weight,
                &ui_family,
                window,
            )
            .min(180.0);
            let think_key_w = shape_text_width(
                "思考强度",
                scale::CAPTION_MEDIUM.font_size,
                scale::CAPTION_MEDIUM.weight,
                &ui_family,
                window,
            );
            let think_val_w = shape_text_width(
                thinking_level_label(current_thinking),
                scale::CAPTION_MEDIUM.font_size,
                scale::CAPTION_MEDIUM.weight,
                &ui_family,
                window,
            );
            // Row chrome: left/right padding (16) + min gap (12) + xs gap (4) + 13px chevron + card chrome (10)
            let row_chrome = PICKER_ROW_PADDING_H + 12.0 + 4.0 + 13.0 + PICKER_CARD_CHROME_H + 4.0;
            let needed = (model_key_w + model_val_w).max(think_key_w + think_val_w) + row_chrome;
            needed.clamp(PICKER_MIN_WIDTH, PICKER_MAX_WIDTH)
        }
        ModelPickerPage::Models => {
            let back_w = PICKER_ROW_PADDING_H
                + 13.0
                + 4.0
                + shape_text_width(
                    "返回",
                    scale::CAPTION_MEDIUM.font_size,
                    scale::CAPTION_MEDIUM.weight,
                    &ui_family,
                    window,
                )
                + PICKER_CARD_CHROME_H;
            let rows_w = if scoped_models.is_empty() {
                shape_text_width(
                    "暂无已就绪模型",
                    scale::CAPTION.font_size,
                    scale::CAPTION.weight,
                    &ui_family,
                    window,
                ) + PICKER_ROW_PADDING_H
                    + PICKER_CARD_CHROME_H
            } else {
                scoped_models
                    .iter()
                    .map(|m| {
                        let label = if m.name.trim().is_empty() {
                            m.id.as_str()
                        } else {
                            m.name.as_str()
                        };
                        let label_w = shape_text_width(
                            label,
                            scale::CAPTION_MEDIUM.font_size,
                            scale::CAPTION_MEDIUM.weight,
                            &ui_family,
                            window,
                        );
                        let provider_w = shape_text_width(
                            &m.provider,
                            px(11.0),
                            FontWeight::NORMAL,
                            &ui_family,
                            window,
                        );
                        // Row chrome: row px (16) + between gap (12) + xs gap (4) + 13px check slot + card chrome (10) + safety (4)
                        label_w
                            + provider_w
                            + PICKER_ROW_PADDING_H
                            + 12.0
                            + 4.0
                            + 13.0
                            + PICKER_CARD_CHROME_H
                            + 4.0
                    })
                    .fold(0.0_f32, f32::max)
            };
            rows_w.max(back_w).clamp(PICKER_MIN_WIDTH, PICKER_MAX_WIDTH)
        }
        ModelPickerPage::Thinking => {
            let fallback = ["low", "medium", "high", "max"];
            let rows_w = if thinking_levels.is_empty() {
                fallback
                    .iter()
                    .map(|lv| {
                        shape_text_width(
                            thinking_level_label(lv),
                            scale::CAPTION_MEDIUM.font_size,
                            scale::CAPTION_MEDIUM.weight,
                            &ui_family,
                            window,
                        )
                    })
                    .fold(0.0_f32, f32::max)
            } else {
                thinking_levels
                    .iter()
                    .map(|lv| {
                        shape_text_width(
                            thinking_level_label(lv),
                            scale::CAPTION_MEDIUM.font_size,
                            scale::CAPTION_MEDIUM.weight,
                            &ui_family,
                            window,
                        )
                    })
                    .fold(0.0_f32, f32::max)
            };
            (rows_w + PICKER_ROW_PADDING_H + 12.0 + 13.0 + PICKER_CARD_CHROME_H + 4.0)
                .clamp(PICKER_MIN_WIDTH, PICKER_MAX_WIDTH)
        }
    };

    let height = compute_picker_height(
        page,
        scoped_models.len(),
        effective_thinking_count,
        viewport_height,
    );
    (raw_width.min(max_width).ceil(), height)
}

/// Deterministic fallback formula used in pure unit tests without a GPUI text system.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn compute_model_picker_dimensions(
    page: ModelPickerPage,
    scoped_models: &[ModelChoiceDto],
    thinking_count: usize,
    viewport_width: f32,
    viewport_height: f32,
) -> (f32, f32) {
    let max_width = (viewport_width - 32.0).max(180.0);
    let raw_width = match page {
        ModelPickerPage::Main | ModelPickerPage::Thinking => PICKER_MIN_WIDTH,
        ModelPickerPage::Models => {
            let longest_px = scoped_models
                .iter()
                .map(|m| {
                    let label = if m.name.trim().is_empty() {
                        m.id.as_str()
                    } else {
                        m.name.as_str()
                    };
                    let label_w = label.chars().fold(0.0_f32, |acc, ch| {
                        acc + if ch.is_ascii() { 7.2 } else { 12.5 }
                    });
                    let provider_w = m.provider.chars().fold(0.0_f32, |acc, ch| {
                        acc + if ch.is_ascii() { 6.2 } else { 11.0 }
                    });
                    label_w
                        + provider_w
                        + PICKER_ROW_PADDING_H
                        + 12.0
                        + 4.0
                        + 13.0
                        + PICKER_CARD_CHROME_H
                        + 4.0
                })
                .fold(0.0_f32, f32::max);
            longest_px.clamp(PICKER_MIN_WIDTH, PICKER_MAX_WIDTH)
        }
    };

    let height = compute_picker_height(page, scoped_models.len(), thinking_count, viewport_height);
    (raw_width.min(max_width).ceil(), height)
}

/// Renders the compact two-level model & thinking strength popover (`cst-pilot-desktop.pen` 「01 · 工作台」
/// `Av2qG` / `d8zzg` + secondary thinking strength list `L7GWjV`, highest level `Max`).
#[allow(clippy::too_many_arguments)]
pub fn render_model_picker_popover(
    interactive: bool,
    progress: f32,
    open_epoch: u64,
    page: ModelPickerPage,
    highlighted_idx: usize,
    scroll_handle: &gpui_kit::ScrollHandle,
    scoped_models: &[ModelChoiceDto],
    current_provider: &str,
    current_model_id: &str,
    current_model_label: &str,
    thinking_levels: &[String],
    current_thinking: &str,
    palette: &DesktopPalette,
    _transparency_enabled: bool,
    _scale_factor: f32,
    on_goto_page: ChangePickerPageFn,
    on_select_model: SelectModelFn,
    on_select_thinking: SelectThinkingFn,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let viewport = window.viewport_size();
    let (target_width, target_height) = measure_model_picker_dimensions(
        page,
        scoped_models,
        current_model_label,
        thinking_levels,
        current_thinking,
        f32::from(viewport.width).max(360.0),
        f32::from(viewport.height).max(360.0),
        window,
        cx,
    );
    let anim_width = transition(
        SharedString::from(format!("model-picker-resize-w-{open_epoch}")),
        target_width,
        Transition::new(motion::RESIZE).ease(motion::ease_out()),
        window,
        cx,
    );
    let anim_height = transition(
        SharedString::from(format!("model-picker-resize-h-{open_epoch}")),
        target_height,
        Transition::new(motion::RESIZE).ease(motion::ease_out()),
        window,
        cx,
    );

    let card = div()
        .id(if interactive {
            "composer-model-picker-d8zzg"
        } else {
            "composer-model-picker-d8zzg-exit"
        })
        .relative()
        .top(px(6.0 * (1.0 - progress)))
        .opacity(0.01 + 0.99 * progress)
        .shadow(motion::scale_shadows(
            palette.flat_popover_shadows(),
            progress.powi(3),
        ))
        .w(px(anim_width))
        .h(px(anim_height))
        .rounded(radius::ROW)
        .bg(palette.bg_glass)
        .border_1()
        .border_color(palette.border_subtle)
        .overflow_hidden();

    // Anchor inner content to the fixed bottom-right corner at its target dimensions so only the
    // outer border/background resizes between pages while text stays stationary without reflow/slide.
    let inner_w = (target_width - PICKER_CARD_CHROME_H).max(1.0);
    let inner_h = (target_height - PICKER_CARD_CHROME_V).max(1.0);
    let mut page_body = div()
        .absolute()
        .right(space::XS)
        .bottom(space::XS)
        .w(px(inner_w))
        .h(px(inner_h))
        .flex()
        .flex_col()
        .gap(space::MICRO);

    match page {
        ModelPickerPage::Main => {
            let goto_m = Rc::clone(&on_goto_page);
            let goto_t = Rc::clone(&on_goto_page);
            let models_row = apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(if interactive {
                    "model-picker-goto-models"
                } else {
                    "model-picker-goto-models-exit"
                })
                .relative()
                .h(height::CONTROL)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .bg(if highlighted_idx == 0 {
                    palette.bg_item_active
                } else {
                    gpui_kit::transparent_black()
                })
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    div()
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("模型")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .min_w_0()
                        .child(
                            div()
                                .max_w(px(180.0))
                                .truncate()
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(current_model_label.to_owned())),
                        )
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(13.0))
                                .text_color(palette.text_muted),
                        ),
                );
            let thinking_row = apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(if interactive {
                    "model-picker-goto-thinking"
                } else {
                    "model-picker-goto-thinking-exit"
                })
                .relative()
                .h(height::CONTROL)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .bg(if highlighted_idx == 1 {
                    palette.bg_item_active
                } else {
                    gpui_kit::transparent_black()
                })
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    div()
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("思考强度")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .child(
                            div()
                                .text_color(palette.text_secondary)
                                .child(thinking_level_label(current_thinking)),
                        )
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(13.0))
                                .text_color(palette.text_muted),
                        ),
                );
            page_body = page_body
                .child(if interactive {
                    models_row
                        .test_support()
                        .a11y_menuitem(
                            "model-picker-goto-models",
                            format!("模型：{current_model_label}"),
                        )
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| (goto_m)(ModelPickerPage::Models, w, cx))
                        .into_any_element()
                } else {
                    models_row.into_any_element()
                })
                .child(if interactive {
                    thinking_row
                        .test_support()
                        .a11y_menuitem(
                            "model-picker-goto-thinking",
                            format!("思考强度：{}", thinking_level_label(current_thinking)),
                        )
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| (goto_t)(ModelPickerPage::Thinking, w, cx))
                        .into_any_element()
                } else {
                    thinking_row.into_any_element()
                });
        }
        ModelPickerPage::Models => {
            let back = Rc::clone(&on_goto_page);
            let back_btn = apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(if interactive {
                    "model-picker-back-btn"
                } else {
                    "model-picker-back-btn-exit"
                })
                .relative()
                .h(height::COMPACT)
                .px(space::SM)
                .text_color(palette.text_secondary)
                .flex()
                .items_center()
                .gap(space::XS)
                .child(Icon::new(IconName::ChevronLeft).size(px(13.0)))
                .child(cjk_inline_label("返回"));
            page_body = page_body.child(if interactive {
                back_btn
                    .test_support()
                    .a11y_button("model-picker-back-btn", "返回上级菜单")
                    .hover(|s| s.text_color(palette.text_primary))
                    .on_click(move |_, w, cx| (back)(ModelPickerPage::Main, w, cx))
                    .into_any_element()
            } else {
                back_btn.into_any_element()
            });

            let mut scroll = div()
                .id(if interactive {
                    "model-picker-models-scroll"
                } else {
                    "model-picker-models-scroll-exit"
                })
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(space::MICRO);
            if interactive {
                scroll = scroll.track_scroll(scroll_handle).overflow_y_scroll();
            } else {
                scroll = scroll.overflow_hidden();
            }

            if scoped_models.is_empty() {
                scroll = scroll.child(
                    apply_scale(div(), scale::CAPTION)
                        .px(space::SM)
                        .py(space::SM)
                        .text_color(palette.text_muted)
                        .child("暂无已就绪模型"),
                );
            } else {
                for (idx, m) in scoped_models.iter().enumerate() {
                    let is_cur = m.provider == current_provider && m.id == current_model_id;
                    let is_hi = idx == highlighted_idx;
                    let prov = m.provider.clone();
                    let prov_display = SharedString::from(m.provider.clone());
                    let mid = m.id.clone();
                    let sel_fn = Rc::clone(&on_select_model);
                    let label = if m.name.trim().is_empty() {
                        m.id.clone()
                    } else {
                        m.name.clone()
                    };
                    let opt_id = SharedString::from(format!("model-option-{prov}-{mid}"));
                    let row = apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id(if interactive {
                            opt_id.clone()
                        } else {
                            SharedString::from(format!("model-option-{prov}-{mid}-exit"))
                        })
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .bg(if is_hi {
                            palette.bg_item_active
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::SM)
                        .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(space::XS)
                                .flex_shrink_0()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .line_height(px(16.0))
                                        .text_color(palette.text_muted)
                                        .child(prov_display),
                                )
                                .children(is_cur.then(|| {
                                    Icon::new(IconName::Check)
                                        .size(px(13.0))
                                        .text_color(palette.accent_ink)
                                })),
                        );
                    scroll = scroll.child(if interactive {
                        row.test_support()
                            .a11y_listitem(opt_id, format!("{prov} · {label}"), is_cur)
                            .hover(|s| s.bg(palette.bg_item_active))
                            .on_click(move |_, w, cx| {
                                (sel_fn)(prov.clone(), mid.clone(), w, cx);
                            })
                            .into_any_element()
                    } else {
                        row.into_any_element()
                    });
                }
            }
            page_body = page_body.child(scroll);
        }
        ModelPickerPage::Thinking => {
            let back = Rc::clone(&on_goto_page);
            let back_btn = apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(if interactive {
                    "thinking-picker-back-btn"
                } else {
                    "thinking-picker-back-btn-exit"
                })
                .relative()
                .h(height::COMPACT)
                .px(space::SM)
                .text_color(palette.text_secondary)
                .flex()
                .items_center()
                .gap(space::XS)
                .child(Icon::new(IconName::ChevronLeft).size(px(13.0)))
                .child(cjk_inline_label("返回"));
            page_body = page_body.child(if interactive {
                back_btn
                    .test_support()
                    .a11y_button("thinking-picker-back-btn", "返回上级菜单")
                    .hover(|s| s.text_color(palette.text_primary))
                    .on_click(move |_, w, cx| (back)(ModelPickerPage::Main, w, cx))
                    .into_any_element()
            } else {
                back_btn.into_any_element()
            });

            let levels: Vec<String> = if thinking_levels.is_empty() {
                vec![
                    "low".to_owned(),
                    "medium".to_owned(),
                    "high".to_owned(),
                    "max".to_owned(),
                ]
            } else {
                thinking_levels.to_vec()
            };
            let mut list = div()
                .id(if interactive {
                    "thinking-picker-list-l7gwjv"
                } else {
                    "thinking-picker-list-l7gwjv-exit"
                })
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(space::MICRO);
            if interactive {
                list = list.overflow_y_scroll();
            } else {
                list = list.overflow_hidden();
            }

            for (idx, lv) in levels.iter().enumerate() {
                let is_cur = lv.eq_ignore_ascii_case(current_thinking);
                let is_hi = idx == highlighted_idx;
                let lv_owned = lv.clone();
                let sel_fn = Rc::clone(&on_select_thinking);
                let opt_id =
                    SharedString::from(format!("thinking-option-{}", lv.to_ascii_lowercase()));
                let lv_label = thinking_level_label(lv);
                let mut row = apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(if interactive {
                        opt_id.clone()
                    } else {
                        SharedString::from(format!(
                            "thinking-option-{}-exit",
                            lv.to_ascii_lowercase()
                        ))
                    })
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .bg(if is_hi {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(thinking_level_label(lv));
                if is_cur {
                    row = row.child(
                        Icon::new(IconName::Check)
                            .size(px(13.0))
                            .text_color(palette.accent_ink),
                    );
                }
                list = list.child(if interactive {
                    row.test_support()
                        .a11y_listitem(opt_id, lv_label, is_cur)
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| {
                            (sel_fn)(lv_owned.clone(), w, cx);
                        })
                        .into_any_element()
                } else {
                    row.into_any_element()
                });
            }
            page_body = page_body.child(if interactive {
                list.test_support().into_any_element()
            } else {
                list.into_any_element()
            });
        }
    }

    let card = card.child(page_body);
    if interactive {
        card.test_support()
            .occlude()
            .a11y_dialog("composer-model-picker-d8zzg", "模型与思考强度选择")
            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .into_any_element()
    } else {
        card.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_model(id: &str, name: &str) -> ModelChoiceDto {
        serde_json::from_value(serde_json::json!({
            "provider": "cstoa",
            "id": id,
            "name": name,
            "reasoning": true,
            "thinkingLevels": ["low", "medium", "high"],
            "contextWindow": 128000,
            "catalogPresent": true,
            "credentialReady": true
        }))
        .expect("valid sample ModelChoiceDto")
    }

    #[test]
    fn model_picker_dimensions_match_web_height_cap_and_stable_width() {
        let short_models = vec![sample_model("m1", "Mini"), sample_model("m2", "Flash")];
        let (mw, mh) =
            compute_model_picker_dimensions(ModelPickerPage::Main, &short_models, 4, 1280.0, 800.0);
        let (tw, th) = compute_model_picker_dimensions(
            ModelPickerPage::Thinking,
            &short_models,
            4,
            1280.0,
            800.0,
        );
        assert_eq!(mw, 224.0);
        assert_eq!(tw, 224.0);
        assert_eq!(mh, 76.0);
        assert!(th > mh);

        let many_models: Vec<ModelChoiceDto> = (0..25)
            .map(|i| sample_model(&format!("m{i}"), "DeepSeek V4.1 Long Display Name"))
            .collect();
        let (lw, lh) = compute_model_picker_dimensions(
            ModelPickerPage::Models,
            &many_models,
            4,
            1280.0,
            600.0,
        );
        assert!((224.0..=320.0).contains(&lw));
        // Capped at min(440, 60vh) = 360px when viewport height is 600px
        assert_eq!(lh, 360.0);
    }
}

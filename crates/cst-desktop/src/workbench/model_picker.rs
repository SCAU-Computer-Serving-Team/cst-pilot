use std::f32::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use cst_client::{ModelChoiceDto, ModelsSnapshot};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, CursorStyle, FocusHandle, InteractiveElement, IntoElement, ParentElement, PathBuilder,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, canvas, div, point,
    px, relative,
};
use serde_json::Value;

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::shell::open_url_in_trusted_browser;
use crate::theme::{
    A11yElementExt as _, DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale,
    space,
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
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(palette.bg_item_hover))
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
    provider: &str,
    context_usage: Option<&Value>,
    usage_stats: Option<&Value>,
    quota: Option<&Value>,
    quota_loading: bool,
    mono_family: SharedString,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
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

    let mut card = render_frosted_surface(
        "composer-context-popover-h5jg6d",
        FrostedGlassSpec::popover_h2p2z(palette),
        palette.bg_elevated,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .a11y_dialog("composer-context-popover-h5jg6d", "上下文容量与剩余额度")
    .w(px(336.0))
    .p(space::LG)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_hairline)
    .flex()
    .flex_col()
    .gap(space::SM)
    .on_click(|_, _, cx| cx.stop_propagation())
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
                    .font_family(mono_family.clone())
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
            .bg(palette.bg_track_recess)
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
                    .font_family(mono_family.clone())
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

        let mut more_btn = apply_scale(div(), scale::CAPTION_MEDIUM)
            .id("context-popover-more-btn")
            .test_support()
            .a11y_button(
                "context-popover-more-btn",
                if provider == "cstoa" {
                    "账号信息"
                } else {
                    "更多额度信息"
                },
            )
            .text_color(palette.accent_ink)
            .flex()
            .items_center()
            .gap(space::MICRO)
            .on_click(move |_, w, cx| (more_action)(w, cx))
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
        if is_external_more {
            more_btn = more_btn.cursor(CursorStyle::PointingHand);
        }

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
                        .font_family(mono_family.clone())
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
                                        .font_family(mono_family.clone())
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
                    row = row.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id("context-popover-retry-quota-btn")
                            .test_support()
                            .a11y_button("context-popover-retry-quota-btn", "重试查询额度")
                            .text_color(palette.accent_solid)
                            .on_click(move |_, w, cx| (retry_fn)(w, cx))
                            .child("重试"),
                    );
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

    card
}

pub type SelectModelFn = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
pub type SelectThinkingFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type ChangePickerPageFn = Rc<dyn Fn(ModelPickerPage, &mut Window, &mut App)>;

/// Renders the compact two-level model & thinking strength popover (`cst-pilot-desktop.pen` 「01 · 工作台」
/// `Av2qG` / `d8zzg` + secondary thinking strength list `L7GWjV`, highest level `Max`).
#[allow(clippy::too_many_arguments)]
pub fn render_model_picker_popover(
    page: ModelPickerPage,
    highlighted_idx: usize,
    scoped_models: &[ModelChoiceDto],
    current_provider: &str,
    current_model_id: &str,
    current_model_label: &str,
    thinking_levels: &[String],
    current_thinking: &str,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    on_goto_page: ChangePickerPageFn,
    on_select_model: SelectModelFn,
    on_select_thinking: SelectThinkingFn,
) -> impl IntoElement {
    let pop_width = match page {
        ModelPickerPage::Main => px(224.0),
        ModelPickerPage::Models => px(248.0),
        ModelPickerPage::Thinking => px(148.0),
    };

    let mut card = render_frosted_surface(
        "composer-model-picker-d8zzg",
        FrostedGlassSpec::popover_h2p2z(palette),
        palette.bg_elevated,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .a11y_dialog("composer-model-picker-d8zzg", "模型与思考强度选择")
    .w(pop_width)
    .p(space::XS)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_hairline)
    .flex()
    .flex_col()
    .gap(space::MICRO)
    .on_click(|_, _, cx| cx.stop_propagation());

    match page {
        ModelPickerPage::Main => {
            let goto_m = Rc::clone(&on_goto_page);
            let goto_t = Rc::clone(&on_goto_page);
            card = card
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id("model-picker-goto-models")
                        .test_support()
                        .a11y_menuitem(
                            "model-picker-goto-models",
                            format!("模型：{current_model_label}"),
                        )
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
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| (goto_m)(ModelPickerPage::Models, w, cx))
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
                                        .max_w(px(124.0))
                                        .truncate()
                                        .text_color(palette.text_secondary)
                                        .child(SharedString::from(current_model_label.to_owned())),
                                )
                                .child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(px(13.0))
                                        .text_color(palette.text_muted),
                                ),
                        ),
                )
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id("model-picker-goto-thinking")
                        .test_support()
                        .a11y_menuitem(
                            "model-picker-goto-thinking",
                            format!("思考强度：{}", thinking_level_label(current_thinking)),
                        )
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
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| (goto_t)(ModelPickerPage::Thinking, w, cx))
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
                        ),
                );
        }
        ModelPickerPage::Models => {
            let back = Rc::clone(&on_goto_page);
            card = card
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id("model-picker-back-btn")
                        .test_support()
                        .a11y_button("model-picker-back-btn", "返回上级菜单")
                        .relative()
                        .h(height::COMPACT)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| (back)(ModelPickerPage::Main, w, cx))
                        .child(
                            Icon::new(IconName::ChevronLeft)
                                .size(px(13.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("返回")),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(1.0))
                        .bg(palette.border_subtle),
                );

            let mut scroll = div()
                .id("model-picker-models-scroll")
                .relative()
                .max_h(px(260.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(space::MICRO);

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
                    let mid = m.id.clone();
                    let sel_fn = Rc::clone(&on_select_model);
                    let label = if m.name.trim().is_empty() {
                        m.id.clone()
                    } else {
                        m.name.clone()
                    };
                    let opt_id = SharedString::from(format!("model-option-{prov}-{mid}"));
                    let mut row = apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id(opt_id.clone())
                        .test_support()
                        .a11y_listitem(opt_id, label.clone(), is_cur)
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
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| {
                            (sel_fn)(prov.clone(), mid.clone(), w, cx);
                        })
                        .child(div().flex_1().min_w_0().truncate().child(label));
                    if is_cur {
                        row = row.child(
                            Icon::new(IconName::Check)
                                .size(px(13.0))
                                .text_color(palette.accent_ink),
                        );
                    }
                    scroll = scroll.child(row);
                }
            }
            card = card.child(scroll);
        }
        ModelPickerPage::Thinking => {
            let back = Rc::clone(&on_goto_page);
            card = card
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .id("thinking-picker-back-btn")
                        .test_support()
                        .a11y_button("thinking-picker-back-btn", "返回上级菜单")
                        .relative()
                        .h(height::COMPACT)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, w, cx| (back)(ModelPickerPage::Main, w, cx))
                        .child(
                            Icon::new(IconName::ChevronLeft)
                                .size(px(13.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("返回")),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(1.0))
                        .bg(palette.border_subtle),
                );

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
                .id("thinking-picker-list-l7gwjv")
                .test_support()
                .relative()
                .flex()
                .flex_col()
                .gap(space::MICRO);

            for (idx, lv) in levels.iter().enumerate() {
                let is_cur = lv.eq_ignore_ascii_case(current_thinking);
                let is_hi = idx == highlighted_idx;
                let lv_owned = lv.clone();
                let sel_fn = Rc::clone(&on_select_thinking);
                let opt_id =
                    SharedString::from(format!("thinking-option-{}", lv.to_ascii_lowercase()));
                let lv_label = thinking_level_label(lv);
                let mut row = apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(opt_id.clone())
                    .test_support()
                    .a11y_listitem(opt_id, lv_label, is_cur)
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
                    .hover(|s| s.bg(palette.bg_item_active))
                    .on_click(move |_, w, cx| {
                        (sel_fn)(lv_owned.clone(), w, cx);
                    })
                    .child(thinking_level_label(lv));
                if is_cur {
                    row = row.child(
                        Icon::new(IconName::Check)
                            .size(px(13.0))
                            .text_color(palette.accent_ink),
                    );
                }
                list = list.child(row);
            }
            card = card.child(list);
        }
    }

    card
}

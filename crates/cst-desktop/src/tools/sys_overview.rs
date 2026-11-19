use cst_client::tool_presenter::{FieldRow, SpecialStatus};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, TestSupportExt, div, px,
};

use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale, space};

pub struct SysOverviewProps<'a> {
    pub call_id: &'a str,
    pub status: SpecialStatus,
    pub title: &'a str,
    pub summary: &'a str,
    pub machine: &'a str,
    pub uptime: &'a str,
    pub cpu_pct: Option<f64>,
    pub cores: Option<i64>,
    pub memory_pct: Option<f64>,
    pub capacity: &'a str,
    pub fields: &'a [FieldRow],
    pub errors: &'a [String],
    pub details_expanded: bool,
}

/// Renders the standalone System Overview persistent card (`sys` with `scope == "overview"`,
/// `cst-pilot-desktop.pen` 「07 · 工具与特殊工具」 `$bg-special` `12px` card).
/// Consolidates motherboard/BIOS, pagefile, kernel pools, and partial degradation notes into a single `"详情"` disclosure.
pub fn render_sys_overview_card(
    props: SysOverviewProps<'_>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    on_toggle_details: crate::tools::ToggleKeyFn,
) -> gpui_kit::AnyElement {
    let status_badge = match props.status {
        SpecialStatus::Running | SpecialStatus::Pending => ("采集中", palette.accent_solid),
        SpecialStatus::Degraded => ("部分降级", palette.status_warning),
        SpecialStatus::Error => ("采集失败", palette.status_danger),
        SpecialStatus::Interrupted => ("已中断", palette.status_warning),
        _ => ("已完成", palette.status_success),
    };

    let cpu_val = props.cpu_pct.unwrap_or(0.0).clamp(0.0, 100.0);
    let mem_val = props.memory_pct.unwrap_or(0.0).clamp(0.0, 100.0);

    let cpu_subtitle = match (props.cpu_pct, props.cores) {
        (Some(p), Some(c)) => format!("{p:.1}% · {c} 核"),
        (Some(p), None) => format!("{p:.1}%"),
        _ => "未采集".to_owned(),
    };
    let mem_subtitle = match (props.memory_pct, props.capacity.trim().is_empty()) {
        (Some(p), false) => format!("{p:.1}% · {}", props.capacity),
        (Some(p), true) => format!("{p:.1}%"),
        (None, false) => props.capacity.to_owned(),
        (None, true) => "未采集".to_owned(),
    };

    let has_details = !props.fields.is_empty() || !props.errors.is_empty();
    let detail_key = format!("sys-overview-details-{}", props.call_id);

    let mut card = div()
        .id(SharedString::from(format!(
            "special-sys-overview-{}",
            props.call_id
        )))
        .test_support()
        .w_full()
        .p(space::LG)
        .rounded(radius::SPECIAL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_special)
        .flex()
        .flex_col()
        .gap(space::MD)
        // Top header: title + machine & uptime + status
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .min_w_0()
                        .child(
                            Icon::new(IconName::Cpu)
                                .size(px(16.0))
                                .text_color(palette.accent_solid),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(if props.title.trim().is_empty() {
                                    "整机概览"
                                } else {
                                    props.title
                                })),
                        )
                        .children((!props.machine.trim().is_empty()).then(|| {
                            apply_scale(div(), scale::CAPTION)
                                .truncate()
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(props.machine.to_owned()))
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::MD)
                        .children((!props.uptime.trim().is_empty()).then(|| {
                            apply_scale(div(), scale::CAPTION)
                                .font_family(mono_family.clone())
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(format!("运行 {}", props.uptime)))
                        }))
                        .child(
                            apply_scale(div(), scale::CAPTION_MEDIUM)
                                .text_color(status_badge.1)
                                .child(cjk_inline_label(status_badge.0)),
                        ),
                ),
        )
        // Two-column `6px` proportional progress bars (`CPU` & `内存`)
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(space::LG)
                .child(render_metric_bar(
                    "处理器",
                    &cpu_subtitle,
                    cpu_val,
                    mono_family.clone(),
                    palette,
                ))
                .child(render_metric_bar(
                    "内存",
                    &mem_subtitle,
                    mem_val,
                    mono_family.clone(),
                    palette,
                )),
        );

    if has_details {
        let dk = detail_key.clone();
        card = card.child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(SharedString::from(format!(
                    "sys-overview-detail-toggle-{}",
                    props.call_id
                )))
                .test_support()
                .h(height::INLINE)
                .flex()
                .items_center()
                .gap(space::XS)
                .text_color(palette.text_secondary)
                .on_click(move |_, w, cx| (on_toggle_details)(dk.clone(), w, cx))
                .child(cjk_inline_label("详情"))
                .child(
                    Icon::new(if props.details_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(11.0))
                    .text_color(palette.text_muted),
                ),
        );

        if props.details_expanded {
            let mut detail_col = div()
                .pt(space::SM)
                .border_t_1()
                .border_color(palette.border_subtle)
                .flex()
                .flex_col()
                .gap(space::XS);

            for row in props.fields {
                detail_col = detail_col.child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap(space::MD)
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(row.label.clone())),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .font_family(mono_family.clone())
                                .text_color(palette.text_primary)
                                .child(SharedString::from(row.value.clone())),
                        ),
                );
            }

            for err in props.errors {
                detail_col = detail_col.child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(palette.status_warning)
                        .child(SharedString::from(err.clone())),
                );
            }

            card = card.child(detail_col);
        }
    }

    card.into_any_element()
}

fn render_metric_bar(
    label: &'static str,
    value_str: &str,
    pct: f64,
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let bar_color = if pct >= 90.0 {
        palette.status_danger
    } else if pct >= 75.0 {
        palette.status_warning
    } else {
        palette.accent_solid
    };
    let fill_px = ((pct / 100.0) * 320.0).round().max(0.0) as f32;

    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(space::XS)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::CAPTION_MEDIUM)
                        .text_color(palette.text_secondary)
                        .child(cjk_inline_label(label)),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .font_family(mono_family)
                        .text_color(palette.text_primary)
                        .truncate()
                        .child(SharedString::from(value_str.to_owned())),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(6.0))
                .rounded(radius::FULL)
                .bg(palette.bg_track_recess)
                .overflow_hidden()
                .child(
                    div()
                        .w(px(fill_px))
                        .h_full()
                        .rounded(radius::FULL)
                        .bg(bar_color),
                ),
        )
}

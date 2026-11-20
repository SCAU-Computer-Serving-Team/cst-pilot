use cst_client::tool_presenter::{FieldRow, SpecialStatus};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, div, px, relative,
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
/// `cst-pilot-desktop.pen` `wmkJw` `$bg-special` `12px` card).
pub fn render_sys_overview_card(
    props: SysOverviewProps<'_>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    on_toggle_details: crate::tools::ToggleKeyFn,
) -> gpui_kit::AnyElement {
    let (status_icon, status_icon_color, status_text) = match props.status {
        SpecialStatus::Running | SpecialStatus::Pending => {
            (IconName::LoaderCircle, palette.accent_ink, "采集中")
        }
        SpecialStatus::Degraded => (
            IconName::TriangleAlert,
            palette.status_warning,
            "部分数据缺失",
        ),
        SpecialStatus::Error => (IconName::CircleAlert, palette.status_danger, "采集失败"),
        SpecialStatus::Interrupted => (IconName::TriangleAlert, palette.status_warning, "已中断"),
        _ => (IconName::CircleCheck, palette.status_success, "已采集"),
    };

    let cpu_val = props.cpu_pct.unwrap_or(0.0).clamp(0.0, 100.0);
    let mem_val = props.memory_pct.unwrap_or(0.0).clamp(0.0, 100.0);

    let cpu_pct_label = match props.cpu_pct {
        Some(p) => format_pct(p),
        None => "--".to_owned(),
    };
    let cpu_sub = match props.cores {
        Some(c) => format!("{c} 个逻辑核心"),
        None => "核心数未采集".to_owned(),
    };

    let mem_pct_label = match props.memory_pct {
        Some(p) => format_pct(p),
        None => "--".to_owned(),
    };
    let mem_sub = if props.capacity.trim().is_empty() {
        "容量未采集".to_owned()
    } else {
        props.capacity.to_owned()
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
        .shadow(palette.craft_stack_shadows())
        .flex()
        .flex_col()
        .gap(space::MD)
        // 1. `28px` Header (`E8ziuK`): left `18px` Monitor + `16/24 @ 500` title; right `12px` status icon + `12/16` text
        .child(
            div()
                .h(height::COMPACT)
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
                            Icon::new(IconName::Monitor)
                                .size(px(18.0))
                                .text_color(palette.text_primary),
                        )
                        .child(
                            apply_scale(div(), scale::BODY_MEDIUM)
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(if props.title.trim().is_empty() {
                                    "整机概览"
                                } else {
                                    props.title
                                })),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .flex_shrink_0()
                        .child(
                            Icon::new(status_icon)
                                .size(px(12.0))
                                .text_color(status_icon_color),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .text_color(palette.text_secondary)
                                .child(cjk_inline_label(status_text)),
                        ),
                ),
        );

    // 2. Standalone machine summary row (`z4HOkb`, `14/20` `text_secondary`)
    if !props.machine.trim().is_empty() {
        card = card.child(
            apply_scale(div(), scale::UI)
                .w_full()
                .truncate()
                .text_color(palette.text_secondary)
                .child(SharedString::from(props.machine.to_owned())),
        );
    }

    // 3. Two-column `24px` gap neutral progress bars (`W8YDpW`)
    card = card.child(
        div()
            .w_full()
            .flex()
            .items_start()
            .gap(space::GROUP)
            .child(render_metric_column(
                "CPU",
                &cpu_pct_label,
                &cpu_sub,
                cpu_val,
                mono_family.clone(),
                palette,
            ))
            .child(render_metric_column(
                "内存",
                &mem_pct_label,
                &mem_sub,
                mem_val,
                mono_family.clone(),
                palette,
            )),
    );

    // 4. `1px` `border_subtle` divider + `24px` bottom bar (`S82Rar`)
    if !props.uptime.trim().is_empty() || has_details {
        card = card.child(div().w_full().h(px(1.0)).bg(palette.border_subtle));

        let uptime_label = if props.uptime.trim().is_empty() {
            "运行时间未采集".to_owned()
        } else {
            format!("已运行 {}", props.uptime)
        };

        let mut bottom_bar = div()
            .w_full()
            .h(height::INLINE)
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
                        Icon::new(IconName::Clock3)
                            .size(px(12.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .truncate()
                            .text_color(palette.text_secondary)
                            .child(SharedString::from(uptime_label)),
                    ),
            );

        if has_details {
            let dk = detail_key;
            bottom_bar = bottom_bar.child(
                apply_scale(div(), scale::CAPTION)
                    .id(SharedString::from(format!(
                        "sys-overview-detail-toggle-{}",
                        props.call_id
                    )))
                    .test_support()
                    .h(height::INLINE)
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .flex_shrink_0()
                    .text_color(palette.text_secondary)
                    .hover(|s| s.text_color(palette.text_primary))
                    .on_click(move |_, w, cx| (on_toggle_details)(dk.clone(), w, cx))
                    .child(cjk_inline_label("详情"))
                    .child(
                        Icon::new(if props.details_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(12.0))
                        .text_color(palette.text_secondary),
                    ),
            );
        }

        card = card.child(bottom_bar);

        if has_details && props.details_expanded {
            let mut detail_col = div()
                .w_full()
                .pt(space::XS)
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

fn format_pct(p: f64) -> String {
    let clamped = p.clamp(0.0, 100.0);
    if (clamped - clamped.round()).abs() < 0.05 {
        format!("{:.0}%", clamped.round())
    } else {
        format!("{clamped:.1}%")
    }
}

fn render_metric_column(
    label: &'static str,
    pct_str: &str,
    sub_str: &str,
    pct: f64,
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let fill_ratio = (pct / 100.0).clamp(0.0, 1.0) as f32;

    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(space::SM)
        .child(
            div()
                .h(height::COMPACT)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label(label)),
                )
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_family(mono_family)
                        .text_color(palette.text_primary)
                        .child(SharedString::from(pct_str.to_owned())),
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
                        .w(relative(fill_ratio))
                        .h_full()
                        .rounded(radius::FULL)
                        .bg(palette.text_secondary),
                ),
        )
        .child(
            apply_scale(div(), scale::CAPTION)
                .truncate()
                .text_color(palette.text_secondary)
                .child(SharedString::from(sub_str.to_owned())),
        )
}

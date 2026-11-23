use cst_client::tool_presenter::{FieldRow, SpecialStatus};
use gpui_kit::assets::IconName;
use gpui_kit::base::motion::{Transition, TransitionId, transition};
use gpui_kit::component::Icon;
use gpui_kit::{
    App, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    TestSupportExt, Window, div, px, relative,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};
use crate::tools::render_special_details_section;

pub struct SysOverviewProps<'a> {
    pub call_id: &'a str,
    pub status: SpecialStatus,
    pub title: &'a str,
    pub summary: &'a str,
    pub raw: &'a str,
    pub machine: &'a str,
    pub uptime: &'a str,
    pub cpu_pct: Option<f64>,
    pub cores: Option<i64>,
    pub memory_pct: Option<f64>,
    pub capacity: &'a str,
    pub fields: &'a [FieldRow],
    pub errors: &'a [String],
    pub details_expanded: bool,
    pub running_light_allowed: bool,
}

/// Renders the standalone System Overview persistent card (`sys` with `scope == "overview"`,
/// `cst-pilot-desktop.pen` `wmkJw` `$bg-special` `12px` card).
#[allow(clippy::too_many_arguments)]
pub fn render_sys_overview_card<V: 'static>(
    props: SysOverviewProps<'_>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_details: crate::tools::ToggleDetailsFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
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
        None => "未获取".to_owned(),
    };
    let cpu_sub = match props.cores {
        Some(c) => format!("{c} 个逻辑核心"),
        None => "逻辑核心未知".to_owned(),
    };

    let mem_pct_label = match props.memory_pct {
        Some(p) => format_pct(p),
        None => "未获取".to_owned(),
    };
    let mem_sub = if props.capacity.trim().is_empty() {
        "容量未知".to_owned()
    } else {
        props.capacity.to_owned()
    };

    let card_title = if props.title.trim().is_empty() {
        "整机概览"
    } else {
        props.title
    };
    let mut card = div()
        .id(SharedString::from(format!(
            "special-sys-overview-{}",
            props.call_id
        )))
        .test_support()
        .a11y_region(
            SharedString::from(format!("sys-overview-card-{}", props.call_id)),
            card_title,
        )
        .w_full()
        .p(space::LG)
        .rounded(radius::SPECIAL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_special)
        .shadow(palette.craft_stack_shadows())
        .flex()
        .flex_col()
        .gap(space::LG)
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
                        .child(if status_icon == IconName::LoaderCircle {
                            motion::animated_spinner(
                                format!("sys-overview-spin-{}", props.call_id),
                                px(12.0),
                                status_icon_color,
                                props.running_light_allowed,
                                window,
                                cx,
                            )
                        } else {
                            Icon::new(status_icon)
                                .size(px(12.0))
                                .text_color(status_icon_color)
                                .into_any_element()
                        })
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .text_color(palette.text_secondary)
                                .child(cjk_inline_label(status_text)),
                        ),
                ),
        );

    // 2. Standalone machine summary row (`z4HOkb` / `TIYSe`, `14/20` `text_secondary`)
    let machine_label = if props.machine.trim().is_empty() {
        "机型信息未获取"
    } else {
        props.machine
    };
    card = card.child(
        apply_scale(div(), scale::UI)
            .w_full()
            .truncate()
            .text_color(palette.text_secondary)
            .child(SharedString::from(machine_label.to_owned())),
    );

    // 3. Two-column `24px` gap neutral progress bars (`W8YDpW`)
    card = card.child(
        div()
            .w_full()
            .flex()
            .items_start()
            .gap(space::GROUP)
            .child(render_metric_column(
                format!("sys-cpu-bar-{}", props.call_id),
                "CPU",
                &cpu_pct_label,
                &cpu_sub,
                cpu_val,
                mono_family.clone(),
                palette,
                window,
                cx,
            ))
            .child(render_metric_column(
                format!("sys-mem-bar-{}", props.call_id),
                "内存",
                &mem_pct_label,
                &mem_sub,
                mem_val,
                mono_family.clone(),
                palette,
                window,
                cx,
            )),
    );

    // 4. `1px` `border_subtle` divider + `24px` bottom bar (`S82Rar`) + expanded `BMZLr` details
    let uptime_label = if props.uptime.trim().is_empty() {
        "运行时间未采集".to_owned()
    } else {
        format!("已运行 {}", props.uptime)
    };
    let left_uptime = div()
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
        )
        .into_any_element();

    let toggle_id = SharedString::from(format!("sys-overview-detail-toggle-{}", props.call_id));
    let raw_block_id = SharedString::from(format!("sys-overview-raw-{}", props.call_id));
    if let Some(details_section) = render_special_details_section(
        toggle_id,
        "整机概览详情",
        props.details_expanded,
        Some("sys(scope: \"overview\")"),
        props.fields,
        props.errors,
        props.status == SpecialStatus::Error,
        raw_block_id,
        props.raw,
        Some(left_uptime),
        mono_family,
        palette,
        is_copied_fn,
        on_toggle_details,
        on_mark_copied,
        window,
        cx,
    ) {
        card = card.child(details_section);
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

#[allow(clippy::too_many_arguments)]
fn render_metric_column(
    bar_id: String,
    label: &'static str,
    pct_str: &str,
    sub_str: &str,
    pct: f64,
    mono_family: SharedString,
    palette: &DesktopPalette,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let target_ratio = (pct / 100.0).clamp(0.0, 1.0) as f32;
    let tid: TransitionId = SharedString::from(bar_id).into();
    let fill_ratio = transition(
        tid,
        target_ratio,
        Transition::new(motion::NORMAL).ease(motion::ease_out()),
        window,
        cx,
    )
    .clamp(0.0, 1.0);

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
                    apply_scale(div(), scale::UI)
                        .text_color(palette.text_secondary)
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

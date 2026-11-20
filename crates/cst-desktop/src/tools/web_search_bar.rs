use std::rc::Rc;

use cst_client::tool_presenter::{FieldRow, SpecialStatus};
use gpui::{Animation, AnimationExt as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};

pub struct WebSearchBarProps<'a> {
    pub call_id: &'a str,
    pub status: SpecialStatus,
    pub title: &'a str,
    pub summary: &'a str,
    pub queries: &'a [String],
    pub successful: Option<i64>,
    pub fields: &'a [FieldRow],
    pub errors: &'a [String],
    pub expanded: bool,
    pub warning_hovered: bool,
}

/// Renders the standalone Web Search persistent bar (`web_search`, `cst-pilot-desktop.pen` `JQMQZ`).
#[allow(clippy::too_many_arguments)]
pub fn render_web_search_bar(
    props: WebSearchBarProps<'_>,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    on_toggle_expand: crate::tools::ToggleDetailsFn,
    on_hover_warning: crate::tools::HoverWarningFn,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::AnyElement {
    let is_running = matches!(
        props.status,
        SpecialStatus::Pending | SpecialStatus::Running
    );
    let is_error = props.status == SpecialStatus::Error;
    let has_hover_issue = !is_running
        && (is_error || !props.errors.is_empty() || props.status == SpecialStatus::Degraded);
    let has_details = !props.queries.is_empty() || !props.fields.is_empty();
    let expanded = has_details && props.expanded;

    let warn_key = format!("web-search-warn-{}", props.call_id);

    let has_summary_count =
        props.queries.is_empty() && !is_running && !props.summary.trim().is_empty();
    let count_text = if !props.queries.is_empty() {
        format!("{} 个关键词", props.queries.len())
    } else if is_running {
        "正在检索…".to_owned()
    } else if has_summary_count {
        props.summary.replace(['\r', '\n'], " ")
    } else if let Some(ok_cnt) = props.successful {
        format!("{ok_cnt} 个关键词")
    } else {
        "0 个关键词".to_owned()
    };

    let display_title = match props.title.trim() {
        "" | "联网检索" if is_running => "正在联网检索",
        "" | "联网检索" if props.status == SpecialStatus::Success => "已联网检索",
        "" => "联网检索",
        title => title,
    };

    let mut left_row = div()
        .flex()
        .items_center()
        .gap(space::SM)
        .min_w_0()
        .child(
            Icon::new(IconName::Globe)
                .size(px(14.0))
                .text_color(palette.accent_ink),
        )
        .child(
            apply_scale(div(), scale::BODY_MEDIUM)
                .truncate()
                .text_color(palette.text_primary)
                .child(cjk_inline_label(display_title)),
        );

    if is_running {
        left_row = left_row.child(
            Icon::new(IconName::LoaderCircle)
                .size(px(14.0))
                .text_color(palette.accent_ink),
        );
    } else if has_hover_issue {
        let wk_hover = warn_key;
        let hover_cb = Rc::clone(&on_hover_warning);
        let (badge_icon, badge_color) = if is_error {
            (IconName::CircleAlert, palette.status_danger)
        } else {
            (IconName::TriangleAlert, palette.status_warning)
        };
        left_row = left_row.child(
            div()
                .id(SharedString::from(format!(
                    "web-search-warn-icon-{}",
                    props.call_id
                )))
                .test_support()
                .w(px(16.0))
                .h(px(16.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .on_hover(move |hovered, w, cx| {
                    (hover_cb)(wk_hover.clone(), *hovered, w, cx);
                })
                .child(Icon::new(badge_icon).size(px(15.0)).text_color(badge_color)),
        );
    } else if props.status == SpecialStatus::Success {
        left_row = left_row.child(
            Icon::new(IconName::CircleCheck)
                .size(px(16.0))
                .text_color(palette.status_success),
        );
    }

    if has_details {
        left_row = left_row.child(motion::animated_chevron(
            format!("web-search-{}", props.call_id),
            expanded,
            false,
            px(12.0),
            palette.text_secondary,
            window,
            cx,
        ));
    }

    let hdr_id = SharedString::from(format!("web-search-bar-hdr-{}", props.call_id));
    let hdr_label = SharedString::from(format!("{display_title}（{count_text}）"));
    let count_label = apply_scale(div(), scale::CAPTION)
        .min_w_0()
        .truncate()
        .text_color(palette.text_secondary)
        .child(SharedString::from(count_text));
    let count_label = if has_summary_count {
        count_label
    } else {
        count_label.flex_shrink_0()
    };
    let header_bar = div()
        .id(hdr_id.clone())
        .test_support()
        .w_full()
        .h(height::COMPACT)
        .flex()
        .items_center()
        .justify_between()
        .gap(space::MD)
        .child(left_row)
        .child(count_label);
    let header_bar = if has_details {
        header_bar
            .a11y_toggle_button(hdr_id, hdr_label, expanded)
            .on_click(move |_, w, cx| (on_toggle_expand)(w, cx))
    } else {
        header_bar.a11y_region(hdr_id, hdr_label)
    };

    let mut card = div()
        .id(SharedString::from(format!(
            "special-web-search-{}",
            props.call_id
        )))
        .test_support()
        .a11y_region(
            SharedString::from(format!("web-search-card-{}", props.call_id)),
            display_title,
        )
        .relative()
        .w_full()
        .px(space::LG)
        .py(space::MD)
        .rounded(radius::SPECIAL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_special)
        .shadow(palette.craft_stack_shadows())
        .flex()
        .flex_col()
        .gap(space::MD)
        .child(header_bar);

    if expanded {
        let mut details = div()
            .w_full()
            .pt(space::XS)
            .flex()
            .flex_col()
            .gap(space::SM);

        for q in props.queries {
            details = details.child(
                apply_scale(div(), scale::CAPTION)
                    .w_full()
                    .text_color(palette.text_primary)
                    .child(SharedString::from(q.clone())),
            );
        }

        for row in props.fields {
            details = details.child(
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
                            .flex_1()
                            .min_w_0()
                            .text_color(palette.text_primary)
                            .child(SharedString::from(row.value.clone())),
                    ),
            );
        }

        card = card.child(details);
    }

    if has_hover_issue && props.warning_hovered {
        let reason_text = if props.errors.is_empty() {
            if is_error {
                "联网检索失败。".to_owned()
            } else {
                "部分检索词未能返回完整网页摘要。".to_owned()
            }
        } else {
            props.errors.join("；")
        };
        card = card.child(
            div()
                .absolute()
                .left(space::LG)
                .bottom_full()
                .pb(space::XS)
                .child(
                    render_frosted_surface(
                        format!("web-search-warn-tooltip-{}", props.call_id),
                        FrostedGlassSpec::popover_h2p2z(palette),
                        palette.bg_elevated,
                        transparency_enabled,
                        scale_factor,
                    )
                    .test_support()
                    .max_w(px(320.0))
                    .px(space::SM)
                    .py(space::XS)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_hairline)
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .relative()
                            .text_color(palette.text_primary)
                            .child(SharedString::from(reason_text)),
                    )
                    .with_animation(
                        SharedString::from(format!(
                            "web-search-warn-tooltip-anim-{}",
                            props.call_id
                        )),
                        Animation::new(motion::FAST).with_easing(motion::ease_out()),
                        |el, d| {
                            let t = d.clamp(0.0, 1.0);
                            el.opacity(0.01 + 0.99 * t).mt(px(4.0 * (1.0 - t)))
                        },
                    ),
                ),
        );
    }

    card.into_any_element()
}

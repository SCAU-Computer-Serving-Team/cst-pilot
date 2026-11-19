use std::rc::Rc;

use cst_client::tool_presenter::{FieldRow, SpecialStatus};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, TestSupportExt, div, px,
};

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale, space};

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

/// Renders the standalone Web Search persistent bar (`web_search`,
/// `cst-pilot-desktop.pen` 「07 · 工具与特殊工具」).
/// Displays a `14×14px` search icon optically aligned with the `16px` title, expands to show the full
/// keyword list & hit summary, and preserves all original keywords on partial failure with a frosted
/// warning tooltip (`H2p2z`) on hover.
pub fn render_web_search_bar(
    props: WebSearchBarProps<'_>,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    on_toggle_expand: crate::tools::ToggleKeyFn,
    on_hover_warning: crate::tools::HoverWarningFn,
) -> gpui_kit::AnyElement {
    let is_running = matches!(
        props.status,
        SpecialStatus::Pending | SpecialStatus::Running
    );
    let has_partial_warning = !props.errors.is_empty()
        || matches!(props.status, SpecialStatus::Degraded | SpecialStatus::Error);

    let expand_key = format!("web-search-expand-{}", props.call_id);
    let warn_key = format!("web-search-warn-{}", props.call_id);
    let ek_click = expand_key.clone();

    let status_text = if is_running {
        "正在检索…".to_owned()
    } else if !props.summary.trim().is_empty() {
        props.summary.to_owned()
    } else if let Some(ok_cnt) = props.successful {
        format!("已完成 {ok_cnt} 组检索")
    } else {
        format!("共 {} 个检索词", props.queries.len())
    };

    let mut right_row = div().flex().items_center().gap(space::SM);

    if has_partial_warning {
        let wk_hover = warn_key.clone();
        let hover_cb = Rc::clone(&on_hover_warning);
        right_row = right_row.child(
            div()
                .id(SharedString::from(format!(
                    "web-search-warn-icon-{}",
                    props.call_id
                )))
                .test_support()
                .w(height::INLINE)
                .h(height::INLINE)
                .flex()
                .items_center()
                .justify_center()
                .on_hover(move |hovered, w, cx| {
                    (hover_cb)(wk_hover.clone(), *hovered, w, cx);
                })
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size(px(14.0))
                        .text_color(palette.status_warning),
                ),
        );
    }

    right_row = right_row
        .child(
            apply_scale(div(), scale::CAPTION)
                .text_color(palette.text_secondary)
                .child(SharedString::from(status_text)),
        )
        .child(
            Icon::new(if props.expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(px(12.0))
            .text_color(palette.text_muted),
        );

    let header_bar = div()
        .id(SharedString::from(format!(
            "web-search-bar-hdr-{}",
            props.call_id
        )))
        .test_support()
        .h(height::FIELD)
        .px(space::MD)
        .flex()
        .items_center()
        .justify_between()
        .gap(space::MD)
        .hover(|s| s.bg(palette.bg_item_hover))
        .on_click(move |_, w, cx| (on_toggle_expand)(ek_click.clone(), w, cx))
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .min_w_0()
                .child(
                    // `14×14px` icon optically aligned with `16px` title
                    Icon::new(if is_running {
                        IconName::Loader
                    } else {
                        IconName::Search
                    })
                    .size(px(14.0))
                    .text_color(if is_running {
                        palette.accent_solid
                    } else {
                        palette.text_secondary
                    }),
                )
                .child(
                    apply_scale(div(), scale::BODY_MEDIUM)
                        .truncate()
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label(if props.title.trim().is_empty() {
                            "联网检索"
                        } else {
                            props.title
                        })),
                ),
        )
        .child(right_row);

    let mut card = div()
        .id(SharedString::from(format!(
            "special-web-search-{}",
            props.call_id
        )))
        .test_support()
        .relative()
        .w_full()
        .rounded(radius::SPECIAL)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_special)
        .flex()
        .flex_col()
        .child(header_bar);

    if props.expanded {
        let mut details = div()
            .px(space::MD)
            .pb(space::MD)
            .pt(space::SM)
            .border_t_1()
            .border_color(palette.border_subtle)
            .flex()
            .flex_col()
            .gap(space::SM);

        if !props.queries.is_empty() {
            let mut tags = div().flex().flex_wrap().gap(space::XS);
            for q in props.queries {
                tags = tags.child(
                    apply_scale(div(), scale::CAPTION)
                        .px(space::SM)
                        .py(space::MICRO)
                        .rounded(radius::FULL)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .bg(palette.bg_surface)
                        .text_color(palette.text_primary)
                        .child(SharedString::from(q.clone())),
                );
            }
            details = details.child(tags);
        }

        for row in props.fields {
            details = details.child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap(space::MD)
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
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

    if has_partial_warning && props.warning_hovered {
        let reason_text = if props.errors.is_empty() {
            "部分检索词未能返回完整网页摘要。".to_owned()
        } else {
            props.errors.join("；")
        };
        card = card.child(
            div()
                .absolute()
                .right(space::MD)
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
                    .p(space::SM)
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_hairline)
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .relative()
                            .text_color(palette.text_primary)
                            .child(SharedString::from(reason_text)),
                    ),
                ),
        );
    }

    card.into_any_element()
}

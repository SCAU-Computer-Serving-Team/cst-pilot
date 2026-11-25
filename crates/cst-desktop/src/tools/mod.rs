pub mod bitlocker_card;
pub mod normal_card;
pub mod runbook_card;
mod secret_input;
pub mod sys_overview;
pub mod web_search_bar;

use std::rc::Rc;
use std::time::Instant;

use cst_client::ClientHandle;
use cst_client::tool_presenter::{FieldRow, SpecialToolView};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, height, motion, scale, space};
use crate::tools::bitlocker_card::{
    BitLockerCardProps, PendingBitLockerPrompt, SecretInput, render_bitlocker_card,
};
use crate::tools::runbook_card::{OpenRunbookFn, RunbookCardProps, render_runbook_card};
use crate::tools::sys_overview::{SysOverviewProps, render_sys_overview_card};
use crate::tools::web_search_bar::{WebSearchBarProps, render_web_search_bar};
use crate::workbench::markdown_view::{
    render_copyable_text_block, render_copyable_text_block_inert,
};

pub type ToggleKeyFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type ToggleDetailsFn = Rc<dyn Fn(&mut Window, &mut App)>;
pub type HoverWarningFn = Rc<dyn Fn(String, bool, &mut Window, &mut App)>;

pub struct SpecialToolDispatchProps<'a> {
    pub session_id: &'a str,
    pub entry_id: &'a str,
    pub call_id: &'a str,
    pub view: &'a SpecialToolView,
    pub pending_bitlocker: Option<PendingBitLockerPrompt>,
    pub secret_input: Option<Entity<SecretInput>>,
    pub details_expanded: bool,
    pub warning_hovered: bool,
    pub transparency_enabled: bool,
    pub scale_factor: f32,
    pub running_light_allowed: bool,
    pub started_at: Option<Instant>,
    pub animate_enter: bool,
}

/// Unified dispatcher for the 4 standalone special persistent components
/// (`sys(overview)`, `bitlocker(unlock/decrypt)`, `command_plan / runbook_deliver / runbook`, `web_search`).
#[allow(clippy::too_many_arguments)]
pub fn render_special_tool_component<V: 'static>(
    props: SpecialToolDispatchProps<'_>,
    client: ClientHandle,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_details: ToggleDetailsFn,
    on_hover_warning: HoverWarningFn,
    on_open_runbook: OpenRunbookFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    on_error_notice: impl Fn(String, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    let enter_key = format!("{}:{}:{}", props.session_id, props.entry_id, props.call_id);
    let animate_enter = props.animate_enter;
    let card_el = match props.view {
        SpecialToolView::SysOverview {
            status,
            title,
            summary,
            raw,
            fields,
            errors,
            machine,
            cpu,
            cores,
            memory,
            capacity,
            uptime,
            ..
        } => render_sys_overview_card(
            SysOverviewProps {
                call_id: props.call_id,
                status: *status,
                title,
                summary,
                raw,
                machine,
                uptime,
                cpu_pct: *cpu,
                cores: *cores,
                memory_pct: *memory,
                capacity,
                fields,
                errors,
                details_expanded: props.details_expanded,
                running_light_allowed: props.running_light_allowed,
            },
            mono_family,
            palette,
            is_copied_fn,
            on_toggle_details,
            on_mark_copied,
            window,
            cx,
        ),
        SpecialToolView::Runbook {
            status,
            title,
            summary,
            raw,
            fields,
            commands,
            level,
            version,
            planned,
            delivery,
            errors,
            ..
        } => render_runbook_card(
            RunbookCardProps {
                call_id: props.call_id,
                entry_id: props.entry_id,
                status: *status,
                title,
                summary,
                raw,
                fields,
                commands,
                level,
                version: *version,
                planned: *planned,
                delivery: delivery.as_ref(),
                errors,
                details_expanded: props.details_expanded,
                running_light_allowed: props.running_light_allowed,
            },
            mono_family,
            palette,
            is_copied_fn,
            on_toggle_details,
            on_open_runbook,
            on_mark_copied,
            window,
            cx,
        ),
        SpecialToolView::WebSearch {
            status,
            title,
            summary,
            queries,
            successful,
            fields,
            errors,
            ..
        } => render_web_search_bar(
            WebSearchBarProps {
                call_id: props.call_id,
                status: *status,
                title,
                summary,
                queries,
                successful: *successful,
                fields,
                errors,
                expanded: props.details_expanded,
                warning_hovered: props.warning_hovered,
                running_light_allowed: props.running_light_allowed,
                started_at: props.started_at,
            },
            palette,
            props.transparency_enabled,
            props.scale_factor,
            on_toggle_details,
            on_hover_warning,
            window,
            cx,
        ),
        SpecialToolView::BitLocker {
            status,
            title,
            summary,
            raw,
            fields,
            errors,
            scope,
            volumes,
            ..
        } => render_bitlocker_card(
            BitLockerCardProps {
                session_id: props.session_id,
                call_id: props.call_id,
                status: *status,
                title,
                summary,
                raw,
                scope,
                volumes,
                fields,
                errors,
                pending: props.pending_bitlocker,
                secret_input: props.secret_input,
                details_expanded: props.details_expanded,
                running_light_allowed: props.running_light_allowed,
            },
            client,
            mono_family,
            palette,
            is_copied_fn,
            on_toggle_details,
            on_mark_copied,
            on_error_notice,
            window,
            cx,
        ),
    };
    motion::animate_card_enter(enter_key, card_el, animate_enter, window, cx)
}

pub fn render_empty_tool_placeholder() -> impl IntoElement {
    div()
}

/// Renders a semantic-icon-only notice/error row matching `.pen` (`14px` icon in semantic color +
/// `12/16px` `text_secondary` text).
pub(crate) fn render_tool_notice_line(
    icon: IconName,
    icon_color: gpui_kit::Hsla,
    text: impl Into<SharedString>,
    palette: &DesktopPalette,
) -> gpui_kit::Div {
    div()
        .w_full()
        .flex()
        .items_start()
        .gap(space::SM)
        .child(
            div()
                .pt(px(2.0))
                .flex_shrink_0()
                .child(Icon::new(icon).size(px(14.0)).text_color(icon_color)),
        )
        .child(
            apply_scale(div(), scale::CAPTION)
                .flex_1()
                .min_w_0()
                .text_color(palette.text_secondary)
                .child(text.into()),
        )
}

/// Computes label column width (`72px` when <= 4 chars, or scaled up to `180px`, matching `.pen`).
pub(crate) fn aligned_label_col_px<'a>(rows: impl IntoIterator<Item = &'a FieldRow>) -> f32 {
    let max_chars = rows
        .into_iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(4);
    if max_chars <= 4 {
        72.0
    } else {
        ((max_chars as f32) * 13.5 + 16.0).clamp(84.0, 180.0)
    }
}

/// Shared bottom `"详情"` toggle + fixed-label-aligned expanded body (`BMZLr` / `ctR8B`) for special tool cards.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_special_details_section<V: 'static>(
    toggle_id: SharedString,
    a11y_label: &'static str,
    expanded: bool,
    call_capsule: Option<&str>,
    fields: &[FieldRow],
    errors: &[String],
    error_is_danger: bool,
    raw_block_id: SharedString,
    raw: &str,
    left_footer: Option<gpui_kit::AnyElement>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle: ToggleDetailsFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Option<gpui_kit::Div> {
    let has_raw = !raw.trim().is_empty();
    let fields = fields
        .iter()
        .filter(|row| !row.label.trim().is_empty() && !row.value.trim().is_empty());
    let has_details = fields.clone().next().is_some() || !errors.is_empty() || has_raw;
    if left_footer.is_none() && !has_details {
        return None;
    }

    let mut wrapper = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space::MD)
        .child(div().w_full().h(px(1.0)).bg(palette.border_subtle));

    let mut bar = div()
        .w_full()
        .h(height::INLINE)
        .flex()
        .items_center()
        .gap(space::MD);
    if let Some(left) = left_footer {
        bar = bar.justify_between().child(left);
    } else {
        bar = bar.justify_end();
    }

    let collapse_id = SharedString::from(format!("special-details-{toggle_id}"));
    if has_details {
        let chev_id = toggle_id.clone();
        bar = bar.child(
            apply_scale(div(), scale::CAPTION)
                .id(toggle_id.clone())
                .test_support()
                .a11y_toggle_button(toggle_id, a11y_label, expanded)
                .h(height::INLINE)
                .flex()
                .items_center()
                .gap(space::XS)
                .flex_shrink_0()
                .text_color(palette.text_secondary)
                .hover(|s| s.text_color(palette.text_primary))
                .on_click(move |_, w, cx| (on_toggle)(w, cx))
                .child(cjk_inline_label("详情"))
                .child(motion::animated_chevron(
                    chev_id,
                    expanded,
                    false,
                    px(12.0),
                    palette.text_secondary,
                    window,
                    cx,
                )),
        );
    }

    wrapper = wrapper.child(bar);

    if has_details
        && let Some(animated_details) = motion::animated_collapse_with_exit(
            collapse_id,
            expanded,
            window,
            cx,
            |interactive, window, cx| {
                let mut detail_col = div().w_full().flex().flex_col().gap(space::MD);
                if let Some(cap) = call_capsule {
                    detail_col = detail_col.child(
                        apply_scale(div(), scale::CAPTION)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_secondary)
                            .child(SharedString::from(cap.to_owned())),
                    );
                }

                if fields.clone().next().is_some() {
                    let label_w = aligned_label_col_px(fields.clone());
                    let mut rows_col = div().w_full().flex().flex_col().gap(space::TIGHT);
                    for row in fields {
                        rows_col = rows_col.child(
                            div()
                                .w_full()
                                .min_h(px(20.0))
                                .flex()
                                .items_start()
                                .gap(space::MD)
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .w(px(label_w))
                                        .flex_shrink_0()
                                        .text_color(palette.text_secondary)
                                        .child(cjk_inline_label(row.label.clone())),
                                )
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .flex_1()
                                        .min_w_0()
                                        .pb(space::MICRO)
                                        .font_family(mono_family.clone())
                                        .text_color(palette.text_primary)
                                        .child(SharedString::from(row.value.clone())),
                                ),
                        );
                    }
                    detail_col = detail_col.child(rows_col);
                }

                for err in errors {
                    let (err_icon, err_color) = if error_is_danger {
                        (IconName::CircleAlert, palette.status_danger)
                    } else {
                        (IconName::TriangleAlert, palette.status_warning)
                    };
                    detail_col = detail_col.child(render_tool_notice_line(
                        err_icon,
                        err_color,
                        err.clone(),
                        palette,
                    ));
                }

                if has_raw {
                    if interactive {
                        let copied = is_copied_fn(raw_block_id.as_ref());
                        detail_col = detail_col.child(render_copyable_text_block(
                            raw_block_id,
                            raw.to_owned(),
                            true,
                            mono_family,
                            copied,
                            palette,
                            on_mark_copied,
                            window,
                            cx,
                        ));
                    } else {
                        detail_col = detail_col.child(render_copyable_text_block_inert(
                            raw.to_owned(),
                            true,
                            mono_family,
                            palette,
                        ));
                    }
                }

                detail_col.into_any_element()
            },
        )
    {
        wrapper = wrapper.child(animated_details);
    }

    Some(wrapper)
}

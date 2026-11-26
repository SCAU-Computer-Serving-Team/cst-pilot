use std::rc::Rc;

use cst_client::tool_presenter::{CommandItem, FieldRow, RunbookDeliveryView, SpecialStatus};
use gpui::CursorStyle;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, ClipboardItem, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};
use crate::tools::{render_special_details_section, render_tool_notice_line};

pub struct RunbookCardProps<'a> {
    pub call_id: &'a str,
    pub entry_id: &'a str,
    pub status: SpecialStatus,
    pub title: &'a str,
    pub summary: &'a str,
    pub raw: &'a str,
    pub fields: &'a [FieldRow],
    pub commands: &'a [CommandItem],
    pub level: &'a str,
    pub version: Option<i64>,
    pub planned: bool,
    pub delivery: Option<&'a RunbookDeliveryView>,
    pub errors: &'a [String],
    pub details_expanded: bool,
    pub running_light_allowed: bool,
}

pub type OpenRunbookFn = Rc<dyn Fn(String, String, &mut Window, &mut App)>;
pub type ExportRunbookFn = Rc<dyn Fn(String, String, &mut Window, &mut App)>;

fn risk_tier_plain_label(level: &str) -> &'static str {
    match level.to_ascii_lowercase().as_str() {
        "safe" => "安全",
        "low" => "低风险",
        "confirm" => "需人工确认",
        _ => "执行建议",
    }
}

fn format_shell_meta(shell: &str, admin: bool) -> String {
    let s = match shell.trim().to_ascii_lowercase().as_str() {
        "" | "powershell" | "pwsh" => "PowerShell",
        "cmd" => "cmd",
        other => {
            return if admin {
                format!("{other} · 管理员")
            } else {
                other.to_owned()
            };
        }
    };
    if admin {
        format!("{s} · 管理员")
    } else {
        s.to_owned()
    }
}

/// Renders the standalone Command Plan & Runbook Delivery card (`command_plan`, `runbook_deliver`, `runbook`,
/// `cst-pilot-desktop.pen` `s3WzY`).
#[allow(clippy::too_many_arguments)]
pub fn render_runbook_card<V: 'static>(
    props: RunbookCardProps<'_>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_details: crate::tools::ToggleDetailsFn,
    on_open_runbook: OpenRunbookFn,
    on_export_runbook: ExportRunbookFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    let tier_label = risk_tier_plain_label(props.level);

    let (status_icon, status_color, status_text) =
        if props.planned && props.status == SpecialStatus::Success {
            (IconName::Clock3, palette.text_secondary, "待核对")
        } else {
            match props.status {
                SpecialStatus::Running | SpecialStatus::Pending => {
                    (IconName::LoaderCircle, palette.accent_ink, "生成中")
                }
                SpecialStatus::Error => (IconName::CircleAlert, palette.status_danger, "生成失败"),
                SpecialStatus::Degraded | SpecialStatus::Interrupted => {
                    (IconName::TriangleAlert, palette.status_warning, "部分缺失")
                }
                _ => (IconName::CircleCheck, palette.status_success, "已生成"),
            }
        };

    let mode_text = if props.planned {
        match props.version {
            Some(v) => format!("方案 v{v} · 尚未导出文本"),
            None => "尚未导出文本".to_owned(),
        }
    } else {
        "手动执行".to_owned()
    };

    let card_title = if props.title.trim().is_empty() {
        "修复命令清单"
    } else {
        props.title
    };
    let mut card = div()
        .id(SharedString::from(format!(
            "special-runbook-card-{}",
            props.call_id
        )))
        .test_support()
        .a11y_region(
            SharedString::from(format!("runbook-card-{}", props.call_id)),
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
        // 1. `28px` Header (`Ymhja`): left `18px` FileTerminal + `16/24 @ 500` title; right `12px` status icon + text
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
                            Icon::new(IconName::FileTerminal)
                                .size(px(18.0))
                                .text_color(palette.text_primary),
                        )
                        .child(
                            apply_scale(div(), scale::BODY_MEDIUM)
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(card_title.replace(['\r', '\n'], " "))),
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
                                format!("runbook-spin-{}", props.call_id),
                                px(12.0),
                                status_color,
                                props.running_light_allowed,
                                window,
                                cx,
                            )
                        } else {
                            Icon::new(status_icon)
                                .size(px(12.0))
                                .text_color(status_color)
                                .into_any_element()
                        })
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .text_color(palette.text_secondary)
                                .child(cjk_inline_label(status_text)),
                        ),
                ),
        )
        // 2. `24px` Plain-text attributes bar (`g5GxvF` — zero colored capsules!)
        .child(
            apply_scale(div(), scale::CAPTION)
                .h(height::INLINE)
                .flex()
                .items_center()
                .gap(space::MD)
                .text_color(palette.text_secondary)
                .child(SharedString::from(tier_label))
                .child(SharedString::from(format!(
                    "{} 条命令",
                    props.commands.len()
                )))
                .child(SharedString::from(mode_text)),
        );

    let trimmed_summary = props.summary.trim();
    if !trimmed_summary.is_empty()
        && trimmed_summary != status_text
        && !matches!(
            trimmed_summary,
            "已生成" | "生成失败" | "待核对" | "方案已保存" | "交付结果缺失"
        )
    {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .text_color(palette.text_secondary)
                .child(SharedString::from(trimmed_summary.to_owned())),
        );
    }

    // 3. Command entries (`TkDiu`): frameless `24px` description row + `40px` `4px`-radius `DXEuq` single-line command box
    if !props.commands.is_empty() {
        let mut cmd_col = div().w_full().flex().flex_col().gap(space::MD);
        for (idx, item) in props.commands.iter().enumerate() {
            let copy_id = SharedString::from(format!("runbook-cmd-copy-{}-{}", props.call_id, idx));
            let copied = is_copied_fn(copy_id.as_ref());
            let cmd_str = item.command.clone();
            let cid_click = copy_id.clone();
            let mark_cb = on_mark_copied.clone();

            let desc_str = if item.summary.trim().is_empty() {
                format!("步骤 {}", idx + 1)
            } else {
                item.summary.clone()
            };
            let meta_str = format_shell_meta(&item.shell, item.admin);

            let copy_label = SharedString::from(format!("复制命令：{desc_str}"));
            cmd_col = cmd_col.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    .child(
                        div()
                            .h(height::INLINE)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::MD)
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(palette.text_primary)
                                    .child(SharedString::from(desc_str)),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child(SharedString::from(meta_str)),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(height::FIELD)
                            .pl(space::MD)
                            .pr(space::XS)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_surface)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono_family.clone())
                                    .text_color(palette.text_primary)
                                    .child(SharedString::from(item.command.clone())),
                            )
                            .child({
                                let cid_motion = cid_click.clone();
                                div()
                                    .id(copy_id.clone())
                                    .test_support()
                                    .a11y_button(copy_id, copy_label)
                                    .w(height::CONTROL)
                                    .h(height::CONTROL)
                                    .flex_shrink_0()
                                    .rounded(radius::CONTROL)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(move |_, _, w, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            cmd_str.clone(),
                                        ));
                                        mark_cb(cid_click.clone(), w, cx);
                                    }))
                                    .child(motion::animated_copy_icon(
                                        cid_motion,
                                        copied,
                                        px(16.0),
                                        if copied {
                                            palette.status_success
                                        } else {
                                            palette.text_secondary
                                        },
                                        window,
                                        cx,
                                    ))
                            }),
                    ),
            );
        }
        card = card.child(cmd_col);
    }

    // 4. File delivery bar (`Yp82l`) below command list
    if props.status != SpecialStatus::Error
        && let Some(deliv) = props.delivery
        && !deliv.file.trim().is_empty()
    {
        let eid = props.entry_id.to_owned();
        let cid = props.call_id.to_owned();
        let open_fn = Rc::clone(&on_open_runbook);
        let file_label = if deliv.name.trim().is_empty() {
            deliv
                .file
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(deliv.file.as_str())
                .to_owned()
        } else {
            deliv.name.clone()
        };

        card = card.child(
            div()
                .w_full()
                .pt(space::MD)
                .border_t_1()
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::MD)
                        .min_w_0()
                        .child(
                            Icon::new(IconName::FileText)
                                .size(px(14.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .truncate()
                                .text_color(palette.accent_ink)
                                .child(SharedString::from(file_label.replace(['\r', '\n'], " "))),
                        ),
                )
                .child({
                    let open_btn_id =
                        SharedString::from(format!("runbook-open-file-btn-{}", props.call_id));
                    apply_scale(div(), scale::CAPTION)
                        .id(open_btn_id.clone())
                        .test_support()
                        .a11y_button(open_btn_id, "打开修复命令文件")
                        .cursor(CursorStyle::PointingHand)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .flex_shrink_0()
                        .text_color(palette.accent_ink)
                        .hover(|s| s.text_color(palette.accent_hover))
                        .on_click(move |_, w, cx| (open_fn)(eid.clone(), cid.clone(), w, cx))
                        .child(cjk_inline_label("打开"))
                        .child(
                            Icon::new(IconName::ExternalLink)
                                .size(px(12.0))
                                .text_color(palette.accent_ink),
                        )
                }),
        );
    } else if props.status == SpecialStatus::Success && props.planned {
        let eid = props.entry_id.to_owned();
        let cid = props.call_id.to_owned();
        let export_fn = Rc::clone(&on_export_runbook);
        let export_btn_id =
            SharedString::from(format!("runbook-export-file-btn-{}", props.call_id));
        card = card.child(
            div()
                .w_full()
                .pt(space::MD)
                .border_t_1()
                .border_color(palette.border_subtle)
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
                            Icon::new(IconName::FileText)
                                .size(px(14.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .truncate()
                                .text_color(palette.text_secondary)
                                .child(cjk_inline_label("核对无误后可直接导出纯文本清单")),
                        ),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .id(export_btn_id.clone())
                        .test_support()
                        .a11y_button(export_btn_id, "导出并用记事本打开 TXT")
                        .cursor(CursorStyle::PointingHand)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .flex_shrink_0()
                        .text_color(palette.accent_ink)
                        .hover(|s| s.text_color(palette.accent_hover))
                        .on_click(move |_, w, cx| (export_fn)(eid.clone(), cid.clone(), w, cx))
                        .child(cjk_inline_label("导出并用记事本打开 TXT"))
                        .child(
                            Icon::new(IconName::ExternalLink)
                                .size(px(12.0))
                                .text_color(palette.accent_ink),
                        ),
                ),
        );
    }

    for err in props.errors {
        card = card.child(render_tool_notice_line(
            IconName::TriangleAlert,
            palette.status_warning,
            err.clone(),
            palette,
        ));
    }

    if let Some(details_section) = render_special_details_section(
        SharedString::from(format!("runbook-detail-toggle-{}", props.call_id)),
        "命令清单详情",
        props.details_expanded,
        None,
        props.fields,
        &[],
        false,
        SharedString::from(format!("runbook-raw-{}", props.call_id)),
        props.raw,
        None,
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

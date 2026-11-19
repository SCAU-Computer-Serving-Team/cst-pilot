use std::rc::Rc;

use cst_client::tool_presenter::{CommandItem, RunbookDeliveryView, SpecialStatus};
use gpui::CursorStyle;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, ClipboardItem, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale, space};

pub struct RunbookCardProps<'a> {
    pub call_id: &'a str,
    pub entry_id: &'a str,
    pub status: SpecialStatus,
    pub title: &'a str,
    pub summary: &'a str,
    pub commands: &'a [CommandItem],
    pub level: &'a str,
    pub version: Option<i64>,
    pub planned: bool,
    pub delivery: Option<&'a RunbookDeliveryView>,
    pub errors: &'a [String],
}

pub type OpenRunbookFn = Rc<dyn Fn(String, String, &mut Window, &mut App)>;

fn risk_tier_meta(level: &str, palette: &DesktopPalette) -> (&'static str, gpui_kit::Hsla) {
    match level.to_ascii_lowercase().as_str() {
        "safe" => ("安全只读", palette.status_success),
        "low" => ("低风险调整", palette.accent_solid),
        "confirm" => ("需人工确认", palette.status_warning),
        _ => ("执行建议", palette.text_secondary),
    }
}

/// Renders the standalone Command Plan & Runbook Delivery card (`command_plan`, `runbook_deliver`, `runbook`,
/// `cst-pilot-desktop.pen` 「07 · 工具与特殊工具」).
/// Groups commands by risk tier (`safe` / `low` / `confirm`), displays `JetBrains Mono` command text with
/// `cmd / powershell` tags, admin requirement badges, impact summary, single-row copy buttons, and opens
/// delivered runbooks in trusted `notepad.exe` (`CursorStyle::PointingHand`).
pub fn render_runbook_card<V: 'static>(
    props: RunbookCardProps<'_>,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_open_runbook: OpenRunbookFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    let (tier_label, tier_color) = risk_tier_meta(props.level, palette);

    let right_meta = if props.planned {
        let ver_prefix = props
            .version
            .map(|v| format!("方案 v{v} · "))
            .unwrap_or_default();
        Some(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .text_color(palette.text_secondary)
                .child(SharedString::from(format!(
                    "{ver_prefix}待核对，尚未导出文本"
                )))
                .into_any_element(),
        )
    } else if props.status != SpecialStatus::Error
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

        Some(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(SharedString::from(format!(
                    "runbook-open-file-btn-{}",
                    props.call_id
                )))
                .test_support()
                .h(height::COMPACT)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .border_1()
                .border_color(palette.border_strong)
                .bg(palette.bg_elevated)
                .text_color(palette.accent_solid)
                .cursor(CursorStyle::PointingHand)
                .flex()
                .items_center()
                .gap(space::XS)
                .hover(|s| s.bg(palette.bg_item_hover))
                .on_click(move |_, w, cx| (open_fn)(eid.clone(), cid.clone(), w, cx))
                .child(
                    Icon::new(IconName::FileText)
                        .size(px(13.0))
                        .text_color(palette.accent_solid),
                )
                .child(cjk_inline_label(file_label))
                .into_any_element(),
        )
    } else {
        None
    };

    let mut card = div()
        .id(SharedString::from(format!(
            "special-runbook-card-{}",
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
                            Icon::new(IconName::SquareTerminal)
                                .size(px(16.0))
                                .text_color(palette.accent_solid),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label(if props.title.trim().is_empty() {
                                    "修复命令清单"
                                } else {
                                    props.title
                                })),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION_MEDIUM)
                                .px(space::TIGHT)
                                .py(space::MICRO)
                                .rounded(radius::CONTROL)
                                .bg(palette.bg_surface)
                                .text_color(tier_color)
                                .child(cjk_inline_label(tier_label)),
                        ),
                )
                .children(right_meta),
        );

    if !props.summary.trim().is_empty() {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .text_color(palette.text_secondary)
                .child(SharedString::from(props.summary.to_owned())),
        );
    }

    if !props.commands.is_empty() {
        let mut cmd_col = div().w_full().flex().flex_col().gap(space::SM);
        for (idx, item) in props.commands.iter().enumerate() {
            let copy_id = SharedString::from(format!("runbook-cmd-copy-{}-{idx}", props.call_id));
            let copied = is_copied_fn(copy_id.as_ref());
            let cmd_str = item.command.clone();
            let cid_click = copy_id.clone();
            let mark_cb = on_mark_copied.clone();

            let shell_label = if item.shell.trim().is_empty() {
                "powershell"
            } else {
                item.shell.as_str()
            };

            cmd_col =
                cmd_col.child(
                    div()
                        .p(space::MD)
                        .rounded(radius::ROW)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .bg(palette.bg_surface)
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
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(space::XS)
                                        .min_w_0()
                                        .child(
                                            apply_scale(div(), scale::CAPTION_MEDIUM)
                                                .px(space::TIGHT)
                                                .py(space::MICRO)
                                                .rounded(radius::CONTROL)
                                                .bg(palette.bg_elevated)
                                                .font_family(mono_family.clone())
                                                .text_color(palette.text_secondary)
                                                .child(SharedString::from(shell_label.to_owned())),
                                        )
                                        .children(item.admin.then(|| {
                                            apply_scale(div(), scale::CAPTION_MEDIUM)
                                                .px(space::TIGHT)
                                                .py(space::MICRO)
                                                .rounded(radius::CONTROL)
                                                .bg(palette.bg_elevated)
                                                .text_color(palette.status_warning)
                                                .child(cjk_inline_label("需管理员"))
                                        }))
                                        .children((!item.summary.trim().is_empty()).then(|| {
                                            apply_scale(div(), scale::CAPTION)
                                                .truncate()
                                                .text_color(palette.text_secondary)
                                                .child(SharedString::from(item.summary.clone()))
                                        })),
                                )
                                .child(
                                    div()
                                        .id(copy_id)
                                        .test_support()
                                        .w(px(32.0))
                                        .h(px(32.0))
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
                                        .child(
                                            Icon::new(if copied {
                                                IconName::Check
                                            } else {
                                                IconName::Copy
                                            })
                                            .size(px(14.0))
                                            .text_color(if copied {
                                                palette.status_success
                                            } else {
                                                palette.text_secondary
                                            }),
                                        ),
                                ),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .font_family(mono_family.clone())
                                .text_color(palette.text_primary)
                                .child(SharedString::from(item.command.clone())),
                        ),
                );
        }
        card = card.child(cmd_col);
    }

    for err in props.errors {
        card = card.child(
            apply_scale(div(), scale::CAPTION)
                .text_color(palette.status_danger)
                .child(SharedString::from(err.clone())),
        );
    }

    card.into_any_element()
}

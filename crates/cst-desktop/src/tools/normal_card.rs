use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use cst_client::tool_presenter::{
    CardStatus, CommandItem, FieldRow, ListingItem, NoticeStatus, ToolBlock, ToolCardView,
};
use gpui::{ImageSource, ObjectFit, StyledImage as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, ClipboardItem, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, img, px,
};

use crate::theme::{DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale, space};
use crate::workbench::image_viewer::{LightboxImageTarget, gpui_image_from_bytes};
use crate::workbench::markdown_view::render_copyable_text_block;
use crate::workbench::turn_segments::format_duration_zh;

pub fn localized_normal_tool_title(tool_name: &str) -> &'static str {
    match tool_name {
        "disk" => "磁盘与卷检查",
        "driver" => "驱动程序检查",
        "eventlog" => "系统事件日志",
        "ls" => "目录列表",
        "startup" => "开机自启审查",
        "sys" => "硬件与进程采样",
        "bitlocker" => "BitLocker 卷状态",
        "read" => "读取文件内容",
        "fffind" => "按名查找文件",
        "ffgrep" => "内容全文检索",
        "source_check" => "软件来源核验",
        "fetch_content" => "抓取网页正文",
        "get_search_content" => "读取检索缓存",
        _ => "诊断工具调用",
    }
}

pub type OpenLightboxFn = Rc<dyn Fn(LightboxImageTarget, &mut Window, &mut App)>;
pub type ToggleKeyFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type CopyMarkedFn = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;

/// Renders an ordinary diagnostic tool card (`cst-pilot-desktop.pen` 「07 · 工具与特殊工具」)
/// with frozen duration, `24px` (`space::GROUP`) indented vertical guide line, longest-label column
/// alignment, `read` image-before-path ordering, and collapsible `"原始数据"` block.
#[allow(clippy::too_many_arguments)]
pub fn render_normal_tool_card<V: 'static>(
    call_id: &str,
    tool_name: &str,
    duration_ms: Option<u64>,
    has_result: bool,
    is_error_result: bool,
    live: bool,
    card_view: &ToolCardView,
    is_expanded: bool,
    is_raw_expanded: bool,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_card: ToggleKeyFn,
    on_toggle_raw: ToggleKeyFn,
    on_open_lightbox: OpenLightboxFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    let status_color = if !has_result {
        if live {
            palette.accent_solid
        } else {
            palette.status_warning
        }
    } else if is_error_result || card_view.status == CardStatus::Error {
        palette.status_danger
    } else if card_view.status == CardStatus::Degraded {
        palette.status_warning
    } else {
        palette.status_success
    };

    let card_key = format!("tool-card-{call_id}");
    let raw_key = format!("tool-raw-{call_id}");

    let duration_badge = duration_ms.filter(|&ms| ms > 0).map(|ms| {
        apply_scale(div(), scale::CAPTION)
            .font_family(mono_family.clone())
            .text_color(palette.text_muted)
            .child(SharedString::from(format_duration_zh(ms)))
    });

    let toggle_hdr = Rc::clone(&on_toggle_card);
    let key_hdr = card_key.clone();

    let header = apply_scale(div(), scale::UI)
        .id(SharedString::from(format!("normal-tool-hdr-{call_id}")))
        .test_support()
        .h(height::COMPACT)
        .px(space::SM)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .justify_between()
        .gap(space::SM)
        .hover(|s| s.bg(palette.bg_item_hover))
        .on_click(move |_, w, cx| (toggle_hdr)(key_hdr.clone(), w, cx))
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .child(
                    div()
                        .w(px(7.0))
                        .h(px(7.0))
                        .rounded(radius::FULL)
                        .bg(status_color),
                )
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label(localized_normal_tool_title(tool_name))),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .font_family(mono_family.clone())
                        .text_color(palette.text_muted)
                        .child(SharedString::from(tool_name.to_owned())),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .children(duration_badge)
                .child(
                    Icon::new(if is_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(12.0))
                    .text_color(palette.text_muted),
                ),
        );

    let mut container = div()
        .id(SharedString::from(format!("normal-tool-card-{call_id}")))
        .test_support()
        .w_full()
        .flex()
        .flex_col()
        .gap(space::XS)
        .child(header);

    if !is_expanded {
        return container.into_any_element();
    }

    // Compute longest field label width across all `Fields` blocks in this card so values align cleanly!
    let max_label_chars = card_view
        .blocks
        .iter()
        .filter_map(|b| match b {
            ToolBlock::Fields { rows, .. } => rows.iter().map(|r| r.label.chars().count()).max(),
            _ => None,
        })
        .max()
        .unwrap_or(6);
    let label_col_px = ((max_label_chars as f32) * 13.5 + 12.0).clamp(72.0, 180.0);

    // `24px` (`space::GROUP`) left indent with continuous vertical guide line
    let mut body = div()
        .pl(space::GROUP)
        .border_l_1()
        .border_color(palette.border_subtle)
        .flex()
        .flex_col()
        .gap(space::SM);

    // Per plan §3.4: for `read` with images, render image thumbnails FIRST, then absolute path / text blocks!
    let mut ordered_blocks: Vec<&ToolBlock> = Vec::with_capacity(card_view.blocks.len());
    for blk in &card_view.blocks {
        if matches!(blk, ToolBlock::Image { .. }) {
            ordered_blocks.push(blk);
        }
    }
    for blk in &card_view.blocks {
        if !matches!(blk, ToolBlock::Image { .. }) {
            ordered_blocks.push(blk);
        }
    }

    for (blk_idx, block) in ordered_blocks.into_iter().enumerate() {
        match block {
            ToolBlock::Image {
                mime_type, data, ..
            } => {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim()) {
                    let gpui_img = gpui_image_from_bytes(mime_type, bytes);
                    let img_open = Arc::clone(&gpui_img);
                    let lb_cb = Rc::clone(&on_open_lightbox);
                    body = body.child(
                        div()
                            .id(SharedString::from(format!(
                                "tool-read-img-{call_id}-{blk_idx}"
                            )))
                            .test_support()
                            .w(px(160.0))
                            .h(px(112.0))
                            .rounded(radius::ROW)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_surface)
                            .overflow_hidden()
                            .on_click(move |_, w, cx| {
                                let prior = w.focused(cx);
                                (lb_cb)(
                                    LightboxImageTarget {
                                        title: SharedString::from("工具读取图片"),
                                        dimensions: None,
                                        image: Arc::clone(&img_open),
                                        prior_focus: prior,
                                    },
                                    w,
                                    cx,
                                );
                            })
                            .child(
                                img(ImageSource::Image(gpui_img))
                                    .size_full()
                                    .object_fit(ObjectFit::Cover),
                            ),
                    );
                }
            }
            ToolBlock::Fields { title, rows, .. } => {
                let mut group = div().w_full().flex().flex_col().gap(space::XS);
                if let Some(t) = title.as_deref().filter(|s| !s.is_empty()) {
                    group = group.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .text_color(palette.text_muted)
                            .child(SharedString::from(t.to_owned())),
                    );
                }
                for row in rows {
                    group = group.child(render_aligned_field_row(
                        row,
                        label_col_px,
                        mono_family.clone(),
                        palette,
                    ));
                }
                body = body.child(group);
            }
            ToolBlock::Listing { items, .. } => {
                body = body.child(render_listing_table(items, mono_family.clone(), palette));
            }
            ToolBlock::Commands { items, .. } => {
                body = body.child(render_inline_commands_list(
                    call_id,
                    blk_idx,
                    items,
                    mono_family.clone(),
                    palette,
                    is_copied_fn,
                    on_mark_copied.clone(),
                    cx,
                ));
            }
            ToolBlock::Notice { text, status, .. } => {
                let color = match status {
                    NoticeStatus::Danger => palette.status_danger,
                    NoticeStatus::Warning => palette.status_warning,
                    NoticeStatus::Default => palette.text_secondary,
                };
                body = body.child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(color)
                        .child(SharedString::from(text.clone())),
                );
            }
            ToolBlock::Text { text, .. } => {
                body = body.child(
                    apply_scale(div(), scale::CAPTION)
                        .font_family(mono_family.clone())
                        .text_color(palette.text_secondary)
                        .child(SharedString::from(text.clone())),
                );
            }
        }
    }

    if !card_view.raw.trim().is_empty() {
        let toggle_raw = Rc::clone(&on_toggle_raw);
        let rk = raw_key.clone();
        body = body.child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id(SharedString::from(format!(
                    "normal-tool-raw-toggle-{call_id}"
                )))
                .test_support()
                .flex()
                .items_center()
                .gap(space::MICRO)
                .text_color(palette.text_muted)
                .on_click(move |_, w, cx| (toggle_raw)(rk.clone(), w, cx))
                .child(cjk_inline_label(if is_raw_expanded {
                    "收起原始数据"
                } else {
                    "原始数据"
                }))
                .child(
                    Icon::new(if is_raw_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(11.0))
                    .text_color(palette.text_muted),
                ),
        );
        if is_raw_expanded {
            let blk_id = SharedString::from(format!("tool-raw-block-{call_id}"));
            let copied = is_copied_fn(blk_id.as_ref());
            let mark_cb = on_mark_copied.clone();
            body = body.child(render_copyable_text_block(
                blk_id,
                card_view.raw.clone(),
                true,
                mono_family,
                copied,
                palette,
                move |cid, w, cx| mark_cb(cid, w, cx),
                cx,
            ));
        }
    }

    container = container.child(body);
    container.into_any_element()
}

fn render_aligned_field_row(
    row: &FieldRow,
    label_col_px: f32,
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let looks_numeric = row
        .value
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, 'C' | 'D' | 'E' | 'F' | '/'));
    let mut val_div = apply_scale(div(), scale::CAPTION)
        .flex_1()
        .min_w_0()
        .text_color(palette.text_primary);
    if looks_numeric {
        val_div = val_div.font_family(mono_family);
    }

    div()
        .w_full()
        .flex()
        .items_start()
        .gap(space::MD)
        .child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .w(px(label_col_px))
                .flex_shrink_0()
                .text_color(palette.text_secondary)
                .child(SharedString::from(row.label.clone())),
        )
        .child(val_div.child(SharedString::from(row.value.clone())))
}

fn render_listing_table(
    items: &[ListingItem],
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let mut table = div()
        .w_full()
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_surface)
        .flex()
        .flex_col();

    for (idx, item) in items.iter().enumerate() {
        let mut metrics_row = div().flex().flex_wrap().items_center().gap(space::MD);
        for m in &item.metrics {
            metrics_row = metrics_row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_muted)
                            .child(SharedString::from(m.label.clone())),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_primary)
                            .child(SharedString::from(m.value.clone())),
                    ),
            );
        }

        let mut row_div = div()
            .px(space::SM)
            .py(space::XS)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                apply_scale(div(), scale::UI)
                    .min_w_0()
                    .truncate()
                    .text_color(palette.text_primary)
                    .child(SharedString::from(item.name.clone())),
            )
            .child(metrics_row);

        if idx > 0 {
            row_div = row_div.border_t_1().border_color(palette.border_subtle);
        }
        table = table.child(row_div);
    }

    table
}

#[allow(clippy::too_many_arguments)]
fn render_inline_commands_list<V: 'static>(
    call_id: &str,
    blk_idx: usize,
    items: &[CommandItem],
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let mut col = div().w_full().flex().flex_col().gap(space::XS);
    for (idx, cmd) in items.iter().enumerate() {
        let copy_id = SharedString::from(format!("norm-cmd-copy-{call_id}-{blk_idx}-{idx}"));
        let copied = is_copied_fn(copy_id.as_ref());
        let cmd_text = cmd.command.clone();
        let cid_click = copy_id.clone();
        let mark_cb = on_mark_copied.clone();

        col = col.child(
            div()
                .p(space::SM)
                .rounded(radius::ROW)
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
                        .font_family(mono_family.clone())
                        .text_color(palette.text_primary)
                        .child(SharedString::from(cmd.command.clone())),
                )
                .child(
                    div()
                        .id(copy_id)
                        .w(px(32.0))
                        .h(px(32.0))
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(move |_, _, w, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(cmd_text.clone()));
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
        );
    }
    col
}

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

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
use serde_json::Value;

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};
use crate::workbench::image_viewer::{LightboxImageTarget, gpui_image_from_bytes};
use crate::workbench::markdown_view::{
    render_copyable_text_block, render_copyable_text_block_inert,
};

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

pub fn format_duration_en_sec(ms: u64) -> String {
    let secs = ((ms as f64) / 1000.0).max(0.1);
    format!("{secs:.1}s")
}

fn format_call_arg_value(val: &Value) -> String {
    match val {
        Value::String(s) => {
            if s.chars().count() > 42 {
                let prefix: String = s.chars().take(39).collect();
                format!("{prefix}…")
            } else {
                format!("{s:?}")
            }
        }
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(arr) => format!("[…{}]", arr.len()),
        Value::Object(_) => "{…}".to_owned(),
    }
}

fn format_call_capsule(tool_name: &str, call_args: Option<&Value>) -> String {
    let Some(obj) = call_args.and_then(Value::as_object) else {
        return format!("{tool_name}()");
    };
    if obj.is_empty() {
        return format!("{tool_name}()");
    }
    let mut parts = Vec::with_capacity(obj.len());
    if let Some(sc) = obj.get("scope") {
        parts.push(format!("scope: {}", format_call_arg_value(sc)));
    }
    for (k, v) in obj {
        if k == "scope" || v.is_null() {
            continue;
        }
        parts.push(format!("{k}: {}", format_call_arg_value(v)));
    }
    format!("{tool_name}({})", parts.join(", "))
}

pub type OpenLightboxFn = Rc<dyn Fn(LightboxImageTarget, &mut Window, &mut App)>;
pub type ToggleKeyFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type CopyMarkedFn = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;

/// Renders an ordinary diagnostic tool card (`BuK2W` collapsed / `E9dWo9`, `VM8ko`, `VwO1J` expanded).
#[allow(clippy::too_many_arguments)]
pub fn render_normal_tool_card<V: 'static>(
    call_id: &str,
    tool_name: &str,
    duration_ms: Option<u64>,
    has_result: bool,
    is_error_result: bool,
    live: bool,
    card_view: Option<&ToolCardView>,
    is_expanded: bool,
    is_raw_expanded: bool,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_card: ToggleKeyFn,
    on_toggle_raw: ToggleKeyFn,
    on_open_lightbox: OpenLightboxFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    render_normal_tool_card_with_args(
        call_id,
        tool_name,
        None,
        duration_ms,
        has_result,
        is_error_result,
        live,
        false,
        None,
        card_view,
        is_expanded,
        is_raw_expanded,
        mono_family,
        palette,
        is_copied_fn,
        on_toggle_card,
        on_toggle_raw,
        on_open_lightbox,
        on_mark_copied,
        window,
        cx,
    )
}

/// Renders an ordinary diagnostic tool card (`cst-pilot-desktop.pen` `BuK2W`, `E9dWo9`, `VM8ko`, `VwO1J`)
/// with `16×16px` semantic status icon, continuous `2px` vertical guide rail, `24px` `$bg-surface`
/// call parameter capsule, frameless aligned rows, `VwO1J` copyable text block, and `24px` `"原始数据"` disclosure.
#[allow(clippy::too_many_arguments)]
pub fn render_normal_tool_card_with_args<V: 'static>(
    call_id: &str,
    tool_name: &str,
    call_args: Option<&Value>,
    duration_ms: Option<u64>,
    has_result: bool,
    is_error_result: bool,
    live: bool,
    running_light_allowed: bool,
    started_at: Option<Instant>,
    card_view: Option<&ToolCardView>,
    is_expanded: bool,
    is_raw_expanded: bool,
    mono_family: SharedString,
    palette: &DesktopPalette,
    is_copied_fn: &dyn Fn(&str) -> bool,
    on_toggle_card: ToggleKeyFn,
    on_toggle_raw: ToggleKeyFn,
    on_open_lightbox: OpenLightboxFn,
    on_mark_copied: impl Fn(SharedString, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui_kit::AnyElement {
    let card_view = has_result.then_some(card_view).flatten();
    let card_status = card_view.map(|view| view.status);
    let is_running = !has_result && live;
    let (status_icon, status_color, status_word) = if !has_result {
        if live {
            (IconName::LoaderCircle, palette.accent_ink, "运行中")
        } else {
            (IconName::TriangleAlert, palette.status_warning, "已中断")
        }
    } else if is_error_result || card_status == Some(CardStatus::Error) {
        (IconName::CircleAlert, palette.status_danger, "执行失败")
    } else if card_status == Some(CardStatus::Degraded) {
        (IconName::TriangleAlert, palette.status_warning, "部分降级")
    } else {
        (IconName::CircleCheck, palette.status_success, "已完成")
    };

    let scope_opt = call_args
        .and_then(|v| v.get("scope"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let summary_text = match scope_opt {
        Some(sc) => format!("{tool_name}({sc}) · {status_word}"),
        None => format!(
            "{tool_name} · {} · {status_word}",
            localized_normal_tool_title(tool_name)
        ),
    };

    let card_key = format!("tool-card-{call_id}");
    let raw_key = format!("tool-raw-{call_id}");

    let duration_badge = duration_ms.filter(|&ms| ms > 0).map(|ms| {
        apply_scale(div(), scale::CAPTION)
            .font_family(mono_family.clone())
            .text_color(palette.text_muted)
            .child(SharedString::from(format_duration_en_sec(ms)))
    });

    let toggle_hdr = Rc::clone(&on_toggle_card);
    let key_hdr = card_key.clone();
    let card_a11y_id = SharedString::from(format!("normal-tool-card-{call_id}"));
    let hdr_a11y_id = SharedString::from(format!("normal-tool-hdr-{call_id}"));
    let summary_label = SharedString::from(summary_text.clone());

    let (hdr_hovered, hdr_hs) =
        motion::PointerFlag::keyed(format!("hover-normal-tool-hdr-{call_id}"), window, cx);
    let hdr_hover_t = motion::animated_state_progress(
        format!("normal-tool-hdr-{call_id}"),
        hdr_hovered,
        window,
        cx,
    );
    let hdr_fg = motion::lerp_hsla(palette.text_secondary, palette.text_primary, hdr_hover_t);

    let status_icon_el = if is_running {
        motion::animated_spinner(
            format!("normal-tool-spin-{call_id}"),
            px(16.0),
            status_color,
            running_light_allowed,
            window,
            cx,
        )
    } else {
        Icon::new(status_icon)
            .size(px(16.0))
            .text_color(status_color)
            .into_any_element()
    };
    let summary_el = if is_running && running_light_allowed {
        crate::workbench::turn_segments::render_running_light_label_with_scale(
            &summary_text,
            true,
            started_at.unwrap_or_else(Instant::now),
            hdr_fg,
            palette.text_primary,
            scale::UI,
            window,
        )
        .into_any_element()
    } else {
        div()
            .min_w_0()
            .truncate()
            .child(cjk_inline_label(summary_text))
            .into_any_element()
    };

    let header_row = apply_scale(div(), scale::UI)
        .id(hdr_a11y_id.clone())
        .test_support()
        .a11y_toggle_button(hdr_a11y_id, summary_label.clone(), is_expanded)
        .w_full()
        .h(height::COMPACT)
        .px(px(0.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(space::SM)
        .text_color(hdr_fg)
        .on_hover(move |active, w, cx| {
            if *active {
                motion::PointerFlag::enter(&hdr_hs, w, cx);
            } else {
                motion::PointerFlag::set(&hdr_hs, false, cx);
            }
        })
        .on_click(move |_, w, cx| (toggle_hdr)(key_hdr.clone(), w, cx))
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .min_w_0()
                .child(status_icon_el)
                .child(summary_el),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .flex_shrink_0()
                .children(duration_badge)
                .child(motion::animated_chevron(
                    format!("normal-card-{call_id}"),
                    is_expanded,
                    false,
                    px(16.0),
                    hdr_fg,
                    window,
                    cx,
                )),
        );

    let mut card_shell = div()
        .id(card_a11y_id.clone())
        .test_support()
        .a11y_region(card_a11y_id, summary_label)
        .w_full()
        .flex()
        .flex_col()
        .child(header_row);

    let call_capsule_str = format_call_capsule(tool_name, call_args);
    let label_col_px = super::aligned_label_col_px(
        card_view
            .into_iter()
            .flat_map(|view| view.blocks.iter())
            .flat_map(|block| match block {
                ToolBlock::Fields { rows, .. } => rows.as_slice(),
                _ => &[],
            }),
    );

    if let Some(animated_body) = motion::animated_collapse_with_exit(
        format!("normal-tool-body-{call_id}"),
        is_expanded,
        window,
        cx,
        |interactive, window, cx| {
            let call_capsule_row = div()
                .id(SharedString::from(format!("normal-tool-capsule-{call_id}")))
                .test_support()
                .w_full()
                .flex()
                .items_center()
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .h(height::INLINE)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .bg(palette.bg_surface)
                        .font_family(mono_family.clone())
                        .text_color(palette.text_secondary)
                        .max_w_full()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(call_capsule_str)),
                        ),
                );

            let mut right_col = div()
                .flex_1()
                .min_w_0()
                .pt(space::MICRO)
                .pb(space::MICRO)
                .flex()
                .flex_col()
                .gap(space::SM)
                .child(call_capsule_row);

            if let Some(card_view) = card_view {
                if interactive {
                    // For `read` with images, render image thumbnails FIRST, then path / text blocks
                    let mut ordered_blocks: Vec<&ToolBlock> =
                        Vec::with_capacity(card_view.blocks.len());
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
                                if let Ok(bytes) =
                                    base64::engine::general_purpose::STANDARD.decode(data.trim())
                                    && !bytes.is_empty()
                                {
                                    let gpui_img = gpui_image_from_bytes(mime_type, bytes);
                                    let img_open = Arc::clone(&gpui_img);
                                    let lb_cb = Rc::clone(&on_open_lightbox);
                                    let read_img_id = SharedString::from(format!(
                                        "tool-read-img-{call_id}-{blk_idx}"
                                    ));
                                    right_col = right_col.child(
                                        div()
                                            .id(read_img_id.clone())
                                            .test_support()
                                            .a11y_button(read_img_id, "预览工具读取图片")
                                            .w(px(300.0))
                                            .h(px(132.0))
                                            .rounded(radius::CONTROL)
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
                                                    .object_fit(ObjectFit::Contain),
                                            ),
                                    );
                                }
                            }
                            ToolBlock::Fields { title, rows, .. } => {
                                let mut group = div().w_full().flex().flex_col().gap(space::MICRO);
                                if let Some(t) = title.as_deref().filter(|s| !s.is_empty()) {
                                    group = group.child(
                                        apply_scale(div(), scale::CAPTION_MEDIUM)
                                            .text_color(palette.text_secondary)
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
                                right_col = right_col.child(group);
                            }
                            ToolBlock::Listing { items, .. } => {
                                right_col = right_col.child(render_listing_table(
                                    items,
                                    mono_family.clone(),
                                    palette,
                                ));
                            }
                            ToolBlock::Commands { items, .. } => {
                                right_col = right_col.child(render_inline_commands_list(
                                    call_id,
                                    blk_idx,
                                    items,
                                    mono_family.clone(),
                                    palette,
                                    is_copied_fn,
                                    on_mark_copied.clone(),
                                    window,
                                    cx,
                                ));
                            }
                            ToolBlock::Notice { text, status, .. } => {
                                let (icon, color) = match status {
                                    NoticeStatus::Danger => {
                                        (IconName::CircleAlert, palette.status_danger)
                                    }
                                    NoticeStatus::Warning => {
                                        (IconName::TriangleAlert, palette.status_warning)
                                    }
                                    NoticeStatus::Default => {
                                        (IconName::Info, palette.text_secondary)
                                    }
                                };
                                right_col = right_col.child(super::render_tool_notice_line(
                                    icon,
                                    color,
                                    text.clone(),
                                    palette,
                                ));
                            }
                            ToolBlock::Text { text, .. } => {
                                let txt_copy_id = SharedString::from(format!(
                                    "norm-text-copy-{call_id}-{blk_idx}"
                                ));
                                let copied = is_copied_fn(txt_copy_id.as_ref());
                                let txt_owned = text.clone();
                                let cid_click = txt_copy_id.clone();
                                let mark_cb = on_mark_copied.clone();

                                // `VwO1J`: `p: 12px, r: 4px, bg: $bg-surface` + right `28×28px` copy button
                                right_col = right_col.child(
                                    div()
                                        .w_full()
                                        .p(space::MD)
                                        .rounded(radius::CONTROL)
                                        .bg(palette.bg_surface)
                                        .flex()
                                        .items_start()
                                        .justify_between()
                                        .gap(space::MD)
                                        .child(
                                            apply_scale(div(), scale::CAPTION)
                                                .flex_1()
                                                .min_w_0()
                                                .font_family(mono_family.clone())
                                                .text_color(palette.text_secondary)
                                                .child(SharedString::from(text.clone())),
                                        )
                                        .child(
                                            div()
                                                .id(txt_copy_id.clone())
                                                .test_support()
                                                .a11y_button(txt_copy_id, "复制输出内容")
                                                .w(height::COMPACT)
                                                .h(height::COMPACT)
                                                .flex_shrink_0()
                                                .rounded(radius::CONTROL)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .hover(|s| s.bg(palette.bg_item_hover))
                                                .on_click(cx.listener(move |_, _, w, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(
                                                            txt_owned.clone(),
                                                        ),
                                                    );
                                                    mark_cb(cid_click.clone(), w, cx);
                                                }))
                                                .child(motion::animated_copy_icon(
                                                    SharedString::from(format!(
                                                        "normal-out-{call_id}-{blk_idx}"
                                                    )),
                                                    copied,
                                                    px(14.0),
                                                    if copied {
                                                        palette.status_success
                                                    } else {
                                                        palette.text_secondary
                                                    },
                                                    window,
                                                    cx,
                                                )),
                                        ),
                                );
                            }
                        }
                    }

                    if !card_view.raw.trim().is_empty() {
                        let toggle_raw = Rc::clone(&on_toggle_raw);
                        let rk = raw_key;
                        let raw_toggle_id =
                            SharedString::from(format!("normal-tool-raw-toggle-{call_id}"));
                        let mut raw_section =
                            div().w_full().flex().flex_col().gap(space::TIGHT).child(
                                apply_scale(div(), scale::CAPTION)
                                    .id(raw_toggle_id.clone())
                                    .test_support()
                                    .a11y_toggle_button(raw_toggle_id, "原始数据", is_raw_expanded)
                                    .w_full()
                                    .h(height::INLINE)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .text_color(palette.text_secondary)
                                    .hover(|s| s.text_color(palette.text_primary))
                                    .on_click(move |_, w, cx| (toggle_raw)(rk.clone(), w, cx))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(space::SM)
                                            .child(
                                                Icon::new(IconName::Braces)
                                                    .size(px(12.0))
                                                    .text_color(palette.text_secondary),
                                            )
                                            .child(cjk_inline_label("原始数据")),
                                    )
                                    .child(motion::animated_chevron(
                                        format!("normal-raw-{call_id}"),
                                        is_raw_expanded,
                                        false,
                                        px(12.0),
                                        palette.text_secondary,
                                        window,
                                        cx,
                                    )),
                            );

                        if let Some(raw_body) = motion::animated_collapse_with_exit(
                            format!("normal-raw-body-{call_id}"),
                            is_raw_expanded,
                            window,
                            cx,
                            |interactive, window, cx| {
                                if !interactive {
                                    return render_copyable_text_block_inert(
                                        card_view.raw.clone(),
                                        true,
                                        mono_family,
                                        palette,
                                    )
                                    .into_any_element();
                                }
                                let blk_id =
                                    SharedString::from(format!("tool-raw-block-{call_id}"));
                                let copied = is_copied_fn(blk_id.as_ref());
                                let mark_cb = on_mark_copied.clone();
                                render_copyable_text_block(
                                    blk_id,
                                    card_view.raw.clone(),
                                    true,
                                    mono_family,
                                    copied,
                                    palette,
                                    move |cid, w, cx| mark_cb(cid, w, cx),
                                    window,
                                    cx,
                                )
                                .into_any_element()
                            },
                        ) {
                            raw_section = raw_section.child(raw_body);
                        }

                        right_col = right_col.child(raw_section);
                    }
                } else {
                    for block in &card_view.blocks {
                        match block {
                            ToolBlock::Fields { title, rows, .. } => {
                                let mut group = div().w_full().flex().flex_col().gap(space::MICRO);
                                if let Some(t) = title.as_deref().filter(|s| !s.is_empty()) {
                                    group = group.child(
                                        apply_scale(div(), scale::CAPTION_MEDIUM)
                                            .text_color(palette.text_secondary)
                                            .child(SharedString::from(t.to_owned())),
                                    );
                                }
                                for row in rows.iter().take(6) {
                                    group = group.child(render_aligned_field_row(
                                        row,
                                        label_col_px,
                                        mono_family.clone(),
                                        palette,
                                    ));
                                }
                                right_col = right_col.child(group);
                            }
                            ToolBlock::Text { text, .. } => {
                                right_col = right_col.child(render_copyable_text_block_inert(
                                    text.clone(),
                                    true,
                                    mono_family.clone(),
                                    palette,
                                ));
                            }
                            _ => {}
                        }
                    }
                }
            }

            let guide_rail = div()
                .w(px(16.0))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .pt(px(2.0))
                .pb(px(4.0))
                .child(
                    div()
                        .w(px(2.0))
                        .h_full()
                        .rounded(radius::FULL)
                        .bg(palette.border_subtle),
                );

            div()
                .w_full()
                .flex()
                .gap(space::SM)
                .child(guide_rail)
                .child(right_col)
                .into_any_element()
        },
    ) {
        card_shell = card_shell.child(animated_body);
    }

    card_shell.into_any_element()
}

/// Visual-only inert clone of a collapsed normal tool status row, used during the parent process
/// drawer's `150ms` close animation so rows shrink cleanly without hard-cutting to bare names.
#[allow(clippy::too_many_arguments)]
pub fn render_normal_tool_row_inert(
    tool_name: &str,
    call_args: Option<&Value>,
    duration_ms: Option<u64>,
    has_result: bool,
    is_error_result: bool,
    live: bool,
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> gpui_kit::AnyElement {
    let (status_icon, status_color, status_word) = if !has_result {
        if live {
            (IconName::LoaderCircle, palette.accent_ink, "运行中")
        } else {
            (IconName::TriangleAlert, palette.status_warning, "已中断")
        }
    } else if is_error_result {
        (IconName::CircleAlert, palette.status_danger, "执行失败")
    } else {
        (IconName::CircleCheck, palette.status_success, "已完成")
    };

    let scope_opt = call_args
        .and_then(|v| v.get("scope"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let summary_text = match scope_opt {
        Some(sc) => format!("{tool_name}({sc}) · {status_word}"),
        None => format!(
            "{tool_name} · {} · {status_word}",
            localized_normal_tool_title(tool_name)
        ),
    };

    let duration_badge = duration_ms.filter(|&ms| ms > 0).map(|ms| {
        apply_scale(div(), scale::CAPTION)
            .font_family(mono_family)
            .text_color(palette.text_muted)
            .child(SharedString::from(format_duration_en_sec(ms)))
    });

    apply_scale(div(), scale::UI)
        .w_full()
        .h(height::COMPACT)
        .px(px(0.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(space::SM)
        .text_color(palette.text_secondary)
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .min_w_0()
                .child(
                    Icon::new(status_icon)
                        .size(px(16.0))
                        .text_color(status_color),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(cjk_inline_label(summary_text)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::SM)
                .flex_shrink_0()
                .children(duration_badge)
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(16.0))
                        .text_color(palette.text_secondary),
                ),
        )
        .into_any_element()
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
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, 'C' | 'D' | 'E' | 'F' | '/' | '\\'));
    let mut val_div = apply_scale(div(), scale::CAPTION)
        .flex_1()
        .min_w_0()
        .text_color(palette.text_secondary);
    if looks_numeric {
        val_div = val_div.font_family(mono_family);
    }

    div()
        .w_full()
        .min_h(px(18.0))
        .flex()
        .items_center()
        .gap(space::SM)
        .child(
            apply_scale(div(), scale::CAPTION)
                .w(px(label_col_px))
                .flex_shrink_0()
                .text_color(palette.text_secondary)
                .child(cjk_inline_label(row.label.clone())),
        )
        .child(val_div.child(SharedString::from(row.value.clone())))
}

/// Frameless transparent aligned ranking/listing rows (`VM8ko` — no outer card border or surface background).
fn render_listing_table(
    items: &[ListingItem],
    mono_family: SharedString,
    palette: &DesktopPalette,
) -> impl IntoElement {
    let mut table = div().w_full().flex().flex_col().gap(space::MICRO);

    for item in items {
        let mut metrics_parts: Vec<String> = Vec::with_capacity(item.metrics.len());
        for m in &item.metrics {
            if m.label.is_empty() || matches!(m.label.as_str(), "大小" | "占比") {
                metrics_parts.push(m.value.clone());
            } else {
                metrics_parts.push(format!("{} {}", m.label, m.value));
            }
        }
        let metrics_text = metrics_parts.join(" · ");

        table = table.child(
            div()
                .w_full()
                .min_h(px(18.0))
                .flex()
                .items_center()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .w(px(160.0))
                        .flex_shrink_0()
                        .truncate()
                        .text_color(palette.text_secondary)
                        .child(SharedString::from(item.name.clone())),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .flex_1()
                        .min_w_0()
                        .font_family(mono_family.clone())
                        .text_color(palette.text_secondary)
                        .child(SharedString::from(metrics_text)),
                ),
        );
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
    window: &mut Window,
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
                        .child(SharedString::from(cmd.command.clone())),
                )
                .child(
                    div()
                        .id(copy_id.clone())
                        .test_support()
                        .a11y_button(copy_id, "复制命令")
                        .w(height::CONTROL)
                        .h(height::CONTROL)
                        .flex_shrink_0()
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(move |_, _, w, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(cmd_text.clone()));
                            mark_cb(cid_click.clone(), w, cx);
                        }))
                        .child(motion::animated_copy_icon(
                            SharedString::from(format!("normal-cmd-{call_id}-{idx}")),
                            copied,
                            px(14.0),
                            if copied {
                                palette.status_success
                            } else {
                                palette.text_secondary
                            },
                            window,
                            cx,
                        )),
                ),
        );
    }
    col
}

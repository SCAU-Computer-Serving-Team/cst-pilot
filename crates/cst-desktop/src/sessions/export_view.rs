use std::rc::Rc;

use cst_client::ClientHandle;
use cst_client::tool_presenter::{ToolCallInput, ToolExecutionMeta, ToolResultInput};
use gpui::CursorStyle;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};
use serde_json::Value;

use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, radius, scale, space,
};
use crate::tools::normal_card::render_normal_tool_card;
use crate::tools::{SpecialToolDispatchProps, render_special_tool_component};
use crate::workbench::turn_segments::{
    ConversationPart, build_turn_segments, extract_message_text, group_turns_and_results,
    split_turn,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormatChoice {
    Png,
    Html,
}

impl ExportFormatChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Html => "html",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG 长图",
            Self::Html => "HTML 离线文档",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportDocTheme {
    Light,
    Dark,
}

impl ExportDocTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn palette(self) -> DesktopPalette {
        match self {
            Self::Light => DesktopPalette::light(),
            Self::Dark => DesktopPalette::dark(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExportViewEvent {
    BackToChat,
    Notice(String),
    ErrorMessage(String),
}

/// Session Export & Native Read-Only Preview (`cst-pilot-desktop.pen` 「02 · 会话管理」 `Y8hs0` -> `Ntay1`):
/// Left `264px` options column (`ER02w`) + Right independent-theme GPUI native preview (`eGkN1`).
pub struct ExportView {
    client: ClientHandle,
    session_id: String,
    format: ExportFormatChoice,
    doc_theme: ExportDocTheme,
    include_thinking: bool,
    include_tools: bool,
    preview_snapshot: Option<Value>,
    loading_preview: bool,
    preview_generation: u64,
    exporting: bool,
    last_completed_export_id: Option<String>,
    last_completed_dir: Option<String>,
}

impl EventEmitter<ExportViewEvent> for ExportView {}

impl ExportView {
    pub fn new(client: ClientHandle, session_id: String, cx: &mut Context<Self>) -> Self {
        let initial_doc_theme = if DesktopThemeGlobal::current(cx).palette.mode.is_dark() {
            ExportDocTheme::Dark
        } else {
            ExportDocTheme::Light
        };
        let mut this = Self {
            client,
            session_id,
            format: ExportFormatChoice::Png,
            doc_theme: initial_doc_theme,
            include_thinking: true,
            include_tools: true,
            preview_snapshot: None,
            loading_preview: false,
            preview_generation: 0,
            exporting: false,
            last_completed_export_id: None,
            last_completed_dir: None,
        };
        this.reload_preview(cx);
        this
    }

    pub fn set_session_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.session_id != session_id {
            self.session_id = session_id;
            self.preview_snapshot = None;
            self.last_completed_export_id = None;
            self.last_completed_dir = None;
            self.reload_preview(cx);
        }
    }

    /// Fetches `/api/sessions/<id>/exports/preview` while retaining `self.preview_snapshot` until the new snapshot lands.
    pub fn reload_preview(&mut self, cx: &mut Context<Self>) {
        if self.session_id.is_empty() {
            return;
        }
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let req_gen = self.preview_generation;
        self.loading_preview = true;
        cx.notify();

        let client = self.client.clone();
        let sid = self.session_id.clone();
        let fmt = self.format.as_str().to_owned();
        let thm = self.doc_theme.as_str().to_owned();
        let inc_think = self.include_thinking;
        let inc_tools = self.include_tools;

        cx.spawn(async move |weak, cx| {
            let res = client
                .preview_session_export(sid, fmt, thm, inc_think, inc_tools)
                .await;
            let _ = weak.update(cx, |this, cx| {
                if this.preview_generation != req_gen {
                    return;
                }
                this.loading_preview = false;
                match res {
                    Ok(snap) => {
                        this.preview_snapshot = Some(snap);
                    }
                    Err(err) => {
                        cx.emit(ExportViewEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_generate_and_save(&mut self, cx: &mut Context<Self>) {
        if self.exporting || self.session_id.is_empty() {
            return;
        }
        self.exporting = true;
        cx.notify();

        let client = self.client.clone();
        let sid = self.session_id.clone();
        let fmt = self.format.as_str().to_owned();
        let thm = self.doc_theme.as_str().to_owned();
        let inc_think = self.include_thinking;
        let inc_tools = self.include_tools;

        cx.spawn(async move |weak, cx| {
            let created = client
                .create_session_export(sid, fmt, thm, inc_think, inc_tools)
                .await;
            let job_id = match created {
                Ok(val) => val
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                Err(err) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.exporting = false;
                        cx.emit(ExportViewEvent::ErrorMessage(err.to_string()));
                        cx.notify();
                    });
                    return;
                }
            };

            if job_id.is_empty() {
                let _ = weak.update(cx, |this, cx| {
                    this.exporting = false;
                    cx.emit(ExportViewEvent::ErrorMessage("导出任务 ID 无效。".into()));
                    cx.notify();
                });
                return;
            }

            // Poll `GET /api/exports/<exportId>` on the Tokio runtime (never blocking GPUI test clock)
            let poll_client = client.clone();
            let jid = job_id.clone();
            let final_job = poll_client.get_export_job(jid.clone()).await;

            let mut resolved_job = final_job;
            for _ in 0..120 {
                let status = resolved_job
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if matches!(status, "completed" | "failed") {
                    break;
                }
                let pc = client.clone();
                let j = jid.clone();
                // Short backoff via Tokio runtime inside `get_export_job`
                resolved_job = pc.get_export_job(j).await;
            }

            match resolved_job {
                Ok(job_val) => {
                    let status = job_val
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("completed");
                    if status == "failed" {
                        let err_msg = job_val
                            .get("error")
                            .and_then(|e| e.get("message").or(Some(e)))
                            .and_then(Value::as_str)
                            .unwrap_or("导出任务生成失败。")
                            .to_owned();
                        let _ = weak.update(cx, |this, cx| {
                            this.exporting = false;
                            cx.emit(ExportViewEvent::ErrorMessage(err_msg));
                            cx.notify();
                        });
                        return;
                    }

                    let dir = job_val
                        .get("directory")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let open_res = client.open_export_directory(job_id.clone()).await;

                    let _ = weak.update(cx, |this, cx| {
                        this.exporting = false;
                        this.last_completed_export_id = Some(job_id);
                        this.last_completed_dir = dir;
                        if let Err(err) = open_res {
                            cx.emit(ExportViewEvent::ErrorMessage(err.to_string()));
                        } else {
                            cx.emit(ExportViewEvent::Notice(
                                "导出已完成并在文件管理器中定位。".into(),
                            ));
                        }
                        cx.notify();
                    });
                }
                Err(err) => {
                    let _ = weak.update(cx, |this, cx| {
                        this.exporting = false;
                        cx.emit(ExportViewEvent::ErrorMessage(err.to_string()));
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn render_readonly_preview(
        &self,
        mono_family: SharedString,
        scale_factor: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let doc_palette = self.doc_theme.palette();

        let Some(ref snap) = self.preview_snapshot else {
            return div()
                .id("export-preview-pane-egkn1")
                .test_support()
                .flex_1()
                .h_full()
                .bg(doc_palette.bg_workspace)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    apply_scale(div(), scale::UI)
                        .text_color(doc_palette.text_muted)
                        .child(if self.loading_preview {
                            "正在加载导出预览…"
                        } else {
                            "暂无可导出的消息内容"
                        }),
                )
                .into_any_element();
        };

        let title = snap
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("会话导出");
        let entries = snap
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (turns, tool_results) = group_turns_and_results(&entries);

        let mut doc_col = div()
            .w_full()
            .max_w(px(760.0))
            .mx_auto()
            .p(space::GROUP)
            .flex()
            .flex_col()
            .gap(space::GROUP)
            .child(
                div()
                    .pb(space::MD)
                    .border_b_1()
                    .border_color(doc_palette.border_subtle)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        apply_scale(div(), scale::TITLE)
                            .text_color(doc_palette.text_primary)
                            .child(SharedString::from(title.to_owned())),
                    )
                    .children(self.loading_preview.then(|| {
                        apply_scale(div(), scale::CAPTION)
                            .text_color(doc_palette.text_muted)
                            .child("正在更新预览…")
                    })),
            );

        for (t_idx, turn) in turns.iter().enumerate() {
            let mut turn_box = div()
                .id(SharedString::from(format!("export-preview-turn-{t_idx}")))
                .test_support()
                .w_full()
                .flex()
                .flex_col()
                .gap(space::MD);

            if let Some(ref u) = turn.user {
                let utext = extract_message_text(u.raw.get("message").unwrap_or(&u.raw));
                if !utext.trim().is_empty() {
                    turn_box = turn_box.child(
                        div().w_full().flex().justify_end().child(
                            apply_scale(div(), scale::BODY)
                                .max_w(px(560.0))
                                .px(space::LG)
                                .py(space::SM)
                                .rounded(radius::SPECIAL)
                                .bg(doc_palette.bg_surface)
                                .text_color(doc_palette.text_primary)
                                .child(SharedString::from(utext)),
                        ),
                    );
                }
            }

            if !turn.assistants.is_empty() {
                let split = split_turn(&turn.assistants);
                let segments = build_turn_segments(split.process);
                for seg in segments {
                    for part in seg.parts {
                        match part.part {
                            ConversationPart::Thinking(th) if !th.thinking.trim().is_empty() => {
                                turn_box = turn_box.child(
                                    apply_scale(div(), scale::CAPTION)
                                        .p(space::MD)
                                        .rounded(radius::ROW)
                                        .bg(doc_palette.bg_surface)
                                        .text_color(doc_palette.text_secondary)
                                        .child(SharedString::from(th.thinking)),
                                );
                            }
                            ConversationPart::Special { call: tc, .. } => {
                                let res_val = tool_results.get(&tc.id);
                                let input = ToolCallInput {
                                    id: tc.id.clone(),
                                    name: tc.name.clone(),
                                    arguments: tc
                                        .arguments
                                        .as_object()
                                        .cloned()
                                        .unwrap_or_default(),
                                    execution: Some(ToolExecutionMeta::default()),
                                };
                                let parsed = res_val.map(ToolResultInput::from_message_json);
                                if let Some(special) = self.client.with_presenter_cache(|cache| {
                                    cache.get_or_map_special(&input, parsed.as_ref(), 1, false)
                                }) {
                                    turn_box = turn_box.child(render_special_tool_component(
                                        SpecialToolDispatchProps {
                                            session_id: &self.session_id,
                                            entry_id: "export-preview",
                                            call_id: &tc.id,
                                            has_result: res_val.is_some(),
                                            view: &special,
                                            bitlocker_snapshot: None,
                                            secret_input: None,
                                            details_expanded: true,
                                            warning_hovered: false,
                                            transparency_enabled: false,
                                            scale_factor,
                                        },
                                        self.client.clone(),
                                        mono_family.clone(),
                                        &doc_palette,
                                        &|_| false,
                                        Rc::new(|_, _, _| {}),
                                        Rc::new(|_, _, _, _| {}),
                                        Rc::new(|_, _, _, _| {}),
                                        |_, _, _| {},
                                        |_, _| {},
                                        window,
                                        cx,
                                    ));
                                }
                            }
                            ConversationPart::Tools(calls) => {
                                for tc in calls {
                                    let res_val = tool_results.get(&tc.id);
                                    let input = ToolCallInput {
                                        id: tc.id.clone(),
                                        name: tc.name.clone(),
                                        arguments: tc
                                            .arguments
                                            .as_object()
                                            .cloned()
                                            .unwrap_or_default(),
                                        execution: Some(ToolExecutionMeta::default()),
                                    };
                                    let parsed = res_val
                                        .map(ToolResultInput::from_message_json)
                                        .unwrap_or_default();
                                    let card_view = self.client.with_presenter_cache(|cache| {
                                        cache.get_or_map_normal(&input, &parsed, 1)
                                    });
                                    turn_box = turn_box.child(render_normal_tool_card(
                                        &tc.id,
                                        &tc.name,
                                        None,
                                        res_val.is_some(),
                                        parsed.is_error,
                                        false,
                                        &card_view,
                                        true,
                                        false,
                                        mono_family.clone(),
                                        &doc_palette,
                                        &|_| false,
                                        Rc::new(|_, _, _| {}),
                                        Rc::new(|_, _, _| {}),
                                        Rc::new(|_, _, _| {}),
                                        |_, _, _| {},
                                        cx,
                                    ));
                                }
                            }
                            ConversationPart::Text(txt) if !txt.trim().is_empty() => {
                                turn_box = turn_box.child(
                                    apply_scale(div(), scale::BODY)
                                        .text_color(doc_palette.text_primary)
                                        .child(SharedString::from(txt)),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                if !split.final_text.trim().is_empty() {
                    turn_box = turn_box.child(
                        apply_scale(div(), scale::BODY)
                            .text_color(doc_palette.text_primary)
                            .child(SharedString::from(split.final_text)),
                    );
                }
            }
            doc_col = doc_col.child(turn_box);
        }

        div()
            .id("export-preview-pane-egkn1")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .bg(doc_palette.bg_workspace)
            .child(doc_col)
            .into_any_element()
    }
}

impl Render for ExportView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let scale_factor = window.scale_factor();

        let left_options = div()
            .id("export-options-pane-er02w")
            .test_support()
            .w(px(264.0))
            .h_full()
            .flex_shrink_0()
            .p(space::LG)
            .border_r_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_surface)
            .flex()
            .flex_col()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::LG)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .id("export-back-btn")
                                    .test_support()
                                    .h(height::CONTROL)
                                    .px(space::SM)
                                    .rounded(radius::CONTROL)
                                    .text_color(palette.text_secondary)
                                    .flex()
                                    .items_center()
                                    .gap(space::XS)
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ExportViewEvent::BackToChat);
                                    }))
                                    .child(
                                        Icon::new(IconName::ArrowLeft)
                                            .size(px(14.0))
                                            .text_color(palette.text_secondary),
                                    )
                                    .child(cjk_inline_label("返回")),
                            )
                            .child(
                                apply_scale(div(), scale::BODY_SEMIBOLD)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("会话导出")),
                            ),
                    )
                    // Format selector (`PNG / HTML`)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::XS)
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child("导出格式"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(space::XS)
                                    .child(export_seg_btn(
                                        "export-format-png-btn",
                                        ExportFormatChoice::Png.label(),
                                        self.format == ExportFormatChoice::Png,
                                        &palette,
                                        cx.listener(|this, _, _, cx| {
                                            this.format = ExportFormatChoice::Png;
                                            this.reload_preview(cx);
                                        }),
                                    ))
                                    .child(export_seg_btn(
                                        "export-format-html-btn",
                                        ExportFormatChoice::Html.label(),
                                        self.format == ExportFormatChoice::Html,
                                        &palette,
                                        cx.listener(|this, _, _, cx| {
                                            this.format = ExportFormatChoice::Html;
                                            this.reload_preview(cx);
                                        }),
                                    )),
                            ),
                    )
                    // Document theme selector (`日间 / 夜间`)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::XS)
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child("文档主题"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(space::XS)
                                    .child(export_seg_btn(
                                        "export-theme-light-btn",
                                        "日间",
                                        self.doc_theme == ExportDocTheme::Light,
                                        &palette,
                                        cx.listener(|this, _, _, cx| {
                                            this.doc_theme = ExportDocTheme::Light;
                                            this.reload_preview(cx);
                                        }),
                                    ))
                                    .child(export_seg_btn(
                                        "export-theme-dark-btn",
                                        "夜间",
                                        self.doc_theme == ExportDocTheme::Dark,
                                        &palette,
                                        cx.listener(|this, _, _, cx| {
                                            this.doc_theme = ExportDocTheme::Dark;
                                            this.reload_preview(cx);
                                        }),
                                    )),
                            ),
                    )
                    // Include checkboxes
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::SM)
                            .child(export_check_row(
                                "export-include-thinking-chk",
                                "包含思考过程",
                                self.include_thinking,
                                &palette,
                                cx.listener(|this, _, _, cx| {
                                    this.include_thinking = !this.include_thinking;
                                    this.reload_preview(cx);
                                }),
                            ))
                            .child(export_check_row(
                                "export-include-tools-chk",
                                "包含工具调用结果",
                                self.include_tools,
                                &palette,
                                cx.listener(|this, _, _, cx| {
                                    this.include_tools = !this.include_tools;
                                    this.reload_preview(cx);
                                }),
                            )),
                    ),
            )
            // Bottom action buttons
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .children(self.last_completed_export_id.as_ref().map(|eid| {
                        let eid_open = eid.clone();
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("export-open-dir-btn")
                            .test_support()
                            .w_full()
                            .h(height::CONTROL)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_elevated)
                            .text_color(palette.text_primary)
                            .cursor(CursorStyle::PointingHand)
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(space::XS)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let c = this.client.clone();
                                let id = eid_open.clone();
                                cx.spawn(async move |weak, cx| {
                                    if let Err(err) = c.open_export_directory(id).await {
                                        let _ = weak.update(cx, |_, cx| {
                                            cx.emit(ExportViewEvent::ErrorMessage(err.to_string()));
                                        });
                                    }
                                })
                                .detach();
                            }))
                            .child(
                                Icon::new(IconName::FolderOpen)
                                    .size(px(14.0))
                                    .text_color(palette.accent_solid),
                            )
                            .child(cjk_inline_label("打开所在目录"))
                    }))
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("export-generate-save-btn")
                            .test_support()
                            .w_full()
                            .h(height::FIELD)
                            .rounded(radius::ROW)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_generate_and_save(cx);
                            }))
                            .child(cjk_inline_label(if self.exporting {
                                "正在生成…"
                            } else {
                                "生成并保存"
                            })),
                    ),
            );

        let right_preview = self.render_readonly_preview(mono_family, scale_factor, window, cx);

        div()
            .id("export-view-ntay1")
            .test_support()
            .size_full()
            .flex()
            .flex_row()
            .items_stretch()
            .child(left_options)
            .child(right_preview)
    }
}

fn export_seg_btn<F>(
    id: &'static str,
    label: &'static str,
    active: bool,
    palette: &DesktopPalette,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
{
    apply_scale(div(), scale::CAPTION_MEDIUM)
        .id(id)
        .test_support()
        .flex_1()
        .h(height::CONTROL)
        .px(space::XS)
        .rounded(radius::CONTROL)
        .border_1()
        .border_color(if active {
            palette.accent_solid
        } else {
            palette.border_subtle
        })
        .bg(if active {
            palette.bg_item_active
        } else {
            palette.bg_elevated
        })
        .text_color(if active {
            palette.text_primary
        } else {
            palette.text_secondary
        })
        .flex()
        .items_center()
        .justify_center()
        .on_click(on_click)
        .child(cjk_inline_label(label))
}

fn export_check_row<F>(
    id: &'static str,
    label: &'static str,
    checked: bool,
    palette: &DesktopPalette,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
{
    apply_scale(div(), scale::UI)
        .id(id)
        .test_support()
        .h(height::CONTROL)
        .rounded(radius::CONTROL)
        .flex()
        .items_center()
        .gap(space::SM)
        .on_click(on_click)
        .child(
            div()
                .w(px(16.0))
                .h(px(16.0))
                .rounded(radius::CONTROL)
                .border_1()
                .border_color(if checked {
                    palette.accent_solid
                } else {
                    palette.border_strong
                })
                .bg(if checked {
                    palette.accent_solid
                } else {
                    palette.bg_elevated
                })
                .flex()
                .items_center()
                .justify_center()
                .children(checked.then(|| {
                    Icon::new(IconName::Check)
                        .size(px(11.0))
                        .text_color(palette.text_on_accent)
                })),
        )
        .child(
            div()
                .text_color(palette.text_primary)
                .child(cjk_inline_label(label)),
        )
}

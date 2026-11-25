use std::rc::Rc;
use std::time::Duration;

use cst_client::ClientHandle;
use cst_client::tool_presenter::{ToolCallInput, ToolResultInput, map_special_tool, map_tool};
use gpui::{KeyDownEvent, MouseMoveEvent, Pixels, Point};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ElementExt, Icon};
use gpui_kit::{
    Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Task, TestSupportExt, Window, div, px,
};
use serde_json::Value;

use crate::accessibility::A11yElementExt as _;
use crate::theme::kbd_focus::{
    KbdBoundsTracker, KbdControlSpec, KbdRailPlacement, KbdTone, pointer_moved_past_threshold,
    render_gliding_control_indicator,
};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius,
    scale, space,
};

const SLOT_FMT_PNG: usize = 0;
const SLOT_FMT_HTML: usize = 1;
const SLOT_THEME_LIGHT: usize = 2;
const SLOT_THEME_DARK: usize = 3;
const SLOT_CHK_THINK: usize = 4;
const SLOT_CHK_TOOLS: usize = 5;
const SLOT_BTN_SAVE: usize = 6;
const SLOT_BTN_OPEN: usize = 7;
use crate::tools::normal_card::render_normal_tool_card_with_args;
use crate::tools::{SpecialToolDispatchProps, render_special_tool_component};
use crate::workbench::turn_segments::{
    ConversationPart, extract_message_text, group_turns_and_results, split_turn,
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
    ExportCompleted,
    Notice(String),
    ErrorMessage(String),
}

/// Session Export & Native Read-Only Preview (`cst-pilot-desktop.pen` 「02 · 会话管理」 `Y8hs0` -> `hesLq` + `Ntay1`):
/// Top full-width `44px` `hesLq` header + `24px` dual-column body (`264px` `ER02w` options + `eGkN1` independent-theme preview).
pub struct ExportView {
    client: ClientHandle,
    session_id: String,
    format: ExportFormatChoice,
    doc_theme: ExportDocTheme,
    include_thinking: bool,
    include_tools: bool,
    preview_snapshot: Option<Value>,
    preview_failed: bool,
    preview_task: Option<Task<()>>,
    export_task: Option<Task<()>>,
    open_directory_task: Option<Task<()>>,
    last_completed_export_id: Option<String>,
    last_completed_dir: Option<String>,
    preview_reveal: motion::SurfaceReveal,
    focus_handle: FocusHandle,
    keyboard_active: bool,
    focus_idx: usize,
    last_pointer_pos: Option<Point<Pixels>>,
    bounds_tracker: KbdBoundsTracker<8>,
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
            preview_failed: false,
            preview_task: None,
            export_task: None,
            open_directory_task: None,
            last_completed_export_id: None,
            last_completed_dir: None,
            preview_reveal: motion::SurfaceReveal::default(),
            focus_handle: cx.focus_handle(),
            keyboard_active: false,
            focus_idx: 0,
            last_pointer_pos: None,
            bounds_tracker: KbdBoundsTracker::new(),
        };
        this.reload_preview(cx);
        this
    }

    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        self.keyboard_active = false;
        self.last_pointer_pos = None;
        cx.notify();
    }

    pub(crate) fn focus_from_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        self.keyboard_active = true;
        self.focus_idx = 0;
        self.last_pointer_pos = Some(window.mouse_position());
        cx.notify();
    }

    pub fn keyboard_active(&self) -> bool {
        self.keyboard_active
    }

    pub fn focus_index(&self) -> usize {
        self.focus_idx
    }

    pub fn format_choice(&self) -> ExportFormatChoice {
        self.format
    }

    pub fn doc_theme_choice(&self) -> ExportDocTheme {
        self.doc_theme
    }

    pub fn include_thinking(&self) -> bool {
        self.include_thinking
    }

    pub fn include_tools(&self) -> bool {
        self.include_tools
    }

    fn max_focus_idx(&self) -> usize {
        if self.last_completed_export_id.is_some() {
            5
        } else {
            4
        }
    }

    fn current_kbd_spec(&self) -> Option<KbdControlSpec> {
        if !self.keyboard_active {
            return None;
        }
        let idx = self.focus_idx.min(self.max_focus_idx());
        let (slot, tone, rail) = match idx {
            0 => (
                match self.format {
                    ExportFormatChoice::Png => SLOT_FMT_PNG,
                    ExportFormatChoice::Html => SLOT_FMT_HTML,
                },
                KbdTone::Neutral,
                KbdRailPlacement::BottomCenter,
            ),
            1 => (
                match self.doc_theme {
                    ExportDocTheme::Light => SLOT_THEME_LIGHT,
                    ExportDocTheme::Dark => SLOT_THEME_DARK,
                },
                KbdTone::Neutral,
                KbdRailPlacement::BottomCenter,
            ),
            2 => (
                SLOT_CHK_THINK,
                KbdTone::Neutral,
                KbdRailPlacement::LeftInside,
            ),
            3 => (
                SLOT_CHK_TOOLS,
                KbdTone::Neutral,
                KbdRailPlacement::LeftInside,
            ),
            4 => (
                SLOT_BTN_SAVE,
                KbdTone::Accent,
                KbdRailPlacement::LeftOutside,
            ),
            5 if self.last_completed_export_id.is_some() => (
                SLOT_BTN_OPEN,
                KbdTone::Neutral,
                KbdRailPlacement::LeftOutside,
            ),
            _ => return None,
        };
        Some(KbdControlSpec {
            slot,
            corner_radius: 4.0,
            tone,
            rail,
        })
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.keyboard_active
            && pointer_moved_past_threshold(&mut self.last_pointer_pos, event.position)
        {
            self.keyboard_active = false;
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers != gpui::Modifiers::default()
            || !self.focus_handle.is_focused(window)
        {
            return;
        }
        let key = event.keystroke.key.as_str();
        let max_idx = self.max_focus_idx();
        let cur = self.focus_idx.min(max_idx);
        let handled = match key {
            "up" => {
                self.focus_idx = if self.keyboard_active {
                    cur.saturating_sub(1)
                } else {
                    cur
                };
                self.keyboard_active = true;
                self.last_pointer_pos = Some(window.mouse_position());
                true
            }
            "down" => {
                self.focus_idx = if self.keyboard_active {
                    (cur + 1).min(max_idx)
                } else {
                    cur
                };
                self.keyboard_active = true;
                self.last_pointer_pos = Some(window.mouse_position());
                true
            }
            "home" => {
                self.focus_idx = 0;
                self.keyboard_active = true;
                self.last_pointer_pos = Some(window.mouse_position());
                true
            }
            "end" => {
                self.focus_idx = max_idx;
                self.keyboard_active = true;
                self.last_pointer_pos = Some(window.mouse_position());
                true
            }
            "left" | "right" => {
                self.focus_idx = cur;
                self.keyboard_active = true;
                self.last_pointer_pos = Some(window.mouse_position());
                let is_left = key == "left";
                if cur == 0 {
                    let next = if is_left {
                        ExportFormatChoice::Png
                    } else {
                        ExportFormatChoice::Html
                    };
                    if self.format != next {
                        self.format = next;
                        self.reload_preview(cx);
                    }
                } else if cur == 1 {
                    let next = if is_left {
                        ExportDocTheme::Light
                    } else {
                        ExportDocTheme::Dark
                    };
                    if self.doc_theme != next {
                        self.doc_theme = next;
                        self.reload_preview(cx);
                    }
                }
                true
            }
            "enter" | "space" => {
                self.focus_idx = cur;
                self.keyboard_active = true;
                self.last_pointer_pos = Some(window.mouse_position());
                match cur {
                    2 => {
                        self.include_thinking = !self.include_thinking;
                        self.reload_preview(cx);
                    }
                    3 => {
                        self.include_tools = !self.include_tools;
                        self.reload_preview(cx);
                    }
                    4 => self.start_generate_and_save(cx),
                    5 if self.last_completed_export_id.is_some() => {
                        self.open_export_directory(false, cx);
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn set_session_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.stop();
        self.session_id = session_id;
        self.reload_preview(cx);
    }

    pub(crate) fn stop(&mut self) {
        // Dropping the tasks cancels UI polling and prevents obsolete jobs from opening Explorer.
        // An export already accepted by the service may still finish writing its files.
        self.preview_task = None;
        self.export_task = None;
        self.open_directory_task = None;
        self.preview_snapshot = None;
        self.preview_failed = false;
        self.last_completed_export_id = None;
        self.last_completed_dir = None;
        self.preview_reveal.clear();
        self.keyboard_active = false;
        self.focus_idx = 0;
        self.last_pointer_pos = None;
        self.bounds_tracker.clear();
    }

    /// Keep the previous preview mounted until the latest request settles.
    pub fn reload_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_task = None;
        cx.notify();
        if self.session_id.is_empty() {
            return;
        }

        let client = self.client.clone();
        let sid = self.session_id.clone();
        let fmt = self.format.as_str().to_owned();
        let thm = self.doc_theme.as_str().to_owned();
        let inc_think = self.include_thinking;
        let inc_tools = self.include_tools;

        self.preview_task = Some(cx.spawn(async move |weak, cx| {
            let res = client
                .preview_session_export(sid, fmt, thm, inc_think, inc_tools)
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.preview_task = None;
                this.preview_failed = false;
                this.preview_snapshot = match res {
                    Ok(snapshot) => {
                        let ready = snapshot
                            .get("entries")
                            .and_then(Value::as_array)
                            .is_some_and(|entries| !entries.is_empty())
                            .then_some(snapshot);
                        if ready.is_some() {
                            this.preview_reveal
                                .start(Duration::from_millis(180), 4.0, 0.25, cx);
                        }
                        ready
                    }
                    Err(err) if err.code() == "empty_export" => None,
                    Err(err) => {
                        this.preview_failed = true;
                        cx.emit(ExportViewEvent::ErrorMessage(err.to_string()));
                        None
                    }
                };
                cx.notify();
            });
        }));
    }

    fn start_generate_and_save(&mut self, cx: &mut Context<Self>) {
        if self.export_task.is_some()
            || self.open_directory_task.is_some()
            || self.session_id.is_empty()
        {
            return;
        }

        let client = self.client.clone();
        let sid = self.session_id.clone();
        let fmt = self.format.as_str().to_owned();
        let thm = self.doc_theme.as_str().to_owned();
        let inc_think = self.include_thinking;
        let inc_tools = self.include_tools;

        self.export_task = Some(cx.spawn(async move |weak, cx| {
            let result = async {
                let created = client
                    .create_session_export(sid, fmt, thm, inc_think, inc_tools)
                    .await
                    .map_err(|err| err.to_string())?;
                let job_id = created
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or("导出任务 ID 无效。")?;
                let mut job = created.clone();
                for _ in 0..120 {
                    if let Some(dir) = completed_export(&job)? {
                        return Ok((job_id.to_owned(), dir));
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    job = client
                        .get_export_job(job_id.to_owned())
                        .await
                        .map_err(|err| err.to_string())?;
                }
                completed_export(&job)?
                    .map(|dir| (job_id.to_owned(), dir))
                    .ok_or_else(|| "等待导出任务超时。".into())
            }
            .await;

            let _ = weak.update(cx, |this, cx| {
                this.export_task = None;
                match result {
                    Ok((id, dir)) => {
                        this.last_completed_export_id = Some(id);
                        this.last_completed_dir = dir;
                        cx.emit(ExportViewEvent::ExportCompleted);
                        this.open_export_directory(true, cx);
                    }
                    Err(msg) => cx.emit(ExportViewEvent::ErrorMessage(msg)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn open_export_directory(&mut self, automatic: bool, cx: &mut Context<Self>) {
        if self.open_directory_task.is_some() || self.export_task.is_some() {
            return;
        }
        let Some(id) = self.last_completed_export_id.clone() else {
            return;
        };
        let client = self.client.clone();
        self.open_directory_task = Some(cx.spawn(async move |weak, cx| {
            let res = client.open_export_directory(id).await;
            let _ = weak.update(cx, |this, cx| {
                this.open_directory_task = None;
                match res {
                    Err(err) => cx.emit(ExportViewEvent::ErrorMessage(err.to_string())),
                    Ok(_) if automatic => cx.emit(ExportViewEvent::Notice(
                        "导出已完成并在文件管理器中定位。".into(),
                    )),
                    Ok(_) => {}
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_readonly_preview(
        &mut self,
        mono_family: SharedString,
        ui_palette: &DesktopPalette,
        scale_factor: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let doc_palette = self.doc_theme.palette();
        let (preview_op, preview_dy) = self.preview_reveal.sample(window, cx);

        let viewport_el = if let Some(ref snap) = self.preview_snapshot {
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
                .relative()
                .top(preview_dy)
                .opacity(preview_op)
                .w_full()
                .max_w(px(760.0))
                .mx_auto()
                .flex()
                .flex_col()
                .gap(space::GROUP)
                .child(
                    div()
                        .pb(space::LG)
                        .border_b_1()
                        .border_color(doc_palette.border_subtle)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::SM)
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .child(
                                    apply_scale(div(), scale::CAPTION_MEDIUM)
                                        .px(space::SM)
                                        .py(space::MICRO)
                                        .rounded(radius::CONTROL)
                                        .bg(doc_palette.bg_surface)
                                        .text_color(doc_palette.text_secondary)
                                        .flex_shrink_0()
                                        .child("CST Pilot"),
                                )
                                .child(
                                    apply_scale(div(), scale::TITLE)
                                        .min_w_0()
                                        .truncate()
                                        .text_color(doc_palette.text_primary)
                                        .child(cjk_inline_label(title.to_owned())),
                                ),
                        )
                        .children(self.preview_task.is_some().then(|| {
                            apply_scale(div(), scale::CAPTION)
                                .text_color(doc_palette.text_muted)
                                .child(cjk_inline_label("正在更新预览…"))
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
                                    .rounded(radius::ROW)
                                    .bg(doc_palette.bg_surface)
                                    .text_color(doc_palette.text_primary)
                                    .child(SharedString::from(utext)),
                            ),
                        );
                    }
                }

                if !turn.assistants.is_empty() {
                    let split = split_turn(&turn.assistants);
                    for part in split.process {
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
                                    execution: None,
                                };
                                let parsed = res_val.map(ToolResultInput::from_message_json);
                                if let Some(special) =
                                    map_special_tool(&input, parsed.as_ref(), false)
                                {
                                    turn_box = turn_box.child(render_special_tool_component(
                                        SpecialToolDispatchProps {
                                            session_id: &self.session_id,
                                            entry_id: "export-preview",
                                            call_id: &tc.id,
                                            view: &special,
                                            pending_bitlocker: None,
                                            secret_input: None,
                                            details_expanded: true,
                                            warning_hovered: false,
                                            transparency_enabled: false,
                                            scale_factor,
                                            running_light_allowed: false,
                                            started_at: None,
                                            animate_enter: false,
                                        },
                                        self.client.clone(),
                                        mono_family.clone(),
                                        &doc_palette,
                                        &|_| false,
                                        Rc::new(|_, _| {}),
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
                                        execution: None,
                                    };
                                    let parsed = res_val.map(ToolResultInput::from_message_json);
                                    let card_view = parsed.as_ref().map(|p| map_tool(&input, p));
                                    // Arguments came from the server's sanitized export snapshot.
                                    turn_box = turn_box.child(render_normal_tool_card_with_args(
                                        &tc.id,
                                        &tc.name,
                                        Some(&tc.arguments),
                                        None,
                                        res_val.is_some(),
                                        parsed.as_ref().is_some_and(|p| p.is_error),
                                        false,
                                        false,
                                        None,
                                        card_view.as_ref(),
                                        true,
                                        false,
                                        mono_family.clone(),
                                        &doc_palette,
                                        &|_| false,
                                        Rc::new(|_, _, _| {}),
                                        Rc::new(|_, _, _| {}),
                                        Rc::new(|_, _, _| {}),
                                        |_, _, _| {},
                                        window,
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
                    if !split.final_text.trim().is_empty() {
                        turn_box = turn_box.child(
                            apply_scale(div(), scale::BODY)
                                .text_color(doc_palette.text_primary)
                                .child(SharedString::from(split.final_text)),
                        );
                    }
                    if let Some(ref err_pres) = split.assistant_error {
                        let mut err_col = div()
                            .id(SharedString::from(format!("export-preview-error-{t_idx}")))
                            .test_support()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::BODY)
                                    .text_color(doc_palette.text_primary)
                                    .child(SharedString::from(err_pres.headline.clone())),
                            );
                        if let Some(ref raw_detail) = err_pres.raw_detail {
                            err_col = err_col.child(
                                div()
                                    .w_full()
                                    .p(space::MD)
                                    .rounded(radius::ROW)
                                    .bg(doc_palette.bg_surface)
                                    .border_1()
                                    .border_color(doc_palette.border_subtle)
                                    .font_family(mono_family.clone())
                                    .text_size(scale::CAPTION.font_size)
                                    .line_height(scale::CAPTION.line_height)
                                    .text_color(doc_palette.text_secondary)
                                    .child(SharedString::from(raw_detail.clone())),
                            );
                        }
                        turn_box = turn_box.child(err_col);
                    }
                    if split.was_aborted {
                        turn_box = turn_box.child(
                            apply_scale(div(), scale::UI)
                                .text_color(doc_palette.text_muted)
                                .child("已停止生成"),
                        );
                    }
                }
                doc_col = doc_col.child(turn_box);
            }

            div()
                .id("export-preview-pane-egkn1")
                .test_support()
                .flex_1()
                .min_h_0()
                .w_full()
                .p(space::GROUP)
                .rounded(radius::ROW)
                .border_1()
                .border_color(ui_palette.border_subtle)
                .overflow_y_scroll()
                .bg(doc_palette.bg_workspace)
                .child(doc_col)
                .into_any_element()
        } else {
            div()
                .id("export-preview-pane-egkn1")
                .test_support()
                .flex_1()
                .min_h_0()
                .w_full()
                .p(space::GROUP)
                .rounded(radius::ROW)
                .border_1()
                .border_color(ui_palette.border_subtle)
                .bg(doc_palette.bg_workspace)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    apply_scale(div(), scale::UI)
                        .text_color(doc_palette.text_muted)
                        .child(if self.preview_task.is_some() {
                            "正在加载导出预览…"
                        } else if self.preview_failed {
                            "导出预览加载失败，请重试"
                        } else {
                            "暂无可导出的消息内容"
                        }),
                )
                .into_any_element()
        };

        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(space::MD)
            .child(
                apply_scale(div(), scale::UI)
                    .text_color(ui_palette.text_secondary)
                    .child(cjk_inline_label("预览")),
            )
            .child(viewport_el)
    }
}

// Outer Option indicates completion; the completed response may omit its directory hint.
fn completed_export(job: &Value) -> Result<Option<Option<String>>, String> {
    match job.get("status").and_then(Value::as_str) {
        Some("completed") => Ok(Some(
            job.get("directory")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )),
        Some("queued" | "rendering") => Ok(None),
        Some("failed") => Err(job
            .get("error")
            .and_then(|e| e.get("message").unwrap_or(e).as_str())
            .unwrap_or("导出任务生成失败。")
            .to_owned()),
        _ => Err("导出任务状态无效。".into()),
    }
}

impl Render for ExportView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let scale_factor = window.scale_factor();

        // Top full-width `44px` header `hesLq`: `pl(24px) pr(16px) border_b_1`, `export-back-btn` + `24px` gap + `"导出会话"`
        let top_header = div()
            .id("export-subpage-header-heslq")
            .test_support()
            .w_full()
            .h(height::HEADER)
            .flex_shrink_0()
            .pl(space::GROUP)
            .pr(space::LG)
            .border_b_1()
            .border_color(palette.border_subtle)
            .flex()
            .items_center()
            .gap(space::GROUP)
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("export-back-btn")
                    .test_support()
                    .a11y_button("export-back-btn", "返回对话")
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.text_secondary)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(ExportViewEvent::BackToChat);
                    }))
                    .child(
                        Icon::new(IconName::ArrowLeft)
                            .size(px(16.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label("返回对话")),
            )
            .child(
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label("导出会话")),
            );

        let kbd_style = self.client.snapshot().desktop_settings.keyboard_focus_style;
        let kbd_spec = self.current_kbd_spec();
        let tracker = self.bounds_tracker.clone();
        tracker.clear();
        let t_open = tracker.clone();
        let t_png = tracker.clone();
        let t_html = tracker.clone();
        let t_light = tracker.clone();
        let t_dark = tracker.clone();
        let t_think = tracker.clone();
        let t_tools = tracker.clone();
        let t_save = tracker.clone();

        // Left `264px` options column `ER02w` (no gray `bg_surface`, no right border)
        let saved_section_opt = self.last_completed_export_id.as_ref().map(|_| {
            let dir_hint = self.last_completed_dir.clone();
            div()
                .pt(space::LG)
                .flex()
                .flex_col()
                .gap(space::MD)
                .child(
                    apply_scale(div(), scale::BODY_MEDIUM)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("已保存 1 个文件")),
                )
                .children(dir_hint.map(|d| {
                    apply_scale(div(), scale::CAPTION)
                        .truncate()
                        .text_color(palette.text_muted)
                        .child(SharedString::from(d))
                }))
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("export-open-dir-btn")
                        .test_support()
                        .a11y_button("export-open-dir-btn", "打开保存目录")
                        .w_full()
                        .h(height::CONTROL)
                        .px(space::MD)
                        .rounded(radius::CONTROL)
                        .border_1()
                        .border_color(palette.border_strong)
                        .bg(palette.bg_workspace)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_idx = 5;
                            this.keyboard_active = false;
                            this.focus_handle.focus(window, cx);
                            this.open_export_directory(false, cx);
                        }))
                        .child(cjk_inline_label("打开保存目录"))
                        .on_prepaint(move |bounds, _, _| t_open.set(SLOT_BTN_OPEN, bounds)),
                )
        });

        let fmt_kbd_hint = self.keyboard_active && self.focus_idx == 0;
        let theme_kbd_hint = self.keyboard_active && self.focus_idx == 1;
        let kbd_overlay = render_gliding_control_indicator(
            "export-options",
            kbd_style,
            kbd_spec,
            tracker,
            &palette,
            window,
            cx,
        );

        let left_options = div()
            .id("export-options-pane-er02w")
            .test_support()
            .relative()
            .w(px(264.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(space::GROUP)
            .child(
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label("导出选项")),
            )
            // Format selector (`PNG / HTML`)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("导出格式")),
                            )
                            .children(fmt_kbd_hint.then(|| {
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_muted)
                                    .child(cjk_inline_label("← → 切换"))
                            })),
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
                                SLOT_FMT_PNG,
                                t_png,
                                cx.listener(|this, _, window, cx| {
                                    this.focus_idx = 0;
                                    this.keyboard_active = false;
                                    this.focus_handle.focus(window, cx);
                                    this.format = ExportFormatChoice::Png;
                                    this.reload_preview(cx);
                                }),
                            ))
                            .child(export_seg_btn(
                                "export-format-html-btn",
                                ExportFormatChoice::Html.label(),
                                self.format == ExportFormatChoice::Html,
                                &palette,
                                SLOT_FMT_HTML,
                                t_html,
                                cx.listener(|this, _, window, cx| {
                                    this.focus_idx = 0;
                                    this.keyboard_active = false;
                                    this.focus_handle.focus(window, cx);
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
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("文档主题")),
                            )
                            .children(theme_kbd_hint.then(|| {
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_muted)
                                    .child(cjk_inline_label("← → 切换"))
                            })),
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
                                SLOT_THEME_LIGHT,
                                t_light,
                                cx.listener(|this, _, window, cx| {
                                    this.focus_idx = 1;
                                    this.keyboard_active = false;
                                    this.focus_handle.focus(window, cx);
                                    this.doc_theme = ExportDocTheme::Light;
                                    this.reload_preview(cx);
                                }),
                            ))
                            .child(export_seg_btn(
                                "export-theme-dark-btn",
                                "夜间",
                                self.doc_theme == ExportDocTheme::Dark,
                                &palette,
                                SLOT_THEME_DARK,
                                t_dark,
                                cx.listener(|this, _, window, cx| {
                                    this.focus_idx = 1;
                                    this.keyboard_active = false;
                                    this.focus_handle.focus(window, cx);
                                    this.doc_theme = ExportDocTheme::Dark;
                                    this.reload_preview(cx);
                                }),
                            )),
                    ),
            )
            // Include checkboxes (`ScJjE`: `12px` gap, `24px` rows `Qwhwo`/`zkzeE`)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(export_check_row(
                        "export-include-thinking-chk",
                        "包含思考过程",
                        self.include_thinking,
                        &palette,
                        SLOT_CHK_THINK,
                        t_think,
                        cx.listener(|this, _, window, cx| {
                            this.focus_idx = 2;
                            this.keyboard_active = false;
                            this.focus_handle.focus(window, cx);
                            this.include_thinking = !this.include_thinking;
                            this.reload_preview(cx);
                        }),
                        window,
                        cx,
                    ))
                    .child(export_check_row(
                        "export-include-tools-chk",
                        "包含工具调用结果",
                        self.include_tools,
                        &palette,
                        SLOT_CHK_TOOLS,
                        t_tools,
                        cx.listener(|this, _, window, cx| {
                            this.focus_idx = 3;
                            this.keyboard_active = false;
                            this.focus_handle.focus(window, cx);
                            this.include_tools = !this.include_tools;
                            this.reload_preview(cx);
                        }),
                        window,
                        cx,
                    )),
            )
            // Primary action button (`32px`, `px: 16px`, `r: 4px`) + completed save area below
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("export-generate-save-btn")
                            .test_support()
                            .a11y_button(
                                "export-save-btn",
                                if self.export_task.is_some() {
                                    "正在生成"
                                } else {
                                    "生成并保存"
                                },
                            )
                            .w_full()
                            .h(height::CONTROL)
                            .px(space::LG)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_idx = 4;
                                this.keyboard_active = false;
                                this.focus_handle.focus(window, cx);
                                this.start_generate_and_save(cx);
                            }))
                            .child(cjk_inline_label(if self.export_task.is_some() {
                                "正在生成…"
                            } else {
                                "生成并保存"
                            }))
                            .on_prepaint(move |bounds, _, _| t_save.set(SLOT_BTN_SAVE, bounds)),
                    )
                    .children(saved_section_opt),
            )
            .child(kbd_overlay);

        let right_preview =
            self.render_readonly_preview(mono_family, &palette, scale_factor, window, cx);

        div()
            .id("export-view-ntay1")
            .test_support()
            .a11y_region("export-view", "导出会话")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .size_full()
            .flex()
            .flex_col()
            .child(top_header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p(space::GROUP)
                    .gap(space::GROUP)
                    .flex()
                    .flex_row()
                    .items_stretch()
                    .child(left_options)
                    .child(right_preview),
            )
    }
}

#[allow(clippy::too_many_arguments)]
fn export_seg_btn<F>(
    id: &'static str,
    label: &'static str,
    active: bool,
    palette: &DesktopPalette,
    slot: usize,
    tracker: KbdBoundsTracker<8>,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
{
    apply_scale(div(), scale::CAPTION_MEDIUM)
        .id(id)
        .test_support()
        .a11y_tab(id, label, active)
        .flex_1()
        .h(height::CONTROL)
        .px(space::XS)
        .rounded(radius::CONTROL)
        .border_1()
        .border_color(if active {
            palette.border_strong
        } else {
            palette.border_subtle
        })
        .bg(if active {
            palette.bg_item_active
        } else {
            palette.bg_workspace
        })
        .text_color(if active {
            palette.text_primary
        } else {
            palette.text_secondary
        })
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(palette.bg_item_hover))
        .on_click(on_click)
        .child(cjk_inline_label(label))
        .on_prepaint(move |bounds, _, _| tracker.set(slot, bounds))
}

#[allow(clippy::too_many_arguments)]
fn export_check_row<F>(
    id: &'static str,
    label: &'static str,
    checked: bool,
    palette: &DesktopPalette,
    slot: usize,
    tracker: KbdBoundsTracker<8>,
    on_click: F,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
{
    let chk_mark =
        motion::animated_checkbox_mark(id, checked, px(12.0), palette.text_on_accent, window, cx);
    apply_scale(div(), scale::UI)
        .id(id)
        .test_support()
        .a11y_checkbox(id, label, checked)
        .h(height::INLINE)
        .pl(space::SM)
        .pr(space::XS)
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
                    palette.bg_workspace
                })
                .flex()
                .items_center()
                .justify_center()
                .children(chk_mark),
        )
        .child(
            div()
                .text_color(palette.text_primary)
                .child(cjk_inline_label(label)),
        )
        .on_prepaint(move |bounds, _, _| tracker.set(slot, bounds))
}

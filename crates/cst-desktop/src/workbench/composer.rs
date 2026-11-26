use std::io::Read as _;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use cst_client::{
    ClientError, ClientHandle, ComposerDraft, DraftKey, InlineToken as DraftToken,
    KeyboardFocusStyleDto,
};
use gpui::{
    ClipboardEntry, ClipboardItem, EntityInputHandler, ExternalPaths, ImageSource, KeyDownEvent,
    MouseMoveEvent, ObjectFit, PathPromptOptions, Pixels, Point, StyledImage as _,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::base::motion::{Transition, TransitionState};
use gpui_kit::component::input::{
    Enter, IndentInline, InlineToken as UiInlineToken, InputContent, InputEvent, MoveDown, MoveUp,
    OutdentInline, Textarea, TextareaState,
};
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TestSupportExt, Window, div, img, px,
    rgba,
};
use serde_json::Value;

use crate::accessibility::A11yElementExt as _;
use crate::theme::kbd_focus::{
    DialogNavStep, KbdDynamicBoundsTracker, KbdDynamicSpec, KbdMenuStep, KbdRailPlacement, KbdTone,
    pointer_moved_past_threshold, render_gliding_dynamic_indicator, resolve_dialog_nav_key,
    resolve_menu_nav_key,
};
use crate::theme::motion::{DesktopTooltipExt as _, IconPressExt as _};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius,
    scale, space,
};
use crate::workbench::image_viewer::{LightboxImageTarget, gpui_image_from_handle};
use crate::workbench::model_picker::{
    ModelPickerPage, is_quota_provider, render_context_quota_popover, render_context_ring_button,
    render_model_picker_popover, resolve_scoped_models, thinking_level_label,
};
use crate::workbench::queue_bar::{
    ParsedQueueSnapshot, QueueBarCallbacks, QueueBarKbdProps, QueueMutation, QueueRowAction,
    VisibleQueueItem, queue_item_action_slots, render_queue_bar,
};

#[derive(Debug, Clone)]
pub enum ComposerHostEvent {
    SubmittedToSession {
        session_id: String,
    },
    RequestEditQueueItem {
        session_id: String,
        item: VisibleQueueItem,
        version: u64,
    },
    MissingCredentials {
        provider: String,
    },
    NavigateSlashRoute(SlashNavigationTarget),
    OpenAccountPage,
    OpenLightbox(LightboxImageTarget),
    ErrorMessage(String),
    TopNotice(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashNavigationTarget {
    Tree,
    Export,
    ForkLatest,
}

#[derive(Debug, Clone)]
pub struct SlashEntrySpec {
    pub command: &'static str,
    pub short_label: &'static str,
    pub description: &'static str,
    pub is_skill_or_token: bool,
    pub nav_target: Option<SlashNavigationTarget>,
}

pub const SLASH_CATALOG: &[SlashEntrySpec] = &[
    SlashEntrySpec {
        command: "/skill:disk",
        short_label: "disk",
        description: "分区容量、磁盘健康与目录空间排查",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:ls",
        short_label: "ls",
        description: "浏览指定目录结构与文件属性",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:sys",
        short_label: "sys",
        description: "整机硬件、处理器、内存、显卡与传感器采样",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:startup",
        short_label: "startup",
        description: "开机自启项、计划任务与第三方服务审查",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:eventlog",
        short_label: "eventlog",
        description: "系统与应用程序关键事件日志检索",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:driver",
        short_label: "driver",
        description: "异常设备、驱动版本与硬件连接排查",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:runbook",
        short_label: "runbook",
        description: "生成可审阅的手动修复命令清单",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:bitlocker",
        short_label: "bitlocker",
        description: "BitLocker 卷状态核查、解锁与关闭加密",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/compact",
        short_label: "compact",
        description: "压缩当前会话上下文历史",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/tree",
        short_label: "tree",
        description: "打开会话分支树视图",
        is_skill_or_token: false,
        nav_target: Some(SlashNavigationTarget::Tree),
    },
    SlashEntrySpec {
        command: "/export",
        short_label: "export",
        description: "预览并导出当前会话",
        is_skill_or_token: false,
        nav_target: Some(SlashNavigationTarget::Export),
    },
    SlashEntrySpec {
        command: "/fork",
        short_label: "fork",
        description: "从当前对话节点派生新会话",
        is_skill_or_token: false,
        nav_target: Some(SlashNavigationTarget::ForkLatest),
    },
];

fn icon_for_token_key(key: &str) -> IconName {
    if key.starts_with("file:") {
        return IconName::FileText;
    }
    let normalized = key
        .strip_prefix("/skill:")
        .or_else(|| key.strip_prefix('/'))
        .unwrap_or(key);
    match normalized {
        "disk" => IconName::HardDrive,
        "ls" => IconName::FolderOpen,
        "sys" => IconName::Cpu,
        "startup" => IconName::Play,
        "eventlog" => IconName::FileText,
        "driver" => IconName::Settings2,
        "runbook" => IconName::FileTerminal,
        "bitlocker" => IconName::LockKeyhole,
        "tree" => IconName::GitBranch,
        "export" => IconName::FileText,
        "fork" => IconName::GitFork,
        _ => IconName::Layers,
    }
}

#[derive(Debug, Clone)]
enum CompletionPopupState {
    Slash {
        replace_range: Range<usize>,
        items: Vec<SlashEntrySpec>,
        selected_idx: usize,
    },
    Files {
        replace_range: Range<usize>,
        paths: Vec<String>,
        selected_idx: usize,
    },
}

pub struct ComposerView {
    client: ClientHandle,
    draft_key: DraftKey,
    last_synced_edit_rev: u64,
    had_active_ime_mark: bool,
    textarea: Entity<TextareaState>,
    completion: Option<CompletionPopupState>,
    completion_source: Option<(InputContent, usize)>,
    completion_scroll: ScrollHandle,
    file_search_task: Option<Task<()>>,
    submitting: Option<DraftKey>,
    model_change_pending: bool,
    picker_open: bool,
    picker_open_epoch: u64,
    picker_presence: TransitionState<f32>,
    picker_page: ModelPickerPage,
    picker_hi_idx: usize,
    picker_kbd_active: bool,
    picker_last_pointer: Option<Point<Pixels>>,
    picker_tracker: KbdDynamicBoundsTracker,
    picker_scroll: ScrollHandle,
    picker_prior_focus: Option<FocusHandle>,
    comp_kbd_active: bool,
    comp_last_pointer: Option<Point<Pixels>>,
    comp_tracker: KbdDynamicBoundsTracker,
    context_popover_open: bool,
    context_presence: TransitionState<f32>,
    context_prior_focus: Option<FocusHandle>,
    cached_quota_provider: Option<String>,
    cached_quota: Option<Value>,
    quota_task: Option<Task<()>>,
    quota_loading: bool,
    queue_pending: Option<(String, ParsedQueueSnapshot)>,
    queue_receipt: Option<(String, ParsedQueueSnapshot)>,
    pending_queue_reorder: Option<(String, Vec<String>)>,
    queue_focus: FocusHandle,
    queue_kbd_active: bool,
    queue_last_pointer: Option<Point<Pixels>>,
    queue_tracker: KbdDynamicBoundsTracker,
    queue_selected_id: Option<String>,
    queue_action: Option<QueueRowAction>,
    attach_btn_focus: FocusHandle,
    model_btn_focus: FocusHandle,
    context_btn_focus: FocusHandle,
    send_btn_focus: FocusHandle,
    footer_kbd_active: bool,
    footer_last_pointer: Option<Point<Pixels>>,
    footer_tracker: KbdDynamicBoundsTracker,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerHostEvent> for ComposerView {}

impl Focusable for ComposerView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.textarea.read(cx).presentation().focus_handle().clone()
    }
}

fn find_model_choice<'a>(
    models: &'a cst_client::ModelsSnapshot,
    provider: &str,
    id: &str,
) -> Option<&'a cst_client::ModelChoiceDto> {
    models
        .selected
        .iter()
        .chain(models.models.iter())
        .chain(models.catalog.iter())
        .chain(models.default_model.iter())
        .find(|m| m.provider == provider && m.id == id)
}

fn active_thinking_levels<'a>(
    models: &'a cst_client::ModelsSnapshot,
    provider: &str,
    id: &str,
) -> &'a [String] {
    find_model_choice(models, provider, id)
        .map(|m| m.thinking_levels.as_slice())
        .unwrap_or_default()
}

impl ComposerView {
    pub fn new(
        client: ClientHandle,
        draft_key: DraftKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = Self::placeholder_for(&draft_key);
        let textarea = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder)
                .submit_on_enter(true)
                .auto_grow(1, 8)
        });

        let attach_btn_focus = cx.focus_handle().tab_stop(true);
        let model_btn_focus = cx.focus_handle().tab_stop(true);
        let context_btn_focus = cx.focus_handle().tab_stop(true);
        let send_btn_focus = cx.focus_handle().tab_stop(true);
        let queue_focus = cx.focus_handle().tab_stop(true);

        let sub = cx.subscribe_in(&textarea, window, Self::on_textarea_event);
        let obs = cx.observe_in(&textarea, window, |this, textarea, window, cx| {
            let marked = textarea
                .update(cx, |st, cx| {
                    EntityInputHandler::marked_text_range(st, window, cx)
                })
                .is_some();
            if marked {
                this.had_active_ime_mark = true;
                if this.clear_completion() {
                    cx.notify();
                }
            }
        });

        let mut view = Self {
            client,
            draft_key,
            last_synced_edit_rev: 0,
            had_active_ime_mark: false,
            textarea,
            completion: None,
            completion_source: None,
            completion_scroll: ScrollHandle::new(),
            file_search_task: None,
            submitting: None,
            model_change_pending: false,
            picker_open: false,
            picker_open_epoch: 0,
            picker_presence: TransitionState::default(),
            picker_page: ModelPickerPage::Main,
            picker_hi_idx: 0,
            picker_kbd_active: false,
            picker_last_pointer: None,
            picker_tracker: KbdDynamicBoundsTracker::new(),
            picker_scroll: ScrollHandle::new(),
            picker_prior_focus: None,
            comp_kbd_active: false,
            comp_last_pointer: None,
            comp_tracker: KbdDynamicBoundsTracker::new(),
            context_popover_open: false,
            context_presence: TransitionState::default(),
            context_prior_focus: None,
            cached_quota_provider: None,
            cached_quota: None,
            quota_task: None,
            quota_loading: false,
            queue_pending: None,
            queue_receipt: None,
            pending_queue_reorder: None,
            queue_focus,
            queue_kbd_active: false,
            queue_last_pointer: None,
            queue_tracker: KbdDynamicBoundsTracker::new(),
            queue_selected_id: None,
            queue_action: None,
            attach_btn_focus,
            model_btn_focus,
            context_btn_focus,
            send_btn_focus,
            footer_kbd_active: false,
            footer_last_pointer: None,
            footer_tracker: KbdDynamicBoundsTracker::new(),
            _subscriptions: vec![sub, obs],
        };
        view.pull_from_workspace(window, cx);
        view
    }

    fn placeholder_for(key: &DraftKey) -> &'static str {
        match key {
            DraftKey::NewSession => "描述这台电脑遇到的问题…",
            DraftKey::Session(_) => "继续提问，或补充这台电脑的情况…",
        }
    }

    pub fn textarea_state(&self) -> &Entity<TextareaState> {
        &self.textarea
    }

    pub fn draft_key(&self) -> &DraftKey {
        &self.draft_key
    }

    pub fn has_unsent_draft_content(&self, cx: &App) -> bool {
        let content = self.textarea.read(cx).content();
        if !content.text().trim().is_empty() || !content.tokens().is_empty() {
            return true;
        }
        self.client
            .with_workspace(|ws| ws.draft(&self.draft_key).is_some_and(|d| !d.is_empty()))
    }

    /// Switches this composer between `DraftKey::NewSession` and `DraftKey::Session(id)` without losing drafts.
    pub fn set_draft_key(
        &mut self,
        next_key: DraftKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.draft_key == next_key {
            self.reconcile_external_draft_changes(window, cx);
            return;
        }
        // Change events already persist committed text; never save IME preedit on switch.
        self.draft_key = next_key.clone();
        self.last_synced_edit_rev = 0;
        self.clear_completion();
        self.close_picker();
        self.context_popover_open = false;
        self.context_prior_focus = None;
        self.had_active_ime_mark = false;
        let ph = Self::placeholder_for(&next_key);
        self.textarea.update(cx, |st, cx| {
            st.set_placeholder(ph, window, cx);
        });
        self.pull_from_workspace(window, cx);
        cx.notify();
    }

    /// Returns `true` when the underlying `TextareaState` currently holds active IME preedit (`marked_text_range`).
    pub fn is_ime_composing(&self, window: &mut Window, cx: &mut App) -> bool {
        self.textarea
            .update(cx, |st, cx| {
                EntityInputHandler::marked_text_range(st, window, cx)
            })
            .is_some()
    }

    pub fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_floating_popovers(cx);
        self.textarea.update(cx, |st, cx| {
            st.focus(window, cx);
        });
        cx.notify();
    }

    pub fn close_floating_popovers(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        if self.clear_completion() {
            changed = true;
        }
        if self.picker_open {
            self.close_picker();
            changed = true;
        }
        if self.context_popover_open {
            self.context_popover_open = false;
            self.context_prior_focus = None;
            changed = true;
        }
        if changed {
            cx.notify();
        }
        changed
    }

    pub fn open_model_picker_shortcut(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.is_ime_composing(window, cx) {
            return false;
        }
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("model_picker");
        self.clear_completion();
        self.context_popover_open = false;
        self.context_prior_focus = None;
        if self.picker_open {
            if let Some(prev) = self.close_picker() {
                prev.focus(window, cx);
            } else {
                self.focus_handle(cx).focus(window, cx);
            }
        } else {
            self.picker_open = true;
            self.picker_open_epoch = self.picker_open_epoch.wrapping_add(1);
            self.set_picker_page(ModelPickerPage::Main);
            self.picker_kbd_active = true;
            self.picker_last_pointer = Some(window.mouse_position());
            self.picker_prior_focus = window.focused(cx);
            self.model_btn_focus.focus(window, cx);
        }
        cx.notify();
        true
    }

    pub fn blur_if_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.focus_handle(cx).is_focused(window) || self.is_ime_composing(window, cx) {
            return false;
        }
        window.blur(cx);
        cx.notify();
        true
    }

    /// Closes the single topmost open composer popover (`completion` / `picker` / `context`) and restores focus.
    pub fn dismiss_topmost_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.clear_completion() {
            cx.notify();
            return true;
        }
        if self.picker_open {
            if self.picker_page != ModelPickerPage::Main {
                let back_idx = usize::from(self.picker_page == ModelPickerPage::Thinking);
                self.set_picker_page(ModelPickerPage::Main);
                self.picker_hi_idx = back_idx;
                cx.notify();
                return true;
            }
            if let Some(prev) = self.close_picker() {
                prev.focus(window, cx);
            } else {
                self.model_btn_focus.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if self.context_popover_open {
            self.context_popover_open = false;
            if let Some(prev) = self.context_prior_focus.take() {
                prev.focus(window, cx);
            } else {
                self.context_btn_focus.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        false
    }

    /// Re-pulls `WorkspaceState` draft into `TextareaState` if an external action (e.g. outbox recovery / edit backfill) mutated it.
    pub fn reconcile_external_draft_changes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ext_rev = self
            .client
            .with_workspace(|ws| ws.draft_edit_revision(&self.draft_key));
        if ext_rev != self.last_synced_edit_rev && !self.is_ime_composing(window, cx) {
            self.pull_from_workspace(window, cx);
        }
    }

    fn pull_from_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (draft, rev) = self.client.with_workspace(|ws| {
            (
                ws.draft(&self.draft_key).cloned().unwrap_or_default(),
                ws.draft_edit_revision(&self.draft_key),
            )
        });
        self.last_synced_edit_rev = rev;
        let content = build_input_content_from_draft(&draft);
        let had_ime_mark = self.is_ime_composing(window, cx);
        if self.textarea.read(cx).content() != content || had_ime_mark {
            self.clear_completion();
            self.had_active_ime_mark = false;
            if had_ime_mark {
                window.cancel_ime_composition();
            }
            self.textarea.update(cx, |st, cx| {
                EntityInputHandler::unmark_text(st, window, cx);
                st.set_value(content, window, cx);
            });
        }
    }

    fn push_to_workspace(&mut self, cx: &mut Context<Self>) {
        let content = self.textarea.read(cx).content();
        let (plain_text, draft_tokens) = extract_draft_text_and_tokens(&content);
        let cursor_chars = plain_text.chars().count();
        let key = self.draft_key.clone();
        let new_rev = self.client.with_workspace_mut(|ws| {
            let d = ws.draft_mut(key.clone());
            d.text = plain_text;
            d.tokens = draft_tokens;
            d.cursor_char_offset = cursor_chars;
            ws.draft_edit_revision(&key)
        });
        self.last_synced_edit_rev = new_rev;
    }

    fn on_textarea_event(
        &mut self,
        _textarea: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                if self.is_ime_composing(window, cx) {
                    self.had_active_ime_mark = true;
                    return;
                }
                self.had_active_ime_mark = false;
                self.push_to_workspace(cx);
                self.update_completion_from_caret(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {}
            InputEvent::Blur => {
                self.clear_completion();
                self.had_active_ime_mark = false;
                cx.notify();
            }
            InputEvent::Focus => {
                if self.picker_open {
                    self.close_picker();
                }
                if self.context_popover_open {
                    self.context_popover_open = false;
                    self.context_prior_focus = None;
                }
                cx.notify();
            }
        }
    }

    fn clear_completion(&mut self) -> bool {
        self.completion_source = None;
        self.comp_kbd_active = false;
        self.comp_last_pointer = None;
        let searching = self.file_search_task.take().is_some();
        self.completion.take().is_some() || searching
    }

    fn completion_is_current(&self, cx: &App) -> bool {
        let st = self.textarea.read(cx);
        self.completion_source
            .as_ref()
            .is_some_and(|(content, caret)| *caret == st.cursor() && *content == st.content())
    }

    fn move_completion(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle(cx).is_focused(window) {
            cx.propagate();
            return;
        }
        if self.is_ime_composing(window, cx) {
            cx.stop_propagation();
            return;
        }
        if !self.completion_is_current(cx) {
            if self.clear_completion() {
                cx.notify();
            }
            if crate::shell::input_nav::handle_textarea_vertical_boundary(
                &self.textarea,
                forward,
                window,
                cx,
            ) {
                cx.stop_propagation();
            } else {
                cx.propagate();
            }
            return;
        }
        let Some(comp) = &mut self.completion else {
            if crate::shell::input_nav::handle_textarea_vertical_boundary(
                &self.textarea,
                forward,
                window,
                cx,
            ) {
                cx.stop_propagation();
            } else {
                cx.propagate();
            }
            return;
        };
        let (index, len, separator) = match comp {
            CompletionPopupState::Slash {
                items,
                selected_idx,
                ..
            } => {
                let separator = items
                    .iter()
                    .position(|item| !item.command.starts_with("/skill:"))
                    .filter(|i| *i > 0);
                (selected_idx, items.len(), separator)
            }
            CompletionPopupState::Files {
                paths,
                selected_idx,
                ..
            } => (selected_idx, paths.len(), None),
        };
        if len == 0 {
            cx.propagate();
            return;
        }
        *index = if forward {
            (*index + 1) % len
        } else {
            (*index + len - 1) % len
        };
        let scroll_row = *index + usize::from(separator.is_some_and(|i| *index >= i));
        self.comp_kbd_active = true;
        self.comp_last_pointer = Some(window.mouse_position());
        self.completion_scroll.scroll_to_item(scroll_row);
        cx.stop_propagation();
        cx.notify();
    }

    fn complete_on_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle(cx).is_focused(window) {
            cx.propagate();
            return;
        }
        if self.is_ime_composing(window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.completion.is_some() && self.completion_is_current(cx) {
            cx.stop_propagation();
            self.accept_selected_completion(window, cx);
            return;
        }
        // No active completion menu: let Tab move focus out of the composer into footer controls.
        cx.propagate();
    }

    fn close_picker(&mut self) -> Option<FocusHandle> {
        self.picker_open = false;
        self.picker_kbd_active = false;
        self.picker_last_pointer = None;
        self.picker_prior_focus.take()
    }

    fn set_picker_page(&mut self, page: ModelPickerPage) {
        self.picker_page = page;
        self.picker_hi_idx = 0;
        self.picker_scroll = ScrollHandle::new();
    }

    fn handle_picker_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.picker_open || !self.model_btn_focus.is_focused(window) {
            return;
        }
        if event.keystroke.key.as_str() == "tab"
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.platform
        {
            // Keep focus trapped inside the open model picker popover until Esc/selection closes it.
            cx.stop_propagation();
            return;
        }
        let snap = self.client.snapshot();
        let empty_models = cst_client::ModelsSnapshot::default();
        let models = snap.models.as_deref().unwrap_or(&empty_models);
        let scoped = resolve_scoped_models(models);
        let (provider, id, _, _) = self.current_model_choice(&snap);
        let levels = super::model_picker::effective_thinking_levels(active_thinking_levels(
            models, &provider, &id,
        ));
        let count = match self.picker_page {
            ModelPickerPage::Main => 2,
            ModelPickerPage::Models => scoped.len(),
            ModelPickerPage::Thinking => levels.len(),
        };
        let step = resolve_menu_nav_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers,
            event.is_held,
            self.picker_hi_idx,
            count,
            true,
        );
        match step {
            KbdMenuStep::Move(next) => {
                self.picker_hi_idx = next;
                self.picker_kbd_active = true;
                self.picker_last_pointer = Some(window.mouse_position());
            }
            KbdMenuStep::SubRight => {
                if self.picker_page != ModelPickerPage::Main {
                    return;
                }
                let index = self.picker_hi_idx.min(1);
                self.set_picker_page(if index == 0 {
                    ModelPickerPage::Models
                } else {
                    ModelPickerPage::Thinking
                });
                self.picker_kbd_active = true;
                self.picker_last_pointer = Some(window.mouse_position());
            }
            KbdMenuStep::BackLeft => {
                if self.picker_page == ModelPickerPage::Main {
                    return;
                }
                let back_idx = usize::from(self.picker_page == ModelPickerPage::Thinking);
                self.set_picker_page(ModelPickerPage::Main);
                self.picker_hi_idx = back_idx;
                self.picker_kbd_active = true;
                self.picker_last_pointer = Some(window.mouse_position());
            }
            KbdMenuStep::Activate(index) => {
                self.picker_kbd_active = true;
                self.picker_last_pointer = Some(window.mouse_position());
                match self.picker_page {
                    ModelPickerPage::Main => self.set_picker_page(if index == 0 {
                        ModelPickerPage::Models
                    } else {
                        ModelPickerPage::Thinking
                    }),
                    ModelPickerPage::Models => {
                        if let Some(model) = scoped.get(index) {
                            self.select_model(model.provider.clone(), model.id.clone(), cx);
                        }
                    }
                    ModelPickerPage::Thinking => {
                        if let Some(level) = levels.get(index) {
                            self.select_thinking(level.clone(), window, cx);
                        }
                    }
                }
            }
            KbdMenuStep::ReorderUp | KbdMenuStep::ReorderDown | KbdMenuStep::Ignore => return,
        }
        if self.picker_page == ModelPickerPage::Models {
            self.picker_scroll.scroll_to_item(self.picker_hi_idx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn select_model(&mut self, provider: String, model_id: String, cx: &mut Context<Self>) {
        if !self.picker_open
            || self.picker_page != ModelPickerPage::Models
            || self.model_change_pending
        {
            return;
        }
        let key = self.draft_key.clone();
        let sid = match &key {
            DraftKey::Session(id) => Some(id.clone()),
            DraftKey::NewSession => None,
        };
        let client = self.client.clone();
        // Only the accepted selection becomes a draft override. On error the old choice stays intact.
        self.model_change_pending = true;
        cx.spawn(async move |weak, cx| {
            let res = client
                .select_model(sid.clone(), provider.clone(), model_id.clone())
                .await;
            if res.is_ok() {
                if let Some(ref session_id) = sid {
                    client.refresh_session_usage(session_id.clone());
                }
                let _ = client.refresh_models(sid).await;
            }
            let _ = weak.update(cx, |this, cx| {
                this.model_change_pending = false;
                match res {
                    Ok(model) => {
                        let selected =
                            serde_json::from_value::<cst_client::ModelChoiceDto>(model).ok();
                        this.client.with_workspace_mut(|ws| {
                            let draft = ws.draft_mut(key.clone());
                            draft.selected_provider = Some(provider);
                            draft.selected_model_id = Some(model_id);
                            let previous = draft.selected_thinking_level.take();
                            if let Some(selected) = selected {
                                draft.selected_thinking_level = previous
                                    .filter(|level| {
                                        matches!(key, DraftKey::NewSession)
                                            && selected.thinking_levels.contains(level)
                                    })
                                    .or(selected.thinking_level);
                            }
                        });
                    }
                    Err(e) => cx.emit(ComposerHostEvent::ErrorMessage(e.to_string())),
                }
                cx.notify();
            });
        })
        .detach();
        self.set_picker_page(ModelPickerPage::Main);
        cx.notify();
    }

    fn select_thinking(&mut self, level: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.picker_open
            || self.picker_page != ModelPickerPage::Thinking
            || self.model_change_pending
        {
            return;
        }
        let key = self.draft_key.clone();
        match key {
            DraftKey::NewSession => self.client.with_workspace_mut(|ws| {
                ws.draft_mut(key).selected_thinking_level = Some(level);
            }),
            DraftKey::Session(ref sid) => {
                let sid = sid.clone();
                let client = self.client.clone();
                self.model_change_pending = true;
                cx.spawn(async move |weak, cx| {
                    let res = client
                        .set_session_thinking_level(sid.clone(), level.clone())
                        .await;
                    if res.is_ok() {
                        let _ = client.refresh_models(Some(sid)).await;
                    }
                    let _ = weak.update(cx, |this, cx| {
                        this.model_change_pending = false;
                        match res {
                            Ok(_) => this.client.with_workspace_mut(|ws| {
                                ws.draft_mut(key).selected_thinking_level = Some(level);
                            }),
                            Err(e) => cx.emit(ComposerHostEvent::ErrorMessage(e.to_string())),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        self.set_picker_page(ModelPickerPage::Main);
        self.picker_hi_idx = 1;
        self.model_btn_focus.focus(window, cx);
        cx.notify();
    }

    fn update_completion_from_caret(&mut self, cx: &mut Context<Self>) {
        self.clear_completion();
        if self.picker_open || self.context_popover_open {
            return;
        }
        let st = self.textarea.read(cx);
        let content = st.content();
        let caret = st.cursor();
        let Some(text) = content.text().get(..caret) else {
            return;
        };
        let tokens = content.tokens();
        let overlaps_token = |start| {
            tokens
                .iter()
                .any(|span| span.range().start < caret && start < span.range().end)
        };

        let line_start = text.rfind('\n').map_or(0, |ix| ix + 1);
        let line = &text[line_start..];
        let trimmed = line.trim_start_matches(' ');
        let slash_start = caret - trimmed.len();
        if let Some(query) = trimmed.strip_prefix('/')
            && !query.contains(char::is_whitespace)
            && !overlaps_token(slash_start)
        {
            let query = query.to_ascii_lowercase();
            let items: Vec<_> = SLASH_CATALOG
                .iter()
                .filter(|item| {
                    item.command[1..].contains(&query)
                        || item.short_label.to_ascii_lowercase().contains(&query)
                })
                .cloned()
                .collect();
            if !items.is_empty() {
                self.completion_source = Some((content, caret));
                self.completion_scroll = ScrollHandle::new();
                self.completion = Some(CompletionPopupState::Slash {
                    replace_range: slash_start..caret,
                    items,
                    selected_idx: 0,
                });
                return;
            }
        }

        if let Some(at) = text.rfind('@')
            && (at == 0 || text[..at].ends_with(char::is_whitespace))
            && !overlaps_token(at)
            && !text[at + 1..].contains(char::is_whitespace)
        {
            let query = text[at + 1..].to_owned();
            let sid = match &self.draft_key {
                DraftKey::Session(id) => Some(id.clone()),
                DraftKey::NewSession => None,
            };
            let client = self.client.clone();
            self.completion_source = Some((content, caret));
            self.file_search_task = Some(cx.spawn(async move |weak, cx| {
                let result = client.search_files(sid, query).await;
                let _ = weak.update(cx, |this, cx| {
                    this.file_search_task = None;
                    if let Ok(paths) = result
                        && !paths.is_empty()
                        && this.completion_is_current(cx)
                    {
                        this.completion_scroll = ScrollHandle::new();
                        this.completion = Some(CompletionPopupState::Files {
                            replace_range: at..caret,
                            paths,
                            selected_idx: 0,
                        });
                    } else {
                        this.clear_completion();
                    }
                    cx.notify();
                });
            }));
        }
    }

    fn accept_selected_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_ime_composing(window, cx) || !self.completion_is_current(cx) {
            self.clear_completion();
            cx.notify();
            return;
        }
        self.completion_source = None;
        let Some(comp) = self.completion.take() else {
            return;
        };
        match comp {
            CompletionPopupState::Slash {
                replace_range,
                items,
                selected_idx,
            } => {
                let Some(spec) = items.get(selected_idx).cloned() else {
                    return;
                };
                if let Some(nav) = spec.nav_target {
                    // Clear the `/tree` / `/export` / `/fork` trigger text and emit navigation
                    self.textarea.update(cx, |st, cx| {
                        let cur = st.value().to_string();
                        let end = replace_range.end.min(cur.len());
                        let start = replace_range.start.min(end);
                        let mut next = cur;
                        next.replace_range(start..end, "");
                        st.set_value(next, window, cx);
                    });
                    self.push_to_workspace(cx);
                    cx.emit(ComposerHostEvent::NavigateSlashRoute(nav));
                    cx.notify();
                    return;
                }
                let tok =
                    UiInlineToken::new(spec.command, spec.command).with_label(spec.short_label);
                self.insert_inline_token_at(replace_range, tok, window, cx);
            }
            CompletionPopupState::Files {
                replace_range,
                paths,
                selected_idx,
                ..
            } => {
                let Some(path) = paths.get(selected_idx).cloned() else {
                    return;
                };
                let short_name = path.rsplit('/').next().unwrap_or(path.as_str()).to_owned();
                let token_text = format!("@{path}");
                let tok =
                    UiInlineToken::new(format!("file:{path}"), token_text).with_label(short_name);
                self.insert_inline_token_at(replace_range, tok, window, cx);
            }
        }
    }

    pub fn insert_inline_token_at(
        &mut self,
        replace_range: Range<usize>,
        token: UiInlineToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.textarea.update(cx, |st, cx| {
            let len = st.value().len();
            let end = replace_range.end.min(len);
            let start = replace_range.start.min(end);
            if st
                .replace_range_with_token(start..end, token, window, cx)
                .is_ok()
            {
                st.insert(" ", window, cx);
            }
        });
        self.push_to_workspace(cx);
        let fh = self.textarea.read(cx).presentation().focus_handle().clone();
        fh.focus(window, cx);
        cx.notify();
    }

    pub fn remove_inline_token_span(
        &mut self,
        span_range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.textarea.update(cx, |st, cx| {
            let content = st.content();
            let raw = content.text().to_string();
            if span_range.end <= raw.len() && span_range.start < span_range.end {
                let mut remove_end = span_range.end;
                if raw[remove_end..].starts_with(' ') {
                    remove_end += 1;
                }
                let mut next_str = String::with_capacity(raw.len());
                next_str.push_str(&raw[..span_range.start]);
                next_str.push_str(&raw[remove_end..]);
                let mut rebuilt = InputContent::new(next_str);
                let removed_len = remove_end - span_range.start;
                for existing in content.tokens() {
                    if existing.range() == span_range {
                        continue;
                    }
                    let next_r = if existing.range().start >= remove_end {
                        (existing.range().start - removed_len)..(existing.range().end - removed_len)
                    } else {
                        existing.range()
                    };
                    if let Ok(updated) =
                        rebuilt.clone().with_token(next_r, existing.token().clone())
                    {
                        rebuilt = updated;
                    }
                }
                st.set_value(rebuilt, window, cx);
            }
        });
        self.push_to_workspace(cx);
        cx.notify();
    }

    pub fn attach_image_bytes(
        &mut self,
        bytes: Vec<u8>,
        file_name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        let key = self.draft_key.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let pool = client.with_workspace(|ws| ws.image_pool().clone());
                    let handle = pool.admit_image(bytes, file_name)?;
                    client.with_workspace_mut(|ws| ws.draft_mut(key).attach_image(handle))
                })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.last_synced_edit_rev = this
                    .client
                    .with_workspace(|ws| ws.draft_edit_revision(&this.draft_key));
                if let Err(err) = res {
                    cx.emit(ComposerHostEvent::ErrorMessage(err.to_string()));
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn attach_image_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let client = self.client.clone();
            let key = self.draft_key.clone();
            cx.spawn(async move |weak, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        let bytes = read_attachment_file(&path)?;
                        let fname = path.file_name().and_then(|s| s.to_str()).map(str::to_owned);
                        let pool = client.with_workspace(|ws| ws.image_pool().clone());
                        let handle = pool.admit_image(bytes, fname)?;
                        client.with_workspace_mut(|ws| ws.draft_mut(key).attach_image(handle))
                    })
                    .await;
                let _ = weak.update(cx, |this, cx| {
                    this.last_synced_edit_rev = this
                        .client
                        .with_workspace(|ws| ws.draft_edit_revision(&this.draft_key));
                    if let Err(err) = res {
                        cx.emit(ComposerHostEvent::ErrorMessage(err.to_string()));
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub fn remove_draft_image(&mut self, index: usize, cx: &mut Context<Self>) {
        let key = self.draft_key.clone();
        self.last_synced_edit_rev = self.client.with_workspace_mut(|ws| {
            let d = ws.draft_mut(key.clone());
            if index < d.images.len() {
                d.images.remove(index);
            }
            ws.draft_edit_revision(&key)
        });
        cx.notify();
    }

    pub fn trigger_submit_from_button(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_ime_composing(window, cx) {
            let committed = window.finish_ime_composition();
            self.textarea.update(cx, |st, cx| {
                if let Some(text) = committed {
                    EntityInputHandler::replace_text_in_range(st, None, &text, window, cx);
                } else {
                    EntityInputHandler::unmark_text(st, window, cx);
                }
            });
            self.had_active_ime_mark = false;
        }
        self.trigger_submit(window, cx);
    }

    pub fn trigger_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting.is_some()
            || self.model_change_pending
            || self.is_ime_composing(window, cx)
            || matches!(&self.draft_key, DraftKey::Session(id)
                if self.client.with_workspace(|ws| ws.active_session_id() != Some(id.as_str())))
        {
            return;
        }
        self.clear_completion();
        self.push_to_workspace(cx);

        // Handle `/compact` command token (with optional custom instructions) inside an active session directly
        let compact_instructions = self.client.with_workspace(|ws| {
            ws.draft(&self.draft_key).and_then(|d| {
                let has_compact_token = matches!(
                    d.tokens.as_slice(),
                    [DraftToken::Command { name, .. }] if name == "/compact"
                );
                (d.images.is_empty() && has_compact_token).then(|| {
                    let trimmed = d.text.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_owned())
                })
            })
        });
        if let Some(instructions) = compact_instructions
            && let DraftKey::Session(sid) = self.draft_key.clone()
        {
            let key = self.draft_key.clone();
            self.submitting = Some(key.clone());
            let draft = self.client.with_workspace_mut(|ws| ws.take_draft(&key));
            self.pull_from_workspace(window, cx);
            let client = self.client.clone();
            cx.spawn_in(window, async move |weak, cx| {
                let res = client.compact_session(sid, instructions).await;
                let _ = weak.update_in(cx, |this, window, cx| {
                    this.submitting = None;
                    match res {
                        Ok(_) => {
                            cx.emit(ComposerHostEvent::TopNotice(
                                "已完成会话上下文压缩。".into(),
                            ));
                        }
                        Err(e) => {
                            this.client.with_workspace_mut(|ws| {
                                ws.restore_draft_if_empty(key, draft);
                            });
                            this.reconcile_external_draft_changes(window, cx);
                            cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
            return;
        }

        let is_empty = self.client.with_workspace(|ws| {
            ws.draft(&self.draft_key)
                .is_none_or(ComposerDraft::is_empty)
        });
        if is_empty {
            return;
        }

        let snap = self.client.snapshot();
        let active_provider = self.current_model_choice(&snap).0;

        let target_key = self.draft_key.clone();
        self.submitting = Some(target_key.clone());
        let client = self.client.clone();
        let submission = client.begin_submit_draft(target_key.clone());
        self.pull_from_workspace(window, cx);
        cx.notify();
        cx.spawn_in(window, async move |weak, cx| {
            let submit_res = submission.await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.submitting = None;
                if this.draft_key != target_key {
                    cx.notify();
                    return;
                }
                this.reconcile_external_draft_changes(window, cx);
                match submit_res {
                    Ok(outcome) => {
                        cx.emit(ComposerHostEvent::SubmittedToSession {
                            session_id: outcome.session_id,
                        });
                    }
                    Err(ClientError::CredentialsRequired { .. }) => {
                        cx.emit(ComposerHostEvent::MissingCredentials {
                            provider: active_provider,
                        });
                    }
                    Err(err) => {
                        cx.emit(ComposerHostEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn queue_snapshot(&self, sid: &str, raw: Option<&Value>) -> ParsedQueueSnapshot {
        let server = ParsedQueueSnapshot::from_json(raw);
        if let Some((_, receipt)) = self
            .queue_receipt
            .as_ref()
            .filter(|(key, queue)| sid == key && queue.version > server.version)
        {
            return receipt.clone();
        }
        server
    }

    fn mutate_queue(
        &mut self,
        sid: String,
        version: u64,
        mutation: QueueMutation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.draft_key != DraftKey::Session(sid.clone()) {
            return;
        }
        if self.queue_pending.is_some() {
            if let QueueMutation::Reorder(ids) = mutation
                && let Some((ref pending_sid, ref mut pending_queue)) = self.queue_pending
                && *pending_sid == sid
            {
                *pending_queue = QueueMutation::Reorder(ids.clone()).project(pending_queue.clone());
                self.pending_queue_reorder = Some((sid, ids));
                cx.notify();
            }
            return;
        }
        let snap = self.client.snapshot();
        let queue = self.queue_snapshot(
            &sid,
            snap.loaded_sessions
                .get(&sid)
                .and_then(|s| s.queue.as_ref()),
        );
        if queue.version != version {
            cx.notify();
            return;
        }
        self.queue_pending = Some((sid.clone(), mutation.project(queue)));
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let result = mutation.send(client, sid.clone(), version).await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.queue_pending = None;
                match result {
                    Ok(response) => {
                        if let Some(raw) = response.get("queue") {
                            this.queue_receipt =
                                Some((sid.clone(), ParsedQueueSnapshot::from_json(Some(raw))));
                        }
                        if let Some((next_sid, next_ids)) = this.pending_queue_reorder.take()
                            && next_sid == sid
                        {
                            let latest_snap = this.client.snapshot();
                            let latest_queue = this.queue_snapshot(
                                &next_sid,
                                latest_snap
                                    .loaded_sessions
                                    .get(&next_sid)
                                    .and_then(|s| s.queue.as_ref()),
                            );
                            if latest_queue.reorderable_ids() != next_ids {
                                this.mutate_queue(
                                    next_sid,
                                    latest_queue.version,
                                    QueueMutation::Reorder(next_ids),
                                    window,
                                    cx,
                                );
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        this.pending_queue_reorder = None;
                        cx.emit(ComposerHostEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_attach_dialog(&self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(SharedString::from("选择图片附件")),
        });
        let ent = cx.entity();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                ent.update(cx, |this, cx| {
                    this.attach_image_paths(paths, cx);
                });
            }
        })
        .detach();
    }

    fn toggle_context_popover(
        &mut self,
        provider: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("context_popover");
        self.clear_completion();
        self.close_picker();
        self.context_popover_open = !self.context_popover_open;
        if self.context_popover_open {
            self.context_prior_focus = window.focused(cx);
            self.context_btn_focus.focus(window, cx);
            self.refresh_quota_for_provider(provider, false, cx);
            if let DraftKey::Session(ref sid) = self.draft_key {
                self.client.refresh_session_usage(sid.clone());
            }
        } else {
            self.context_prior_focus = None;
        }
        cx.notify();
    }

    fn toggle_model_picker(&mut self, from_kbd: bool, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("model_picker");
        self.clear_completion();
        self.context_popover_open = false;
        self.context_prior_focus = None;
        if self.picker_open {
            self.close_picker();
        } else {
            self.picker_open = true;
            self.picker_open_epoch = self.picker_open_epoch.wrapping_add(1);
            self.set_picker_page(ModelPickerPage::Main);
            self.picker_kbd_active = from_kbd;
            self.picker_last_pointer = from_kbd.then(|| window.mouse_position());
            self.picker_prior_focus = window.focused(cx);
            self.model_btn_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn handle_queue_key(
        &mut self,
        sid: &str,
        queue: &ParsedQueueSnapshot,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if queue.items.is_empty() {
            return;
        }
        let cur_idx = self
            .queue_selected_id
            .as_deref()
            .and_then(|id| queue.items.iter().position(|item| item.id == id))
            .unwrap_or(0);
        let item = &queue.items[cur_idx];
        let queued_ids = queue.reorderable_ids();
        let slots = queue_item_action_slots(item, &queued_ids);
        let step = resolve_menu_nav_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers,
            event.is_held,
            cur_idx,
            queue.items.len(),
            true,
        );
        match step {
            KbdMenuStep::Move(next) => {
                self.queue_selected_id = Some(queue.items[next].id.clone());
                self.queue_action = None;
                self.queue_kbd_active = true;
                self.queue_last_pointer = Some(window.mouse_position());
                cx.stop_propagation();
                cx.notify();
            }
            KbdMenuStep::SubRight => {
                if slots.is_empty() {
                    return;
                }
                let cur_pos = self
                    .queue_action
                    .and_then(|act| slots.iter().position(|&s| s == act));
                let next_a = match cur_pos {
                    None => 0,
                    Some(i) => (i + 1).min(slots.len() - 1),
                };
                self.queue_selected_id = Some(item.id.clone());
                self.queue_action = slots.get(next_a).copied();
                self.queue_kbd_active = true;
                self.queue_last_pointer = Some(window.mouse_position());
                cx.stop_propagation();
                cx.notify();
            }
            KbdMenuStep::BackLeft => {
                let cur_pos = self
                    .queue_action
                    .and_then(|act| slots.iter().position(|&s| s == act));
                let next_a = match cur_pos {
                    None | Some(0) => None,
                    Some(i) => slots.get(i - 1).copied(),
                };
                self.queue_selected_id = Some(item.id.clone());
                self.queue_action = next_a;
                self.queue_kbd_active = true;
                self.queue_last_pointer = Some(window.mouse_position());
                cx.stop_propagation();
                cx.notify();
            }
            KbdMenuStep::ReorderUp | KbdMenuStep::ReorderDown => {
                if let Some(pos) = queued_ids.iter().position(|id| id == &item.id) {
                    let target = if step == KbdMenuStep::ReorderUp {
                        pos.checked_sub(1)
                    } else {
                        (pos + 1 < queued_ids.len()).then_some(pos + 1)
                    };
                    if let Some(swap_pos) = target {
                        let mut next_ids = queued_ids.clone();
                        next_ids.swap(pos, swap_pos);
                        let next_slots = queue_item_action_slots(item, &next_ids);
                        // Preserve action identity across reorder; fall back to row if that action disappeared.
                        self.queue_action = self.queue_action.filter(|a| next_slots.contains(a));
                        self.queue_selected_id = Some(item.id.clone());
                        self.queue_kbd_active = true;
                        self.queue_last_pointer = Some(window.mouse_position());
                        self.mutate_queue(
                            sid.to_owned(),
                            queue.version,
                            QueueMutation::Reorder(next_ids),
                            window,
                            cx,
                        );
                        cx.stop_propagation();
                    }
                }
            }
            KbdMenuStep::Activate(_) => {
                let Some(act) = self.queue_action.filter(|a| slots.contains(a)) else {
                    self.queue_action = slots.first().copied();
                    if self.queue_action.is_some() {
                        self.queue_selected_id = Some(item.id.clone());
                        self.queue_kbd_active = true;
                        self.queue_last_pointer = Some(window.mouse_position());
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                };
                cx.stop_propagation();
                match act {
                    QueueRowAction::MoveUp => {
                        if let Some(pos) = queued_ids.iter().position(|id| id == &item.id)
                            && pos > 0
                        {
                            let mut next_ids = queued_ids.clone();
                            next_ids.swap(pos, pos - 1);
                            self.mutate_queue(
                                sid.to_owned(),
                                queue.version,
                                QueueMutation::Reorder(next_ids),
                                window,
                                cx,
                            );
                        }
                    }
                    QueueRowAction::MoveDown => {
                        if let Some(pos) = queued_ids.iter().position(|id| id == &item.id)
                            && pos + 1 < queued_ids.len()
                        {
                            let mut next_ids = queued_ids.clone();
                            next_ids.swap(pos, pos + 1);
                            self.mutate_queue(
                                sid.to_owned(),
                                queue.version,
                                QueueMutation::Reorder(next_ids),
                                window,
                                cx,
                            );
                        }
                    }
                    QueueRowAction::Steer => {
                        self.mutate_queue(
                            sid.to_owned(),
                            queue.version,
                            QueueMutation::Steer(item.id.clone()),
                            window,
                            cx,
                        );
                    }
                    QueueRowAction::Retry => {
                        self.mutate_queue(
                            sid.to_owned(),
                            queue.version,
                            QueueMutation::Retry(item.id.clone()),
                            window,
                            cx,
                        );
                    }
                    QueueRowAction::Edit => {
                        if self.queue_pending.is_none()
                            && self.draft_key == DraftKey::Session(sid.to_owned())
                        {
                            cx.emit(ComposerHostEvent::RequestEditQueueItem {
                                session_id: sid.to_owned(),
                                item: item.clone(),
                                version: queue.version,
                            });
                        }
                    }
                    QueueRowAction::Delete => {
                        self.mutate_queue(
                            sid.to_owned(),
                            queue.version,
                            QueueMutation::Delete(item.id.clone()),
                            window,
                            cx,
                        );
                    }
                }
            }
            KbdMenuStep::Ignore => {
                if event.keystroke.key.as_str() == "tab"
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                    && !event.keystroke.modifiers.platform
                {
                    self.queue_kbd_active = false;
                }
            }
        }
    }

    fn footer_focused_slot(&self, window: &Window) -> Option<usize> {
        if self.attach_btn_focus.is_focused(window) {
            Some(0)
        } else if self.context_btn_focus.is_focused(window) {
            Some(1)
        } else if self.model_btn_focus.is_focused(window) {
            Some(2)
        } else if self.send_btn_focus.is_focused(window) {
            Some(3)
        } else {
            None
        }
    }

    fn handle_footer_key(
        &mut self,
        provider: String,
        abort_session_id: Option<String>,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.picker_open || self.context_popover_open {
            return;
        }
        let Some(cur) = self.footer_focused_slot(window) else {
            return;
        };
        let focuses = [
            &self.attach_btn_focus,
            &self.context_btn_focus,
            &self.model_btn_focus,
            &self.send_btn_focus,
        ];
        match resolve_dialog_nav_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers,
            event.is_held,
            cur,
            focuses.len(),
        ) {
            DialogNavStep::Move(next) => {
                focuses[next].focus(window, cx);
                self.footer_kbd_active = true;
                self.footer_last_pointer = Some(window.mouse_position());
                cx.stop_propagation();
                cx.notify();
            }
            DialogNavStep::Activate(slot) => {
                self.footer_kbd_active = true;
                self.footer_last_pointer = Some(window.mouse_position());
                cx.stop_propagation();
                match slot {
                    0 => self.open_attach_dialog(cx),
                    1 => self.toggle_context_popover(provider, window, cx),
                    2 => self.toggle_model_picker(true, window, cx),
                    3 => {
                        if let Some(sid_abort) = abort_session_id {
                            let c = self.client.clone();
                            cx.spawn(async move |weak, cx| {
                                if let Err(e) = c.abort_session(sid_abort).await {
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                    });
                                }
                            })
                            .detach();
                        } else {
                            self.trigger_submit_from_button(window, cx);
                        }
                    }
                    _ => {}
                }
            }
            DialogNavStep::TabActivated => {
                self.footer_kbd_active = true;
                self.footer_last_pointer = Some(window.mouse_position());
                cx.notify();
            }
            DialogNavStep::Ignore => {}
        }
    }

    /// Atomically deletes the queued item using optimistic-lock `version` and backfills its content
    /// into the bottom main composer ONLY after `200 OK`.
    pub(crate) fn execute_queue_item_backfill(
        &mut self,
        sid: String,
        item: VisibleQueueItem,
        version: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.queue_pending.is_some() || self.draft_key != DraftKey::Session(sid.clone()) {
            return;
        }
        let snap = self.client.snapshot();
        let queue = self.queue_snapshot(
            &sid,
            snap.loaded_sessions
                .get(&sid)
                .and_then(|s| s.queue.as_ref()),
        );
        if queue.version != version {
            cx.emit(ComposerHostEvent::ErrorMessage(
                "排队消息状态已更新，请重新点击编辑。".into(),
            ));
            cx.notify();
            return;
        }
        let mutation = QueueMutation::Delete(item.id.clone());
        self.queue_pending = Some((sid.clone(), mutation.project(queue)));
        let client = self.client.clone();
        let key = DraftKey::Session(sid.clone());
        let (draft_revision, pool) =
            client.with_workspace(|ws| (ws.draft_edit_revision(&key), ws.image_pool().clone()));
        cx.spawn_in(window, async move |weak, cx| {
            let result = async {
                let mut draft = ComposerDraft {
                    cursor_char_offset: item.text.chars().count(),
                    text: item.text,
                    ..ComposerDraft::default()
                };
                if let Some(skill) = item.skill {
                    let cmd = format!("/skill:{skill}");
                    draft.tokens.push(DraftToken::Command {
                        raw: cmd.clone(),
                        name: cmd,
                    });
                }
                for id in item.attachment_ids {
                    let bytes = client.get_session_attachment_bytes(sid.clone(), id).await?;
                    draft.attach_image(pool.admit_image(bytes, None)?)?;
                }
                let response = mutation.send(client.clone(), sid.clone(), version).await?;
                Ok::<_, ClientError>((response, draft))
            }
            .await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.queue_pending = None;
                match result {
                    Ok((response, mut draft)) => {
                        if let Some(raw) = response.get("queue") {
                            this.queue_receipt =
                                Some((sid.clone(), ParsedQueueSnapshot::from_json(Some(raw))));
                        }
                        let active = this.draft_key == key;
                        let composing = active && this.is_ime_composing(window, cx);
                        let installed = this.client.with_workspace_mut(|ws| {
                            if composing || ws.draft_edit_revision(&key) != draft_revision {
                                return false;
                            }
                            let target = ws.draft_mut(key);
                            target.cursor_char_offset = draft.cursor_char_offset;
                            target.text = std::mem::take(&mut draft.text);
                            target.tokens = std::mem::take(&mut draft.tokens);
                            target.images = std::mem::take(&mut draft.images);
                            true
                        });
                        if !installed {
                            match this.client.retain_recovered_payload(&sid, &draft) {
                                Ok(()) => cx.emit(ComposerHostEvent::TopNotice(
                                    "当前草稿有新内容，排队消息已保留在对话中。".into(),
                                )),
                                Err(err) => {
                                    cx.emit(ComposerHostEvent::ErrorMessage(err.to_string()))
                                }
                            }
                        } else if active {
                            this.pull_from_workspace(window, cx);
                            this.focus_handle(cx).focus(window, cx);
                        }
                    }
                    Err(err) => cx.emit(ComposerHostEvent::ErrorMessage(err.to_string())),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn current_model_choice(
        &self,
        snap: &cst_client::ClientSnapshot,
    ) -> (String, String, String, String) {
        let draft_override = self.client.with_workspace(|ws| {
            ws.draft(&self.draft_key).map(|d| {
                (
                    d.selected_provider.clone(),
                    d.selected_model_id.clone(),
                    d.selected_thinking_level.clone(),
                )
            })
        });
        let empty_models = cst_client::ModelsSnapshot::default();
        let models_ref = snap.models.as_deref().unwrap_or(&empty_models);
        let base_model = models_ref
            .selected
            .as_ref()
            .or(models_ref.default_model.as_ref())
            .or_else(|| models_ref.models.first());

        let provider = draft_override
            .as_ref()
            .and_then(|o| o.0.clone())
            .or_else(|| base_model.map(|m| m.provider.clone()))
            .unwrap_or_else(|| "cstoa".to_owned());
        let model_id = draft_override
            .as_ref()
            .and_then(|o| o.1.clone())
            .or_else(|| base_model.map(|m| m.id.clone()))
            .unwrap_or_else(|| "未选择模型".to_owned());
        let active_model = find_model_choice(models_ref, &provider, &model_id);
        let model_label = active_model
            .map(|m| {
                if m.name.trim().is_empty() {
                    m.id.clone()
                } else {
                    m.name.clone()
                }
            })
            .unwrap_or_else(|| model_id.clone());
        let thinking = draft_override
            .and_then(|o| o.2)
            .or_else(|| active_model.and_then(|m| m.thinking_level.clone()));
        let levels = active_thinking_levels(models_ref, &provider, &model_id);
        let thinking = thinking
            .filter(|level| levels.contains(level))
            .or_else(|| levels.first().cloned())
            .unwrap_or_else(|| "off".to_owned());

        (provider, model_id, model_label, thinking)
    }

    fn refresh_quota_for_provider(
        &mut self,
        provider: String,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if !is_quota_provider(&provider) {
            return;
        }
        let same_provider = self.cached_quota_provider.as_deref() == Some(provider.as_str());
        if same_provider && (self.quota_loading || (!force && self.cached_quota.is_some())) {
            return;
        }
        if !same_provider {
            self.cached_quota = None;
        }
        self.cached_quota_provider = Some(provider.clone());
        self.quota_loading = true;
        let client = self.client.clone();
        self.quota_task = Some(cx.spawn(async move |weak, cx| {
            let res = client.get_provider_quota(provider, force).await;
            let _ = weak.update(cx, |this, cx| {
                this.quota_loading = false;
                if let Ok(val) = res {
                    this.cached_quota = Some(val);
                }
                cx.notify();
            });
        }));
    }

    #[allow(clippy::too_many_arguments)]
    fn render_completion_popover(
        &self,
        _mono_family: SharedString,
        palette: &DesktopPalette,
        kbd_style: KeyboardFocusStyleDto,
        _transparency_enabled: bool,
        _scale_factor: f32,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<gpui_kit::AnyElement> {
        let comp = self.completion.as_ref()?;
        self.comp_tracker.clear();
        let kbd_active = self.comp_kbd_active;
        let ent_move = entity.clone();
        let mut pop = div()
            .id("composer-completion-popover-p2bit")
            .test_support()
            .occlude()
            .relative()
            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(move |ev: &MouseMoveEvent, _w, cx| {
                ent_move.update(cx, |this, cx| {
                    if this.comp_kbd_active
                        && pointer_moved_past_threshold(&mut this.comp_last_pointer, ev.position)
                    {
                        this.comp_kbd_active = false;
                        cx.notify();
                    }
                });
            })
            .w_full()
            .p(space::XS)
            .rounded(radius::ROW)
            .bg(palette.bg_glass)
            .border_1()
            .border_color(palette.border_subtle)
            .flex()
            .flex_col()
            .gap(space::MICRO);

        let mut active_spec: Option<KbdDynamicSpec> = None;

        match comp {
            CompletionPopupState::Slash {
                items,
                selected_idx,
                ..
            } => {
                if kbd_active && !items.is_empty() {
                    active_spec = Some(KbdDynamicSpec::row(format!(
                        "comp-{}",
                        (*selected_idx).min(items.len() - 1)
                    )));
                }
                let mut scroll = div()
                    .id("composer-slash-list")
                    .track_scroll(&self.completion_scroll)
                    .relative()
                    .max_h(px(264.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(space::MICRO);
                let mut saw_skill = false;
                let mut inserted_sep = false;
                for (idx, item) in items.iter().enumerate() {
                    let is_skill = item.command.starts_with("/skill:");
                    if is_skill {
                        saw_skill = true;
                    } else if saw_skill && !inserted_sep {
                        inserted_sep = true;
                        scroll = scroll.child(
                            div()
                                .w_full()
                                .py(space::XS)
                                .child(div().w_full().h(px(1.0)).bg(palette.border_subtle)),
                        );
                    }
                    let is_sel = idx == *selected_idx;
                    let ent = entity.clone();
                    let tr = self.comp_tracker.clone();
                    let row_key = SharedString::from(format!("comp-{idx}"));
                    let icon = icon_for_token_key(item.short_label);
                    let slash_id = SharedString::from(format!("slash-item-{}", item.short_label));
                    let slash_label =
                        SharedString::from(format!("{} {}", item.short_label, item.description));
                    scroll = scroll.child(
                        apply_scale(div(), scale::UI)
                            .id(slash_id.clone())
                            .test_support()
                            .a11y_listitem(slash_id, slash_label, is_sel)
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if is_sel && !kbd_active {
                                palette.bg_track_recess
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::MD)
                            .hover(move |s| {
                                if kbd_active {
                                    s
                                } else {
                                    s.bg(palette.bg_item_hover)
                                }
                            })
                            .on_click(move |_, w, cx| {
                                ent.update(cx, |this, cx| {
                                    if let Some(CompletionPopupState::Slash {
                                        ref mut selected_idx,
                                        ..
                                    }) = this.completion
                                    {
                                        *selected_idx = idx;
                                    }
                                    this.accept_selected_completion(w, cx);
                                });
                            })
                            .on_prepaint(move |bounds, _, _| tr.set(row_key.clone(), bounds))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(space::SM)
                                    .child(Icon::new(icon).size(px(14.0)).text_color(if is_sel {
                                        palette.accent_ink
                                    } else {
                                        palette.text_secondary
                                    }))
                                    .child(
                                        apply_scale(div(), scale::CAPTION_MEDIUM)
                                            .text_color(palette.text_primary)
                                            .child(item.short_label),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .line_height(px(16.0))
                                    .text_color(if is_sel {
                                        palette.text_primary
                                    } else {
                                        palette.text_secondary
                                    })
                                    .truncate()
                                    .child(cjk_inline_label(item.description)),
                            ),
                    );
                }
                pop = pop.child(scroll);
            }
            CompletionPopupState::Files {
                paths,
                selected_idx,
                ..
            } => {
                if kbd_active && !paths.is_empty() {
                    active_spec = Some(KbdDynamicSpec::row(format!(
                        "comp-{}",
                        (*selected_idx).min(paths.len() - 1)
                    )));
                }
                let mut scroll = div()
                    .id("composer-files-list")
                    .track_scroll(&self.completion_scroll)
                    .relative()
                    .max_h(px(240.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(space::MICRO);
                for (idx, path) in paths.iter().enumerate() {
                    let is_sel = idx == *selected_idx;
                    let ent = entity.clone();
                    let tr = self.comp_tracker.clone();
                    let row_key = SharedString::from(format!("comp-{idx}"));
                    let file_id = SharedString::from(format!("file-completion-item-{idx}"));
                    scroll = scroll.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(file_id.clone())
                            .test_support()
                            .a11y_listitem(file_id, path.clone(), is_sel)
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if is_sel && !kbd_active {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .hover(move |s| {
                                if kbd_active {
                                    s
                                } else {
                                    s.bg(palette.bg_item_hover)
                                }
                            })
                            .on_click(move |_, w, cx| {
                                ent.update(cx, |this, cx| {
                                    if let Some(CompletionPopupState::Files {
                                        ref mut selected_idx,
                                        ..
                                    }) = this.completion
                                    {
                                        *selected_idx = idx;
                                    }
                                    this.accept_selected_completion(w, cx);
                                });
                            })
                            .on_prepaint(move |bounds, _, _| tr.set(row_key.clone(), bounds))
                            .child(
                                Icon::new(IconName::FileText)
                                    .size(px(13.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(path.clone())),
                    );
                }
                pop = pop.child(scroll);
            }
        }

        pop = pop.child(render_gliding_dynamic_indicator(
            "composer-comp",
            kbd_style,
            active_spec,
            self.comp_tracker.clone(),
            palette,
            window,
            cx,
        ));

        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_full()
                .pb(px(2.0))
                .child(motion::animate_popover_enter(
                    "composer-comp-pop-anim",
                    pop,
                    palette.flat_popover_shadows(),
                    6.0,
                ))
                .into_any_element(),
        )
    }
}

impl Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reconcile_external_draft_changes(window, cx);
        if self.completion.is_some() && !self.completion_is_current(cx) {
            self.clear_completion();
        }

        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();
        let mono_family = theme_global.mono_font_family;

        let snap = self.client.snapshot();
        let kbd_style = snap.desktop_settings.keyboard_focus_style;
        let active_session = match &self.draft_key {
            DraftKey::Session(sid) => snap.loaded_sessions.get(sid).cloned(),
            DraftKey::NewSession => None,
        };
        let is_session_running = active_session.as_ref().is_some_and(|s| s.running);

        let draft_images = self.client.with_workspace(|ws| {
            ws.draft(&self.draft_key)
                .map(|d| d.images.clone())
                .unwrap_or_default()
        });

        let empty_models = cst_client::ModelsSnapshot::default();
        let models_ref = snap.models.as_deref().unwrap_or(&empty_models);
        let (cur_provider, cur_model_id, cur_model_label, cur_thinking) =
            self.current_model_choice(&snap);
        let scoped_models = resolve_scoped_models(models_ref);
        let (context_usage, usage_stats) = match &self.draft_key {
            DraftKey::Session(_) => (
                active_session
                    .as_ref()
                    .and_then(|s| s.context_usage.as_ref()),
                active_session.as_ref().and_then(|s| s.usage.as_ref()),
            ),
            DraftKey::NewSession => (None, None),
        };
        let percent_opt = context_usage
            .and_then(|u| u.get("percent"))
            .and_then(Value::as_f64);

        let is_focused = self
            .textarea
            .read(cx)
            .presentation()
            .focus_handle()
            .is_focused(window);

        let entity = cx.entity();

        // 1. Optional QueueBar (`rnehA`)
        let queue_element = if let DraftKey::Session(ref sid) = self.draft_key {
            let server_queue =
                self.queue_snapshot(sid, active_session.as_ref().and_then(|s| s.queue.as_ref()));
            let parsed_queue = self
                .queue_pending
                .as_ref()
                .filter(|(key, queue)| key == sid && queue.version == server_queue.version)
                .map_or(&server_queue, |(_, projected)| projected);
            let q1 = (entity.clone(), sid.clone());
            let q2 = (entity.clone(), sid.clone());
            let q3 = (entity.clone(), sid.clone());
            let q4 = (entity.clone(), sid.clone());
            let q_edit = (entity.clone(), sid.clone());
            let q_key = (entity.clone(), sid.clone(), parsed_queue.clone());
            let q_move = entity.clone();
            let queue_focused = self.queue_focus.is_focused(window);
            let queue_selected_id = self
                .queue_selected_id
                .clone()
                .filter(|id| parsed_queue.items.iter().any(|item| &item.id == id))
                .or_else(|| parsed_queue.items.first().map(|item| item.id.clone()));
            render_queue_bar(
                parsed_queue,
                sid,
                &palette,
                QueueBarKbdProps {
                    kbd_style,
                    kbd_active: queue_focused && self.queue_kbd_active,
                    selected_id: queue_selected_id,
                    action: self.queue_action,
                    tracker: self.queue_tracker.clone(),
                    focus_handle: self.queue_focus.clone(),
                    on_key_down: Rc::new(move |ev, w, cx| {
                        q_key.0.update(cx, |this, cx| {
                            this.handle_queue_key(&q_key.1, &q_key.2, ev, w, cx);
                        });
                    }),
                    on_mouse_move: Rc::new(move |ev, _w, cx| {
                        q_move.update(cx, |this, cx| {
                            if this.queue_kbd_active
                                && pointer_moved_past_threshold(
                                    &mut this.queue_last_pointer,
                                    ev.position,
                                )
                            {
                                this.queue_kbd_active = false;
                                cx.notify();
                            }
                        });
                    }),
                },
                window,
                cx,
                QueueBarCallbacks {
                    on_steer: Rc::new(move |id, version, w, cx| {
                        q1.0.update(cx, |this, cx| {
                            this.mutate_queue(
                                q1.1.clone(),
                                version,
                                QueueMutation::Steer(id),
                                w,
                                cx,
                            );
                        });
                    }),
                    on_retry: Rc::new(move |id, version, w, cx| {
                        q2.0.update(cx, |this, cx| {
                            this.mutate_queue(
                                q2.1.clone(),
                                version,
                                QueueMutation::Retry(id),
                                w,
                                cx,
                            );
                        });
                    }),
                    on_delete: Rc::new(move |id, version, w, cx| {
                        q3.0.update(cx, |this, cx| {
                            this.mutate_queue(
                                q3.1.clone(),
                                version,
                                QueueMutation::Delete(id),
                                w,
                                cx,
                            );
                        });
                    }),
                    on_reorder: Rc::new(move |version, ids, w, cx| {
                        q4.0.update(cx, |this, cx| {
                            this.mutate_queue(
                                q4.1.clone(),
                                version,
                                QueueMutation::Reorder(ids),
                                w,
                                cx,
                            );
                        });
                    }),
                    on_edit: Rc::new(move |item, version, _w, cx| {
                        let sid_for_edit = q_edit.1.clone();
                        q_edit.0.update(cx, |this, cx| {
                            if this.queue_pending.is_some()
                                || this.draft_key != DraftKey::Session(sid_for_edit.clone())
                            {
                                return;
                            }
                            cx.emit(ComposerHostEvent::RequestEditQueueItem {
                                session_id: sid_for_edit,
                                item,
                                version,
                            });
                        });
                    }),
                },
            )
        } else {
            None
        };

        // 2. Optional `80×64px` draft image thumbnails row
        let thumbs_element = (!draft_images.is_empty()).then(|| {
            let mut row = div()
                .id("composer-draft-thumbs-row")
                .test_support()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(space::SM);

            for (idx, handle) in draft_images.iter().enumerate() {
                let gpui_img = gpui_image_from_handle(handle);
                let title = SharedString::from(handle.file_name().unwrap_or("草稿附图").to_owned());
                let dims = Some(handle.dimensions());
                let ent_open = entity.clone();
                let ent_rm = entity.clone();
                let gpui_img_open = gpui_img.clone();

                let thumb_id = SharedString::from(format!("composer-draft-thumb-{idx}"));
                let thumb_rm_id = SharedString::from(format!("composer-draft-thumb-rm-{idx}"));
                let thumb_label = SharedString::from(format!("预览附图：{title}"));
                let thumb_rm_label = SharedString::from(format!("移除附图：{title}"));

                row = row.child(
                    div()
                        .id(thumb_id.clone())
                        .test_support()
                        .a11y_button(thumb_id, thumb_label)
                        .tab_index(0)
                        .relative()
                        .w(px(80.0))
                        .h(px(64.0))
                        .rounded(radius::CONTROL)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .bg(palette.bg_surface)
                        .overflow_hidden()
                        .on_click(move |_, w, cx| {
                            let prior = w.focused(cx);
                            let target = LightboxImageTarget {
                                title: title.clone(),
                                dimensions: dims,
                                image: gpui_img_open.clone(),
                                prior_focus: prior,
                            };
                            ent_open.update(cx, |_, cx| {
                                cx.emit(ComposerHostEvent::OpenLightbox(target));
                            });
                        })
                        .child(
                            img(ImageSource::Image(gpui_img))
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        )
                        .child(
                            div()
                                .id(thumb_rm_id.clone())
                                .test_support()
                                .a11y_button(thumb_rm_id, thumb_rm_label)
                                .tab_index(0)
                                .absolute()
                                .top(space::XS)
                                .right(space::XS)
                                .w(px(20.0))
                                .h(px(20.0))
                                .rounded(radius::FULL)
                                .bg(rgba(0x1111_14BF))
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(palette.status_danger))
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    ent_rm.update(cx, |this, cx| {
                                        this.remove_draft_image(idx, cx);
                                    });
                                })
                                .child(
                                    Icon::new(IconName::X)
                                        .size(px(11.0))
                                        .text_color(palette.text_on_accent),
                                ),
                        ),
                );
            }
            row
        });

        // 3. `Textarea` with `28px` `InlineToken` (`MkfFr`) renderer & clipboard image paste hook
        let has_inline_tokens = !self.textarea.read(cx).content().tokens().is_empty();
        let ent_tok = entity.clone();
        let ent_paste = entity.clone();
        let textarea_element = Textarea::new(&self.textarea)
            .accessibility_id("composer-input")
            .aria_label("对话输入框")
            .xsmall()
            .appearance(false)
            .bordered(false)
            .text_size(scale::BODY.font_size)
            // `gpui-base` clips custom inline tokens to `line_height`; raise to `32px` while tokens are
            // present so the `28px` `MkfFr` chip keeps crisp top/bottom borders, corners, and shadow.
            .line_height(if has_inline_tokens {
                px(32.0)
            } else {
                scale::BODY.line_height
            })
            .token(move |tok_ctx, _window, _cx| {
                let tok = tok_ctx.token();
                let label = tok.label().clone();
                let has_non_ascii = !label.is_ascii();
                let token_icon = icon_for_token_key(tok.id().as_ref());
                let span_range = tok_ctx.range();
                let ent_click = ent_tok.clone();
                let rm_id = SharedString::from(format!("inline-token-rm-{}", span_range.start));
                let rm_label = SharedString::from(format!("移除标记：{label}"));
                let chip_shadow = vec![gpui::BoxShadow {
                    color: if palette.mode.is_dark() {
                        rgba(0x0000_0038).into()
                    } else {
                        rgba(0x1A20_240D).into()
                    },
                    offset: gpui::point(px(0.0), px(1.0)),
                    blur_radius: px(2.0),
                    spread_radius: px(0.0),
                    inset: false,
                }];

                div()
                    .h(height::COMPACT)
                    .pl(space::XS)
                    .pr(space::SM)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_hairline)
                    .bg(palette.bg_special)
                    .shadow(chip_shadow)
                    .flex()
                    .items_center()
                    .gap(space::TIGHT)
                    .child(
                        div()
                            .w(px(20.0))
                            .h(px(20.0))
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_subtle)
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .child(
                                Icon::new(token_icon)
                                    .size(px(14.0))
                                    .text_color(palette.accent_ink),
                            ),
                    )
                    .child(if has_non_ascii {
                        apply_scale(div(), scale::UI_MEDIUM)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label(label))
                            .into_any_element()
                    } else {
                        apply_scale(div(), scale::UI_MEDIUM)
                            .text_color(palette.text_primary)
                            .child(label)
                            .into_any_element()
                    })
                    .child(
                        div()
                            .id(rm_id.clone())
                            .test_support()
                            .a11y_button(rm_id, rm_label)
                            .w(px(18.0))
                            .h(px(18.0))
                            .ml(px(-2.0))
                            .mr(px(-4.0))
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(move |_, w, cx| {
                                cx.stop_propagation();
                                ent_click.update(cx, |this, cx| {
                                    this.remove_inline_token_span(span_range.clone(), w, cx);
                                });
                            })
                            .child(
                                Icon::new(IconName::X)
                                    .size(px(11.0))
                                    .text_color(palette.text_muted),
                            ),
                    )
            })
            .on_paste(move |clip: &ClipboardItem, _window, cx| {
                let mut consumed = false;
                let mut pasted_images: Vec<Vec<u8>> = Vec::new();
                let mut pasted_paths: Vec<PathBuf> = Vec::new();
                for entry in clip.entries() {
                    match entry {
                        ClipboardEntry::Image(img) => {
                            pasted_images.push(img.bytes().to_vec());
                            consumed = true;
                        }
                        ClipboardEntry::ExternalPaths(ext) => {
                            let img_paths: Vec<PathBuf> = ext
                                .paths()
                                .iter()
                                .filter(|p| {
                                    p.extension().and_then(|e| e.to_str()).is_some_and(|ext| {
                                        matches!(
                                            ext.to_ascii_lowercase().as_str(),
                                            "png" | "jpg" | "jpeg" | "gif" | "webp"
                                        )
                                    })
                                })
                                .cloned()
                                .collect();
                            if !img_paths.is_empty() {
                                pasted_paths.extend(img_paths);
                                consumed = true;
                            }
                        }
                        _ => {}
                    }
                }
                if consumed {
                    ent_paste.update(cx, |this, cx| {
                        for bytes in pasted_images {
                            this.attach_image_bytes(bytes, Some("clipboard.png".into()), cx);
                        }
                        if !pasted_paths.is_empty() {
                            this.attach_image_paths(pasted_paths, cx);
                        }
                    });
                }
                consumed
            });

        // 4. Bottom `28px` action bar (`QG05v`)
        self.footer_tracker.clear();
        let tr_attach = self.footer_tracker.clone();
        let tr_ctx = self.footer_tracker.clone();
        let tr_model = self.footer_tracker.clone();
        let tr_send = self.footer_tracker.clone();
        let ent_attach = entity.clone();
        let (attach_pressed, attach_press) =
            motion::icon_press_state("composer-attach-btn", window, cx);
        let attach_btn = div()
            .id("composer-attach-btn")
            .test_support()
            .a11y_button("composer-attach-btn", "添加图片附件")
            .desktop_tooltip("composer-attach-btn", "添加图片附件")
            .track_focus(&self.attach_btn_focus)
            .tab_index(0)
            .w(height::COMPACT)
            .h(height::COMPACT)
            .rounded(radius::CONTROL)
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(palette.bg_item_hover))
            .on_icon_press(attach_press)
            .on_click(move |_, _, cx| {
                ent_attach.update(cx, |this, cx| this.open_attach_dialog(cx));
            })
            .on_prepaint(move |bounds, _, _| tr_attach.set("footer-0", bounds))
            .child(motion::animated_icon_press(
                "composer-attach-btn",
                attach_pressed,
                Icon::new(IconName::Plus)
                    .size(px(16.0))
                    .text_color(palette.text_secondary),
                window,
                cx,
            ));

        let ent_ctx_btn = entity.clone();
        let prov_for_ctx = cur_provider.clone();
        let context_ring = div()
            .on_prepaint(move |bounds, _, _| tr_ctx.set("footer-1", bounds))
            .child(render_context_ring_button(
                percent_opt,
                &self.context_btn_focus,
                &palette,
                move |w, cx| {
                    let p = prov_for_ctx.clone();
                    ent_ctx_btn.update(cx, |this, cx| {
                        this.toggle_context_popover(p, w, cx);
                    });
                },
            ));

        let ent_picker_btn = entity.clone();
        let model_btn = apply_scale(div(), scale::CAPTION)
            .id("composer-model-picker-btn")
            .test_support()
            .a11y_toggle_button(
                "composer-model-picker-btn",
                format!(
                    "模型与思考强度：{} {}",
                    cur_model_label,
                    thinking_level_label(&cur_thinking)
                ),
                self.picker_open,
            )
            .track_focus(&self.model_btn_focus)
            .tab_index(0)
            .h(height::COMPACT)
            .px(space::TIGHT)
            .rounded(radius::CONTROL)
            .flex()
            .items_center()
            .gap(space::XS)
            .hover(|s| s.bg(palette.bg_item_hover))
            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(move |_, w, cx| {
                ent_picker_btn.update(cx, |this, cx| {
                    this.toggle_model_picker(false, w, cx);
                });
            })
            .on_prepaint(move |bounds, _, _| tr_model.set("footer-2", bounds))
            .child(
                div()
                    .max_w(px(148.0))
                    .truncate()
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(cur_model_label.clone())),
            )
            .child(
                div()
                    .text_color(palette.text_secondary)
                    .child(cjk_inline_label(thinking_level_label(&cur_thinking))),
            )
            .child(motion::animated_chevron(
                format!(
                    "composer-model-{}",
                    match &self.draft_key {
                        DraftKey::NewSession => "home",
                        DraftKey::Session(s) => s.as_str(),
                    }
                ),
                self.picker_open,
                true,
                px(12.0),
                palette.text_secondary,
                window,
                cx,
            ));

        // Unified `28×28px` primary button (`composer-send-btn-b0mrik`, `OQrq0`):
        // when session is running AND draft has neither text/tokens nor attached images, displays `12px` white `Square` to abort;
        // otherwise displays `14px` white `ArrowUp` to send/enqueue.
        let ta_content = self.textarea.read(cx).content();
        let has_draft_content = !ta_content.text().trim().is_empty()
            || !ta_content.tokens().is_empty()
            || !draft_images.is_empty();
        let abort_session_id = match (&self.draft_key, is_session_running && !has_draft_content) {
            (DraftKey::Session(sid), true) => Some(sid.clone()),
            _ => None,
        };
        let abort_session_id_for_key = abort_session_id.clone();
        let is_abort_mode = abort_session_id.is_some();

        let ent_send = entity.clone();
        let (send_pressed, send_press) =
            motion::icon_press_state("composer-send-btn-b0mrik", window, cx);
        let send_label = if is_abort_mode {
            "停止生成"
        } else {
            "发送消息"
        };
        let send_btn = div()
            .id("composer-send-btn-b0mrik")
            .test_support()
            .a11y_button("composer-send-btn-b0mrik", send_label)
            .desktop_tooltip("composer-send-btn-b0mrik", send_label)
            .track_focus(&self.send_btn_focus)
            .tab_index(0)
            .w(height::COMPACT)
            .h(height::COMPACT)
            .rounded(radius::CONTROL)
            .bg(palette.accent_solid)
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(palette.accent_hover))
            .on_icon_press(send_press)
            .on_click(move |_, w, cx| {
                if let Some(ref sid_abort) = abort_session_id {
                    let s = sid_abort.clone();
                    ent_send.update(cx, |this, cx| {
                        let c = this.client.clone();
                        cx.spawn(async move |weak, cx| {
                            if let Err(e) = c.abort_session(s).await {
                                let _ = weak.update(cx, |_, cx| {
                                    cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                });
                            }
                        })
                        .detach();
                    });
                } else {
                    ent_send.update(cx, |this, cx| this.trigger_submit_from_button(w, cx));
                }
            })
            .on_prepaint(move |bounds, _, _| tr_send.set("footer-3", bounds))
            .child(motion::animated_icon_press(
                "composer-send-btn-b0mrik",
                send_pressed,
                if is_abort_mode {
                    Icon::new(IconName::Square)
                        .size(px(12.0))
                        .text_color(palette.text_on_accent)
                } else {
                    Icon::new(IconName::ArrowUp)
                        .size(px(14.0))
                        .text_color(palette.text_on_accent)
                },
                window,
                cx,
            ));

        let ctx_open = self.context_popover_open;
        let ctx_progress = self
            .context_presence
            .sample(
                if ctx_open { 1.0_f32 } else { 0.0 },
                Transition::new(if ctx_open {
                    motion::POPOVER_OPEN
                } else {
                    motion::SURFACE_CLOSE
                })
                .ease(motion::ease_out()),
                window,
                cx,
            )
            .value
            .clamp(0.0, 1.0);
        let ctx_popover_opt = (ctx_open || ctx_progress > 0.001).then(|| {
            let ent_retry = entity.clone();
            let ent_acc = entity.clone();
            let prov_retry = cur_provider.clone();
            div()
                .absolute()
                .right(px(36.0))
                .bottom_full()
                .pb(space::SM)
                .child(render_context_quota_popover(
                    ctx_open,
                    ctx_progress,
                    &cur_provider,
                    context_usage,
                    usage_stats,
                    self.cached_quota.as_ref().filter(|_| {
                        self.cached_quota_provider.as_deref() == Some(cur_provider.as_str())
                    }),
                    self.quota_loading
                        && self.cached_quota_provider.as_deref() == Some(cur_provider.as_str()),
                    mono_family.clone(),
                    &palette,
                    transparency_enabled,
                    scale_factor,
                    Rc::new(move |_, cx| {
                        let p = prov_retry.clone();
                        ent_retry.update(cx, |this, cx| {
                            this.refresh_quota_for_provider(p, true, cx);
                        });
                    }),
                    Rc::new(move |_, cx| {
                        ent_acc.update(cx, |this, cx| {
                            this.context_popover_open = false;
                            this.context_prior_focus = None;
                            cx.emit(ComposerHostEvent::OpenAccountPage);
                            cx.notify();
                        });
                    }),
                ))
        });

        let picker_open = self.picker_open;
        let picker_progress = self
            .picker_presence
            .sample(
                if picker_open { 1.0_f32 } else { 0.0 },
                Transition::new(if picker_open {
                    motion::POPOVER_OPEN
                } else {
                    motion::SURFACE_CLOSE
                })
                .ease(motion::ease_out()),
                window,
                cx,
            )
            .value
            .clamp(0.0, 1.0);
        let picker_popover_opt = (picker_open || picker_progress > 0.001).then(|| {
            let ent_page = entity.clone();
            let ent_sel_m = entity.clone();
            let ent_sel_t = entity.clone();
            let ent_move_p = entity.clone();
            let levels = active_thinking_levels(models_ref, &cur_provider, &cur_model_id);
            div()
                .absolute()
                .right_0()
                .bottom_full()
                .pb(space::SM)
                .flex()
                .flex_col()
                .items_end()
                .justify_end()
                .child(render_model_picker_popover(
                    picker_open,
                    picker_progress,
                    self.picker_open_epoch,
                    self.picker_page,
                    self.picker_hi_idx,
                    kbd_style,
                    self.picker_kbd_active,
                    self.picker_tracker.clone(),
                    &self.picker_scroll,
                    &scoped_models,
                    &cur_provider,
                    &cur_model_id,
                    &cur_model_label,
                    levels,
                    &cur_thinking,
                    &palette,
                    transparency_enabled,
                    scale_factor,
                    Rc::new(move |next_page, _, cx| {
                        ent_page.update(cx, |this, cx| {
                            this.set_picker_page(next_page);
                            cx.notify();
                        });
                    }),
                    Rc::new(move |provider, model_id, _, cx| {
                        ent_sel_m.update(cx, |this, cx| this.select_model(provider, model_id, cx));
                    }),
                    Rc::new(move |level, w, cx| {
                        ent_sel_t.update(cx, |this, cx| this.select_thinking(level, w, cx));
                    }),
                    Rc::new(move |ev, _w, cx| {
                        ent_move_p.update(cx, |this, cx| {
                            if this.picker_kbd_active
                                && pointer_moved_past_threshold(
                                    &mut this.picker_last_pointer,
                                    ev.position,
                                )
                            {
                                this.picker_kbd_active = false;
                                cx.notify();
                            }
                        });
                    }),
                    window,
                    cx,
                ))
        });

        let right_cluster = div()
            .relative()
            .flex()
            .items_center()
            .gap(space::SM)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child(context_ring)
                    .child(
                        div()
                            .relative()
                            .child(model_btn)
                            .children(picker_popover_opt),
                    ),
            )
            .child(send_btn)
            .children(ctx_popover_opt);

        let footer_slot =
            (!self.picker_open && !self.context_popover_open && self.footer_kbd_active)
                .then(|| self.footer_focused_slot(window))
                .flatten();
        let footer_spec = footer_slot.map(|slot| {
            let tone = if slot == 3 {
                KbdTone::Accent
            } else {
                KbdTone::Neutral
            };
            KbdDynamicSpec::control(format!("footer-{slot}"), KbdRailPlacement::BottomCenter)
                .with_tone(tone)
        });
        let footer_prov = cur_provider.clone();
        let action_bar = div()
            .relative()
            .h(height::COMPACT)
            .flex()
            .items_center()
            .justify_between()
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, w, cx| {
                this.handle_footer_key(
                    footer_prov.clone(),
                    abort_session_id_for_key.clone(),
                    ev,
                    w,
                    cx,
                );
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.footer_kbd_active
                    && pointer_moved_past_threshold(&mut this.footer_last_pointer, ev.position)
                {
                    this.footer_kbd_active = false;
                    cx.notify();
                }
            }))
            .child(attach_btn)
            .child(right_cluster)
            .child(render_gliding_dynamic_indicator(
                "composer-footer",
                kbd_style,
                footer_spec,
                self.footer_tracker.clone(),
                &palette,
                window,
                cx,
            ));

        let focus_t =
            motion::animated_state_progress("composer-card-focus", is_focused, window, cx);
        let card_border_color =
            motion::lerp_hsla(palette.border_strong, palette.border_focus, focus_t);
        let card_shadows = motion::lerp_shadows(
            &palette.composer_shadows(false),
            &palette.composer_shadows(true),
            focus_t,
        );
        #[cfg(feature = "test-support")]
        if crate::ux_probe::is_enabled() {
            let rgba_val = card_border_color.to_rgb();
            crate::ux_probe::record_composer_style(
                is_focused,
                [rgba_val.r, rgba_val.g, rgba_val.b, rgba_val.a],
                card_shadows.len(),
            );
        }

        let ent_drop = entity.clone();
        let mut card = div()
            .id("composer-card-zwas1")
            .test_support()
            .a11y_region("composer-card-zwas1", "消息输入卡片")
            .w_full()
            .min_h(px(88.0))
            .p(space::MD)
            .rounded(radius::ROW)
            .border_1()
            .border_color(card_border_color)
            .bg(palette.bg_workspace)
            .shadow(card_shadows)
            .flex()
            .flex_col()
            .justify_between()
            .gap(space::MD)
            .on_action(
                cx.listener(|this, _: &crate::shell::DismissTopSurface, window, cx| {
                    // `InputBaseState::escape` unmarks active IME composition BEFORE propagating `Escape`;
                    // consume that `Escape` here so cancelling IME pinyin never closes popovers or modals!
                    if this.had_active_ime_mark || this.is_ime_composing(window, cx) {
                        this.had_active_ime_mark = false;
                        return;
                    }
                    if !this.dismiss_topmost_popover(window, cx) {
                        cx.propagate();
                    }
                }),
            )
            // Bound actions run before raw key-down. Capture at the card so the
            // input never sees a completion/submit gesture that has been handled.
            .capture_action(cx.listener(|this, action: &Enter, w, cx| {
                if !this.focus_handle(cx).is_focused(w) {
                    return;
                }
                if this.is_ime_composing(w, cx) {
                    cx.stop_propagation();
                    return;
                }
                if action.shift && !action.secondary {
                    return; // Let Textarea insert the line break.
                }
                cx.stop_propagation();
                if !action.secondary && this.completion.is_some() && this.completion_is_current(cx)
                {
                    this.accept_selected_completion(w, cx);
                } else {
                    this.trigger_submit(w, cx);
                }
            }))
            .capture_action(
                cx.listener(|this, _: &MoveUp, w, cx| this.move_completion(false, w, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &MoveDown, w, cx| this.move_completion(true, w, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &IndentInline, w, cx| this.complete_on_tab(w, cx)),
            )
            .capture_action(cx.listener(|this, _: &OutdentInline, w, cx| {
                if !this.focus_handle(cx).is_focused(w) || !this.is_ime_composing(w, cx) {
                    if this.clear_completion() {
                        cx.notify();
                    }
                } else {
                    cx.stop_propagation();
                }
            }))
            .capture_key_down(cx.listener(Self::handle_picker_key))
            .on_drop(move |paths: &ExternalPaths, _, cx| {
                let files: Vec<PathBuf> = paths.paths().to_vec();
                ent_drop.update(cx, |this, cx| {
                    this.attach_image_paths(files, cx);
                });
            });

        if let Some(q) = queue_element {
            card = card.child(q);
        }
        if let Some(th) = thumbs_element {
            card = card.child(th);
        }
        card = card
            .child(
                div()
                    .w_full()
                    .min_h(px(24.0))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(textarea_element),
            )
            .child(action_bar);
        #[cfg(feature = "test-support")]
        if let Some(layer) = crate::ux_probe::bounds_probe_layer("composer", None) {
            card = card.relative().child(layer);
        }

        // Outer relative wrapper (`card` first, upward slash/file completion popover `p2biT` painted above `card`;
        // `d8zzg` and `h5jG6d` anchor directly above their triggers inside `action_bar`).
        let mut wrapper = div().relative().w_full().child(card);

        if let Some(comp_pop) = self.render_completion_popover(
            mono_family,
            &palette,
            kbd_style,
            transparency_enabled,
            scale_factor,
            &entity,
            window,
            cx,
        ) {
            wrapper = wrapper.child(comp_pop);
        }

        wrapper
    }
}

fn read_attachment_file(path: &std::path::Path) -> Result<Vec<u8>, ClientError> {
    const LIMIT: usize = cst_client::workspace::MAX_SUBMISSION_IMAGE_BYTES;
    let file = std::fs::File::open(path)
        .map_err(|e| ClientError::InvalidImage(format!("无法读取图片文件：{e}")))?;
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ClientError::InvalidImage(format!("无法读取图片字节：{e}")))?;
    if bytes.len() > LIMIT {
        return Err(ClientError::ImageBudgetExceeded(
            "单张图片原始体积超过 12 MiB 上限。".into(),
        ));
    }
    Ok(bytes)
}

fn build_input_content_from_draft(draft: &ComposerDraft) -> InputContent {
    let mut full = String::new();
    let mut pending_spans: Vec<(Range<usize>, UiInlineToken)> = Vec::new();

    for tok in &draft.tokens {
        match tok {
            DraftToken::Command { raw, name } => {
                let start = full.len();
                let tok_str = if raw.is_empty() {
                    name.as_str()
                } else {
                    raw.as_str()
                };
                full.push_str(tok_str);
                let end = full.len();
                let short = name
                    .strip_prefix("/skill:")
                    .or_else(|| name.strip_prefix('/'))
                    .unwrap_or(name.as_str());
                pending_spans.push((
                    start..end,
                    UiInlineToken::new(name.clone(), tok_str.to_owned())
                        .with_label(short.to_owned()),
                ));
                full.push(' ');
            }
            DraftToken::FileRef { path, display_name } => {
                let start = full.len();
                let tok_str = format!("@{path}");
                full.push_str(&tok_str);
                let end = full.len();
                pending_spans.push((
                    start..end,
                    UiInlineToken::new(format!("file:{path}"), tok_str)
                        .with_label(display_name.clone()),
                ));
                full.push(' ');
            }
        }
    }

    full.push_str(&draft.text);

    let mut content = InputContent::new(full);
    for (range, ui_tok) in pending_spans {
        if let Ok(next) = content.clone().with_token(range, ui_tok) {
            content = next;
        }
    }
    content
}

fn extract_draft_text_and_tokens(content: &InputContent) -> (String, Vec<DraftToken>) {
    let raw = content.text().as_ref();
    let spans = content.tokens();
    if spans.is_empty() {
        return (raw.to_owned(), Vec::new());
    }

    let mut tokens = Vec::with_capacity(spans.len());
    let mut plain = String::with_capacity(raw.len());
    let mut cursor = 0usize;

    for span in spans {
        let r = span.range();
        if r.start > cursor && r.start <= raw.len() {
            plain.push_str(&raw[cursor..r.start]);
        }
        let ui_tok = span.token();
        let id_str = ui_tok.id().as_ref();
        if let Some(file_path) = id_str.strip_prefix("file:") {
            tokens.push(DraftToken::FileRef {
                path: file_path.to_owned(),
                display_name: ui_tok.label().to_string(),
            });
        } else {
            tokens.push(DraftToken::Command {
                raw: ui_tok.text().to_string(),
                name: id_str.to_owned(),
            });
        }
        cursor = r.end.min(raw.len());
        if raw[cursor..].starts_with(' ') {
            cursor += 1;
        }
    }
    if cursor < raw.len() {
        plain.push_str(&raw[cursor..]);
    }
    (plain, tokens)
}

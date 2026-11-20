use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use cst_client::{ClientError, ClientHandle, ComposerDraft, DraftKey, InlineToken as DraftToken};
use gpui::{
    ClipboardEntry, ClipboardItem, EntityInputHandler, ExternalPaths, ImageSource, KeyDownEvent,
    ObjectFit, PathPromptOptions, StyledImage as _,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{
    Enter, IndentInline, InlineToken as UiInlineToken, InputContent, InputEvent, InputState,
    MoveDown, MoveUp, OutdentInline, Textarea, TextareaState,
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
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, radius, scale, space,
};
use crate::workbench::image_viewer::{LightboxImageTarget, gpui_image_from_handle};
use crate::workbench::model_picker::{
    ModelPickerPage, is_quota_provider, render_context_quota_popover, render_context_ring_button,
    render_model_picker_popover, resolve_scoped_models, thinking_level_label,
};
use crate::workbench::queue_bar::{
    ParsedQueueSnapshot, QueueBarCallbacks, QueueMutation, render_queue_bar,
};

#[derive(Debug, Clone)]
pub enum ComposerHostEvent {
    SubmittedToSession { session_id: String },
    MissingCredentials { provider: String },
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
    picker_page: ModelPickerPage,
    picker_hi_idx: usize,
    picker_scroll: ScrollHandle,
    picker_prior_focus: Option<FocusHandle>,
    context_popover_open: bool,
    context_prior_focus: Option<FocusHandle>,
    cached_quota_provider: Option<String>,
    cached_quota: Option<Value>,
    quota_task: Option<Task<()>>,
    quota_loading: bool,
    queue_pending: Option<(String, ParsedQueueSnapshot)>,
    queue_receipt: Option<(String, ParsedQueueSnapshot)>,
    editing_queue_item_id: Option<String>,
    queue_edit_input: Option<Entity<InputState>>,
    model_btn_focus: FocusHandle,
    context_btn_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerHostEvent> for ComposerView {}

impl Focusable for ComposerView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.textarea.read(cx).presentation().focus_handle().clone()
    }
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

        let model_btn_focus = cx.focus_handle().tab_stop(true);
        let context_btn_focus = cx.focus_handle().tab_stop(true);

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
            picker_page: ModelPickerPage::Main,
            picker_hi_idx: 0,
            picker_scroll: ScrollHandle::new(),
            picker_prior_focus: None,
            context_popover_open: false,
            context_prior_focus: None,
            cached_quota_provider: None,
            cached_quota: None,
            quota_task: None,
            quota_loading: false,
            queue_pending: None,
            queue_receipt: None,
            editing_queue_item_id: None,
            queue_edit_input: None,
            model_btn_focus,
            context_btn_focus,
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
        self.editing_queue_item_id = None;
        self.queue_edit_input = None;
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

    /// Closes the single topmost open composer popover (`completion` / `picker` / `context`) and restores focus.
    pub fn dismiss_topmost_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.clear_completion() {
            cx.notify();
            return true;
        }
        if self.picker_open {
            if self.picker_page != ModelPickerPage::Main {
                self.set_picker_page(ModelPickerPage::Main);
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
        if self.textarea.read(cx).content() != content {
            self.clear_completion();
            self.textarea.update(cx, |st, cx| {
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
                cx.notify();
            }
        }
    }

    fn clear_completion(&mut self) -> bool {
        self.completion_source = None;
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
            cx.propagate();
            return;
        }
        let Some(comp) = &mut self.completion else {
            cx.propagate();
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
        self.completion_scroll
            .scroll_to_item(*index + usize::from(separator.is_some_and(|i| *index >= i)));
        cx.stop_propagation();
        cx.notify();
    }

    fn complete_on_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle(cx).is_focused(window) {
            cx.propagate();
            return;
        }
        cx.stop_propagation();
        if self.is_ime_composing(window, cx) {
            return;
        }
        if self.completion.is_some() && self.completion_is_current(cx) {
            self.accept_selected_completion(window, cx);
            return;
        }

        self.clear_completion();
        self.close_picker();
        self.context_popover_open = false;
        self.context_prior_focus = None;
        let st = self.textarea.read(cx);
        let pos = st.cursor();
        self.completion_source = Some((st.content(), pos));
        self.completion_scroll = ScrollHandle::new();
        self.completion = Some(CompletionPopupState::Slash {
            replace_range: pos..pos,
            items: SLASH_CATALOG.to_vec(),
            selected_idx: 0,
        });
        cx.notify();
    }

    fn close_picker(&mut self) -> Option<FocusHandle> {
        self.picker_open = false;
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
        if !self.picker_open
            || !self.model_btn_focus.is_focused(window)
            || event.keystroke.modifiers.number_of_modifiers() != 0
        {
            return;
        }
        let snap = self.client.snapshot();
        let empty_models = cst_client::ModelsSnapshot::default();
        let models = snap.models.as_deref().unwrap_or(&empty_models);
        let scoped = resolve_scoped_models(models);
        let levels = &models.thinking_levels;
        let count = match self.picker_page {
            ModelPickerPage::Main => 2,
            ModelPickerPage::Models => scoped.len(),
            ModelPickerPage::Thinking => levels.len(),
        };
        let index = self.picker_hi_idx.min(count.saturating_sub(1));
        match event.keystroke.key.as_str() {
            "down" if count > 0 => self.picker_hi_idx = (index + 1) % count,
            "up" if count > 0 => self.picker_hi_idx = (index + count - 1) % count,
            "home" if count > 0 => self.picker_hi_idx = 0,
            "end" if count > 0 => self.picker_hi_idx = count - 1,
            "down" | "up" | "home" | "end" => {}
            "enter" if !event.is_held => match self.picker_page {
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
            },
            _ => return,
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
                let _ = client.refresh_models(sid).await;
            }
            let _ = weak.update(cx, |this, cx| {
                this.model_change_pending = false;
                match res {
                    Ok(_) => {
                        this.client.with_workspace_mut(|ws| {
                            let draft = ws.draft_mut(key);
                            draft.selected_provider = Some(provider);
                            draft.selected_model_id = Some(model_id);
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
        self.close_picker()
            .unwrap_or_else(|| self.model_btn_focus.clone())
            .focus(window, cx);
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
                        let meta = std::fs::metadata(&path).map_err(|e| {
                            ClientError::InvalidImage(format!("无法读取图片文件：{e}"))
                        })?;
                        if meta.len() > cst_client::workspace::MAX_SUBMISSION_IMAGE_BYTES as u64 {
                            return Err(ClientError::ImageBudgetExceeded(
                                "单张图片原始体积超过 12 MiB 上限。".into(),
                            ));
                        }
                        let bytes = std::fs::read(&path).map_err(|e| {
                            ClientError::InvalidImage(format!("无法读取图片字节：{e}"))
                        })?;
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

    pub fn trigger_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting.is_some()
            || self.model_change_pending
            || self.is_ime_composing(window, cx)
        {
            return;
        }
        self.clear_completion();
        self.push_to_workspace(cx);

        // Handle `/compact` command token inside an active session directly
        let is_compact_only = self.client.with_workspace(|ws| {
            ws.draft(&self.draft_key).is_some_and(|d| {
                d.text.trim().is_empty()
                    && d.images.is_empty()
                    && matches!(d.tokens.as_slice(), [DraftToken::Command { name, .. }] if name == "/compact")
            })
        });
        if is_compact_only && let DraftKey::Session(sid) = self.draft_key.clone() {
            let key = self.draft_key.clone();
            self.submitting = Some(key.clone());
            let draft = self.client.with_workspace_mut(|ws| ws.take_draft(&key));
            self.pull_from_workspace(window, cx);
            let client = self.client.clone();
            cx.spawn(async move |weak, cx| {
                let res = client.compact_session(sid, None).await;
                let _ = weak.update(cx, |this, cx| {
                    this.submitting = None;
                    match res {
                        Ok(_) => cx.emit(ComposerHostEvent::TopNotice(
                            "已完成会话上下文压缩。".into(),
                        )),
                        Err(e) => {
                            this.client.with_workspace_mut(|ws| {
                                ws.restore_draft_if_empty(key, draft);
                            });
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
            cx.notify();
            return;
        }
        let edit_id = match &mutation {
            QueueMutation::Edit(id, _) => Some(id.clone()),
            _ => None,
        };
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
                        if this.draft_key == DraftKey::Session(sid)
                            && edit_id.is_some()
                            && this.editing_queue_item_id == edit_id
                        {
                            this.close_queue_editor(window, cx);
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

    fn close_queue_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.queue_edit_input.take()
            && input
                .read(cx)
                .presentation()
                .focus_handle()
                .is_focused(window)
        {
            self.focus_handle(cx).focus(window, cx);
        }
        self.editing_queue_item_id = None;
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
        let model_label = models_ref
            .catalog
            .iter()
            .find(|m| m.provider == provider && m.id == model_id)
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
            .or_else(|| base_model.and_then(|m| m.thinking_level.clone()))
            .unwrap_or_else(|| "medium".to_owned());

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

    fn render_completion_popover(
        &self,
        mono_family: SharedString,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        entity: &Entity<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let comp = self.completion.as_ref()?;
        let mut pop = render_frosted_surface(
            "composer-completion-popover-p2bit",
            FrostedGlassSpec::popover_h2p2z(palette),
            palette.bg_elevated,
            transparency_enabled,
            scale_factor,
        )
        .test_support()
        .occlude()
        .w_full()
        .p(space::XS)
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_hairline)
        .flex()
        .flex_col()
        .gap(space::MICRO);

        let header_title = match comp {
            CompletionPopupState::Slash { .. } => "诊断技能与斜杠命令",
            CompletionPopupState::Files { .. } => "工作区文件引用",
        };
        pop = pop.child(
            apply_scale(div(), scale::CAPTION)
                .relative()
                .w_full()
                .h(px(22.0))
                .px(space::SM)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .child(
                    div()
                        .text_color(palette.text_secondary)
                        .child(cjk_inline_label(header_title)),
                )
                .child(
                    div()
                        .text_color(palette.text_muted)
                        .child(cjk_inline_label("↑↓ 选择 · Tab / Enter 插入 · Esc 关闭")),
                ),
        );

        match comp {
            CompletionPopupState::Slash {
                items,
                selected_idx,
                ..
            } => {
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
                    let icon = icon_for_token_key(item.short_label);
                    let slash_id = SharedString::from(format!("slash-item-{}", item.short_label));
                    let slash_label =
                        SharedString::from(format!("{} {}", item.command, item.description));
                    scroll = scroll.child(
                        apply_scale(div(), scale::UI)
                            .id(slash_id.clone())
                            .test_support()
                            .a11y_listitem(slash_id, slash_label, is_sel)
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if is_sel {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::MD)
                            .hover(|s| s.bg(palette.bg_item_active))
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
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(space::SM)
                                    .child(
                                        div()
                                            .w(px(20.0))
                                            .h(px(20.0))
                                            .rounded(px(4.0))
                                            .bg(palette.accent_subtle)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .flex_shrink_0()
                                            .child(
                                                Icon::new(icon)
                                                    .size(px(13.0))
                                                    .text_color(palette.accent_ink),
                                            ),
                                    )
                                    .child(
                                        apply_scale(div(), scale::CAPTION_MEDIUM)
                                            .font_family(mono_family.clone())
                                            .text_color(palette.text_primary)
                                            .child(item.command),
                                    ),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_secondary)
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
                    let file_id = SharedString::from(format!("file-completion-item-{idx}"));
                    scroll = scroll.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(file_id.clone())
                            .test_support()
                            .a11y_listitem(file_id, path.clone(), is_sel)
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if is_sel {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .hover(|s| s.bg(palette.bg_item_active))
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

        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_full()
                .pb(space::SM)
                .child(pop)
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
        let context_usage = active_session
            .as_ref()
            .and_then(|s| s.context_usage.as_ref())
            .or(models_ref.context_usage.as_ref());
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
            let q_save = (entity.clone(), sid.clone());
            let q_cancel = entity.clone();
            render_queue_bar(
                parsed_queue,
                self.editing_queue_item_id.as_deref(),
                self.queue_edit_input.as_ref(),
                &palette,
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
                    on_begin_edit: Rc::new(move |id, text, w, cx| {
                        q_edit.0.update(cx, |this, cx| {
                            if this.queue_pending.is_some()
                                || this.draft_key != DraftKey::Session(q_edit.1.clone())
                            {
                                return;
                            }
                            let input = cx.new(|cx| {
                                let mut state = InputState::new(w, cx);
                                state.set_value(text, w, cx);
                                state.focus(w, cx);
                                state
                            });
                            this.editing_queue_item_id = Some(id);
                            this.queue_edit_input = Some(input);
                            cx.notify();
                        });
                    }),
                    on_commit_edit: Rc::new(move |id, version, w, cx| {
                        q_save.0.update(cx, |this, cx| {
                            if this.editing_queue_item_id.as_deref() != Some(id.as_str()) {
                                return;
                            }
                            let Some(input) = &this.queue_edit_input else {
                                return;
                            };
                            let composing = input.update(cx, |st, cx| {
                                EntityInputHandler::marked_text_range(st, w, cx).is_some()
                            });
                            if !composing {
                                let text = input.read(cx).value().to_string();
                                this.mutate_queue(
                                    q_save.1.clone(),
                                    version,
                                    QueueMutation::Edit(id, text),
                                    w,
                                    cx,
                                );
                            }
                        });
                    }),
                    on_cancel_edit: Rc::new(move |w, cx| {
                        q_cancel.update(cx, |this, cx| this.close_queue_editor(w, cx));
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
        let ent_tok = entity.clone();
        let ent_paste = entity.clone();
        let textarea_element = Textarea::new(&self.textarea)
            .accessibility_id("composer-input")
            .aria_label("对话输入框")
            .xsmall()
            .appearance(false)
            .bordered(false)
            .text_size(scale::BODY.font_size)
            .line_height(scale::BODY.line_height)
            .token(move |tok_ctx, _window, _cx| {
                let tok = tok_ctx.token();
                let label = tok.label().clone();
                let token_icon = icon_for_token_key(tok.id().as_ref());
                let span_range = tok_ctx.range();
                let ent_click = ent_tok.clone();
                let rm_id = SharedString::from(format!("inline-token-rm-{}", span_range.start));
                let rm_label = SharedString::from(format!("移除标记：{label}"));

                div()
                    .h(height::COMPACT)
                    .pl(space::XS)
                    .pr(space::SM)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_hairline)
                    .bg(palette.bg_special)
                    .flex()
                    .items_center()
                    .gap(space::TIGHT)
                    .child(
                        div()
                            .w(px(20.0))
                            .h(px(20.0))
                            .rounded(px(4.0))
                            .bg(palette.accent_subtle)
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .child(
                                Icon::new(token_icon)
                                    .size(px(13.0))
                                    .text_color(palette.accent_ink),
                            ),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label(label)),
                    )
                    .child(
                        div()
                            .id(rm_id.clone())
                            .test_support()
                            .a11y_button(rm_id, rm_label)
                            .w(px(18.0))
                            .h(px(18.0))
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
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
        let ent_attach = entity.clone();
        let attach_btn = div()
            .id("composer-attach-btn")
            .test_support()
            .a11y_button("composer-attach-btn", "添加图片附件")
            .w(height::COMPACT)
            .h(height::COMPACT)
            .rounded(radius::CONTROL)
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(palette.bg_item_hover))
            .on_click(move |_, _, cx| {
                let rx = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: true,
                    prompt: Some(SharedString::from("选择图片附件")),
                });
                let ent = ent_attach.clone();
                cx.spawn(async move |cx| {
                    if let Ok(Ok(Some(paths))) = rx.await {
                        ent.update(cx, |this, cx| {
                            this.attach_image_paths(paths, cx);
                        });
                    }
                })
                .detach();
            })
            .child(
                Icon::new(IconName::Plus)
                    .size(px(16.0))
                    .text_color(palette.text_secondary),
            );

        let ent_ctx_btn = entity.clone();
        let prov_for_ctx = cur_provider.clone();
        let context_ring = render_context_ring_button(
            percent_opt,
            &self.context_btn_focus,
            &palette,
            move |w, cx| {
                let p = prov_for_ctx.clone();
                ent_ctx_btn.update(cx, |this, cx| {
                    this.clear_completion();
                    this.close_picker();
                    this.context_popover_open = !this.context_popover_open;
                    if this.context_popover_open {
                        this.context_prior_focus = w.focused(cx);
                        this.context_btn_focus.focus(w, cx);
                        this.refresh_quota_for_provider(p, false, cx);
                    } else {
                        this.context_prior_focus = None;
                    }
                    cx.notify();
                });
            },
        );

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
            .on_click(move |_, w, cx| {
                ent_picker_btn.update(cx, |this, cx| {
                    this.clear_completion();
                    this.context_popover_open = false;
                    this.context_prior_focus = None;
                    if this.picker_open {
                        this.close_picker();
                    } else {
                        this.picker_open = true;
                        this.set_picker_page(ModelPickerPage::Main);
                        this.picker_prior_focus = w.focused(cx);
                        this.model_btn_focus.focus(w, cx);
                    }
                    cx.notify();
                });
            })
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
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(px(12.0))
                    .text_color(palette.text_secondary),
            );

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
        let is_abort_mode = abort_session_id.is_some();

        let ent_send = entity.clone();
        let send_btn = div()
            .id("composer-send-btn-b0mrik")
            .test_support()
            .a11y_button(
                "composer-send-btn-b0mrik",
                if is_abort_mode {
                    "停止生成"
                } else {
                    "发送消息"
                },
            )
            .w(height::COMPACT)
            .h(height::COMPACT)
            .rounded(radius::CONTROL)
            .bg(palette.accent_solid)
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(palette.accent_hover))
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
                    ent_send.update(cx, |this, cx| this.trigger_submit(w, cx));
                }
            })
            .child(if is_abort_mode {
                Icon::new(IconName::Square)
                    .size(px(12.0))
                    .text_color(palette.text_on_accent)
            } else {
                Icon::new(IconName::ArrowUp)
                    .size(px(14.0))
                    .text_color(palette.text_on_accent)
            });

        let right_cluster = div()
            .flex()
            .items_center()
            .gap(space::SM)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child(context_ring)
                    .child(model_btn),
            )
            .child(send_btn);

        let action_bar = div()
            .h(height::COMPACT)
            .flex()
            .items_center()
            .justify_between()
            .child(attach_btn)
            .child(right_cluster);

        let card_border_color = if is_focused {
            palette.border_focus
        } else {
            palette.border_strong
        };

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
            .shadow(palette.composer_shadows(is_focused))
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
            .child(div().w_full().min_h(px(24.0)).child(textarea_element))
            .child(action_bar);

        // Outer relative wrapper (`card` first, upward floating popovers `p2biT` / `d8zzg` / `h5jG6d` painted above `card`)
        let mut wrapper = div().relative().w_full().child(card);

        if let Some(comp_pop) = self.render_completion_popover(
            mono_family.clone(),
            &palette,
            transparency_enabled,
            scale_factor,
            &entity,
        ) {
            wrapper = wrapper.child(comp_pop);
        }

        if self.context_popover_open {
            let ent_retry = entity.clone();
            let ent_acc = entity.clone();
            let prov_retry = cur_provider.clone();
            wrapper = wrapper.child(
                div()
                    .absolute()
                    .right(px(48.0))
                    .bottom_full()
                    .pb(space::SM)
                    .child(render_context_quota_popover(
                        &cur_provider,
                        context_usage,
                        models_ref.usage.as_ref(),
                        self.cached_quota.as_ref().filter(|_| {
                            self.cached_quota_provider.as_deref() == Some(cur_provider.as_str())
                        }),
                        self.quota_loading
                            && self.cached_quota_provider.as_deref() == Some(cur_provider.as_str()),
                        mono_family,
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
                    )),
            );
        }

        if self.picker_open {
            let ent_page = entity.clone();
            let ent_sel_m = entity.clone();
            let ent_sel_t = entity.clone();
            wrapper = wrapper.child(
                div()
                    .absolute()
                    .right(px(36.0))
                    .bottom_full()
                    .pb(space::SM)
                    .child(render_model_picker_popover(
                        self.picker_page,
                        self.picker_hi_idx,
                        &self.picker_scroll,
                        &scoped_models,
                        &cur_provider,
                        &cur_model_id,
                        &cur_model_label,
                        &models_ref.thinking_levels,
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
                            ent_sel_m
                                .update(cx, |this, cx| this.select_model(provider, model_id, cx));
                        }),
                        Rc::new(move |level, w, cx| {
                            ent_sel_t.update(cx, |this, cx| this.select_thinking(level, w, cx));
                        }),
                    )),
            );
        }

        wrapper
    }
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

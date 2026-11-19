use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use cst_client::{
    ClientError, ClientHandle, ComposerDraft, DraftKey, InlineToken as DraftToken, SendMode,
};
use gpui::{
    ClipboardEntry, ClipboardItem, EntityInputHandler, ExternalPaths, ImageSource, KeyDownEvent,
    ObjectFit, PathPromptOptions, StyledImage as _,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{
    InlineToken as UiInlineToken, InputContent, InputEvent, InputState, Textarea, TextareaState,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TestSupportExt, Window, div, img, px,
};
use serde_json::Value;

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, radius, scale, space,
};
use crate::workbench::image_viewer::{LightboxImageTarget, gpui_image_from_handle};
use crate::workbench::model_picker::{
    ModelPickerPage, is_quota_provider, render_context_quota_popover, render_context_ring_button,
    render_model_picker_popover, resolve_scoped_models, thinking_level_label,
};
use crate::workbench::queue_bar::{ParsedQueueSnapshot, QueueBarCallbacks, render_queue_bar};

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
        command: "/skill:cst-overview",
        short_label: "cst-overview",
        description: "整机软硬件与运行状态快速巡检",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:bsod-triage",
        short_label: "bsod-triage",
        description: "蓝屏转储、错误代码与驱动故障定位",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:disk-space",
        short_label: "disk-space",
        description: "分区剩余容量与大体积目录排查",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:perf-slow",
        short_label: "perf-slow",
        description: "卡顿掉帧、高占用进程与温度功耗分析",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:network-diag",
        short_label: "network-diag",
        description: "网卡、DNS、代理与网关连通性诊断",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:startup-audit",
        short_label: "startup-audit",
        description: "开机自启项、计划任务与第三方服务审查",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:software-issue",
        short_label: "software-issue",
        description: "软件闪退、运行库缺失与安装报错排查",
        is_skill_or_token: true,
        nav_target: None,
    },
    SlashEntrySpec {
        command: "/skill:bitlocker",
        short_label: "bitlocker",
        description: "BitLocker 卷状态核查、解锁与全盘解密",
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
        description: "预览并导出当前会话为图片或网页",
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
    file_search_task: Option<Task<()>>,
    picker_open: bool,
    picker_page: ModelPickerPage,
    picker_hi_idx: usize,
    picker_prior_focus: Option<FocusHandle>,
    context_popover_open: bool,
    context_prior_focus: Option<FocusHandle>,
    cached_quota_provider: Option<String>,
    cached_quota: Option<Value>,
    quota_loading: bool,
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

        let model_btn_focus = cx.focus_handle();
        let context_btn_focus = cx.focus_handle();

        let sub = cx.subscribe_in(&textarea, window, Self::on_textarea_event);
        let obs = cx.observe_in(&textarea, window, |this, textarea, window, cx| {
            let marked = textarea
                .update(cx, |st, cx| {
                    EntityInputHandler::marked_text_range(st, window, cx)
                })
                .is_some();
            if marked {
                this.had_active_ime_mark = true;
            }
        });

        let mut view = Self {
            client,
            draft_key,
            last_synced_edit_rev: 0,
            had_active_ime_mark: false,
            textarea,
            completion: None,
            file_search_task: None,
            picker_open: false,
            picker_page: ModelPickerPage::Main,
            picker_hi_idx: 0,
            picker_prior_focus: None,
            context_popover_open: false,
            context_prior_focus: None,
            cached_quota_provider: None,
            cached_quota: None,
            quota_loading: false,
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
        self.push_to_workspace(cx);
        self.draft_key = next_key.clone();
        self.last_synced_edit_rev = 0;
        self.completion = None;
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
        if self.completion.take().is_some() {
            let fh = self.textarea.read(cx).presentation().focus_handle().clone();
            fh.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.picker_open {
            if self.picker_page != ModelPickerPage::Main {
                self.picker_page = ModelPickerPage::Main;
                self.picker_hi_idx = 0;
                cx.notify();
                return true;
            }
            self.picker_open = false;
            if let Some(prev) = self.picker_prior_focus.take() {
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
        if ext_rev != self.last_synced_edit_rev {
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
            InputEvent::PressEnter { secondary, shift } => {
                // Strict IME composition guard: NEVER confirm completion or submit while marked preedit exists!
                if self.is_ime_composing(window, cx) {
                    return;
                }
                if *shift && !*secondary {
                    return;
                }
                if self.completion.is_some() {
                    self.accept_selected_completion(window, cx);
                    return;
                }
                self.trigger_submit(window, cx);
            }
            InputEvent::Focus | InputEvent::Blur => {
                cx.notify();
            }
        }
    }

    fn handle_capture_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        // 1. IME composition isolation: while marked text is active, Enter/Escape/Tab stay strictly with IME!
        if self.is_ime_composing(window, cx) {
            if matches!(key, "enter" | "escape" | "tab") {
                cx.stop_propagation();
            }
            return;
        }

        // 2. Completion popup navigation (`Up` / `Down` / `Tab` / `Escape`)
        if let Some(ref mut comp) = self.completion {
            let len = match comp {
                CompletionPopupState::Slash { items, .. } => items.len(),
                CompletionPopupState::Files { paths, .. } => paths.len(),
            };
            match key {
                "down" if len > 0 => {
                    match comp {
                        CompletionPopupState::Slash { selected_idx, .. }
                        | CompletionPopupState::Files { selected_idx, .. } => {
                            *selected_idx = (*selected_idx + 1) % len;
                        }
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "up" if len > 0 => {
                    match comp {
                        CompletionPopupState::Slash { selected_idx, .. }
                        | CompletionPopupState::Files { selected_idx, .. } => {
                            *selected_idx = if *selected_idx == 0 {
                                len - 1
                            } else {
                                *selected_idx - 1
                            };
                        }
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "tab" | "enter" if len > 0 && !event.keystroke.modifiers.shift => {
                    cx.stop_propagation();
                    self.accept_selected_completion(window, cx);
                    return;
                }
                "escape" => {
                    self.completion = None;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        } else if key == "tab" && !event.keystroke.modifiers.shift {
            // Pressing Tab opens the `/` slash command completion menu at the current cursor!
            let val = self.textarea.read(cx).value();
            let pos = val.len();
            self.completion = Some(CompletionPopupState::Slash {
                replace_range: pos..pos,
                items: SLASH_CATALOG.to_vec(),
                selected_idx: 0,
            });
            cx.stop_propagation();
            cx.notify();
            return;
        }

        // 3. Model picker keyboard navigation (`Up` / `Down` / `Home` / `End` / `Enter` / `Escape`)
        if self.picker_open {
            let snap = self.client.snapshot();
            let empty_models = cst_client::ModelsSnapshot::default();
            let models_ref = snap.models.as_deref().unwrap_or(&empty_models);
            let scoped = resolve_scoped_models(models_ref);
            let levels = &models_ref.thinking_levels;
            let item_count = match self.picker_page {
                ModelPickerPage::Main => 2,
                ModelPickerPage::Models => scoped.len().max(1),
                ModelPickerPage::Thinking => levels.len().max(1),
            };
            match key {
                "down" => {
                    self.picker_hi_idx = (self.picker_hi_idx + 1) % item_count;
                    cx.stop_propagation();
                    cx.notify();
                }
                "up" => {
                    self.picker_hi_idx = if self.picker_hi_idx == 0 {
                        item_count - 1
                    } else {
                        self.picker_hi_idx - 1
                    };
                    cx.stop_propagation();
                    cx.notify();
                }
                "home" => {
                    self.picker_hi_idx = 0;
                    cx.stop_propagation();
                    cx.notify();
                }
                "end" => {
                    self.picker_hi_idx = item_count.saturating_sub(1);
                    cx.stop_propagation();
                    cx.notify();
                }
                "enter" => {
                    match self.picker_page {
                        ModelPickerPage::Main => {
                            self.picker_page = if self.picker_hi_idx == 0 {
                                ModelPickerPage::Models
                            } else {
                                ModelPickerPage::Thinking
                            };
                            self.picker_hi_idx = 0;
                        }
                        ModelPickerPage::Models => {
                            if let Some(m) = scoped.get(self.picker_hi_idx) {
                                let prov = m.provider.clone();
                                let mid = m.id.clone();
                                let key_d = self.draft_key.clone();
                                self.last_synced_edit_rev = self.client.with_workspace_mut(|ws| {
                                    let d = ws.draft_mut(key_d.clone());
                                    d.selected_provider = Some(prov.clone());
                                    d.selected_model_id = Some(mid.clone());
                                    ws.draft_edit_revision(&key_d)
                                });
                                let sid = match &self.draft_key {
                                    DraftKey::Session(id) => Some(id.clone()),
                                    DraftKey::NewSession => None,
                                };
                                let c = self.client.clone();
                                cx.spawn(async move |weak, cx| {
                                    let res = c.select_model(sid.clone(), prov, mid).await;
                                    let _ = c.refresh_models(sid).await;
                                    if let Err(e) = res {
                                        let _ = weak.update(cx, |_, cx| {
                                            cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                        });
                                    }
                                })
                                .detach();
                                self.picker_page = ModelPickerPage::Main;
                            }
                        }
                        ModelPickerPage::Thinking => {
                            if let Some(lvl) = levels.get(self.picker_hi_idx).cloned() {
                                let key_d = self.draft_key.clone();
                                self.last_synced_edit_rev = self.client.with_workspace_mut(|ws| {
                                    let d = ws.draft_mut(key_d.clone());
                                    d.selected_thinking_level = Some(lvl.clone());
                                    ws.draft_edit_revision(&key_d)
                                });
                                if let DraftKey::Session(sid) = self.draft_key.clone() {
                                    let c = self.client.clone();
                                    cx.spawn(async move |weak, cx| {
                                        let res =
                                            c.set_session_thinking_level(sid.clone(), lvl).await;
                                        let _ = c.refresh_models(Some(sid)).await;
                                        if let Err(e) = res {
                                            let _ = weak.update(cx, |_, cx| {
                                                cx.emit(ComposerHostEvent::ErrorMessage(
                                                    e.to_string(),
                                                ));
                                            });
                                        }
                                    })
                                    .detach();
                                }
                                self.picker_open = false;
                                if let Some(prev) = self.picker_prior_focus.take() {
                                    prev.focus(window, cx);
                                }
                            }
                        }
                    }
                    cx.stop_propagation();
                    cx.notify();
                }
                "escape" => {
                    let _ = self.dismiss_topmost_popover(window, cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
        }
    }

    fn update_completion_from_caret(&mut self, cx: &mut Context<Self>) {
        let content = self.textarea.read(cx).content();
        let text = content.text().as_ref();
        let tokens = content.tokens();

        // 1. Leading `/` command trigger (outside existing inline tokens)
        let trimmed_start = text.trim_start_matches(' ');
        let leading_spaces = text.len() - trimmed_start.len();
        if let Some(after_slash) = trimmed_start.strip_prefix('/')
            && !after_slash.contains(char::is_whitespace)
            && !tokens
                .iter()
                .any(|s| s.range().start <= leading_spaces && leading_spaces < s.range().end)
        {
            let q = after_slash.to_ascii_lowercase();
            let filtered: Vec<SlashEntrySpec> = SLASH_CATALOG
                .iter()
                .filter(|item| {
                    item.command[1..].to_ascii_lowercase().contains(&q)
                        || item.short_label.to_ascii_lowercase().contains(&q)
                })
                .cloned()
                .collect();
            if !filtered.is_empty() {
                self.completion = Some(CompletionPopupState::Slash {
                    replace_range: leading_spaces..text.len(),
                    items: filtered,
                    selected_idx: 0,
                });
                return;
            }
        }

        // 2. Trailing `@<query>` file reference trigger
        if let Some(at_pos) = text.rfind('@') {
            let valid_boundary = at_pos == 0 || text[..at_pos].ends_with(char::is_whitespace);
            let after_at = &text[at_pos + 1..];
            let inside_token = tokens
                .iter()
                .any(|s| s.range().start <= at_pos && at_pos < s.range().end);
            if valid_boundary && !inside_token && !after_at.contains(char::is_whitespace) {
                let query_bg = after_at.to_owned();
                let replace_range = at_pos..text.len();
                let sid = match &self.draft_key {
                    DraftKey::Session(id) => Some(id.clone()),
                    DraftKey::NewSession => None,
                };
                let client = self.client.clone();
                self.file_search_task = Some(cx.spawn(async move |weak, cx| {
                    let res = client.search_files(sid, query_bg).await;
                    if let Ok(paths) = res {
                        let _ = weak.update(cx, |this, cx| {
                            if paths.is_empty() {
                                this.completion = None;
                            } else {
                                this.completion = Some(CompletionPopupState::Files {
                                    replace_range,
                                    paths,
                                    selected_idx: 0,
                                });
                            }
                            cx.notify();
                        });
                    }
                }));
                return;
            }
        }

        self.completion = None;
    }

    fn accept_selected_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                if spec.command == "/compact"
                    && let DraftKey::Session(ref sid) = self.draft_key
                {
                    // `/compact` inside an active session can also be inserted as an atomic token or executed on submit;
                    // per plan §3.3.2: "选中诊断技能或 `/compact` 后在 `TextareaState` 中插入 `InlineToken`"
                    let _ = sid;
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
        if self.is_ime_composing(window, cx) {
            return;
        }
        self.push_to_workspace(cx);

        // Handle `/compact` command token inside an active session directly
        let is_compact_only = self.client.with_workspace(|ws| {
            ws.draft(&self.draft_key).is_some_and(|d| {
                d.text.trim().is_empty()
                    && d.images.is_empty()
                    && d.tokens.iter().any(
                        |t| matches!(t, DraftToken::Command { name, .. } if name == "/compact"),
                    )
            })
        });
        if is_compact_only && let DraftKey::Session(sid) = self.draft_key.clone() {
            let key = self.draft_key.clone();
            let _ = self.client.with_workspace_mut(|ws| ws.take_draft(&key));
            self.pull_from_workspace(window, cx);
            let client = self.client.clone();
            cx.spawn(async move |weak, cx| {
                let res = client.compact_session(sid, None).await;
                let _ = weak.update(cx, |_, cx| match res {
                    Ok(_) => cx.emit(ComposerHostEvent::TopNotice(
                        "已完成会话上下文压缩。".into(),
                    )),
                    Err(e) => cx.emit(ComposerHostEvent::ErrorMessage(e.to_string())),
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
        // Clear editor immediately on optimistic take; if Home submission fails, `submit_draft` restores it into `WorkspaceState`!
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let submit_res = client.submit_draft(target_key.clone()).await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.pull_from_workspace(window, cx);
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
        if !force
            && self.cached_quota_provider.as_deref() == Some(provider.as_str())
            && self.cached_quota.is_some()
        {
            return;
        }
        self.cached_quota_provider = Some(provider.clone());
        self.quota_loading = true;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.get_provider_quota(provider.clone(), force).await;
            let _ = weak.update(cx, |this, cx| {
                this.quota_loading = false;
                if let Ok(val) = res {
                    this.cached_quota = Some(val);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_completion_popover(
        &self,
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
        .w_full()
        .p(space::XS)
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_hairline)
        .flex()
        .flex_col()
        .gap(space::MICRO);

        match comp {
            CompletionPopupState::Slash {
                items,
                selected_idx,
                ..
            } => {
                let mut scroll = div()
                    .id("composer-slash-list")
                    .relative()
                    .max_h(px(264.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(space::MICRO);
                for (idx, item) in items.iter().enumerate() {
                    let is_sel = idx == *selected_idx;
                    let ent = entity.clone();
                    scroll = scroll.child(
                        apply_scale(div(), scale::UI)
                            .id(SharedString::from(format!(
                                "slash-item-{}",
                                item.short_label
                            )))
                            .test_support()
                            .h(height::CONTROL)
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
                            .hover(|s| s.bg(palette.bg_item_hover))
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
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_primary)
                                    .child(item.command),
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
                    .relative()
                    .max_h(px(240.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(space::MICRO);
                for (idx, path) in paths.iter().enumerate() {
                    let is_sel = idx == *selected_idx;
                    let ent = entity.clone();
                    scroll = scroll.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(SharedString::from(format!("file-completion-item-{idx}")))
                            .test_support()
                            .h(height::CONTROL)
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
                            .hover(|s| s.bg(palette.bg_item_hover))
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
        let parsed_queue =
            ParsedQueueSnapshot::from_json(active_session.as_ref().and_then(|s| s.queue.as_ref()));

        let (draft_images, send_mode) = self.client.with_workspace(|ws| {
            ws.draft(&self.draft_key)
                .map(|d| (d.images.clone(), d.send_mode))
                .unwrap_or_else(|| (Vec::new(), SendMode::Direct))
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
            let sid_s = sid.clone();
            let sid_r = sid.clone();
            let sid_d = sid.clone();
            let sid_ord = sid.clone();
            let sid_save = sid.clone();
            let q1 = entity.clone();
            let q2 = entity.clone();
            let q3 = entity.clone();
            let q4 = entity.clone();
            let q5 = entity.clone();
            let q6 = entity.clone();
            let q7 = entity.clone();

            render_queue_bar(
                &parsed_queue,
                self.editing_queue_item_id.as_deref(),
                self.queue_edit_input.as_ref(),
                &palette,
                QueueBarCallbacks {
                    on_steer: Rc::new(move |iid, ver, _, cx| {
                        let s = sid_s.clone();
                        q1.update(cx, |this, cx| {
                            let c = this.client.clone();
                            cx.spawn(async move |weak, cx| {
                                if let Err(e) = c.steer_queue_item(s, iid, ver).await {
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                    });
                                }
                            })
                            .detach();
                        });
                    }),
                    on_retry: Rc::new(move |iid, ver, _, cx| {
                        let s = sid_r.clone();
                        q2.update(cx, |this, cx| {
                            let c = this.client.clone();
                            cx.spawn(async move |weak, cx| {
                                if let Err(e) = c.retry_queue_item(s, iid, ver).await {
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                    });
                                }
                            })
                            .detach();
                        });
                    }),
                    on_delete: Rc::new(move |iid, ver, _, cx| {
                        let s = sid_d.clone();
                        q3.update(cx, |this, cx| {
                            let c = this.client.clone();
                            cx.spawn(async move |weak, cx| {
                                if let Err(e) = c.delete_queue_item(s, iid, ver).await {
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                    });
                                }
                            })
                            .detach();
                        });
                    }),
                    on_reorder: Rc::new(move |ver, ids, _, cx| {
                        let s = sid_ord.clone();
                        q4.update(cx, |this, cx| {
                            let c = this.client.clone();
                            cx.spawn(async move |weak, cx| {
                                if let Err(e) = c.reorder_queue(s, ver, ids).await {
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                    });
                                }
                            })
                            .detach();
                        });
                    }),
                    on_begin_edit: Rc::new(move |iid, cur_text, window, cx| {
                        q5.update(cx, |this, cx| {
                            let inp = cx.new(|cx| {
                                let mut s = InputState::new(window, cx);
                                s.set_value(cur_text, window, cx);
                                s
                            });
                            this.editing_queue_item_id = Some(iid);
                            this.queue_edit_input = Some(inp);
                            cx.notify();
                        });
                    }),
                    on_commit_edit: Rc::new(move |iid, ver, _, cx| {
                        let s = sid_save.clone();
                        q6.update(cx, |this, cx| {
                            let Some(ref inp) = this.queue_edit_input else {
                                return;
                            };
                            let new_text = inp.read(cx).value().to_string();
                            this.editing_queue_item_id = None;
                            this.queue_edit_input = None;
                            let c = this.client.clone();
                            cx.spawn(async move |weak, cx| {
                                if let Err(e) = c.edit_queue_item(s, iid, ver, new_text).await {
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                    });
                                }
                            })
                            .detach();
                            cx.notify();
                        });
                    }),
                    on_cancel_edit: Rc::new(move |_, cx| {
                        q7.update(cx, |this, cx| {
                            this.editing_queue_item_id = None;
                            this.queue_edit_input = None;
                            cx.notify();
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

                row = row.child(
                    div()
                        .id(SharedString::from(format!("composer-draft-thumb-{idx}")))
                        .test_support()
                        .relative()
                        .w(px(80.0))
                        .h(px(64.0))
                        .rounded(radius::ROW)
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
                                .id(SharedString::from(format!("composer-draft-thumb-rm-{idx}")))
                                .test_support()
                                .absolute()
                                .top(space::MICRO)
                                .right(space::MICRO)
                                .w(px(18.0))
                                .h(px(18.0))
                                .rounded(radius::FULL)
                                .bg(palette.bg_modal)
                                .border_1()
                                .border_color(palette.border_subtle)
                                .flex()
                                .items_center()
                                .justify_center()
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    ent_rm.update(cx, |this, cx| {
                                        this.remove_draft_image(idx, cx);
                                    });
                                })
                                .child(
                                    Icon::new(IconName::X)
                                        .size(px(11.0))
                                        .text_color(palette.text_primary),
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
            .appearance(false)
            .bordered(false)
            .token(move |tok_ctx, _window, _cx| {
                let tok = tok_ctx.token();
                let label = tok.label().clone();
                let is_file = tok.id().starts_with("file:");
                let span_range = tok_ctx.range();
                let ent_click = ent_tok.clone();

                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .bg(palette.bg_surface)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child(
                        Icon::new(if is_file {
                            IconName::FileText
                        } else {
                            IconName::Layers
                        })
                        .size(px(13.0))
                        .text_color(palette.accent_solid),
                    )
                    .child(cjk_inline_label(label))
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "inline-token-rm-{}",
                                span_range.start
                            )))
                            .w(px(16.0))
                            .h(px(16.0))
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
                                    .text_color(palette.text_secondary),
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

        // Busy session delivery mode toggle (`排队` vs `插话`)
        let delivery_toggle = is_session_running.then(|| {
            let ent_mode = entity.clone();
            let is_steer = send_mode == SendMode::Steer;
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id("composer-delivery-mode-btn")
                .test_support()
                .h(height::COMPACT)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .border_1()
                .border_color(if is_steer {
                    palette.accent_solid
                } else {
                    palette.border_subtle
                })
                .bg(if is_steer {
                    palette.accent_subtle
                } else {
                    palette.bg_surface
                })
                .text_color(if is_steer {
                    palette.accent_solid
                } else {
                    palette.text_secondary
                })
                .flex()
                .items_center()
                .on_click(move |_, _, cx| {
                    ent_mode.update(cx, |this, cx| {
                        let key = this.draft_key.clone();
                        this.last_synced_edit_rev = this.client.with_workspace_mut(|ws| {
                            let d = ws.draft_mut(key.clone());
                            d.send_mode = if d.send_mode == SendMode::Steer {
                                SendMode::Queue
                            } else {
                                SendMode::Steer
                            };
                            ws.draft_edit_revision(&key)
                        });
                        cx.notify();
                    });
                })
                .child(cjk_inline_label(if is_steer { "插话" } else { "排队" }))
        });

        let ent_ctx_btn = entity.clone();
        let prov_for_ctx = cur_provider.clone();
        let context_ring = render_context_ring_button(
            percent_opt,
            &self.context_btn_focus,
            &palette,
            move |w, cx| {
                let p = prov_for_ctx.clone();
                ent_ctx_btn.update(cx, |this, cx| {
                    this.picker_open = false;
                    this.context_popover_open = !this.context_popover_open;
                    if this.context_popover_open {
                        this.context_prior_focus = w.focused(cx);
                        this.refresh_quota_for_provider(p, false, cx);
                    }
                    cx.notify();
                });
            },
        );

        let ent_picker_btn = entity.clone();
        let model_btn = apply_scale(div(), scale::CAPTION_MEDIUM)
            .id("composer-model-picker-btn")
            .test_support()
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
                    this.context_popover_open = false;
                    this.picker_open = !this.picker_open;
                    if this.picker_open {
                        this.picker_page = ModelPickerPage::Main;
                        this.picker_hi_idx = 0;
                        this.picker_prior_focus = w.focused(cx);
                    }
                    cx.notify();
                });
            })
            .child(
                div()
                    .max_w(px(148.0))
                    .truncate()
                    .text_color(palette.text_primary)
                    .child(SharedString::from(cur_model_label.clone())),
            )
            .child(
                div()
                    .text_color(palette.text_secondary)
                    .child(thinking_level_label(&cur_thinking)),
            )
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(px(12.0))
                    .text_color(palette.text_secondary),
            );

        let abort_btn = if is_session_running && let DraftKey::Session(ref sid) = self.draft_key {
            let sid_abort = sid.clone();
            let ent_abort = entity.clone();
            Some(
                div()
                    .id("composer-abort-btn")
                    .test_support()
                    .w(height::COMPACT)
                    .h(height::COMPACT)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(palette.bg_elevated)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, _, cx| {
                        let s = sid_abort.clone();
                        ent_abort.update(cx, |this, cx| {
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
                    })
                    .child(
                        Icon::new(IconName::Square)
                            .size(px(12.0))
                            .text_color(palette.status_danger),
                    ),
            )
        } else {
            None
        };

        let ent_send = entity.clone();
        let send_btn = div()
            .id("composer-send-btn-b0mrik")
            .test_support()
            .w(height::COMPACT)
            .h(height::COMPACT)
            .rounded(radius::CONTROL)
            .bg(palette.accent_solid)
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(palette.accent_hover))
            .on_click(move |_, w, cx| {
                ent_send.update(cx, |this, cx| this.trigger_submit(w, cx));
            })
            .child(
                Icon::new(IconName::ArrowUp)
                    .size(px(14.0))
                    .text_color(palette.text_on_accent),
            );

        let mut right_cluster = div().flex().items_center().gap(space::SM).child(
            div()
                .flex()
                .items_center()
                .gap(space::XS)
                .child(context_ring)
                .child(model_btn),
        );
        if let Some(dt) = delivery_toggle {
            right_cluster = right_cluster.child(dt);
        }
        if let Some(ab) = abort_btn {
            right_cluster = right_cluster.child(ab);
        }
        right_cluster = right_cluster.child(send_btn);

        let action_bar = div()
            .h(height::COMPACT)
            .flex()
            .items_center()
            .justify_between()
            .child(attach_btn)
            .child(right_cluster);

        let ent_drop = entity.clone();
        let mut card = div()
            .id("composer-card-zwas1")
            .test_support()
            .w_full()
            .min_h(px(88.0))
            .px(space::MD)
            .py(px(11.0))
            .rounded(radius::ROW)
            .border_1()
            .border_color(if is_focused {
                palette.accent_solid
            } else {
                palette.border_strong
            })
            .bg(palette.bg_workspace)
            .flex()
            .flex_col()
            .justify_between()
            .gap(space::SM)
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
            .capture_key_down(cx.listener(Self::handle_capture_key_down))
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

        // Outer relative wrapper anchoring upward popovers (`p2biT`, `d8zzg`, `h5jG6d`)
        let mut wrapper = div().relative().w_full();

        if let Some(comp_pop) =
            self.render_completion_popover(&palette, transparency_enabled, scale_factor, &entity)
        {
            wrapper = wrapper.child(comp_pop);
        }

        if self.context_popover_open {
            let ent_retry = entity.clone();
            let ent_acc = entity.clone();
            let prov_retry = cur_provider.clone();
            wrapper = wrapper.child(div().absolute().right(px(64.0)).bottom(px(44.0)).child(
                render_context_quota_popover(
                    &cur_provider,
                    context_usage,
                    models_ref.usage.as_ref(),
                    self.cached_quota.as_ref(),
                    self.quota_loading,
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
                            cx.emit(ComposerHostEvent::OpenAccountPage);
                            cx.notify();
                        });
                    }),
                ),
            ));
        }

        if self.picker_open {
            let ent_page = entity.clone();
            let ent_sel_m = entity.clone();
            let ent_sel_t = entity.clone();
            wrapper = wrapper.child(div().absolute().right(px(36.0)).bottom(px(44.0)).child(
                render_model_picker_popover(
                    self.picker_page,
                    self.picker_hi_idx,
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
                            this.picker_page = next_page;
                            this.picker_hi_idx = 0;
                            cx.notify();
                        });
                    }),
                    Rc::new(move |prov, mid, _, cx| {
                        ent_sel_m.update(cx, |this, cx| {
                            let key = this.draft_key.clone();
                            this.last_synced_edit_rev = this.client.with_workspace_mut(|ws| {
                                let d = ws.draft_mut(key.clone());
                                d.selected_provider = Some(prov.clone());
                                d.selected_model_id = Some(mid.clone());
                                ws.draft_edit_revision(&key)
                            });
                            let sid = match &this.draft_key {
                                DraftKey::Session(id) => Some(id.clone()),
                                DraftKey::NewSession => None,
                            };
                            let c = this.client.clone();
                            cx.spawn(async move |weak, cx| {
                                let res = c.select_model(sid.clone(), prov, mid).await;
                                let _ = c.refresh_models(sid).await;
                                if let Err(e) = res {
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                    });
                                }
                            })
                            .detach();
                            this.picker_page = ModelPickerPage::Main;
                            cx.notify();
                        });
                    }),
                    Rc::new(move |lvl, w, cx| {
                        ent_sel_t.update(cx, |this, cx| {
                            let key = this.draft_key.clone();
                            this.last_synced_edit_rev = this.client.with_workspace_mut(|ws| {
                                let d = ws.draft_mut(key.clone());
                                d.selected_thinking_level = Some(lvl.clone());
                                ws.draft_edit_revision(&key)
                            });
                            if let DraftKey::Session(sid) = this.draft_key.clone() {
                                let c = this.client.clone();
                                cx.spawn(async move |weak, cx| {
                                    let res = c.set_session_thinking_level(sid.clone(), lvl).await;
                                    let _ = c.refresh_models(Some(sid)).await;
                                    if let Err(e) = res {
                                        let _ = weak.update(cx, |_, cx| {
                                            cx.emit(ComposerHostEvent::ErrorMessage(e.to_string()));
                                        });
                                    }
                                })
                                .detach();
                            }
                            this.picker_open = false;
                            if let Some(prev) = this.picker_prior_focus.take() {
                                prev.focus(w, cx);
                            }
                            cx.notify();
                        });
                    }),
                ),
            ));
        }

        wrapper.child(card)
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

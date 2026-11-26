mod editor;

use std::collections::HashSet;
use std::sync::Arc;

use cst_client::{AccountStatusDto, ClientError, ClientHandle, ModelsSnapshot};
use gpui::{KeyDownEvent, MouseDownEvent, MouseMoveEvent, Pixels, Point};
use gpui_kit::assets::IconName;
use gpui_kit::base::{ElementExt as _, FocusTrapElement as _};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputContentType, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt, Window, div, px,
};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::accessibility::A11yElementExt as _;
use crate::settings::preferences_tab::scope::ModelScope;
use crate::theme::kbd_focus::{
    DialogNavStep, KbdDynamicBoundsTracker, KbdDynamicSpec, KbdMenuStep, KbdRailPlacement, KbdTone,
    pointer_moved_past_threshold, render_gliding_dynamic_indicator, resolve_dialog_nav_key,
    resolve_menu_nav_key,
};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius,
    scale, space,
};

pub(crate) fn format_cstoa_status(
    account: Option<&AccountStatusDto>,
    requires_login: bool,
) -> (String, bool) {
    let signed_in = account.is_some_and(|a| a.signed_in);
    if signed_in && !requires_login {
        let profile = account.map(|a| &a.profile);
        let name = profile
            .and_then(|p| p.name.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "--");
        let student_id = profile
            .and_then(|p| p.student_id.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "--");
        let text = match (name, student_id) {
            (Some(nm), Some(sid)) => format!("{nm}（{sid}） · 已登录"),
            (Some(nm), None) => format!("{nm} · 已登录"),
            (None, Some(sid)) => format!("{sid} · 已登录"),
            (None, None) => "账号授权 · 已登录".to_owned(),
        };
        (text, false)
    } else if requires_login {
        ("凭据已失效，请重新登录".to_owned(), true)
    } else {
        ("账号授权 · 未登录".to_owned(), true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomApiProtocol {
    OpenaiCompletions,
    OpenaiResponses,
    AnthropicMessages,
    GoogleGenerativeAi,
}

impl CustomApiProtocol {
    pub const ALL: [Self; 4] = [
        Self::OpenaiCompletions,
        Self::OpenaiResponses,
        Self::AnthropicMessages,
        Self::GoogleGenerativeAi,
    ];

    pub fn wire_value(self) -> &'static str {
        match self {
            Self::OpenaiCompletions => "openai-completions",
            Self::OpenaiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
            Self::GoogleGenerativeAi => "google-generative-ai",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenaiCompletions => "OpenAI Chat Completions",
            Self::OpenaiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::GoogleGenerativeAi => "Google Gemini",
        }
    }

    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "openai-responses" => Self::OpenaiResponses,
            "anthropic-messages" => Self::AnthropicMessages,
            "google-generative-ai" => Self::GoogleGenerativeAi,
            _ => Self::OpenaiCompletions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenFieldKind {
    ContextWindow,
    MaxTokens,
}

pub(super) const CONTEXT_WINDOW_PRESETS: &[(&str, u64)] = &[
    ("128K", 128_000),
    ("256K", 256_000),
    ("512K", 512_000),
    ("1M", 1_000_000),
];

pub(super) const MAX_OUTPUT_PRESETS: &[(&str, u64)] = &[
    ("16K", 16_384),
    ("32K", 32_768),
    ("64K", 65_536),
    ("128K", 128_000),
    ("256K", 256_000),
    ("384K", 384_000),
];

pub struct EditableModelRow {
    pub id_input: Entity<InputState>,
    pub name_input: Entity<InputState>,
    pub context_window: u64,
    pub max_tokens: u64,
    pub reasoning: bool,
    pub image: bool,
    pub is_new: bool,
    pub _subs: Vec<Subscription>,
}

fn format_token_window(n: u64) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 16_384 && n.is_multiple_of(1_024) {
        format!("{}K", n / 1_024)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

pub(super) fn cycle_token_preset(current: u64, presets: &[(&str, u64)]) -> u64 {
    if let Some(pos) = presets.iter().position(|&(_, v)| v == current) {
        presets[(pos + 1) % presets.len()].1
    } else {
        presets
            .iter()
            .find(|&&(_, v)| v > current)
            .unwrap_or(&presets[0])
            .1
    }
}

fn parse_token_window_input(raw: &str) -> Option<u64> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !matches!(c, ',' | '_' | ' '))
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let lower = cleaned.to_ascii_lowercase();
    let (num_part, mult) = if let Some(s) = lower.strip_suffix('m') {
        (s, 1_000_000.0_f64)
    } else if let Some(s) = lower.strip_suffix('k') {
        (s, 1_000.0_f64)
    } else {
        (lower.as_str(), 1.0_f64)
    };
    let val = num_part.parse::<f64>().ok()?;
    if !val.is_finite() || val <= 0.0 {
        return None;
    }
    let out = (val * mult).round() as u64;
    (out > 0).then_some(out)
}

#[derive(Debug, Clone)]
pub enum ProvidersTabEvent {
    OpenLoginOrAccount { signed_in: bool },
    CredentialsSaved,
    Notice(String),
    ErrorMessage(String),
}

pub struct ProvidersTabView {
    client: ClientHandle,
    active_provider_id: Option<String>,
    adding_provider: bool,
    creating_new_custom: bool,
    inline_edit_mode: bool,
    search_visible: bool,
    search_input: Entity<InputState>,
    add_search_input: Entity<InputState>,
    provider_id_input: Entity<InputState>,
    provider_name_input: Entity<InputState>,
    base_url_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    protocol: CustomApiProtocol,
    protocol_dropdown_open: bool,
    protocol_dropdown_index: usize,
    protocol_kbd_active: bool,
    protocol_last_pointer: Option<Point<Pixels>>,
    protocol_tracker: KbdDynamicBoundsTracker,
    protocol_focus: FocusHandle,
    protocol_trigger_focus: FocusHandle,
    list_focus: FocusHandle,
    list_kbd_active: bool,
    list_selected_idx: usize,
    return_focus_idx: usize,
    list_last_pointer: Option<Point<Pixels>>,
    list_tracker: KbdDynamicBoundsTracker,
    confirm_remove: bool,
    remove_trigger_focus: FocusHandle,
    remove_cancel_focus: FocusHandle,
    remove_confirm_focus: FocusHandle,
    remove_confirm_kbd_active: bool,
    remove_confirm_last_pointer: Option<Point<Pixels>>,
    remove_confirm_tracker: KbdDynamicBoundsTracker,
    inline_token_edit: Option<(usize, TokenFieldKind)>,
    inline_token_input: Entity<InputState>,
    revision: Option<String>,
    editable_rows: Vec<EditableModelRow>,
    saving: bool,
    loading: bool,
    generation: u64,
    window: gpui::AnyWindowHandle,
    key_error: Option<SharedString>,
    form_error: Option<SharedString>,
    providers_expanded: bool,
    enabled_scope: ModelScope,
    confirmed_scope: ModelScope,
    observed_models: Option<Arc<ModelsSnapshot>>,
    scope_saving: bool,
    scope_pending: bool,
    scope_revision: u64,
    page_slide: motion::PageSlide,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProvidersTabEvent> for ProvidersTabView {}

impl ProvidersTabView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("筛选模型…"));
        let add_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索提供商…"));
        let provider_id_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("如 my-gateway"));
        let provider_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("如 本地网关"));
        let base_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.example.com/v1"));
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("输入 API Key")
                .masked(true)
        });
        let inline_token_input = cx.new(|cx| InputState::new(window, cx).placeholder("128K"));

        let mut subs = vec![
            cx.subscribe(
                &search_input,
                |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.dismiss_click_outside_dropdowns(cx);
                    }
                    _ => {}
                },
            ),
            cx.subscribe(
                &add_search_input,
                |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.list_selected_idx = 0;
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.dismiss_click_outside_dropdowns(cx);
                    }
                    _ => {}
                },
            ),
        ];
        subs.push(
            cx.subscribe(&inline_token_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.commit_inline_token_edit(cx);
                }
            }),
        );
        for inp in [&provider_id_input, &provider_name_input, &base_url_input] {
            subs.push(
                cx.subscribe(inp, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.form_error = None;
                        this.key_error = None;
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.dismiss_click_outside_dropdowns(cx);
                    }
                    _ => {}
                }),
            );
        }
        subs.push(cx.subscribe(
            &api_key_input,
            |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    this.form_error = None;
                    this.key_error = None;
                    cx.notify();
                }
                InputEvent::Focus => {
                    this.dismiss_click_outside_dropdowns(cx);
                }
                _ => {}
            },
        ));

        Self {
            client,
            active_provider_id: None,
            adding_provider: false,
            creating_new_custom: false,
            inline_edit_mode: false,
            search_visible: false,
            search_input,
            add_search_input,
            provider_id_input,
            provider_name_input,
            base_url_input,
            api_key_input,
            protocol: CustomApiProtocol::OpenaiCompletions,
            protocol_dropdown_open: false,
            protocol_dropdown_index: 0,
            protocol_kbd_active: false,
            protocol_last_pointer: None,
            protocol_tracker: KbdDynamicBoundsTracker::new(),
            protocol_focus: cx.focus_handle(),
            protocol_trigger_focus: cx.focus_handle().tab_stop(true),
            list_focus: cx.focus_handle().tab_stop(true),
            list_kbd_active: false,
            list_selected_idx: 0,
            return_focus_idx: 0,
            list_last_pointer: None,
            list_tracker: KbdDynamicBoundsTracker::new(),
            confirm_remove: false,
            remove_trigger_focus: cx.focus_handle().tab_stop(true),
            remove_cancel_focus: cx.focus_handle().tab_stop(true),
            remove_confirm_focus: cx.focus_handle().tab_stop(true),
            remove_confirm_kbd_active: false,
            remove_confirm_last_pointer: None,
            remove_confirm_tracker: KbdDynamicBoundsTracker::new(),
            inline_token_edit: None,
            inline_token_input,
            revision: None,
            editable_rows: Vec::new(),
            saving: false,
            loading: false,
            generation: 0,
            window: window.window_handle(),
            key_error: None,
            form_error: None,
            providers_expanded: false,
            enabled_scope: ModelScope::default(),
            confirmed_scope: ModelScope::default(),
            observed_models: None,
            scope_saving: false,
            scope_pending: false,
            scope_revision: 0,
            page_slide: motion::PageSlide::default(),
            _subscriptions: subs,
        }
    }

    pub fn open_new_custom_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page_slide.start(1.0, cx);
        self.reset_form();
        self.creating_new_custom = true;
        self.inline_edit_mode = true;
        self.revision = None;
        self.key_error = None;
        self.form_error = None;
        self.protocol = CustomApiProtocol::OpenaiCompletions;
        self.provider_id_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.provider_name_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.base_url_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.clear_key(window, cx);
        self.search_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.append_blank_editable_row(window, cx);
        cx.notify();
    }

    pub fn leave(&mut self, cx: &mut Context<Self>) {
        self.providers_expanded = false;
        self.reset_form();
        let key = self.api_key_input.downgrade();
        let window = self.window;
        cx.spawn(async move |_, cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = key.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.set_masked(true, window, cx);
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn return_to_provider_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page_slide.start(-1.0, cx);
        self.reset_form();
        self.clear_key(window, cx);
        self.list_selected_idx = self.return_focus_idx;
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    fn open_remove_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.loading || (self.inline_edit_mode && self.revision.is_none()) {
            return;
        }
        self.confirm_remove = true;
        self.remove_confirm_kbd_active = true;
        self.remove_confirm_last_pointer = Some(window.mouse_position());
        self.remove_cancel_focus.focus(window, cx);
        cx.notify();
    }

    fn cancel_remove_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.confirm_remove = false;
        self.remove_confirm_kbd_active = false;
        self.remove_trigger_focus.focus(window, cx);
        cx.notify();
    }

    fn clear_key(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.api_key_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_masked(true, window, cx);
        });
    }

    pub fn has_dropdown(&self) -> bool {
        self.protocol_dropdown_open || self.inline_token_edit.is_some() || self.confirm_remove
    }

    pub fn open_add_provider_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.return_focus_idx = self.list_selected_idx;
        self.page_slide.start(1.0, cx);
        self.reset_form();
        self.adding_provider = true;
        self.providers_expanded = false;
        self.list_selected_idx = 0;
        self.add_search_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    pub fn focus_from_tabbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list_kbd_active = true;
        self.list_last_pointer = Some(window.mouse_position());
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    pub fn dismiss_topmost(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.protocol_dropdown_open {
            self.protocol_dropdown_open = false;
            self.protocol_kbd_active = false;
            self.protocol_trigger_focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.dismiss_dropdowns(cx) {
            return true;
        }
        if self.confirm_remove {
            if !self.saving {
                self.cancel_remove_confirm(window, cx);
            }
            return true;
        }
        if self.adding_provider
            && self
                .add_search_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            if self.add_search_input.read(cx).value().is_empty() {
                self.list_focus.focus(window, cx);
            } else {
                self.add_search_input
                    .update(cx, |i, cx| i.set_value("", window, cx));
            }
            cx.notify();
            return true;
        }
        if self.search_visible {
            self.search_visible = false;
            self.search_input
                .update(cx, |i, cx| i.set_value("", window, cx));
            cx.notify();
            return true;
        }
        if self.creating_new_custom
            || self.inline_edit_mode
            || self.active_provider_id.is_some()
            || self.adding_provider
        {
            self.return_to_provider_list(window, cx);
            return true;
        }
        false
    }

    pub fn focus_primary_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.inline_token_edit.is_some() {
            self.inline_token_input
                .update(cx, |inp, cx| inp.focus(window, cx));
            cx.notify();
            return true;
        }
        if self.protocol_dropdown_open {
            return false;
        }
        if self.adding_provider {
            self.add_search_input
                .update(cx, |inp, cx| inp.focus(window, cx));
            cx.notify();
            return true;
        }
        if self.creating_new_custom || self.inline_edit_mode {
            self.provider_name_input
                .update(cx, |inp, cx| inp.focus(window, cx));
            cx.notify();
            return true;
        }
        if self.search_visible {
            self.search_input
                .update(cx, |inp, cx| inp.focus(window, cx));
            cx.notify();
            return true;
        }
        if self.active_provider_id.is_some() {
            self.api_key_input
                .update(cx, |inp, cx| inp.focus(window, cx));
            cx.notify();
            return true;
        }
        false
    }

    pub fn dismiss_dropdowns(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        if self.protocol_dropdown_open {
            self.protocol_dropdown_open = false;
            self.protocol_kbd_active = false;
            changed = true;
        }
        if self.inline_token_edit.take().is_some() {
            changed = true;
        }
        if changed {
            cx.notify();
        }
        changed
    }

    pub fn dismiss_click_outside_dropdowns(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.protocol_dropdown_open {
            return false;
        }
        self.protocol_dropdown_open = false;
        self.protocol_kbd_active = false;
        cx.notify();
        true
    }

    pub(super) fn start_inline_token_edit(
        &mut self,
        row_idx: usize,
        kind: TokenFieldKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.editable_rows.get(row_idx) else {
            return;
        };
        self.protocol_dropdown_open = false;
        let current = match kind {
            TokenFieldKind::ContextWindow => row.context_window,
            TokenFieldKind::MaxTokens => row.max_tokens,
        };
        let initial = format_token_window(current);
        self.inline_token_edit = Some((row_idx, kind));
        self.inline_token_input.update(cx, |inp, cx| {
            inp.set_value(initial, window, cx);
            inp.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn commit_inline_token_edit(&mut self, cx: &mut Context<Self>) {
        let Some((row_idx, kind)) = self.inline_token_edit.take() else {
            return;
        };
        let raw = self.inline_token_input.read(cx).value().to_string();
        if let Some(parsed) = parse_token_window_input(&raw)
            && let Some(row) = self.editable_rows.get_mut(row_idx)
        {
            match kind {
                TokenFieldKind::ContextWindow => row.context_window = parsed,
                TokenFieldKind::MaxTokens => row.max_tokens = parsed,
            }
        }
        cx.notify();
    }

    fn reset_form(&mut self) {
        self.generation += 1;
        self.loading = false;
        self.revision = None;
        self.editable_rows.clear();
        self.adding_provider = false;
        self.inline_edit_mode = false;
        self.creating_new_custom = false;
        self.active_provider_id = None;
        self.search_visible = false;
        self.protocol_dropdown_open = false;
        self.protocol_kbd_active = false;
        self.confirm_remove = false;
        self.remove_confirm_kbd_active = false;
        self.inline_token_edit = None;
        self.key_error = None;
        self.form_error = None;
    }

    pub(super) fn current_scope(&self) -> Option<ModelScope> {
        (self.observed_models.is_some() || self.scope_saving || self.scope_pending)
            .then(|| self.enabled_scope.clone())
    }

    pub(super) fn mirror_external_scope(&mut self, scope: ModelScope, cx: &mut Context<Self>) {
        if self.scope_saving || self.scope_pending {
            return;
        }
        if self.enabled_scope != scope {
            self.enabled_scope = scope;
            cx.notify();
        }
    }

    fn sync_scope_from_snapshot(&mut self) {
        if self.scope_saving || self.scope_pending {
            return;
        }
        let snap = self.client.snapshot();
        let Some(models) = snap.models.as_ref() else {
            return;
        };
        if self
            .observed_models
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, models))
        {
            return;
        }
        self.observed_models = Some(models.clone());
        let next = ModelScope::from_snapshot(models);
        self.confirmed_scope = next.clone();
        if next != self.enabled_scope {
            self.enabled_scope = next;
        }
    }

    fn compute_auto_default(
        &self,
        snap_models: Option<&ModelsSnapshot>,
    ) -> Option<(String, String)> {
        let ready_keys: HashSet<(&str, &str)> = snap_models
            .into_iter()
            .flat_map(|m| &m.models)
            .filter(|m| m.credential_ready && m.catalog_present)
            .map(|m| (m.provider.as_str(), m.id.as_str()))
            .collect();
        let enabled_pairs: Vec<(String, String)> = self
            .enabled_scope
            .model_rows()
            .filter_map(|(_, full_id)| {
                let (prov, mid) = full_id.split_once('/')?;
                ready_keys
                    .contains(&(prov, mid))
                    .then(|| (prov.to_owned(), mid.to_owned()))
            })
            .collect();
        let current_default = snap_models
            .and_then(|m| m.default_model.as_ref())
            .map(|d| (d.provider.clone(), d.id.clone()));
        match (current_default, enabled_pairs.first()) {
            (Some(cur), Some(first)) if !enabled_pairs.contains(&cur) => Some(first.clone()),
            (None, Some(first)) => Some(first.clone()),
            _ => None,
        }
    }

    fn toggle_builtin_model_enabled(&mut self, full_id: String, cx: &mut Context<Self>) {
        self.sync_scope_from_snapshot();
        self.enabled_scope.toggle(full_id);
        self.enqueue_scope_save(cx);
    }

    fn enqueue_scope_save(&mut self, cx: &mut Context<Self>) {
        self.scope_revision = self.scope_revision.wrapping_add(1);
        self.scope_pending = true;
        cx.notify();

        if self.scope_saving {
            return;
        }
        self.scope_saving = true;
        let client = self.client.clone();

        cx.spawn(async move |weak, cx| {
            loop {
                let step = weak.update(cx, |this, _| {
                    if !this.scope_pending {
                        return None;
                    }
                    this.scope_pending = false;
                    let rev = this.scope_revision;
                    let scope_snapshot = this.enabled_scope.clone();
                    let patterns = scope_snapshot.patterns();
                    let snap = this.client.snapshot();
                    let auto_default = this.compute_auto_default(snap.models.as_deref());
                    Some((rev, scope_snapshot, patterns, auto_default))
                });
                let Ok(Some((rev, saved_scope, patterns, auto_default))) = step else {
                    let _ = weak.update(cx, |this, cx| {
                        this.scope_saving = false;
                        cx.notify();
                    });
                    break;
                };

                let res = client.set_explicit_model_scope(patterns).await;
                let default_res = if res.is_ok() {
                    if let Some((prov, mid)) = auto_default {
                        Some(client.set_default_model(prov, mid).await)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let action = weak
                    .update(cx, |this, cx| {
                        if res.is_ok() {
                            this.confirmed_scope = saved_scope;
                        }
                        if this.scope_pending || this.scope_revision != rev {
                            return 1u8;
                        }
                        if let Err(err) = res {
                            this.enabled_scope = this.confirmed_scope.clone();
                            this.scope_saving = false;
                            cx.emit(ProvidersTabEvent::ErrorMessage(format!(
                                "保存启用模型范围失败，已自动回滚：{err}"
                            )));
                            cx.notify();
                            return 0u8;
                        }
                        if let Some(Err(err)) = default_res {
                            cx.emit(ProvidersTabEvent::ErrorMessage(format!(
                                "同步默认模型失败：{err}"
                            )));
                        }
                        2u8
                    })
                    .unwrap_or(0);

                if action == 1 {
                    continue;
                }
                if action == 0 {
                    break;
                }

                let refreshed = client.refresh_models(None).await.is_ok();
                let again = weak
                    .update(cx, |this, cx| {
                        if this.scope_pending || this.scope_revision != rev {
                            return true;
                        }
                        this.scope_saving = false;
                        if refreshed {
                            this.sync_scope_from_snapshot();
                        }
                        cx.notify();
                        false
                    })
                    .unwrap_or(false);
                if !again {
                    break;
                }
            }
        })
        .detach();
    }

    fn select_provider(
        &mut self,
        provider_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snap = self.client.snapshot();
        let is_custom = snap
            .auth_providers
            .as_ref()
            .and_then(|list| list.iter().find(|p| p.id == provider_id))
            .and_then(|p| p.custom)
            .unwrap_or(false);

        self.page_slide.start(1.0, cx);
        self.reset_form();
        self.clear_key(window, cx);
        self.search_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.active_provider_id = Some(provider_id.clone());
        self.inline_edit_mode = is_custom;
        self.provider_id_input
            .update(cx, |i, cx| i.set_value(provider_id.clone(), window, cx));
        let pname = snap
            .auth_providers
            .as_ref()
            .and_then(|list| list.iter().find(|p| p.id == provider_id))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.provider_name_input
            .update(cx, |i, cx| i.set_value(pname, window, cx));
        self.base_url_input
            .update(cx, |i, cx| i.set_value("", window, cx));

        if is_custom {
            self.loading = true;
            let generation = self.generation;
            let client = self.client.clone();
            cx.spawn_in(window, async move |weak, cx| {
                let result = client.get_custom_provider(provider_id.clone()).await;
                let _ = weak.update_in(cx, |this, window, cx| {
                    if this.generation != generation {
                        return;
                    }
                    this.loading = false;
                    match result {
                        Ok(val)
                            if val.get("id").and_then(Value::as_str)
                                == Some(provider_id.as_str())
                                && val.get("revision").and_then(Value::as_str).is_some() =>
                        {
                            this.populate_from_custom_json(&val, window, cx);
                        }
                        _ => this.form_error = Some("无法读取服务配置，请返回列表重试。".into()),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn populate_from_custom_json(
        &mut self,
        val: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pid = val.get("id").and_then(Value::as_str).unwrap_or("");
        let name = val.get("name").and_then(Value::as_str).unwrap_or("");
        let base_url = val.get("baseUrl").and_then(Value::as_str).unwrap_or("");
        let api = val
            .get("api")
            .and_then(Value::as_str)
            .unwrap_or("openai-completions");
        self.revision = val
            .get("revision")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.protocol = CustomApiProtocol::from_wire(api);
        self.provider_id_input
            .update(cx, |i, cx| i.set_value(pid.to_owned(), window, cx));
        self.provider_name_input
            .update(cx, |i, cx| i.set_value(name.to_owned(), window, cx));
        self.base_url_input
            .update(cx, |i, cx| i.set_value(base_url.to_owned(), window, cx));

        self.editable_rows.clear();
        if let Some(arr) = val.get("models").and_then(Value::as_array) {
            for m in arr {
                let mid = m.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
                let mname = m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&mid)
                    .to_owned();
                let cw = m
                    .get("contextWindow")
                    .and_then(Value::as_u64)
                    .unwrap_or(128_000);
                let mt = m.get("maxTokens").and_then(Value::as_u64).unwrap_or(16_384);
                let reasoning = m.get("reasoning").and_then(Value::as_bool).unwrap_or(false);
                let image = m.get("image").and_then(Value::as_bool).unwrap_or(false);
                let id_inp = cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("模型 ID");
                    s.set_value(mid, window, cx);
                    s
                });
                let name_inp = cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("显示名称");
                    s.set_value(mname, window, cx);
                    s
                });
                self.editable_rows.push(EditableModelRow {
                    id_input: id_inp,
                    name_input: name_inp,
                    context_window: cw,
                    max_tokens: mt,
                    reasoning,
                    image,
                    is_new: false,
                    _subs: Vec::new(),
                });
            }
        }
        cx.notify();
    }

    fn append_blank_editable_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.inline_edit_mode || self.loading || self.saving {
            return;
        }
        self.form_error = None;

        let id_input = cx.new(|cx| InputState::new(window, cx).placeholder("模型 ID"));
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("显示名称"));
        let subs = vec![
            cx.subscribe(&id_input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&name_input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        id_input.update(cx, |i, cx| i.focus(window, cx));
        self.editable_rows.push(EditableModelRow {
            id_input,
            name_input,
            context_window: 128_000,
            max_tokens: 65_536,
            reasoning: true,
            image: false,
            is_new: true,
            _subs: subs,
        });
        cx.notify();
    }

    fn trigger_refresh_models(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.refresh_models_catalog().await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |_, cx| {
                match res {
                    Ok(_) => cx.emit(ProvidersTabEvent::Notice("模型目录已更新。".into())),
                    Err(err) => cx.emit(ProvidersTabEvent::ErrorMessage(err.to_string())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_active_provider_credentials(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.loading || (self.inline_edit_mode && self.revision.is_none()) {
            return;
        }
        let Some(provider) = self.active_provider_id.clone() else {
            return;
        };
        self.saving = true;
        self.key_error = None;
        cx.notify();

        let client = self.client.clone();
        let revision = self.revision.clone();
        let generation = self.generation;
        cx.spawn_in(window, async move |weak, cx| {
            let custom = revision.is_some();
            let res = match revision {
                Some(revision) => client.delete_custom_provider(provider, revision).await,
                None => client.logout_provider(provider).await,
            };
            if res.is_ok() {
                let _ = client.refresh_globals().await;
                let _ = client.refresh_models(None).await;
            }
            let _ = weak.update_in(cx, |this, window, cx| {
                this.saving = false;
                cx.notify();
                if this.generation != generation {
                    return;
                }
                match res {
                    Ok(_) => {
                        this.return_to_provider_list(window, cx);
                        cx.emit(ProvidersTabEvent::Notice(if custom {
                            "已删除自定义提供商。".into()
                        } else {
                            "已移除服务凭据。".into()
                        }));
                    }
                    Err(err) => {
                        this.confirm_remove = false;
                        this.remove_confirm_kbd_active = false;
                        this.remove_trigger_focus.focus(window, cx);
                        cx.emit(ProvidersTabEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_builtin_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.inline_edit_mode {
            return;
        }
        let Some(provider) = self.active_provider_id.clone() else {
            return;
        };
        let raw_key = self.read_key(cx);
        if raw_key.is_empty() {
            self.key_error = Some("请输入要更新的 API Key。".into());
            cx.notify();
            return;
        }
        self.key_error = None;
        let base_url = self.base_url_input.read(cx).value().trim().to_owned();
        self.saving = true;
        cx.notify();

        let generation = self.generation;
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let res = client
                .save_provider_api_key(
                    provider,
                    raw_key,
                    (!base_url.is_empty()).then_some(base_url),
                )
                .await;
            if res.is_ok() {
                let _ = client.refresh_globals().await;
                let _ = client.refresh_models(None).await;
            }
            let _ = weak.update_in(cx, |this, window, cx| {
                this.saving = false;
                cx.notify();
                if this.generation != generation {
                    return;
                }
                match res {
                    Ok(_) => {
                        this.clear_key(window, cx);
                        cx.emit(ProvidersTabEvent::CredentialsSaved);
                        cx.emit(ProvidersTabEvent::Notice("服务凭据已保存。".into()));
                    }
                    Err(ClientError::BadRequest { .. }) => {
                        this.key_error = Some("无法保存凭据，请检查 API Key 和服务地址。".into());
                    }
                    Err(_) => cx.emit(ProvidersTabEvent::ErrorMessage(
                        "保存凭据失败，请重试。".into(),
                    )),
                }
            });
        })
        .detach();
    }

    fn read_key(&self, cx: &gpui::App) -> Zeroizing<String> {
        let input = Zeroizing::new(self.api_key_input.read(cx).text().to_string());
        Zeroizing::new(input.trim().to_owned())
    }

    fn save_inline_provider_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_inline_token_edit(cx);
        self.protocol_dropdown_open = false;
        if self.saving
            || self.loading
            || !self.inline_edit_mode
            || (!self.creating_new_custom && self.revision.is_none())
        {
            return;
        }
        let pid = self
            .active_provider_id
            .clone()
            .unwrap_or_else(|| self.provider_id_input.read(cx).value().trim().to_owned());
        let name = self.provider_name_input.read(cx).value().trim().to_owned();
        let base_url = self.base_url_input.read(cx).value().trim().to_owned();
        let raw_key = self.read_key(cx);

        if pid.is_empty() || name.is_empty() || base_url.is_empty() {
            self.form_error = Some("请完整填写服务标识、显示名称与 Base URL。".into());
            cx.notify();
            return;
        }
        self.form_error = None;

        let mut models_arr = Vec::new();
        for row in &self.editable_rows {
            let mid = row.id_input.read(cx).value().trim().to_owned();
            if mid.is_empty() {
                continue;
            }
            let mname = {
                let n = row.name_input.read(cx).value().trim().to_owned();
                if n.is_empty() { mid.clone() } else { n }
            };
            models_arr.push(json!({
                "id": mid,
                "name": mname,
                "contextWindow": row.context_window,
                "maxTokens": row.max_tokens,
                "reasoning": row.reasoning,
                "image": row.image,
            }));
        }

        if models_arr.is_empty() {
            cx.emit(ProvidersTabEvent::ErrorMessage(
                "请至少保留或填写一行有效的模型配置。".into(),
            ));
            return;
        }

        let mut payload = serde_json::Map::new();
        payload.insert("id".into(), Value::String(pid.clone()));
        payload.insert("name".into(), Value::String(name));
        payload.insert("baseUrl".into(), Value::String(base_url));
        payload.insert(
            "api".into(),
            Value::String(self.protocol.wire_value().to_owned()),
        );
        let key = (!raw_key.is_empty()).then_some(raw_key);
        payload.insert("models".into(), Value::Array(models_arr));
        if let Some(ref rev) = self.revision {
            payload.insert("revision".into(), Value::String(rev.clone()));
        }

        self.saving = true;
        cx.notify();

        let client = self.client.clone();
        let generation = self.generation;
        cx.spawn_in(window, async move |weak, cx| {
            let res = client.save_custom_provider(payload, key).await;
            if res.is_ok() {
                let _ = client.refresh_globals().await;
                let _ = client.refresh_models(None).await;
            }
            let _ = weak.update_in(cx, |this, window, cx| {
                this.saving = false;
                cx.notify();
                if this.generation != generation {
                    return;
                }
                match res {
                    Ok(saved) => {
                        this.clear_key(window, cx);
                        this.revision = saved
                            .get("revision")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        this.creating_new_custom = false;
                        this.active_provider_id = Some(pid);
                        cx.emit(ProvidersTabEvent::CredentialsSaved);
                        cx.emit(ProvidersTabEvent::Notice("提供商配置已保存。".into()));
                    }
                    Err(ClientError::BadRequest { .. }) => {
                        this.form_error = Some("无法保存提供商，请检查配置和密钥。".into());
                    }
                    Err(_) => cx.emit(ProvidersTabEvent::ErrorMessage(
                        "保存提供商失败，请重试。".into(),
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_back_bar(
        &self,
        palette: &DesktopPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (hovered, hover_state) =
            motion::hairline_hover_state("providers-back-to-list", window, cx);
        let fg = motion::hairline_hover_color(
            "providers-back-to-list",
            hovered,
            palette.text_secondary,
            palette.text_primary,
            window,
            cx,
        );
        div().w_full().flex().items_center().child(
            apply_scale(div(), scale::UI_MEDIUM)
                .id("providers-back-to-list-btn")
                .test_support()
                .a11y_button("providers-back-to-list-btn", "返回提供商列表")
                .ml(px(-8.0))
                .h(height::CONTROL)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .text_color(fg)
                .flex()
                .items_center()
                .gap(space::XS)
                .hover(|s| s.bg(palette.bg_item_hover))
                .active(|s| {
                    s.bg(palette.bg_active_press)
                        .text_color(palette.text_primary)
                })
                .on_hover(move |&is_hovered, _, cx| {
                    motion::PointerFlag::set(&hover_state, is_hovered, cx);
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.return_to_provider_list(window, cx);
                }))
                .child(Icon::new(IconName::ArrowLeft).size(px(14.0)).text_color(fg))
                .child(cjk_inline_label("提供商")),
        )
    }
}

impl Render for ProvidersTabView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_scope_from_snapshot();
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let snap = self.client.snapshot();
        let kbd_style = snap.desktop_settings.keyboard_focus_style;
        let (slide_opacity, slide_dx) = self.page_slide.sample(window, cx);
        self.list_tracker.clear();

        // 1. Custom / Inline Provider Editor Mode (`FxSIA`)
        if self.creating_new_custom || self.inline_edit_mode {
            return div()
                .id("settings-providers-tab")
                .test_support()
                .a11y_region("settings-providers-tab", "提供商设置")
                .w_full()
                .relative()
                .left(slide_dx)
                .opacity(slide_opacity)
                .flex()
                .flex_col()
                .gap(space::GROUP)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.protocol_dropdown_open = false;
                        this.protocol_kbd_active = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::MD)
                        .child(self.render_back_bar(&palette, window, cx))
                        .child(
                            apply_scale(div(), scale::TITLE)
                                .text_color(palette.text_primary)
                                .child("自定义提供商"),
                        ),
                )
                .child(self.render_custom_provider_editor(mono_family, &palette, window, cx));
        }

        // 2. Add Provider Catalog Mode (`FRRjK`)
        if self.adding_provider {
            let providers = snap
                .auth_providers
                .as_ref()
                .map(|p| p.as_slice())
                .unwrap_or(&[]);
            let query = self.add_search_input.read(cx).value().trim().to_lowercase();
            let available: Vec<_> = providers
                .iter()
                .filter(|p| p.id != "cstoa" && p.configured != Some(true) && !p.requires_login)
                .filter(|p| {
                    query.is_empty()
                        || p.name.to_lowercase().contains(&query)
                        || p.id.to_lowercase().contains(&query)
                })
                .collect();
            let total_avail = available.len();
            const ADD_COLLAPSED_LIMIT: usize = 8;
            let can_collapse = query.is_empty() && total_avail > ADD_COLLAPSED_LIMIT;
            let visible_avail: Vec<_> = if can_collapse && !self.providers_expanded {
                available
                    .iter()
                    .take(ADD_COLLAPSED_LIMIT)
                    .copied()
                    .collect()
            } else {
                available
            };
            // Keyboard nav list: index 0 = 自定义提供商, 1..=visible_avail.len() = available providers
            let kbd_ids: Vec<Option<String>> = std::iter::once(None)
                .chain(visible_avail.iter().map(|p| Some(p.id.clone())))
                .collect();
            let total_kbd_items = kbd_ids.len();
            let list_focused = self.list_focus.is_focused(window);
            let active_spec =
                (list_focused && self.list_kbd_active && total_kbd_items > 0).then(|| {
                    KbdDynamicSpec::row(format!(
                        "add-row-{}",
                        self.list_selected_idx.min(total_kbd_items - 1)
                    ))
                    .with_radius(4.0)
                });
            let kbd_active = self.list_kbd_active;
            let search_focused = self
                .add_search_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
            let search_focus_ent = self.add_search_input.clone();

            let tr_custom = self.list_tracker.clone();
            let (custom_hovered, custom_hover_state) =
                motion::hairline_hover_state("providers-add-custom", window, cx);
            let custom_t = motion::hairline_hover_progress(
                "providers-add-custom",
                custom_hovered && !kbd_active,
                window,
                cx,
            );
            let custom_border =
                motion::lerp_hsla(palette.border_subtle, palette.accent_solid, custom_t);
            let custom_action_fg =
                motion::lerp_hsla(palette.text_secondary, palette.text_primary, custom_t);
            let custom_row = div()
                .id("providers-add-custom-row")
                .test_support()
                .a11y_button(
                    "providers-add-custom-row",
                    "自定义提供商，兼容 OpenAI / Anthropic / Gemini API",
                )
                .w_full()
                .h(px(52.0))
                .border_b_1()
                .border_color(custom_border)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::LG)
                .active(|s| s.border_color(palette.accent_hover))
                .on_hover(move |&is_hovered, _, cx| {
                    motion::PointerFlag::set(&custom_hover_state, is_hovered, cx);
                })
                .on_click(cx.listener(|this, _, w, cx| {
                    this.list_kbd_active = false;
                    this.open_new_custom_provider(w, cx);
                }))
                .on_prepaint(move |bounds, _, _| tr_custom.set("add-row-0", bounds))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::MD)
                        .min_w_0()
                        .child(
                            apply_scale(div(), scale::BODY)
                                .text_color(palette.text_primary)
                                .child("自定义提供商"),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .pt(px(2.0))
                                .truncate()
                                .text_color(palette.text_muted)
                                .child("兼容 OpenAI / Anthropic / Gemini API"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .flex_shrink_0()
                        .text_color(custom_action_fg)
                        .child(apply_scale(div(), scale::UI).child(cjk_inline_label("配置")))
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(14.0))
                                .text_color(custom_action_fg),
                        ),
                );

            let mut avail_rows = div().w_full().flex().flex_col();
            if visible_avail.is_empty() {
                avail_rows = avail_rows.child(
                    apply_scale(div(), scale::UI)
                        .py(space::LG)
                        .text_color(palette.text_muted)
                        .child("暂无匹配的提供商"),
                );
            } else {
                for (idx, prov) in visible_avail.iter().enumerate() {
                    let pid = prov.id.clone();
                    let tr = self.list_tracker.clone();
                    let track_key = SharedString::from(format!("add-row-{}", idx + 1));
                    let row_id = SharedString::from(format!("provider-add-row-{pid}"));
                    let auth_hint = if prov.supports_oauth && prov.supports_api_key {
                        "OAuth · API Key"
                    } else if prov.supports_oauth {
                        "OAuth"
                    } else {
                        "API Key"
                    };
                    let (row_hovered, row_hover_state) =
                        motion::hairline_hover_state(format!("providers-add-{pid}"), window, cx);
                    let row_t = motion::hairline_hover_progress(
                        format!("providers-add-{pid}"),
                        row_hovered && !kbd_active,
                        window,
                        cx,
                    );
                    let row_border =
                        motion::lerp_hsla(palette.border_subtle, palette.accent_solid, row_t);
                    let row_action_fg =
                        motion::lerp_hsla(palette.text_secondary, palette.text_primary, row_t);
                    avail_rows = avail_rows.child(
                        div()
                            .id(row_id.clone())
                            .test_support()
                            .a11y_listitem(row_id, format!("{}（{auth_hint}）", prov.name), false)
                            .w_full()
                            .h(px(48.0))
                            .border_b_1()
                            .border_color(row_border)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::LG)
                            .active(|s| s.border_color(palette.accent_hover))
                            .on_hover(move |&is_hovered, _, cx| {
                                motion::PointerFlag::set(&row_hover_state, is_hovered, cx);
                            })
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.list_kbd_active = false;
                                this.select_provider(pid.clone(), w, cx);
                            }))
                            .on_prepaint(move |bounds, _, _| tr.set(track_key.clone(), bounds))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(space::MD)
                                    .min_w_0()
                                    .child(
                                        apply_scale(div(), scale::BODY)
                                            .truncate()
                                            .text_color(palette.text_primary)
                                            .child(SharedString::from(prov.name.clone())),
                                    )
                                    .child(
                                        apply_scale(div(), scale::CAPTION)
                                            .pt(px(2.0))
                                            .flex_shrink_0()
                                            .text_color(palette.text_muted)
                                            .child(auth_hint),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(space::XS)
                                    .flex_shrink_0()
                                    .text_color(row_action_fg)
                                    .child(
                                        apply_scale(div(), scale::UI)
                                            .child(cjk_inline_label("连接")),
                                    )
                                    .child(
                                        Icon::new(IconName::ChevronRight)
                                            .size(px(14.0))
                                            .text_color(row_action_fg),
                                    ),
                            ),
                    );
                }
                if can_collapse {
                    let expanded = self.providers_expanded;
                    let toggle_label = if expanded {
                        SharedString::from("收起列表")
                    } else {
                        SharedString::from(format!("查看全部提供商（{total_avail}）"))
                    };
                    let (exp_hovered, exp_hover_state) =
                        motion::hairline_hover_state("providers-add-expand", window, cx);
                    let exp_t = motion::hairline_hover_progress(
                        "providers-add-expand",
                        exp_hovered,
                        window,
                        cx,
                    );
                    let exp_border =
                        motion::lerp_hsla(palette.border_subtle, palette.accent_solid, exp_t);
                    let exp_fg =
                        motion::lerp_hsla(palette.text_secondary, palette.text_primary, exp_t);
                    avail_rows = avail_rows.child(
                        apply_scale(div(), scale::UI)
                            .id("providers-toggle-expand-btn")
                            .test_support()
                            .a11y_button("providers-toggle-expand-btn", toggle_label.clone())
                            .w_full()
                            .h(height::HEADER)
                            .border_b_1()
                            .border_color(exp_border)
                            .text_color(exp_fg)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .active(|s| {
                                s.border_color(palette.accent_hover)
                                    .text_color(palette.text_primary)
                            })
                            .on_hover(move |&is_hovered, _, cx| {
                                motion::PointerFlag::set(&exp_hover_state, is_hovered, cx);
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.providers_expanded = !this.providers_expanded;
                                cx.notify();
                            }))
                            .child(cjk_inline_label(toggle_label))
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(px(14.0))
                                .text_color(exp_fg),
                            ),
                    );
                }
            }

            return div()
                .id("settings-providers-tab")
                .test_support()
                .a11y_region("settings-providers-tab", "添加提供商")
                .relative()
                .left(slide_dx)
                .opacity(slide_opacity)
                .track_focus(&self.list_focus)
                .tab_index(0)
                .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, w, cx| {
                    if !this.list_focus.is_focused(w) {
                        return;
                    }
                    match resolve_menu_nav_key(
                        ev.keystroke.key.as_str(),
                        ev.keystroke.modifiers,
                        ev.is_held,
                        this.list_selected_idx,
                        kbd_ids.len(),
                        false,
                    ) {
                        KbdMenuStep::Move(next) => {
                            this.list_selected_idx = next;
                            this.list_kbd_active = true;
                            this.list_last_pointer = Some(w.mouse_position());
                            cx.stop_propagation();
                            cx.notify();
                        }
                        KbdMenuStep::Activate(idx) => {
                            cx.stop_propagation();
                            match kbd_ids.get(idx).and_then(|x| x.clone()) {
                                None => this.open_new_custom_provider(w, cx),
                                Some(pid) => this.select_provider(pid, w, cx),
                            }
                        }
                        _ => {}
                    }
                }))
                .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                    if this.list_kbd_active
                        && pointer_moved_past_threshold(&mut this.list_last_pointer, ev.position)
                    {
                        this.list_kbd_active = false;
                        cx.notify();
                    }
                }))
                .w_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::MD)
                        .child(self.render_back_bar(&palette, window, cx))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    apply_scale(div(), scale::TITLE)
                                        .text_color(palette.text_primary)
                                        .child("添加提供商"),
                                )
                                .child(
                                    apply_scale(div(), scale::UI)
                                        .id("providers-add-refresh-btn")
                                        .test_support()
                                        .a11y_button("providers-add-refresh-btn", "更新模型")
                                        .mr(px(-8.0))
                                        .h(height::COMPACT)
                                        .px(space::SM)
                                        .rounded(radius::CONTROL)
                                        .text_color(palette.text_secondary)
                                        .flex()
                                        .items_center()
                                        .gap(space::XS)
                                        .hover(|s| {
                                            s.bg(palette.bg_item_hover)
                                                .text_color(palette.text_primary)
                                        })
                                        .active(|s| s.bg(palette.bg_active_press))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.trigger_refresh_models(cx);
                                        }))
                                        .child(Icon::new(IconName::RefreshCw).size(px(13.0)))
                                        .child(cjk_inline_label("更新模型")),
                                ),
                        ),
                )
                .child(div().mt(space::MD).w_full().child(custom_row))
                .child(
                    div()
                        .mt(px(32.0))
                        .w_full()
                        .flex()
                        .flex_col()
                        .child(
                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                .mb(space::SM)
                                .text_color(palette.text_primary)
                                .child("可添加的提供商"),
                        )
                        .child(
                            div()
                                .id("providers-add-search-box")
                                .w_full()
                                .h(px(40.0))
                                .border_b_1()
                                .border_color(if search_focused {
                                    palette.accent_solid
                                } else {
                                    palette.border_subtle
                                })
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .on_click(move |_, w, cx| {
                                    search_focus_ent.focus_handle(cx).focus(w, cx);
                                })
                                .child(
                                    Icon::new(IconName::Search)
                                        .size(px(14.0))
                                        .text_color(palette.text_muted),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.add_search_input)
                                            .accessibility_id("providers-add-search-input")
                                            .aria_label("搜索提供商")
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .p_0(),
                                    ),
                                ),
                        )
                        .child(avail_rows),
                )
                .child(render_gliding_dynamic_indicator(
                    "providers-add-list",
                    kbd_style,
                    active_spec,
                    self.list_tracker.clone(),
                    &palette,
                    window,
                    cx,
                ));
        }

        // 3. Built-in Provider Detail / Credential Management Mode (`QOtGr` / `J9tNX3` / `xogtT`)
        if let Some(ref pid) = self.active_provider_id {
            let pid_owned = pid.clone();
            let prov_entry = snap
                .auth_providers
                .as_ref()
                .and_then(|list| list.iter().find(|p| p.id == pid_owned));
            let prov_name = prov_entry
                .map(|p| p.name.clone())
                .unwrap_or_else(|| pid_owned.clone());
            let is_configured = prov_entry.and_then(|p| p.configured) == Some(true);
            let filter_q = self.search_input.read(cx).value().trim().to_lowercase();
            let all_provider_models: Vec<_> = snap
                .models
                .as_ref()
                .map(|m| {
                    m.catalog
                        .iter()
                        .filter(|mc| mc.provider == pid_owned)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let total_models_count = all_provider_models.len();
            let enabled_set: HashSet<String> = self
                .enabled_scope
                .model_rows()
                .map(|(_, id)| id.to_owned())
                .collect();
            let enabled_in_provider = if is_configured {
                all_provider_models
                    .iter()
                    .filter(|m| enabled_set.contains(&format!("{}/{}", m.provider, m.id)))
                    .count()
            } else {
                0
            };
            let catalog_models: Vec<_> = all_provider_models
                .into_iter()
                .filter(|mc| {
                    filter_q.is_empty()
                        || mc.id.to_lowercase().contains(&filter_q)
                        || mc.name.to_lowercase().contains(&filter_q)
                })
                .collect();

            let url_focused = self
                .base_url_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
            let key_focused = self
                .api_key_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);

            let mut model_rows = div().w_full().flex().flex_col();
            if catalog_models.is_empty() {
                model_rows = model_rows.child(
                    apply_scale(div(), scale::UI)
                        .py(space::LG)
                        .text_color(palette.text_muted)
                        .child("暂无匹配的可用模型"),
                );
            } else {
                for m in &catalog_models {
                    let full_id = format!("{}/{}", m.provider, m.id);
                    let is_enabled = is_configured && enabled_set.contains(&full_id);
                    let fid_row = full_id.clone();
                    let fid_chk = full_id.clone();
                    let row_id = SharedString::from(format!("provider-model-row-{}", m.id));
                    let chk_id = SharedString::from(format!("provider-model-chk-{}", m.id));
                    let display_name = if m.name.trim().is_empty() {
                        m.id.clone()
                    } else {
                        m.name.clone()
                    };
                    let ctx_spec = format!(
                        "{} 上下文",
                        format_token_window(m.context_window.unwrap_or(128_000))
                    );
                    let chk_mark = motion::animated_checkbox_mark(
                        format!("prov-chk-{full_id}"),
                        is_enabled,
                        px(11.0),
                        palette.text_on_accent,
                        window,
                        cx,
                    );
                    let (row_hovered, row_hover_state) = motion::hairline_hover_state(
                        format!("provider-model-{full_id}"),
                        window,
                        cx,
                    );
                    let row_border = motion::hairline_hover_color(
                        format!("provider-model-{full_id}"),
                        is_configured && row_hovered,
                        palette.border_subtle,
                        palette.accent_solid,
                        window,
                        cx,
                    );
                    model_rows = model_rows.child(
                        div()
                            .id(row_id)
                            .w_full()
                            .h(px(52.0))
                            .border_b_1()
                            .border_color(row_border)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::MD)
                            .active(move |s| {
                                if is_configured {
                                    s.border_color(palette.accent_hover)
                                } else {
                                    s
                                }
                            })
                            .on_hover(move |&is_hovered, _, cx| {
                                motion::PointerFlag::set(&row_hover_state, is_hovered, cx);
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if is_configured {
                                    this.toggle_builtin_model_enabled(fid_row.clone(), cx);
                                }
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(space::MD)
                                    .child(
                                        div()
                                            .id(chk_id.clone())
                                            .test_support()
                                            .a11y_checkbox(
                                                chk_id,
                                                SharedString::from(format!(
                                                    "启用模型：{display_name}"
                                                )),
                                                is_enabled,
                                            )
                                            .w(px(16.0))
                                            .h(px(16.0))
                                            .rounded(radius::CONTROL)
                                            .border_1()
                                            .opacity(if is_configured { 1.0 } else { 0.45 })
                                            .border_color(if is_enabled {
                                                palette.accent_solid
                                            } else {
                                                palette.border_strong
                                            })
                                            .bg(if is_enabled {
                                                palette.accent_solid
                                            } else {
                                                palette.bg_workspace
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .flex_shrink_0()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if is_configured {
                                                    this.toggle_builtin_model_enabled(
                                                        fid_chk.clone(),
                                                        cx,
                                                    );
                                                }
                                                cx.stop_propagation();
                                            }))
                                            .children(chk_mark),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap(space::MD)
                                            .child(
                                                apply_scale(div(), scale::BODY)
                                                    .truncate()
                                                    .text_color(if is_enabled {
                                                        palette.text_primary
                                                    } else {
                                                        palette.text_secondary
                                                    })
                                                    .child(SharedString::from(display_name)),
                                            )
                                            .child(
                                                apply_scale(div(), scale::CAPTION)
                                                    .pt(px(2.0))
                                                    .truncate()
                                                    .text_color(palette.text_muted)
                                                    .child(SharedString::from(m.id.clone())),
                                            ),
                                    ),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .flex_shrink_0()
                                    .text_color(palette.text_muted)
                                    .child(SharedString::from(ctx_spec)),
                            ),
                    );
                }
            }

            return div()
                .id("settings-providers-tab")
                .test_support()
                .a11y_region("settings-providers-tab", "提供商设置")
                .w_full()
                .relative()
                .left(slide_dx)
                .opacity(slide_opacity)
                .flex()
                .flex_col()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::MD)
                        .child(self.render_back_bar(&palette, window, cx))
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .gap(space::MD)
                                .child(
                                    apply_scale(div(), scale::TITLE)
                                        .text_color(palette.text_primary)
                                        .child(SharedString::from(prov_name)),
                                )
                                .children(is_configured.then(|| {
                                    div()
                                        .pb(px(3.0))
                                        .flex()
                                        .items_center()
                                        .gap(space::TIGHT)
                                        .child(
                                            div()
                                                .w(px(6.0))
                                                .h(px(6.0))
                                                .rounded(radius::FULL)
                                                .bg(palette.status_success),
                                        )
                                        .child(
                                            apply_scale(div(), scale::CAPTION)
                                                .text_color(palette.text_secondary)
                                                .child(cjk_inline_label("已连接")),
                                        )
                                })),
                        ),
                )
                // 24px gap -> frameless 52px hairline rows (no `连接与凭据` heading)
                .child(
                    div()
                        .mt(px(24.0))
                        .w_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .w_full()
                                .h(px(52.0))
                                .border_b_1()
                                .border_color(if url_focused {
                                    palette.accent_solid
                                } else {
                                    palette.border_subtle
                                })
                                .flex()
                                .items_center()
                                .gap(space::LG)
                                .child(
                                    apply_scale(div(), scale::UI)
                                        .w(px(184.0))
                                        .flex_shrink_0()
                                        .text_color(palette.text_secondary)
                                        .child(cjk_inline_label("服务地址")),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.base_url_input)
                                            .accessibility_id("providers-baseurl-input")
                                            .aria_label("服务地址")
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .p_0(),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(52.0))
                                .border_b_1()
                                .border_color(if self.key_error.is_some() {
                                    palette.status_danger
                                } else if key_focused {
                                    palette.accent_solid
                                } else {
                                    palette.border_subtle
                                })
                                .flex()
                                .items_center()
                                .gap(space::LG)
                                .child(
                                    apply_scale(div(), scale::UI)
                                        .w(px(184.0))
                                        .flex_shrink_0()
                                        .text_color(palette.text_secondary)
                                        .child(cjk_inline_label("API 密钥")),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.api_key_input)
                                            .accessibility_id("providers-apikey-input")
                                            .content_type(InputContentType::Password)
                                            .aria_label("服务 API Key")
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .p_0()
                                            .mask_toggle(),
                                    ),
                                )
                                .child(
                                    apply_scale(div(), scale::UI_MEDIUM)
                                        .id("providers-builtin-save-key-btn")
                                        .test_support()
                                        .a11y_button(
                                            "providers-builtin-save-key-btn",
                                            if self.saving {
                                                "正在保存"
                                            } else {
                                                "保存密钥"
                                            },
                                        )
                                        .mr(px(-8.0))
                                        .h(height::COMPACT)
                                        .px(space::SM)
                                        .rounded(radius::CONTROL)
                                        .text_color(palette.accent_ink)
                                        .flex()
                                        .items_center()
                                        .flex_shrink_0()
                                        .hover(|s| s.bg(palette.bg_item_hover))
                                        .active(|s| s.bg(palette.bg_active_press))
                                        .on_click(cx.listener(|this, _, w, cx| {
                                            this.save_builtin_api_key(w, cx);
                                        }))
                                        .child(cjk_inline_label(if self.saving {
                                            "正在保存…"
                                        } else {
                                            "保存密钥"
                                        })),
                                ),
                        )
                        .children(self.key_error.clone().map(|err| {
                            apply_scale(div(), scale::CAPTION)
                                .id("providers-apikey-error-text")
                                .test_support()
                                .a11y_status("providers-apikey-error-text", err.clone())
                                .mt(space::XS)
                                .text_color(palette.status_danger)
                                .child(err)
                        })),
                )
                // 40px gap -> 可用模型
                .child(
                    div()
                        .mt(px(40.0))
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::SM)
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(space::SM)
                                        .child(
                                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                                .text_color(palette.text_primary)
                                                .child("可用模型"),
                                        )
                                        .child(
                                            apply_scale(div(), scale::CAPTION)
                                                .pt(px(2.0))
                                                .text_color(palette.text_muted)
                                                .child(SharedString::from(format!(
                                                    "{enabled_in_provider} / {total_models_count}"
                                                ))),
                                        ),
                                )
                                .child(
                                    div()
                                        .mr(px(-4.0))
                                        .flex()
                                        .items_center()
                                        .gap(space::XS)
                                        .child(
                                            div()
                                                .id("providers-hdr-search-btn")
                                                .test_support()
                                                .a11y_toggle_button(
                                                    "providers-hdr-search-btn",
                                                    "搜索模型",
                                                    self.search_visible,
                                                )
                                                .w(height::COMPACT)
                                                .h(height::COMPACT)
                                                .rounded(radius::CONTROL)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .hover(|s| s.bg(palette.bg_item_hover))
                                                .active(|s| s.bg(palette.bg_active_press))
                                                .on_click(cx.listener(|this, _, w, cx| {
                                                    this.search_visible = !this.search_visible;
                                                    if this.search_visible {
                                                        this.search_input
                                                            .update(cx, |i, cx| i.focus(w, cx));
                                                    }
                                                    cx.notify();
                                                }))
                                                .child(
                                                    Icon::new(IconName::Search)
                                                        .size(px(14.0))
                                                        .text_color(palette.text_secondary),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("providers-hdr-refresh-btn")
                                                .test_support()
                                                .a11y_button(
                                                    "providers-hdr-refresh-btn",
                                                    "刷新模型目录",
                                                )
                                                .w(height::COMPACT)
                                                .h(height::COMPACT)
                                                .rounded(radius::CONTROL)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .hover(|s| s.bg(palette.bg_item_hover))
                                                .active(|s| s.bg(palette.bg_active_press))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.trigger_refresh_models(cx);
                                                }))
                                                .child(
                                                    Icon::new(IconName::RefreshCw)
                                                        .size(px(14.0))
                                                        .text_color(palette.text_secondary),
                                                ),
                                        ),
                                ),
                        )
                        .children(self.search_visible.then(|| {
                            let search_focused = self
                                .search_input
                                .read(cx)
                                .focus_handle(cx)
                                .is_focused(window);
                            let search_focus_ent = self.search_input.clone();
                            div()
                                .id("providers-builtin-search-box")
                                .w_full()
                                .h(px(40.0))
                                .border_b_1()
                                .border_color(if search_focused {
                                    palette.accent_solid
                                } else {
                                    palette.border_subtle
                                })
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .on_click(move |_, w, cx| {
                                    search_focus_ent.focus_handle(cx).focus(w, cx);
                                })
                                .child(
                                    Icon::new(IconName::Search)
                                        .size(px(14.0))
                                        .text_color(palette.text_muted),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.search_input)
                                            .accessibility_id("providers-search-input")
                                            .aria_label("筛选模型")
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .p_0(),
                                    ),
                                )
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .text_color(palette.text_muted)
                                        .child(SharedString::from(format!(
                                            "{} / {total_models_count}",
                                            catalog_models.len()
                                        ))),
                                )
                                .child(
                                    div()
                                        .id("providers-builtin-search-close")
                                        .w(px(20.0))
                                        .h(px(20.0))
                                        .rounded(radius::CONTROL)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .hover(|s| s.bg(palette.bg_item_hover))
                                        .on_click(cx.listener(|this, _, w, cx| {
                                            this.search_input
                                                .update(cx, |i, cx| i.set_value("", w, cx));
                                            this.search_visible = false;
                                            cx.notify();
                                            cx.stop_propagation();
                                        }))
                                        .child(
                                            Icon::new(IconName::X)
                                                .size(px(13.0))
                                                .text_color(palette.text_muted),
                                        ),
                                )
                        }))
                        .child(model_rows),
                )
                .children(is_configured.then(|| {
                    div().mt(px(40.0)).w_full().child(
                        self.render_remove_credentials_section(&palette, kbd_style, window, cx),
                    )
                }));
        }

        // 4. Configured Providers List (`Q4QFim`) — frameless 64px hairline rows, no repeated H1
        let providers = snap
            .auth_providers
            .as_ref()
            .map(|p| p.as_slice())
            .unwrap_or(&[]);
        let cstoa_requires_login = providers
            .iter()
            .find(|p| p.id == "cstoa")
            .is_some_and(|p| p.requires_login);
        let cstoa_signed_in =
            snap.account.as_ref().is_some_and(|a| a.signed_in) && !cstoa_requires_login;
        let (cstoa_meta, cstoa_warning) =
            format_cstoa_status(snap.account.as_deref(), cstoa_requires_login);
        let cstoa_meta_color = if cstoa_warning {
            palette.status_warning
        } else {
            palette.text_secondary
        };

        let configured_others: Vec<_> = providers
            .iter()
            .filter(|p| p.id != "cstoa" && (p.configured == Some(true) || p.requires_login))
            .collect();
        #[derive(Clone)]
        enum ProviderListTarget {
            Cstoa,
            Configured(String),
            AddProvider,
        }
        let kbd_targets: Vec<ProviderListTarget> = std::iter::once(ProviderListTarget::Cstoa)
            .chain(
                configured_others
                    .iter()
                    .map(|p| ProviderListTarget::Configured(p.id.clone())),
            )
            .chain(std::iter::once(ProviderListTarget::AddProvider))
            .collect();
        let clamped_idx = self.list_selected_idx.min(kbd_targets.len() - 1);
        let list_focused = self.list_focus.is_focused(window);
        let active_spec =
            (list_focused && self.list_kbd_active && !kbd_targets.is_empty()).then(|| {
                match kbd_targets[clamped_idx] {
                    ProviderListTarget::AddProvider => {
                        KbdDynamicSpec::control("cfg-row-add", KbdRailPlacement::BottomCenter)
                    }
                    _ => KbdDynamicSpec::row(format!("cfg-row-{clamped_idx}")).with_radius(4.0),
                }
            });
        let kbd_active = self.list_kbd_active;
        let tr_cstoa = self.list_tracker.clone();
        let (cstoa_hovered, cstoa_hover_state) =
            motion::hairline_hover_state("providers-cstoa-row", window, cx);
        let cstoa_t = motion::hairline_hover_progress(
            "providers-cstoa-row",
            cstoa_hovered && !kbd_active,
            window,
            cx,
        );
        let cstoa_border = motion::lerp_hsla(palette.border_subtle, palette.accent_solid, cstoa_t);
        let cstoa_action_fg =
            motion::lerp_hsla(palette.text_secondary, palette.text_primary, cstoa_t);

        let mut rows_list = div().w_full().flex().flex_col().child(
            div()
                .id("providers-cstoa-pinned-card")
                .test_support()
                .w_full()
                .h(px(64.0))
                .border_b_1()
                .border_color(cstoa_border)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::LG)
                .active(|s| s.border_color(palette.accent_hover))
                .on_hover(move |&is_hovered, _, cx| {
                    motion::PointerFlag::set(&cstoa_hover_state, is_hovered, cx);
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.list_kbd_active = false;
                    cx.emit(ProvidersTabEvent::OpenLoginOrAccount {
                        signed_in: cstoa_signed_in,
                    });
                }))
                .on_prepaint(move |bounds, _, _| tr_cstoa.set("cfg-row-0", bounds))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space::MICRO)
                        .min_w_0()
                        .child(
                            apply_scale(div(), scale::BODY)
                                .text_color(palette.text_primary)
                                .child("计维CSTOA"),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .text_color(cstoa_meta_color)
                                .child(SharedString::from(cstoa_meta)),
                        ),
                )
                .child(
                    div()
                        .id("providers-cstoa-action-btn")
                        .test_support()
                        .a11y_button(
                            "providers-cstoa-action-btn",
                            if cstoa_signed_in {
                                "管理 计维CSTOA 账号"
                            } else {
                                "扫码登录 计维CSTOA"
                            },
                        )
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .text_color(cstoa_action_fg)
                        .child(apply_scale(div(), scale::UI).child(cjk_inline_label(
                            if cstoa_signed_in {
                                "管理"
                            } else {
                                "扫码登录"
                            },
                        )))
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(14.0))
                                .text_color(cstoa_action_fg),
                        ),
                ),
        );

        for (idx, prov) in configured_others.iter().enumerate() {
            let pid = prov.id.clone();
            let pid_btn = pid.clone();
            let tr = self.list_tracker.clone();
            let track_key = SharedString::from(format!("cfg-row-{}", idx + 1));
            let (status_label, status_color, action_label) = if prov.requires_login {
                ("凭据已失效，请重新配置", palette.status_warning, "重新登录")
            } else {
                ("已连接", palette.text_secondary, "管理")
            };
            let row_id = SharedString::from(format!("provider-row-{pid}"));
            let manage_id = SharedString::from(format!("provider-manage-btn-{pid_btn}"));
            let (row_hovered, row_hover_state) =
                motion::hairline_hover_state(format!("provider-cfg-{pid}"), window, cx);
            let row_t = motion::hairline_hover_progress(
                format!("provider-cfg-{pid}"),
                row_hovered && !kbd_active,
                window,
                cx,
            );
            let row_border = motion::lerp_hsla(palette.border_subtle, palette.accent_solid, row_t);
            let row_action_fg =
                motion::lerp_hsla(palette.text_secondary, palette.text_primary, row_t);
            rows_list = rows_list.child(
                div()
                    .id(row_id.clone())
                    .test_support()
                    .a11y_listitem(row_id, format!("{}（{status_label}）", prov.name), false)
                    .w_full()
                    .h(px(64.0))
                    .border_b_1()
                    .border_color(row_border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::LG)
                    .active(|s| s.border_color(palette.accent_hover))
                    .on_hover(move |&is_hovered, _, cx| {
                        motion::PointerFlag::set(&row_hover_state, is_hovered, cx);
                    })
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.list_kbd_active = false;
                        this.select_provider(pid.clone(), w, cx);
                    }))
                    .on_prepaint(move |bounds, _, _| tr.set(track_key.clone(), bounds))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::MICRO)
                            .min_w_0()
                            .child(
                                apply_scale(div(), scale::BODY)
                                    .text_color(palette.text_primary)
                                    .child(SharedString::from(prov.name.clone())),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(status_color)
                                    .child(status_label),
                            ),
                    )
                    .child(
                        div()
                            .id(manage_id.clone())
                            .test_support()
                            .a11y_button(manage_id, format!("{action_label} {}", prov.name))
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .text_color(row_action_fg)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.list_kbd_active = false;
                                this.select_provider(pid_btn.clone(), w, cx);
                                cx.stop_propagation();
                            }))
                            .child(
                                apply_scale(div(), scale::UI).child(cjk_inline_label(action_label)),
                            )
                            .child(
                                Icon::new(IconName::ChevronRight)
                                    .size(px(14.0))
                                    .text_color(row_action_fg),
                            ),
                    ),
            );
        }

        div()
            .id("settings-providers-tab")
            .test_support()
            .a11y_region("settings-providers-tab", "提供商设置")
            .relative()
            .left(slide_dx)
            .opacity(slide_opacity)
            .track_focus(&self.list_focus)
            .tab_index(0)
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, w, cx| {
                if !this.list_focus.is_focused(w) {
                    return;
                }
                match resolve_menu_nav_key(
                    ev.keystroke.key.as_str(),
                    ev.keystroke.modifiers,
                    ev.is_held,
                    this.list_selected_idx,
                    kbd_targets.len(),
                    false,
                ) {
                    KbdMenuStep::Move(next) => {
                        this.list_selected_idx = next;
                        this.list_kbd_active = true;
                        this.list_last_pointer = Some(w.mouse_position());
                        cx.stop_propagation();
                        cx.notify();
                    }
                    KbdMenuStep::Activate(idx) => {
                        cx.stop_propagation();
                        this.return_focus_idx = idx;
                        match kbd_targets.get(idx) {
                            Some(ProviderListTarget::Cstoa) => {
                                cx.emit(ProvidersTabEvent::OpenLoginOrAccount {
                                    signed_in: cstoa_signed_in,
                                });
                            }
                            Some(ProviderListTarget::Configured(pid)) => {
                                this.select_provider(pid.clone(), w, cx);
                            }
                            Some(ProviderListTarget::AddProvider) => {
                                this.open_add_provider_catalog(w, cx);
                            }
                            None => {}
                        }
                    }
                    _ => {}
                }
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.list_kbd_active
                    && pointer_moved_past_threshold(&mut this.list_last_pointer, ev.position)
                {
                    this.list_kbd_active = false;
                    cx.notify();
                }
            }))
            .w_full()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child("已配置"),
                    )
                    .child({
                        let tr_add = self.list_tracker.clone();
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-new-custom-btn")
                            .test_support()
                            .a11y_button("providers-new-custom-btn", "添加提供商")
                            .tab_index(0)
                            .mr(px(-8.0))
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .hover(move |s| {
                                if kbd_active {
                                    s
                                } else {
                                    s.bg(palette.bg_item_hover)
                                }
                            })
                            .active(|s| s.bg(palette.bg_active_press))
                            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                                if !ev.is_held
                                    && ev.keystroke.modifiers == gpui::Modifiers::default()
                                    && matches!(ev.keystroke.key.as_str(), "enter" | "space")
                                {
                                    cx.stop_propagation();
                                    this.open_add_provider_catalog(w, cx);
                                }
                            }))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.list_kbd_active = false;
                                this.open_add_provider_catalog(w, cx);
                            }))
                            .on_prepaint(move |bounds, _, _| tr_add.set("cfg-row-add", bounds))
                            .child(
                                Icon::new(IconName::Plus)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("添加提供商"))
                    }),
            )
            .child(rows_list)
            .child(render_gliding_dynamic_indicator(
                "providers-configured-list",
                kbd_style,
                active_spec,
                self.list_tracker.clone(),
                &palette,
                window,
                cx,
            ))
    }
}

impl ProvidersTabView {
    pub(super) fn render_remove_credentials_section(
        &self,
        palette: &DesktopPalette,
        kbd_style: cst_client::KeyboardFocusStyleDto,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.remove_confirm_tracker.clear();
        let cancel_focused = self.remove_cancel_focus.is_focused(window);
        let confirm_focused = self.remove_confirm_focus.is_focused(window);
        let confirm_kbd_active = self.confirm_remove
            && self.remove_confirm_kbd_active
            && (cancel_focused || confirm_focused);
        let active_spec = confirm_kbd_active.then(|| {
            if confirm_focused {
                KbdDynamicSpec::control("rm-confirm", KbdRailPlacement::BottomCenter)
                    .with_tone(KbdTone::Danger)
            } else {
                KbdDynamicSpec::control("rm-cancel", KbdRailPlacement::BottomCenter)
            }
        });
        let right_el = if self.confirm_remove {
            let tr_cancel = self.remove_confirm_tracker.clone();
            let tr_confirm = self.remove_confirm_tracker.clone();
            div()
                .relative()
                .flex()
                .items_center()
                .gap(space::SM)
                .flex_shrink_0()
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                    let cur = usize::from(this.remove_confirm_focus.is_focused(w));
                    match resolve_dialog_nav_key(
                        ev.keystroke.key.as_str(),
                        ev.keystroke.modifiers,
                        ev.is_held,
                        cur,
                        2,
                    ) {
                        DialogNavStep::Move(next) => {
                            this.remove_confirm_kbd_active = true;
                            this.remove_confirm_last_pointer = Some(w.mouse_position());
                            if next == 0 {
                                this.remove_cancel_focus.focus(w, cx);
                            } else {
                                this.remove_confirm_focus.focus(w, cx);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                        DialogNavStep::Activate(slot) => {
                            cx.stop_propagation();
                            if slot == 0 {
                                this.cancel_remove_confirm(w, cx);
                            } else {
                                this.remove_active_provider_credentials(w, cx);
                            }
                        }
                        DialogNavStep::TabActivated => {
                            this.remove_confirm_kbd_active = true;
                            this.remove_confirm_last_pointer = Some(w.mouse_position());
                            cx.notify();
                        }
                        DialogNavStep::Ignore => {}
                    }
                }))
                .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                    if this.remove_confirm_kbd_active
                        && pointer_moved_past_threshold(
                            &mut this.remove_confirm_last_pointer,
                            ev.position,
                        )
                    {
                        this.remove_confirm_kbd_active = false;
                        cx.notify();
                    }
                }))
                .child(
                    apply_scale(div(), scale::UI)
                        .id("providers-remove-cancel-btn")
                        .test_support()
                        .a11y_button("providers-remove-cancel-btn", "取消移除")
                        .track_focus(&self.remove_cancel_focus)
                        .tab_index(0)
                        .h(height::COMPACT)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .hover(move |s| {
                            if confirm_kbd_active {
                                s
                            } else {
                                s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                            }
                        })
                        .active(|s| s.bg(palette.bg_active_press))
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.cancel_remove_confirm(w, cx);
                        }))
                        .on_prepaint(move |bounds, _, _| tr_cancel.set("rm-cancel", bounds))
                        .child(cjk_inline_label("取消")),
                )
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("providers-remove-confirm-btn")
                        .test_support()
                        .a11y_button("providers-remove-confirm-btn", "确认移除")
                        .track_focus(&self.remove_confirm_focus)
                        .tab_index(0)
                        .mr(px(-8.0))
                        .h(height::COMPACT)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.status_danger)
                        .flex()
                        .items_center()
                        .hover(move |s| {
                            if confirm_kbd_active {
                                s
                            } else {
                                s.bg(palette.status_danger_subtle)
                            }
                        })
                        .active(|s| s.bg(palette.status_danger_subtle).opacity(0.85))
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.remove_active_provider_credentials(w, cx);
                        }))
                        .on_prepaint(move |bounds, _, _| tr_confirm.set("rm-confirm", bounds))
                        .child(cjk_inline_label(if self.saving {
                            "移除中…"
                        } else {
                            "确认移除"
                        })),
                )
                .child(render_gliding_dynamic_indicator(
                    "providers-remove-confirm",
                    kbd_style,
                    active_spec,
                    self.remove_confirm_tracker.clone(),
                    palette,
                    window,
                    cx,
                ))
                .into_any_element()
        } else {
            apply_scale(div(), scale::UI)
                .id("providers-remove-cred-btn")
                .test_support()
                .a11y_button("providers-remove-cred-btn", "移除")
                .track_focus(&self.remove_trigger_focus)
                .tab_index(0)
                .mr(px(-8.0))
                .h(height::COMPACT)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .text_color(palette.status_danger)
                .flex()
                .items_center()
                .gap(space::TIGHT)
                .flex_shrink_0()
                .hover(|s| s.bg(palette.status_danger_subtle))
                .active(|s| s.bg(palette.status_danger_subtle).opacity(0.85))
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                    if !ev.is_held
                        && ev.keystroke.modifiers == gpui::Modifiers::default()
                        && matches!(ev.keystroke.key.as_str(), "enter" | "space")
                    {
                        cx.stop_propagation();
                        this.open_remove_confirm(w, cx);
                    }
                }))
                .on_click(cx.listener(|this, _, w, cx| {
                    this.open_remove_confirm(w, cx);
                }))
                .child(
                    Icon::new(IconName::Trash)
                        .size(px(14.0))
                        .text_color(palette.status_danger),
                )
                .child(cjk_inline_label("移除"))
                .into_any_element()
        };

        div()
            .id("providers-remove-credentials-card-gawme")
            .test_support()
            .w_full()
            .h(px(44.0))
            .border_t_1()
            .border_color(palette.border_subtle)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::LG)
            .child(
                apply_scale(div(), scale::CAPTION)
                    .min_w_0()
                    .truncate()
                    .text_color(palette.text_muted)
                    .child("移除后将隐藏该服务下的关联模型"),
            )
            .child(right_el)
    }
}

fn capability_checkbox<F>(
    id: SharedString,
    label: SharedString,
    checked: bool,
    palette: &DesktopPalette,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
{
    div()
        .id(id.clone())
        .test_support()
        .a11y_checkbox(id, label, checked)
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
        .on_click(on_click)
        .children(checked.then(|| {
            Icon::new(IconName::Check)
                .size(px(11.0))
                .text_color(palette.text_on_accent)
        }))
}

#[cfg(test)]
mod tests {
    use cst_client::AccountStatusDto;
    use serde_json::json;

    use super::format_cstoa_status;

    fn account(signed_in: bool, name: Option<&str>, student_id: Option<&str>) -> AccountStatusDto {
        serde_json::from_value(json!({
            "providerId": "cstoa",
            "signedIn": signed_in,
            "profile": {
                "supported": true,
                "name": name,
                "studentId": student_id
            }
        }))
        .unwrap()
    }

    #[test]
    fn cstoa_status_omits_placeholder_dashes_and_redundant_ready_copy() {
        let missing = account(true, None, None);
        let (text, warn) = format_cstoa_status(Some(&missing), false);
        assert_eq!(text, "账号授权 · 已登录");
        assert!(!warn);
        assert!(!text.contains("--"));
        assert!(!text.contains("已就绪"));

        let dash_only = account(true, Some("--"), Some("  "));
        assert_eq!(
            format_cstoa_status(Some(&dash_only), false),
            ("账号授权 · 已登录".to_owned(), false)
        );

        let full = account(true, Some("张同学"), Some("20260888"));
        assert_eq!(
            format_cstoa_status(Some(&full), false),
            ("张同学（20260888） · 已登录".to_owned(), false)
        );

        let name_only = account(true, Some("张同学"), None);
        assert_eq!(
            format_cstoa_status(Some(&name_only), false),
            ("张同学 · 已登录".to_owned(), false)
        );

        let sid_only = account(true, None, Some("20260888"));
        assert_eq!(
            format_cstoa_status(Some(&sid_only), false),
            ("20260888 · 已登录".to_owned(), false)
        );

        let expired = account(true, Some("张同学"), Some("20260888"));
        assert_eq!(
            format_cstoa_status(Some(&expired), true),
            ("凭据已失效，请重新登录".to_owned(), true)
        );

        let signed_out = account(false, None, None);
        assert_eq!(
            format_cstoa_status(Some(&signed_out), false),
            ("账号授权 · 未登录".to_owned(), true)
        );
    }
}

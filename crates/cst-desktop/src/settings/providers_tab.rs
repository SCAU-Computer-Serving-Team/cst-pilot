mod editor;

use cst_client::{ClientError, ClientHandle};
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
use crate::theme::kbd_focus::{
    KbdDynamicBoundsTracker, KbdDynamicSpec, KbdMenuStep, pointer_moved_past_threshold,
    render_gliding_dynamic_indicator, resolve_menu_nav_key,
};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius,
    scale, space,
};

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
    list_focus: FocusHandle,
    list_kbd_active: bool,
    list_selected_idx: usize,
    list_last_pointer: Option<Point<Pixels>>,
    list_tracker: KbdDynamicBoundsTracker,
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
            list_focus: cx.focus_handle().tab_stop(true),
            list_kbd_active: false,
            list_selected_idx: 0,
            list_last_pointer: None,
            list_tracker: KbdDynamicBoundsTracker::new(),
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
        cx.notify();
    }

    fn clear_key(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.api_key_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_masked(true, window, cx);
        });
    }

    pub fn has_dropdown(&self) -> bool {
        self.protocol_dropdown_open || self.inline_token_edit.is_some()
    }

    pub fn open_add_provider_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page_slide.start(1.0, cx);
        self.reset_form();
        self.adding_provider = true;
        self.providers_expanded = false;
        self.list_selected_idx = 0;
        self.add_search_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
    }

    pub fn focus_from_tabbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list_kbd_active = true;
        self.list_last_pointer = Some(window.mouse_position());
        self.list_focus.focus(window, cx);
        cx.notify();
    }

    pub fn dismiss_topmost(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.dismiss_dropdowns(cx) {
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
        self.inline_token_edit = None;
        self.key_error = None;
        self.form_error = None;
    }

    fn toggle_builtin_model_enabled(&mut self, full_id: String, cx: &mut Context<Self>) {
        let snap = self.client.snapshot();
        let Some(models) = snap.models.as_ref() else {
            return;
        };
        let mut next: Vec<String> = match models.enabled.as_ref() {
            Some(list) => list.clone(),
            None => models
                .models
                .iter()
                .filter(|m| m.credential_ready && m.catalog_present)
                .map(|m| format!("{}/{}", m.provider, m.id))
                .collect(),
        };
        if let Some(pos) = next.iter().position(|x| x == &full_id) {
            next.remove(pos);
        } else {
            next.push(full_id);
        }
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.set_explicit_model_scope(next).await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |_, cx| {
                if let Err(err) = res {
                    cx.emit(ProvidersTabEvent::ErrorMessage(err.to_string()));
                }
                cx.notify();
            });
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div().w_full().flex().items_center().child(
            apply_scale(div(), scale::UI_MEDIUM)
                .id("providers-back-to-list-btn")
                .test_support()
                .a11y_button("providers-back-to-list-btn", "返回提供商列表")
                .h(height::CONTROL)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .text_color(palette.text_secondary)
                .flex()
                .items_center()
                .gap(space::XS)
                .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.return_to_provider_list(window, cx);
                }))
                .child(
                    Icon::new(IconName::ArrowLeft)
                        .size(px(14.0))
                        .text_color(palette.text_secondary),
                )
                .child(cjk_inline_label("提供商")),
        )
    }
}

impl Render for ProvidersTabView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::MD)
                        .child(self.render_back_bar(&palette, cx))
                        .child(
                            apply_scale(div(), scale::TITLE)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label("自定义提供商")),
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
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::LG)
                .hover(move |s| {
                    if kbd_active {
                        s
                    } else {
                        s.bg(palette.bg_item_hover)
                    }
                })
                .on_click(cx.listener(|this, _, w, cx| {
                    this.open_new_custom_provider(w, cx);
                }))
                .on_prepaint(move |bounds, _, _| tr_custom.set("add-row-0", bounds))
                .child(
                    div()
                        .flex()
                        .items_end()
                        .gap(space::MD)
                        .min_w_0()
                        .child(
                            apply_scale(div(), scale::BODY)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label("自定义提供商")),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .pb(px(1.0))
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
                        .text_color(palette.text_secondary)
                        .child(apply_scale(div(), scale::UI).child(cjk_inline_label("配置")))
                        .child(Icon::new(IconName::ChevronRight).size(px(14.0))),
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
                    avail_rows = avail_rows.child(
                        div()
                            .id(row_id.clone())
                            .test_support()
                            .a11y_listitem(row_id, format!("{}（{auth_hint}）", prov.name), false)
                            .w_full()
                            .h(px(48.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::LG)
                            .hover(move |s| {
                                if kbd_active {
                                    s
                                } else {
                                    s.bg(palette.bg_item_hover)
                                }
                            })
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.select_provider(pid.clone(), w, cx);
                            }))
                            .on_prepaint(move |bounds, _, _| tr.set(track_key.clone(), bounds))
                            .child(
                                div()
                                    .flex()
                                    .items_end()
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
                                            .pb(px(1.0))
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
                                    .text_color(palette.text_secondary)
                                    .child(
                                        apply_scale(div(), scale::UI)
                                            .child(cjk_inline_label("连接")),
                                    )
                                    .child(Icon::new(IconName::ChevronRight).size(px(14.0))),
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
                    avail_rows = avail_rows.child(
                        apply_scale(div(), scale::UI)
                            .id("providers-toggle-expand-btn")
                            .test_support()
                            .a11y_button("providers-toggle-expand-btn", toggle_label.clone())
                            .w_full()
                            .h(height::HEADER)
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
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
                                .text_color(palette.text_secondary),
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
                        .child(self.render_back_bar(&palette, cx))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    apply_scale(div(), scale::TITLE)
                                        .text_color(palette.text_primary)
                                        .child(cjk_inline_label("添加提供商")),
                                )
                                .child(
                                    apply_scale(div(), scale::UI)
                                        .id("providers-add-refresh-btn")
                                        .test_support()
                                        .a11y_button("providers-add-refresh-btn", "更新模型")
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
                            div()
                                .flex()
                                .items_end()
                                .gap(space::SM)
                                .mb(space::SM)
                                .child(
                                    apply_scale(div(), scale::BODY_SEMIBOLD)
                                        .text_color(palette.text_primary)
                                        .child(cjk_inline_label("可添加的提供商")),
                                )
                                .child(
                                    apply_scale(div(), scale::CAPTION)
                                        .pb(px(1.0))
                                        .text_color(palette.text_muted)
                                        .child(SharedString::from(total_avail.to_string())),
                                ),
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
            let catalog_models: Vec<_> = all_provider_models
                .into_iter()
                .filter(|mc| {
                    filter_q.is_empty()
                        || mc.id.to_lowercase().contains(&filter_q)
                        || mc.name.to_lowercase().contains(&filter_q)
                })
                .collect();
            let enabled_set: Option<std::collections::HashSet<String>> = snap
                .models
                .as_ref()
                .and_then(|m| m.enabled.as_ref())
                .map(|list| list.iter().cloned().collect());

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
                    let is_enabled = match &enabled_set {
                        Some(set) => set.contains(&full_id),
                        None => true,
                    };
                    let fid_toggle = full_id.clone();
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
                    model_rows = model_rows.child(
                        div()
                            .w_full()
                            .h(px(52.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::MD)
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(space::MD)
                                    .child(capability_checkbox(
                                        chk_id,
                                        SharedString::from(format!("启用模型：{display_name}")),
                                        is_enabled,
                                        &palette,
                                        cx.listener(move |this, _, _, cx| {
                                            this.toggle_builtin_model_enabled(
                                                fid_toggle.clone(),
                                                cx,
                                            );
                                        }),
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .items_end()
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
                                                    .pb(px(1.0))
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

            let enabled_in_provider = match &enabled_set {
                Some(set) => catalog_models
                    .iter()
                    .filter(|m| set.contains(&format!("{}/{}", m.provider, m.id)))
                    .count(),
                None => catalog_models.len(),
            };

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
                        .child(self.render_back_bar(&palette, cx))
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
                                        .h(height::COMPACT)
                                        .px(space::SM)
                                        .rounded(radius::CONTROL)
                                        .text_color(palette.accent_ink)
                                        .flex()
                                        .items_center()
                                        .flex_shrink_0()
                                        .hover(|s| s.bg(palette.bg_item_hover))
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
                                        .items_end()
                                        .gap(space::SM)
                                        .child(
                                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                                .text_color(palette.text_primary)
                                                .child(cjk_inline_label("可用模型")),
                                        )
                                        .child(
                                            apply_scale(div(), scale::CAPTION)
                                                .pb(px(1.0))
                                                .text_color(palette.text_muted)
                                                .child(SharedString::from(format!(
                                                    "{enabled_in_provider} / {total_models_count}"
                                                ))),
                                        ),
                                )
                                .child(
                                    div()
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
                    div()
                        .mt(px(40.0))
                        .w_full()
                        .child(render_remove_credentials_card(
                            &palette,
                            cx.listener(|this, _, window, cx| {
                                this.remove_active_provider_credentials(window, cx);
                            }),
                        ))
                }));
        }

        // 4. Configured Providers List (`Q4QFim`) — frameless 64px hairline rows, no repeated H1
        let cstoa_signed_in = snap.account.as_ref().is_some_and(|a| a.signed_in);
        let (cstoa_meta, cstoa_meta_color) = if cstoa_signed_in {
            let acc = snap.account.as_ref().unwrap();
            let sid = acc.profile.student_id.as_deref().unwrap_or("--");
            let nm = acc.profile.name.as_deref().unwrap_or("已登录");
            (
                format!("扫码登录 · 已就绪 · {nm}（{sid}）"),
                palette.text_secondary,
            )
        } else {
            ("扫码登录 · 未登录".to_owned(), palette.status_warning)
        };

        let providers = snap
            .auth_providers
            .as_ref()
            .map(|p| p.as_slice())
            .unwrap_or(&[]);
        let configured_others: Vec<_> = providers
            .iter()
            .filter(|p| p.id != "cstoa" && (p.configured == Some(true) || p.requires_login))
            .collect();
        let configured_total = 1 + configured_others.len();
        let kbd_targets: Vec<Option<String>> = std::iter::once(None)
            .chain(configured_others.iter().map(|p| Some(p.id.clone())))
            .collect();
        let list_focused = self.list_focus.is_focused(window);
        let active_spec =
            (list_focused && self.list_kbd_active && !kbd_targets.is_empty()).then(|| {
                KbdDynamicSpec::row(format!(
                    "cfg-row-{}",
                    self.list_selected_idx.min(kbd_targets.len() - 1)
                ))
                .with_radius(4.0)
            });
        let kbd_active = self.list_kbd_active;
        let tr_cstoa = self.list_tracker.clone();

        let mut rows_list = div().w_full().flex().flex_col().child(
            div()
                .id("providers-cstoa-pinned-card")
                .test_support()
                .w_full()
                .h(px(64.0))
                .border_b_1()
                .border_color(palette.border_subtle)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::LG)
                .hover(move |s| {
                    if kbd_active {
                        s
                    } else {
                        s.bg(palette.bg_item_hover)
                    }
                })
                .on_click(cx.listener(move |_, _, _, cx| {
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
                                .child(cjk_inline_label("计维CSTOA")),
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
                        .text_color(palette.text_secondary)
                        .child(apply_scale(div(), scale::UI).child(cjk_inline_label(
                            if cstoa_signed_in {
                                "管理"
                            } else {
                                "扫码登录"
                            },
                        )))
                        .child(Icon::new(IconName::ChevronRight).size(px(14.0))),
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
            rows_list = rows_list.child(
                div()
                    .id(row_id.clone())
                    .test_support()
                    .a11y_listitem(row_id, format!("{}（{status_label}）", prov.name), false)
                    .w_full()
                    .h(px(64.0))
                    .border_b_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::LG)
                    .hover(move |s| {
                        if kbd_active {
                            s
                        } else {
                            s.bg(palette.bg_item_hover)
                        }
                    })
                    .on_click(cx.listener(move |this, _, w, cx| {
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
                                    .child(cjk_inline_label(status_label)),
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
                            .text_color(palette.text_secondary)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.select_provider(pid_btn.clone(), w, cx);
                                cx.stop_propagation();
                            }))
                            .child(
                                apply_scale(div(), scale::UI).child(cjk_inline_label(action_label)),
                            )
                            .child(Icon::new(IconName::ChevronRight).size(px(14.0))),
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
                        match kbd_targets.get(idx).and_then(|x| x.clone()) {
                            None => cx.emit(ProvidersTabEvent::OpenLoginOrAccount {
                                signed_in: cstoa_signed_in,
                            }),
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
                            .items_end()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::BODY_SEMIBOLD)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("已配置")),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .pb(px(1.0))
                                    .text_color(palette.text_muted)
                                    .child(SharedString::from(configured_total.to_string())),
                            ),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-new-custom-btn")
                            .test_support()
                            .a11y_button("providers-new-custom-btn", "添加提供商")
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.open_add_provider_catalog(w, cx);
                            }))
                            .child(
                                Icon::new(IconName::Plus)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("添加提供商")),
                    ),
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

fn render_remove_credentials_card<F>(palette: &DesktopPalette, on_remove: F) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
{
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
        .child(
            apply_scale(div(), scale::UI)
                .id("providers-remove-cred-btn")
                .test_support()
                .a11y_button("providers-remove-cred-btn", "移除")
                .h(height::COMPACT)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .text_color(palette.status_danger)
                .flex()
                .items_center()
                .gap(space::TIGHT)
                .flex_shrink_0()
                .hover(|s| s.bg(palette.status_danger_subtle))
                .on_click(on_remove)
                .child(
                    Icon::new(IconName::Trash)
                        .size(px(14.0))
                        .text_color(palette.status_danger),
                )
                .child(cjk_inline_label("移除")),
        )
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

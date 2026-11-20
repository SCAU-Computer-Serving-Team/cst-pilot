use cst_client::{ClientError, ClientHandle};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputContentType, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, TestSupportExt, Window,
    div, px,
};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, radius, scale, space,
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
            Self::OpenaiCompletions => "OpenAI Chat",
            Self::OpenaiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic",
            Self::GoogleGenerativeAi => "Gemini",
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

pub struct EditableModelRow {
    pub id_input: Entity<InputState>,
    pub name_input: Entity<InputState>,
    pub context_window: u64,
    pub max_tokens: u64,
    pub reasoning: bool,
    pub image: bool,
}

fn format_token_window(n: u64) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

fn next_context_window(cur: u64) -> u64 {
    match cur {
        0..=32_768 => 64_000,
        32_769..=65_536 => 128_000,
        65_537..=131_072 => 200_000,
        131_073..=262_144 => 1_000_000,
        _ => 32_000,
    }
}

fn next_max_tokens(cur: u64) -> u64 {
    match cur {
        0..=4_096 => 8_192,
        4_097..=8_192 => 16_384,
        8_193..=16_384 => 32_768,
        16_385..=32_768 => 65_536,
        _ => 4_096,
    }
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
    creating_new_custom: bool,
    inline_edit_mode: bool,
    search_visible: bool,
    search_input: Entity<InputState>,
    provider_id_input: Entity<InputState>,
    provider_name_input: Entity<InputState>,
    base_url_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    protocol: CustomApiProtocol,
    revision: Option<String>,
    editable_rows: Vec<EditableModelRow>,
    saving: bool,
    loading: bool,
    generation: u64,
    window: gpui::AnyWindowHandle,
    key_error: Option<SharedString>,
    form_error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProvidersTabEvent> for ProvidersTabView {}

impl ProvidersTabView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("按模型 ID 或名称筛选…"));
        let provider_id_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("服务标识（小写字母/数字/连字符，如 my-gateway）")
        });
        let provider_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("服务显示名称（如 本地网关）"));
        let base_url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Base URL（如 https://api.example.com/v1）")
        });
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("输入或更新 API Key（留空保持已有密钥）")
                .masked(true)
        });

        let mut subs = vec![cx.subscribe(&search_input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        for inp in [&provider_id_input, &provider_name_input, &base_url_input] {
            subs.push(cx.subscribe(inp, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.form_error = None;
                    this.key_error = None;
                    cx.notify();
                }
            }));
        }
        subs.push(
            cx.subscribe(&api_key_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.form_error = None;
                    this.key_error = None;
                    cx.notify();
                }
            }),
        );

        Self {
            client,
            active_provider_id: None,
            creating_new_custom: false,
            inline_edit_mode: false,
            search_visible: false,
            search_input,
            provider_id_input,
            provider_name_input,
            base_url_input,
            api_key_input,
            protocol: CustomApiProtocol::OpenaiCompletions,
            revision: None,
            editable_rows: Vec::new(),
            saving: false,
            loading: false,
            generation: 0,
            window: window.window_handle(),
            key_error: None,
            form_error: None,
            _subscriptions: subs,
        }
    }

    pub fn open_new_custom_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn reset_form(&mut self) {
        self.generation += 1;
        self.loading = false;
        self.revision = None;
        self.editable_rows.clear();
        self.inline_edit_mode = false;
        self.creating_new_custom = false;
        self.active_provider_id = None;
        self.search_visible = false;
        self.key_error = None;
        self.form_error = None;
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
                    let mut s = InputState::new(window, cx).placeholder("model-id");
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

        let id_input = cx.new(|cx| InputState::new(window, cx).placeholder("新模型 ID…"));
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("模型名称…"));
        id_input.update(cx, |i, cx| i.focus(window, cx));
        self.editable_rows.push(EditableModelRow {
            id_input,
            name_input,
            context_window: 128_000,
            max_tokens: 16_384,
            reasoning: true,
            image: false,
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
                            "已删除自定义模型服务。".into()
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
                        cx.emit(ProvidersTabEvent::Notice("模型服务配置已保存。".into()));
                    }
                    Err(ClientError::BadRequest { .. }) => {
                        this.form_error = Some("无法保存模型服务，请检查配置和密钥。".into());
                    }
                    Err(_) => cx.emit(ProvidersTabEvent::ErrorMessage(
                        "保存模型服务失败，请重试。".into(),
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
                .a11y_button("providers-back-to-list-btn", "返回模型服务列表")
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
                .child(cjk_inline_label("模型服务")),
        )
    }

    fn render_group_header_with_3_icon_buttons(
        &self,
        title: &'static str,
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Strictly 3 icon-only action buttons (`search`, `refresh-cw`, `plus`) on right — ZERO text buttons!
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .child(
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(title)),
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
                                    this.search_input.update(cx, |i, cx| i.focus(w, cx));
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
                            .a11y_button("providers-hdr-refresh-btn", "刷新模型目录")
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
                    )
                    .child(
                        div()
                            .id("providers-hdr-add-model-btn")
                            .test_support()
                            .a11y_button("providers-hdr-add-model-btn", "添加模型行")
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.append_blank_editable_row(w, cx);
                            }))
                            .child(
                                Icon::new(IconName::Plus)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
    }

    fn render_borderless_hairline_editor(
        &self,
        mono_family: SharedString,
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut protocol_row = div().flex().flex_wrap().gap(space::XS);
        for proto in CustomApiProtocol::ALL {
            let active = self.protocol == proto;
            let proto_id = SharedString::from(format!("custom-proto-{}", proto.wire_value()));
            protocol_row = protocol_row.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(proto_id.clone())
                    .test_support()
                    .a11y_tab(proto_id, proto.label(), active)
                    .h(height::COMPACT)
                    .px(space::SM)
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
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.protocol = proto;
                        cx.notify();
                    }))
                    .child(SharedString::from(proto.label())),
            );
        }

        // Borderless hairline editable table with `32px` column header row (`思考` / `图片` 16×16 checkboxes)
        let mut hairline_table = div()
            .id("providers-hairline-edit-table")
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .h(height::CONTROL)
                    .border_b_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .flex_1()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("模型 ID")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .flex_1()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("显示名称")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(76.0))
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("上下文窗口")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(68.0))
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("最大输出")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(40.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("思考")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(40.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("图片")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(36.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("移除")),
                    ),
            );

        let filter_q = self.search_input.read(cx).value().to_lowercase();
        for (idx, row) in self.editable_rows.iter().enumerate() {
            if self.search_visible && !filter_q.is_empty() {
                let id_v = row.id_input.read(cx).value().to_lowercase();
                let nm_v = row.name_input.read(cx).value().to_lowercase();
                if !id_v.contains(&filter_q) && !nm_v.contains(&filter_q) {
                    continue;
                }
            }
            let reasoning = row.reasoning;
            let image = row.image;
            let cw_text = format_token_window(row.context_window);
            let mt_text = format_token_window(row.max_tokens);
            let cw_id = SharedString::from(format!("edit-row-cw-{idx}"));
            let mt_id = SharedString::from(format!("edit-row-mt-{idx}"));
            let rm_id = SharedString::from(format!("edit-row-remove-{idx}"));

            hairline_table = hairline_table.child(
                div()
                    .py(space::SM)
                    .border_b_1()
                    .border_color(palette.border_hairline)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        div().flex_1().child(
                            Input::new(&row.id_input)
                                .disabled(self.saving)
                                .accessibility_id(SharedString::from(format!(
                                    "providers-row-id-input-{idx}"
                                )))
                                .aria_label(format!("第 {} 行模型 ID", idx + 1)),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            Input::new(&row.name_input)
                                .disabled(self.saving)
                                .accessibility_id(SharedString::from(format!(
                                    "providers-row-name-input-{idx}"
                                )))
                                .aria_label(format!("第 {} 行模型显示名称", idx + 1)),
                        ),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .id(cw_id.clone())
                            .test_support()
                            .a11y_button(cw_id, format!("切换上下文窗口：{cw_text}"))
                            .w(px(76.0))
                            .h(height::COMPACT)
                            .px(space::XS)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_workspace)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.editable_rows.get_mut(idx) {
                                    r.context_window = next_context_window(r.context_window);
                                    cx.notify();
                                }
                            }))
                            .child(SharedString::from(cw_text)),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .id(mt_id.clone())
                            .test_support()
                            .a11y_button(mt_id, format!("切换最大输出：{mt_text}"))
                            .w(px(68.0))
                            .h(height::COMPACT)
                            .px(space::XS)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_workspace)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.editable_rows.get_mut(idx) {
                                    r.max_tokens = next_max_tokens(r.max_tokens);
                                    cx.notify();
                                }
                            }))
                            .child(SharedString::from(mt_text)),
                    )
                    .child(
                        div()
                            .w(px(40.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(capability_checkbox(
                                SharedString::from(format!("edit-row-reasoning-{idx}")),
                                SharedString::from(format!("第 {} 行支持思考", idx + 1)),
                                reasoning,
                                palette,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(r) = this.editable_rows.get_mut(idx) {
                                        r.reasoning = !r.reasoning;
                                        cx.notify();
                                    }
                                }),
                            )),
                    )
                    .child(
                        div()
                            .w(px(40.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(capability_checkbox(
                                SharedString::from(format!("edit-row-image-{idx}")),
                                SharedString::from(format!("第 {} 行支持图片", idx + 1)),
                                image,
                                palette,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(r) = this.editable_rows.get_mut(idx) {
                                        r.image = !r.image;
                                        cx.notify();
                                    }
                                }),
                            )),
                    )
                    .child(
                        div()
                            .w(px(36.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .id(rm_id.clone())
                                    .test_support()
                                    .a11y_button(rm_id, format!("移除第 {} 行模型", idx + 1))
                                    .w(height::COMPACT)
                                    .h(height::COMPACT)
                                    .rounded(radius::CONTROL)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if idx < this.editable_rows.len() {
                                            this.editable_rows.remove(idx);
                                            cx.notify();
                                        }
                                    }))
                                    .child(
                                        Icon::new(IconName::Trash)
                                            .size(px(13.0))
                                            .text_color(palette.text_muted),
                                    ),
                            ),
                    ),
            );
        }

        let pid_empty =
            self.form_error.is_some() && self.provider_id_input.read(cx).value().trim().is_empty();
        let name_empty = self.form_error.is_some()
            && self.provider_name_input.read(cx).value().trim().is_empty();
        let url_empty =
            self.form_error.is_some() && self.base_url_input.read(cx).value().trim().is_empty();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::MD)
            .child(self.render_back_bar(palette, cx))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("服务标识")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(if pid_empty {
                                        palette.status_danger
                                    } else {
                                        palette.border_subtle
                                    })
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.provider_id_input)
                                            .accessibility_id("providers-id-input")
                                            .disabled(
                                                self.loading
                                                    || self.saving
                                                    || !self.creating_new_custom,
                                            )
                                            .aria_label("服务标识")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full(),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("显示名称")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(if name_empty {
                                        palette.status_danger
                                    } else {
                                        palette.border_subtle
                                    })
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.provider_name_input)
                                            .accessibility_id("providers-name-input")
                                            .disabled(self.loading || self.saving)
                                            .aria_label("服务显示名称")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full(),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_h(px(44.0))
                            .py(space::XS)
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("接口协议")),
                            )
                            .child(div().flex_1().child(protocol_row)),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child("Base URL"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(if url_empty {
                                        palette.status_danger
                                    } else {
                                        palette.border_subtle
                                    })
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.base_url_input)
                                            .accessibility_id("providers-baseurl-input")
                                            .disabled(self.loading || self.saving)
                                            .aria_label("服务 Base URL")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full(),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child("API Key"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_subtle)
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.api_key_input)
                                            .accessibility_id("providers-apikey-input")
                                            .disabled(self.loading || self.saving)
                                            .content_type(InputContentType::Password)
                                            .aria_label("服务 API Key")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full()
                                            .mask_toggle(),
                                    ),
                            ),
                    ),
            )
            .children(self.form_error.clone().map(|err| {
                apply_scale(div(), scale::CAPTION)
                    .id("providers-form-error-text")
                    .test_support()
                    .a11y_status("providers-form-error-text", err.clone())
                    .text_color(palette.status_danger)
                    .child(err)
            }))
            .child(self.render_group_header_with_3_icon_buttons("模型列表", palette, cx))
            .children(self.search_visible.then(|| {
                Input::new(&self.search_input)
                    .accessibility_id("providers-search-input")
                    .aria_label("筛选模型")
                    .h(height::FIELD)
                    .cleanable(true)
            }))
            .child(hairline_table)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-inline-cancel-btn")
                            .test_support()
                            .a11y_button("providers-inline-cancel-btn", "取消")
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_workspace)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.return_to_provider_list(window, cx);
                            }))
                            .child(cjk_inline_label("取消")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-inline-save-btn")
                            .test_support()
                            .a11y_button(
                                "providers-inline-save-btn",
                                if self.saving {
                                    "正在保存"
                                } else {
                                    "保存"
                                },
                            )
                            .h(height::CONTROL)
                            .px(space::LG)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.save_inline_provider_form(w, cx);
                            }))
                            .child(cjk_inline_label(if self.saving {
                                "正在保存…"
                            } else {
                                "保存"
                            })),
                    ),
            )
            .children(
                (!self.creating_new_custom && self.active_provider_id.is_some()).then(|| {
                    render_remove_credentials_card(
                        palette,
                        cx.listener(|this, _, window, cx| {
                            this.remove_active_provider_credentials(window, cx);
                        }),
                    )
                }),
            )
    }
}

impl Render for ProvidersTabView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let snap = self.client.snapshot();

        // 1. Custom / Inline Provider Editor Mode (`q8NLwK`)
        if self.creating_new_custom || self.inline_edit_mode {
            return div()
                .id("settings-providers-tab")
                .test_support()
                .a11y_region("settings-providers-tab", "模型服务设置")
                .w_full()
                .flex()
                .flex_col()
                .gap(space::MD)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space::XS)
                        .child(
                            apply_scale(div(), scale::TITLE)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label("自定义模型服务")),
                        )
                        .child(
                            apply_scale(div(), scale::UI)
                                .text_color(palette.text_secondary)
                                .child(cjk_inline_label(
                                    "连接兼容 OpenAI、Anthropic 或 Google 接口的服务。",
                                )),
                        ),
                )
                .child(self.render_borderless_hairline_editor(mono_family, &palette, cx));
        }

        // 2. Built-in Provider Detail / Credential Management Mode (`T5us8O`)
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
            let filter_q = self.search_input.read(cx).value().to_lowercase();
            let catalog_models: Vec<_> = snap
                .models
                .as_ref()
                .map(|m| {
                    m.catalog
                        .iter()
                        .filter(|mc| {
                            mc.provider == pid_owned
                                && (filter_q.is_empty()
                                    || mc.id.to_lowercase().contains(&filter_q)
                                    || mc.name.to_lowercase().contains(&filter_q))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            let mut model_table = div().w_full().flex().flex_col().child(
                div()
                    .w_full()
                    .h(height::CONTROL)
                    .border_b_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .flex_1()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("模型 ID")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .flex_1()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("显示名称")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(76.0))
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("上下文窗口")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(40.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("思考")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(40.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("图片")),
                    ),
            );

            for m in &catalog_models {
                let has_image = m.input.iter().any(|x| x == "image");
                model_table = model_table.child(
                    div()
                        .h(px(44.0))
                        .border_b_1()
                        .border_color(palette.border_hairline)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(mono_family.clone())
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(m.id.clone())),
                        )
                        .child(
                            apply_scale(div(), scale::UI)
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(palette.text_primary)
                                .child(SharedString::from(m.name.clone())),
                        )
                        .child(
                            apply_scale(div(), scale::CAPTION)
                                .w(px(76.0))
                                .font_family(mono_family.clone())
                                .text_color(palette.text_secondary)
                                .child(SharedString::from(format_token_window(
                                    m.context_window.unwrap_or(128_000),
                                ))),
                        )
                        .child(
                            div()
                                .w(px(40.0))
                                .flex()
                                .justify_center()
                                .child(readonly_capability_box(m.reasoning, &palette)),
                        )
                        .child(
                            div()
                                .w(px(40.0))
                                .flex()
                                .justify_center()
                                .child(readonly_capability_box(has_image, &palette)),
                        ),
                );
            }

            return div()
                .id("settings-providers-tab")
                .test_support()
                .a11y_region("settings-providers-tab", "模型服务设置")
                .w_full()
                .flex()
                .flex_col()
                .gap(space::MD)
                .child(
                    apply_scale(div(), scale::TITLE)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("模型服务")),
                )
                .child(self.render_back_bar(&palette, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::MD)
                        .child(
                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                .text_color(palette.text_primary)
                                .child(SharedString::from(prov_name)),
                        )
                        .children(is_configured.then(|| {
                            div()
                                .h(height::INLINE)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .bg(palette.bg_surface)
                                .flex()
                                .items_center()
                                .gap(space::XS)
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
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(space::TIGHT)
                        .child(
                            div()
                                .flex()
                                .gap(space::SM)
                                .child(
                                    div()
                                        .flex_1()
                                        .h(height::FIELD)
                                        .px(space::XS)
                                        .rounded(radius::CONTROL)
                                        .border_1()
                                        .border_color(if self.key_error.is_some() {
                                            palette.status_danger
                                        } else {
                                            palette.border_strong
                                        })
                                        .bg(palette.bg_workspace)
                                        .flex()
                                        .items_center()
                                        .child(
                                            Input::new(&self.api_key_input)
                                                .accessibility_id("providers-apikey-input")
                                                .content_type(InputContentType::Password)
                                                .aria_label("服务 API Key")
                                                .appearance(false)
                                                .bordered(false)
                                                .w_full()
                                                .h_full()
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
                                        .h(height::FIELD)
                                        .px(space::LG)
                                        .rounded(radius::CONTROL)
                                        .bg(palette.accent_solid)
                                        .text_color(palette.text_on_accent)
                                        .flex()
                                        .items_center()
                                        .hover(|s| s.bg(palette.accent_hover))
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
                                .text_color(palette.status_danger)
                                .child(err)
                        })),
                )
                .child(self.render_group_header_with_3_icon_buttons("模型列表", &palette, cx))
                .children(self.search_visible.then(|| {
                    Input::new(&self.search_input)
                        .accessibility_id("providers-search-input")
                        .aria_label("筛选模型")
                        .h(height::FIELD)
                        .cleanable(true)
                }))
                .child(model_table)
                .children(is_configured.then(|| {
                    render_remove_credentials_card(
                        &palette,
                        cx.listener(|this, _, window, cx| {
                            this.remove_active_provider_credentials(window, cx);
                        }),
                    )
                }));
        }

        // 3. Main Merged Provider List (`$bg-workspace` + `8px` radius + `72px` rows)
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

        let mut merged_card = div()
            .w_full()
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_workspace)
            .flex()
            .flex_col()
            // First row retains `providers-cstoa-pinned-card` ID for test compatibility
            .child(
                div()
                    .id("providers-cstoa-pinned-card")
                    .test_support()
                    .w_full()
                    .h(px(72.0))
                    .px(space::LG)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::LG)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::MICRO)
                            .min_w_0()
                            .child(
                                apply_scale(div(), scale::BODY)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("CSTOA 科协专属服务")),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(cstoa_meta_color)
                                    .child(SharedString::from(cstoa_meta)),
                            ),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-cstoa-action-btn")
                            .test_support()
                            .a11y_button(
                                "providers-cstoa-action-btn",
                                if cstoa_signed_in {
                                    "管理 CSTOA 账号"
                                } else {
                                    "扫码登录 CSTOA"
                                },
                            )
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_workspace)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ProvidersTabEvent::OpenLoginOrAccount {
                                    signed_in: cstoa_signed_in,
                                });
                            }))
                            .child(cjk_inline_label(if cstoa_signed_in {
                                "管理"
                            } else {
                                "扫码登录"
                            })),
                    ),
            );

        let providers = snap
            .auth_providers
            .as_ref()
            .map(|p| p.as_slice())
            .unwrap_or(&[]);
        for prov in providers.iter().filter(|p| p.id != "cstoa") {
            let pid = prov.id.clone();
            let pid_btn = pid.clone();
            let (status_label, status_color) = if prov.requires_login {
                ("凭据已失效，请重新配置", palette.status_warning)
            } else if prov.configured == Some(true) {
                ("已就绪", palette.status_success)
            } else {
                ("尚未配置凭据", palette.status_warning)
            };

            let row_id = SharedString::from(format!("provider-row-{pid}"));
            let row_label = SharedString::from(format!("{}（{}）", prov.name, status_label));
            let manage_id = SharedString::from(format!("provider-manage-btn-{pid_btn}"));
            let manage_label = SharedString::from(format!("管理 {}", prov.name));

            merged_card = merged_card.child(
                div()
                    .id(row_id.clone())
                    .test_support()
                    .a11y_listitem(row_id, row_label, false)
                    .w_full()
                    .h(px(72.0))
                    .px(space::LG)
                    .border_t_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::LG)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.select_provider(pid.clone(), w, cx);
                    }))
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
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id(manage_id.clone())
                            .test_support()
                            .a11y_button(manage_id, manage_label)
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_workspace)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.select_provider(pid_btn.clone(), w, cx);
                                cx.stop_propagation();
                            }))
                            .child(cjk_inline_label("管理")),
                    ),
            );
        }

        let configured_count = usize::from(cstoa_signed_in)
            + providers
                .iter()
                .filter(|p| p.id != "cstoa" && p.configured == Some(true))
                .count();

        div()
            .id("settings-providers-tab")
            .test_support()
            .a11y_region("settings-providers-tab", "模型服务设置")
            .w_full()
            .flex()
            .flex_col()
            .gap(space::GROUP)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        apply_scale(div(), scale::TITLE)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("模型服务")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-new-custom-btn")
                            .test_support()
                            .a11y_button("providers-new-custom-btn", "添加服务")
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_workspace)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.open_new_custom_provider(w, cx);
                            }))
                            .child(
                                Icon::new(IconName::Plus)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("添加服务")),
                    ),
            )
            .child(
                div()
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
                                    .child(cjk_inline_label("服务列表")),
                            )
                            .child(
                                apply_scale(div(), scale::UI)
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label(format!(
                                        "{configured_count} 个已配置"
                                    ))),
                            ),
                    )
                    .child(merged_card),
            )
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
        .min_h(px(72.0))
        .px(space::LG)
        .py(space::MD)
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_special)
        .flex()
        .items_center()
        .justify_between()
        .gap(space::LG)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(space::MICRO)
                .min_w_0()
                .child(
                    apply_scale(div(), scale::BODY_MEDIUM)
                        .text_color(palette.text_primary)
                        .child(cjk_inline_label("移除此服务凭据")),
                )
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(palette.text_secondary)
                        .child("移除后本机保存的 API Key 将立即清除，关联模型从选择器中隐藏。"),
                ),
        )
        .child(
            apply_scale(div(), scale::UI)
                .id("providers-remove-cred-btn")
                .test_support()
                .a11y_button("providers-remove-cred-btn", "移除凭据")
                .h(height::CONTROL)
                .px(space::MD)
                .rounded(radius::CONTROL)
                .border_1()
                .border_color(palette.border_strong)
                .bg(palette.bg_workspace)
                .text_color(palette.status_danger)
                .flex()
                .items_center()
                .gap(space::SM)
                .flex_shrink_0()
                .hover(|s| s.bg(palette.status_danger_subtle))
                .on_click(on_remove)
                .child(
                    Icon::new(IconName::Trash)
                        .size(px(16.0))
                        .text_color(palette.status_danger),
                )
                .child(cjk_inline_label("移除凭据")),
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

fn readonly_capability_box(checked: bool, palette: &DesktopPalette) -> impl IntoElement {
    div()
        .w(px(16.0))
        .h(px(16.0))
        .rounded(radius::CONTROL)
        .border_1()
        .border_color(if checked {
            palette.accent_solid
        } else {
            palette.border_subtle
        })
        .bg(if checked {
            palette.accent_solid
        } else {
            palette.bg_workspace
        })
        .flex()
        .items_center()
        .justify_center()
        .children(checked.then(|| {
            Icon::new(IconName::Check)
                .size(px(11.0))
                .text_color(palette.text_on_accent)
        }))
}

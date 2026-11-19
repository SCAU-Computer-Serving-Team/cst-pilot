use cst_client::ClientHandle;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};
use serde_json::{Value, json};
use zeroize::Zeroizing;

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
        }
    }

    pub fn open_new_custom_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_provider_id = None;
        self.creating_new_custom = true;
        self.inline_edit_mode = true;
        self.revision = None;
        self.protocol = CustomApiProtocol::OpenaiCompletions;
        self.provider_id_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.provider_name_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.base_url_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.api_key_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.editable_rows.clear();
        self.append_blank_editable_row(window, cx);
        cx.notify();
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

        self.active_provider_id = Some(provider_id.clone());
        self.creating_new_custom = false;
        self.inline_edit_mode = is_custom;
        self.api_key_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.editable_rows.clear();

        if is_custom {
            let client = self.client.clone();
            let pid = provider_id.clone();
            cx.spawn(async move |weak, cx| {
                if let Ok(val) = client.get_custom_provider(pid).await {
                    let _ = weak.update_in(cx, |this, window, cx| {
                        this.populate_from_custom_json(&val, window, cx);
                    });
                }
            })
            .detach();
        } else {
            let pname = snap
                .auth_providers
                .as_ref()
                .and_then(|list| list.iter().find(|p| p.id == provider_id))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| provider_id.clone());
            self.provider_id_input
                .update(cx, |i, cx| i.set_value(provider_id, window, cx));
            self.provider_name_input
                .update(cx, |i, cx| i.set_value(pname, window, cx));
            self.base_url_input
                .update(cx, |i, cx| i.set_value("", window, cx));
            self.revision = None;
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
        // If switching a built-in provider into inline edit mode for the first time, seed existing models first!
        if !self.inline_edit_mode {
            self.inline_edit_mode = true;
            if let Some(ref pid) = self.active_provider_id
                && let Some(models) = self.client.snapshot().models.as_ref()
            {
                for m in models.catalog.iter().filter(|m| &m.provider == pid) {
                    let mid = m.id.clone();
                    let mname = m.name.clone();
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
                        context_window: m.context_window.unwrap_or(128_000),
                        max_tokens: m.max_tokens.unwrap_or(16_384),
                        reasoning: m.reasoning,
                        image: m.input.iter().any(|x| x == "image"),
                    });
                }
            }
        }

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

    fn save_inline_provider_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let pid = self.provider_id_input.read(cx).value().trim().to_owned();
        let name = self.provider_name_input.read(cx).value().trim().to_owned();
        let base_url = self.base_url_input.read(cx).value().trim().to_owned();
        let raw_key = Zeroizing::new(self.api_key_input.read(cx).value().trim().to_owned());
        self.api_key_input
            .update(cx, |i, cx| i.set_value("", window, cx));

        if pid.is_empty() || name.is_empty() || base_url.is_empty() {
            cx.emit(ProvidersTabEvent::ErrorMessage(
                "请完整填写服务标识、名称与 Base URL。".into(),
            ));
            return;
        }

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
        if !raw_key.is_empty() {
            payload.insert("apiKey".into(), Value::String(raw_key.as_str().to_owned()));
        }
        drop(raw_key);
        payload.insert("models".into(), Value::Array(models_arr));
        if let Some(ref rev) = self.revision {
            payload.insert("revision".into(), Value::String(rev.clone()));
        }

        self.saving = true;
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.save_custom_provider(Value::Object(payload)).await;
            let _ = client.refresh_globals().await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |this, cx| {
                this.saving = false;
                match res {
                    Ok(saved) => {
                        this.revision = saved
                            .get("revision")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        this.creating_new_custom = false;
                        this.active_provider_id = Some(pid);
                        cx.emit(ProvidersTabEvent::CredentialsSaved);
                        cx.emit(ProvidersTabEvent::Notice("模型服务配置已保存。".into()));
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
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .text_color(palette.text_secondary)
                    .child(title),
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
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut protocol_row = div().flex().flex_wrap().gap(space::XS);
        for proto in CustomApiProtocol::ALL {
            let active = self.protocol == proto;
            protocol_row = protocol_row.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(SharedString::from(format!(
                        "custom-proto-{}",
                        proto.wire_value()
                    )))
                    .test_support()
                    .h(height::COMPACT)
                    .px(space::SM)
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
                        palette.bg_surface
                    })
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.protocol = proto;
                        cx.notify();
                    }))
                    .child(SharedString::from(proto.label())),
            );
        }

        // Borderless hairline editable table (`border_t_1` hairlines only, no outer heavy frame!)
        let mut hairline_table = div()
            .id("providers-hairline-edit-table")
            .test_support()
            .w_full()
            .flex()
            .flex_col();

        for (idx, row) in self.editable_rows.iter().enumerate() {
            let reasoning = row.reasoning;
            let image = row.image;
            hairline_table = hairline_table.child(
                div()
                    .py(space::SM)
                    .border_b_1()
                    .border_color(palette.border_hairline)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(div().flex_1().child(Input::new(&row.id_input)))
                    .child(div().flex_1().child(Input::new(&row.name_input)))
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(SharedString::from(format!("edit-row-reasoning-{idx}")))
                            .test_support()
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if reasoning {
                                palette.bg_item_active
                            } else {
                                palette.bg_surface
                            })
                            .text_color(if reasoning {
                                palette.accent_solid
                            } else {
                                palette.text_muted
                            })
                            .flex()
                            .items_center()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.editable_rows.get_mut(idx) {
                                    r.reasoning = !r.reasoning;
                                    cx.notify();
                                }
                            }))
                            .child(cjk_inline_label("推理")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(SharedString::from(format!("edit-row-image-{idx}")))
                            .test_support()
                            .h(height::COMPACT)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if image {
                                palette.bg_item_active
                            } else {
                                palette.bg_surface
                            })
                            .text_color(if image {
                                palette.accent_solid
                            } else {
                                palette.text_muted
                            })
                            .flex()
                            .items_center()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.editable_rows.get_mut(idx) {
                                    r.image = !r.image;
                                    cx.notify();
                                }
                            }))
                            .child(cjk_inline_label("多模态")),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("edit-row-remove-{idx}")))
                            .test_support()
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
            );
        }

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::MD)
            .child(
                div()
                    .flex()
                    .gap(space::SM)
                    .child(div().flex_1().child(Input::new(&self.provider_id_input)))
                    .child(div().flex_1().child(Input::new(&self.provider_name_input))),
            )
            .child(protocol_row)
            .child(Input::new(&self.base_url_input))
            .child(Input::new(&self.api_key_input))
            .child(self.render_group_header_with_3_icon_buttons("模型列表", palette, cx))
            .children(self.search_visible.then(|| Input::new(&self.search_input)))
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
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_elevated)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inline_edit_mode = false;
                                this.creating_new_custom = false;
                                this.active_provider_id = None;
                                cx.notify();
                            }))
                            .child(cjk_inline_label("取消")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-inline-save-btn")
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
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
    }
}

impl Render for ProvidersTabView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = DesktopThemeGlobal::current(cx).palette;
        let snap = self.client.snapshot();

        let cstoa_signed_in = snap.account.as_ref().is_some_and(|a| a.signed_in);
        let cstoa_meta = if cstoa_signed_in {
            let acc = snap.account.as_ref().unwrap();
            let sid = acc.profile.student_id.as_deref().unwrap_or("--");
            let nm = acc.profile.name.as_deref().unwrap_or("已登录");
            format!("扫码登录 · 已就绪 · {nm}（{sid}）")
        } else {
            "扫码登录 · 未登录".to_owned()
        };

        // Top-pinned CSTOA card
        let cstoa_card = div()
            .id("providers-cstoa-pinned-card")
            .test_support()
            .w_full()
            .p(space::LG)
            .rounded(radius::SPECIAL)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_surface)
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::MICRO)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .text_color(palette.text_primary)
                            .child("CSTOA 科协专属服务"),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_secondary)
                            .child(SharedString::from(cstoa_meta)),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .id("providers-cstoa-action-btn")
                    .test_support()
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(palette.bg_elevated)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ProvidersTabEvent::OpenLoginOrAccount {
                            signed_in: cstoa_signed_in,
                        });
                    }))
                    .child(cjk_inline_label(if cstoa_signed_in {
                        "账号详情"
                    } else {
                        "扫码登录"
                    })),
            );

        // Provider list
        let providers = snap
            .auth_providers
            .as_ref()
            .map(|p| p.as_slice())
            .unwrap_or(&[]);
        let mut provider_list = div()
            .w_full()
            .rounded(radius::SPECIAL)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_surface)
            .flex()
            .flex_col();

        let non_cstoa: Vec<_> = providers.iter().filter(|p| p.id != "cstoa").collect();
        if non_cstoa.is_empty() {
            provider_list = provider_list.child(
                apply_scale(div(), scale::CAPTION)
                    .p(space::LG)
                    .text_color(palette.text_muted)
                    .child("暂无其他已配置或待更新凭据的模型服务，可点击下方添加自定义服务。"),
            );
        } else {
            for (idx, prov) in non_cstoa.iter().enumerate() {
                let pid = prov.id.clone();
                let status_str = if prov.requires_login {
                    ("凭据已失效", palette.status_danger)
                } else if prov.configured == Some(true) {
                    ("已就绪", palette.status_success)
                } else {
                    ("未配置", palette.text_muted)
                };
                let mut row = div()
                    .id(SharedString::from(format!("provider-row-{pid}")))
                    .test_support()
                    .px(space::LG)
                    .py(space::MD)
                    .flex()
                    .items_center()
                    .justify_between()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.select_provider(pid.clone(), w, cx);
                    }))
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .text_color(palette.text_primary)
                            .child(SharedString::from(prov.name.clone())),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .text_color(status_str.1)
                            .child(cjk_inline_label(status_str.0)),
                    );
                if idx > 0 {
                    row = row.border_t_1().border_color(palette.border_subtle);
                }
                provider_list = provider_list.child(row);
            }
        }

        let detail_section = if self.creating_new_custom || self.inline_edit_mode {
            Some(
                self.render_borderless_hairline_editor(&palette, cx)
                    .into_any_element(),
            )
        } else if let Some(ref pid) = self.active_provider_id {
            let filter_q = self.search_input.read(cx).value().to_lowercase();
            let catalog_models: Vec<_> = snap
                .models
                .as_ref()
                .map(|m| {
                    m.catalog
                        .iter()
                        .filter(|mc| {
                            &mc.provider == pid
                                && (filter_q.is_empty()
                                    || mc.id.to_lowercase().contains(&filter_q)
                                    || mc.name.to_lowercase().contains(&filter_q))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            let mut model_rows = div()
                .w_full()
                .rounded(radius::SPECIAL)
                .border_1()
                .border_color(palette.border_subtle)
                .bg(palette.bg_surface)
                .flex()
                .flex_col();

            for (idx, m) in catalog_models.iter().enumerate() {
                let mut r = div()
                    .px(space::LG)
                    .py(space::SM)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_primary)
                            .child(SharedString::from(m.name.clone())),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_muted)
                            .child(SharedString::from(m.id.clone())),
                    );
                if idx > 0 {
                    r = r.border_t_1().border_color(palette.border_subtle);
                }
                model_rows = model_rows.child(r);
            }

            Some(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .child(self.render_group_header_with_3_icon_buttons("模型列表", &palette, cx))
                    .children(self.search_visible.then(|| Input::new(&self.search_input)))
                    .child(model_rows)
                    .into_any_element(),
            )
        } else {
            None
        };

        div()
            .id("settings-providers-tab")
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::GROUP)
            .child(cstoa_card)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child("第三方与自定义模型服务"),
                            )
                            .child(
                                div()
                                    .id("providers-new-custom-btn")
                                    .test_support()
                                    .w(height::COMPACT)
                                    .h(height::COMPACT)
                                    .rounded(radius::CONTROL)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.open_new_custom_provider(w, cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::Plus)
                                            .size(px(14.0))
                                            .text_color(palette.text_secondary),
                                    ),
                            ),
                    )
                    .child(provider_list),
            )
            .children(detail_section)
    }
}

use std::sync::Arc;

use cst_client::ClientHandle;
use gpui::{Image, StyledImage as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, TestSupportExt,
    Window, div, img, px,
};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::auth::cstoa_qr::{now_epoch_ms, parse_cstoa_oauth_payload, rasterize_qr_image_offline};
use crate::background::{SkyPreset, render_login_9_stop_fade, render_sky_background};
use crate::shell::open_url_in_trusted_browser;
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, radius, scale, space,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethodTab {
    CstoaQr,
    OAuth,
    ApiKey,
}

#[derive(Debug, Clone)]
pub enum LoginViewEvent {
    NavigateToAccount,
    NavigateToCustomGatewaySettings,
    CredentialsSavedReturn,
    ErrorMessage(String),
}

/// Login View (`cst-pilot-desktop.pen` 「04 · 账号与登录」 `tM7YQ` light / `jiQRy` dark):
/// Left `864×800` dynamic sky shader canvas + Right `300px` centered login viewport.
pub struct LoginView {
    client: ClientHandle,
    tab: LoginMethodTab,
    sky_time_secs: f32,
    qr_image: Option<Arc<Image>>,
    qr_flow_id: Option<String>,
    qr_expires_at_ms: u64,
    qr_loading: bool,
    qr_error: Option<SharedString>,
    selected_provider_id: String,
    api_key_input: Entity<InputState>,
    base_url_input: Entity<InputState>,
    saving_key: bool,
}

impl EventEmitter<LoginViewEvent> for LoginView {}

impl LoginView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("输入 API Key 密钥…")
                .masked(true)
        });
        let base_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("可选自定义 Base URL…"));

        Self {
            client,
            tab: LoginMethodTab::CstoaQr,
            sky_time_secs: 0.0,
            qr_image: None,
            qr_flow_id: None,
            qr_expires_at_ms: 0,
            qr_loading: false,
            qr_error: None,
            selected_provider_id: "deepseek".into(),
            api_key_input,
            base_url_input,
            saving_key: false,
        }
    }

    pub fn set_sky_time(&mut self, sky_time_secs: f32) {
        self.sky_time_secs = sky_time_secs;
    }

    pub fn start_cstoa_qr_flow(&mut self, cx: &mut Context<Self>) {
        if self.qr_loading {
            return;
        }
        self.qr_loading = true;
        self.qr_error = None;
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client
                .start_auth_flow("cstoa".into(), "oauth".into(), None)
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.qr_loading = false;
                match res {
                    Ok(val) => {
                        if let Some(parsed) = parse_cstoa_oauth_payload(&val) {
                            match rasterize_qr_image_offline(&parsed.verification_uri_complete) {
                                Ok(img_arc) => {
                                    this.qr_image = Some(img_arc);
                                    this.qr_flow_id = parsed.flow_id;
                                    this.qr_expires_at_ms = parsed.expires_at_ms;
                                    this.qr_error = None;
                                    this.poll_cstoa_oauth_completion(cx);
                                }
                                Err(err) => {
                                    this.qr_error = Some(SharedString::from(err));
                                }
                            }
                        } else {
                            this.qr_error =
                                Some(SharedString::from("未能获取扫码登录地址，请重试。"));
                        }
                    }
                    Err(err) => {
                        this.qr_error = Some(SharedString::from(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn poll_cstoa_oauth_completion(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let status_res = client
                .get_auth_flow_status("cstoa".into(), "oauth".into())
                .await;
            if let Ok(st) = status_res {
                let state_str = st
                    .get("status")
                    .or_else(|| st.get("state"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if matches!(state_str, "succeeded" | "completed" | "done") {
                    let _ = client.refresh_account().await;
                    let _ = client.refresh_globals().await;
                    let _ = client.refresh_models(None).await;
                    let _ = weak.update(cx, |this, cx| {
                        this.qr_image = None;
                        cx.emit(LoginViewEvent::NavigateToAccount);
                    });
                }
            }
        })
        .detach();
    }

    fn start_provider_oauth(&mut self, provider_id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            match client
                .start_auth_flow(provider_id.clone(), "oauth".into(), None)
                .await
            {
                Ok(val) => {
                    if let Some(url) = val
                        .get("url")
                        .or_else(|| val.get("verificationUriComplete"))
                        .and_then(Value::as_str)
                    {
                        open_url_in_trusted_browser(url);
                    }
                }
                Err(err) => {
                    let _ = weak.update(cx, |_, cx| {
                        cx.emit(LoginViewEvent::ErrorMessage(err.to_string()));
                    });
                }
            }
        })
        .detach();
    }

    fn submit_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving_key {
            return;
        }
        let raw_key = Zeroizing::new(self.api_key_input.read(cx).value().trim().to_owned());
        if raw_key.is_empty() {
            cx.emit(LoginViewEvent::ErrorMessage(
                "请输入有效的 API Key。".into(),
            ));
            return;
        }
        let base_url = {
            let b = self.base_url_input.read(cx).value().trim().to_owned();
            (!b.is_empty()).then_some(b)
        };
        let provider = self.selected_provider_id.clone();

        // Wipe UI input immediately so plaintext does not linger
        self.api_key_input
            .update(cx, |inp, cx| inp.set_value("", window, cx));
        self.saving_key = true;
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client
                .save_provider_api_key(provider, raw_key, base_url)
                .await;
            let _ = client.refresh_globals().await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |this, cx| {
                this.saving_key = false;
                match res {
                    Ok(_) => {
                        cx.emit(LoginViewEvent::CredentialsSavedReturn);
                    }
                    Err(err) => {
                        cx.emit(LoginViewEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_cstoa_qr_pane(
        &mut self,
        palette: &DesktopPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Auto-navigate to Account view as soon as global SSE `oauth_result` / `auth_changed` marks CSTOA signed in!
        if self
            .client
            .snapshot()
            .account
            .as_ref()
            .is_some_and(|a| a.signed_in)
        {
            let ent = cx.entity();
            cx.defer(move |cx| {
                ent.update(cx, |_, cx| {
                    cx.emit(LoginViewEvent::NavigateToAccount);
                });
            });
        }

        if self.qr_image.is_none() && !self.qr_loading && self.qr_error.is_none() {
            let ent = cx.entity();
            cx.defer(move |cx| {
                ent.update(cx, |this, cx| this.start_cstoa_qr_flow(cx));
            });
        }

        let now = now_epoch_ms();
        let expired = self.qr_expires_at_ms > 0 && now >= self.qr_expires_at_ms;
        let rem_secs = if self.qr_expires_at_ms > now {
            window.request_animation_frame();
            Some(self.qr_expires_at_ms.saturating_sub(now).div_ceil(1000))
        } else {
            None
        };

        let qr_box = if let Some(ref qr_img) = self.qr_image
            && !expired
        {
            div()
                .id("cstoa-qr-image-box")
                .test_support()
                .w(px(216.0))
                .h(px(216.0))
                .p(space::SM)
                .rounded(radius::SPECIAL)
                .border_1()
                .border_color(palette.border_subtle)
                .bg(gpui_kit::white())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    img(qr_img.clone())
                        .w(px(196.0))
                        .h(px(196.0))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element()
        } else {
            div()
                .id("cstoa-qr-refresh-placeholder")
                .test_support()
                .w(px(216.0))
                .h(px(216.0))
                .rounded(radius::SPECIAL)
                .border_1()
                .border_color(palette.border_subtle)
                .bg(palette.bg_surface)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(space::MD)
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .px(space::MD)
                        .text_color(if self.qr_error.is_some() {
                            palette.status_danger
                        } else {
                            palette.text_secondary
                        })
                        .child(if self.qr_loading {
                            SharedString::from("正在生成本地二维码…")
                        } else if expired {
                            SharedString::from("二维码已过期")
                        } else if let Some(ref err) = self.qr_error {
                            err.clone()
                        } else {
                            SharedString::from("点击获取扫码二维码")
                        }),
                )
                .children((!self.qr_loading).then(|| {
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("cstoa-qr-refresh-btn")
                        .test_support()
                        .h(height::CONTROL)
                        .px(space::MD)
                        .rounded(radius::CONTROL)
                        .bg(palette.accent_solid)
                        .text_color(palette.text_on_accent)
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.qr_image = None;
                            this.start_cstoa_qr_flow(cx);
                        }))
                        .child(
                            Icon::new(IconName::RefreshCw)
                                .size(px(13.0))
                                .text_color(palette.text_on_accent),
                        )
                        .child(cjk_inline_label("刷新二维码"))
                }))
                .into_any_element()
        };

        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(space::MD)
            .child(qr_box)
            .child(
                apply_scale(div(), scale::CAPTION)
                    .text_color(palette.text_secondary)
                    .child(match rem_secs {
                        Some(s) => {
                            SharedString::from(format!("请使用微信扫码确认（有效剩余 {s} 秒）"))
                        }
                        None => SharedString::from("使用科协专属账号扫码登录"),
                    }),
            )
    }
}

impl Render for LoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;

        // Left `864×800` dynamic sky canvas (`tM7YQ` / `jiQRy`) with 9-stop horizontal right fade
        let left_canvas = div()
            .id("login-sky-canvas-864")
            .test_support()
            .relative()
            .w(crate::theme::layout::LOGIN_SHADER_WIDTH)
            .min_w(px(360.0))
            .flex_shrink(1.0)
            .h_full()
            .overflow_hidden()
            .child(render_sky_background(
                SkyPreset::login_for_theme(palette.mode),
                self.sky_time_secs,
                1.0,
            ))
            .child(render_login_9_stop_fade(&palette));

        let snap = self.client.snapshot();
        let providers = snap
            .auth_providers
            .as_ref()
            .map(|p| p.as_slice())
            .unwrap_or(&[]);

        let tab_content = match self.tab {
            LoginMethodTab::CstoaQr => self
                .render_cstoa_qr_pane(&palette, window, cx)
                .into_any_element(),
            LoginMethodTab::OAuth => {
                let mut col = div().w_full().flex().flex_col().gap(space::SM);
                let oauth_providers: Vec<_> = providers
                    .iter()
                    .filter(|p| p.supports_oauth && p.id != "cstoa")
                    .collect();
                if oauth_providers.is_empty() {
                    col = col.child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_muted)
                            .child("当前暂无其他支持浏览器 OAuth 的模型服务。"),
                    );
                } else {
                    for p in oauth_providers {
                        let pid = p.id.clone();
                        let pname = p.name.clone();
                        col = col.child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id(SharedString::from(format!("login-oauth-btn-{pid}")))
                                .test_support()
                                .w_full()
                                .h(height::FIELD)
                                .px(space::MD)
                                .rounded(radius::ROW)
                                .border_1()
                                .border_color(palette.border_strong)
                                .bg(palette.bg_surface)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .justify_between()
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_provider_oauth(pid.clone(), cx);
                                }))
                                .child(SharedString::from(format!("使用 {pname} 授权登录")))
                                .child(
                                    Icon::new(IconName::ExternalLink)
                                        .size(px(14.0))
                                        .text_color(palette.text_secondary),
                                ),
                        );
                    }
                }
                col.into_any_element()
            }
            LoginMethodTab::ApiKey => {
                let mut provider_pills = div().flex().flex_wrap().gap(space::XS);
                let key_providers: Vec<(String, String)> = {
                    let from_snap: Vec<(String, String)> = providers
                        .iter()
                        .filter(|p| p.supports_api_key)
                        .map(|p| (p.id.clone(), p.name.clone()))
                        .collect();
                    if from_snap.is_empty() {
                        vec![
                            ("deepseek".into(), "DeepSeek".into()),
                            ("openai".into(), "OpenAI".into()),
                            ("anthropic".into(), "Anthropic".into()),
                        ]
                    } else {
                        from_snap
                    }
                };
                for (pid, pname) in key_providers {
                    let active = self.selected_provider_id == pid;
                    let pid_click = pid.clone();
                    provider_pills = provider_pills.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(SharedString::from(format!("login-apikey-provider-{pid}")))
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
                                this.selected_provider_id = pid_click.clone();
                                cx.notify();
                            }))
                            .child(SharedString::from(pname)),
                    );
                }

                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(provider_pills)
                    .child(Input::new(&self.api_key_input))
                    .child(Input::new(&self.base_url_input))
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("login-apikey-save-btn")
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
                            .on_click(cx.listener(|this, _, w, cx| this.submit_api_key(w, cx)))
                            .child(cjk_inline_label(if self.saving_key {
                                "正在保存…"
                            } else {
                                "保存凭据"
                            })),
                    )
                    .into_any_element()
            }
        };

        // Right `300px` centered login viewport
        let right_viewport = div()
            .flex_1()
            .min_w(px(360.0))
            .h_full()
            .bg(palette.bg_workspace)
            .flex()
            .items_center()
            .justify_center()
            .px(space::GROUP)
            .child(
                div()
                    .id("login-viewport-300")
                    .test_support()
                    .w(px(300.0))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap(space::LG)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::XS)
                            .child(
                                apply_scale(div(), scale::TITLE)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("登录模型服务")),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_secondary)
                                    .child("各模型服务凭据彼此独立，任一就绪即可开始诊断。"),
                            ),
                    )
                    // 3-tab selector (`CSTOA / OAuth / APIKEY`)
                    .child(
                        div()
                            .w_full()
                            .p(space::MICRO)
                            .rounded(radius::ROW)
                            .bg(palette.bg_surface)
                            .flex()
                            .gap(space::MICRO)
                            .child(login_tab_btn(
                                "login-tab-cstoa",
                                "CSTOA 扫码",
                                self.tab == LoginMethodTab::CstoaQr,
                                &palette,
                                cx.listener(|this, _, _, cx| {
                                    this.tab = LoginMethodTab::CstoaQr;
                                    cx.notify();
                                }),
                            ))
                            .child(login_tab_btn(
                                "login-tab-oauth",
                                "OAuth",
                                self.tab == LoginMethodTab::OAuth,
                                &palette,
                                cx.listener(|this, _, _, cx| {
                                    this.tab = LoginMethodTab::OAuth;
                                    cx.notify();
                                }),
                            ))
                            .child(login_tab_btn(
                                "login-tab-apikey",
                                "API Key",
                                self.tab == LoginMethodTab::ApiKey,
                                &palette,
                                cx.listener(|this, _, _, cx| {
                                    this.tab = LoginMethodTab::ApiKey;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(tab_content)
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id("login-goto-custom-gateway-btn")
                            .test_support()
                            .w_full()
                            .h(height::CONTROL)
                            .rounded(radius::CONTROL)
                            .text_color(palette.accent_solid)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(LoginViewEvent::NavigateToCustomGatewaySettings);
                            }))
                            .child(cjk_inline_label("配置自定义模型网关 →")),
                    ),
            );

        div()
            .id("login-view-tm7yq")
            .test_support()
            .size_full()
            .flex()
            .flex_row()
            .items_stretch()
            .child(left_canvas)
            .child(right_viewport)
    }
}

fn login_tab_btn<F>(
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
        .rounded(radius::CONTROL)
        .bg(if active {
            palette.bg_elevated
        } else {
            gpui_kit::transparent_black()
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

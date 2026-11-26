use std::{sync::Arc, time::Duration};

use cst_client::{ClientError, ClientHandle};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputContentType, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription,
    TestSupportExt, Window, div, px,
};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::accessibility::A11yElementExt as _;
use crate::auth::cstoa_qr::{
    QrMatrix, encode_qr_matrix_offline, extract_cstoa_oauth_error, now_epoch_ms,
    parse_cstoa_oauth_payload,
};
use crate::background::{
    FrostedGlassSpec, SkyPreset, render_frosted_surface, render_login_9_stop_fade,
    render_sky_background_rounded,
};
use crate::shell::open_url_in_trusted_browser;
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, motion,
    radius, scale, space,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethodTab {
    CstoaQr,
    ApiKey,
    OAuth,
}

#[derive(Debug, Clone)]
pub enum LoginViewEvent {
    NavigateToAccount,
    NavigateToCustomGatewaySettings,
    CredentialsSavedReturn,
    ErrorMessage(String),
}

/// Login View (`cst-pilot-desktop.pen` 「04 · 账号与登录」 `tM7YQ` light / `jiQRy` dark):
/// Full-bleed `$bg-workspace` with absolute left `864×800` dynamic sky shader canvas + 9-stop horizontal fade,
/// and right `300px` centered login viewport (`172×172` QR box `cstoa-qr-image-box` with `148×148` QR image).
pub struct LoginView {
    client: ClientHandle,
    tab: LoginMethodTab,
    sky_time_secs: f32,
    qr_matrix: Option<Arc<QrMatrix>>,
    active: bool,
    generation: u64,
    qr_generation: u64,
    qr_running: bool,
    active_qr_flow_id: Option<String>,
    oauth_pending: bool,
    window: gpui::AnyWindowHandle,
    qr_expires_at_ms: u64,
    qr_loading: bool,
    qr_error: Option<SharedString>,
    selected_provider_id: String,
    api_key_input: Entity<InputState>,
    base_url_input: Entity<InputState>,
    api_key_error: Option<SharedString>,
    provider_dropdown_open: bool,
    provider_trigger_focus: gpui::FocusHandle,
    provider_menu_focus: gpui::FocusHandle,
    provider_index: usize,
    provider_scroll: gpui::ScrollHandle,
    saving_key: bool,
    enter_opacity: f32,
    enter_dx: gpui::Pixels,
    enter_dy: gpui::Pixels,
    tab_slide: motion::PageSlide,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LoginViewEvent> for LoginView {}

impl LoginView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("输入 API Key")
                .masked(true)
        });
        let base_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.example.com/v1"));

        let subscriptions = [&api_key_input, &base_url_input]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        if this.api_key_error.take().is_some() {
                            cx.notify();
                        }
                    }
                    InputEvent::Focus => {
                        this.dismiss_dropdowns_on_outside_click(cx);
                    }
                    _ => {}
                })
            })
            .collect();

        Self {
            client,
            tab: LoginMethodTab::CstoaQr,
            sky_time_secs: 0.0,
            qr_matrix: None,
            active: false,
            generation: 0,
            qr_generation: 0,
            qr_running: false,
            active_qr_flow_id: None,
            oauth_pending: false,
            window: window.window_handle(),
            qr_expires_at_ms: 0,
            qr_loading: false,
            qr_error: None,
            selected_provider_id: "deepseek".into(),
            api_key_input,
            base_url_input,
            api_key_error: None,
            provider_dropdown_open: false,
            provider_trigger_focus: cx.focus_handle().tab_stop(true),
            provider_menu_focus: cx.focus_handle(),
            provider_index: 0,
            provider_scroll: gpui::ScrollHandle::new(),
            saving_key: false,
            enter_opacity: 1.0,
            enter_dx: px(0.0),
            enter_dy: px(0.0),
            tab_slide: motion::PageSlide::default(),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        self.tab_slide = motion::PageSlide::default();
        if !active {
            self.enter_opacity = 1.0;
            self.enter_dx = px(0.0);
            self.enter_dy = px(0.0);
            self.set_tab_inner(LoginMethodTab::CstoaQr, false, cx);
        } else if self.tab == LoginMethodTab::CstoaQr {
            self.start_cstoa_qr_flow(cx);
        }
        cx.notify();
    }

    pub fn open_tab_without_slide(&mut self, tab: LoginMethodTab, cx: &mut Context<Self>) {
        self.tab_slide = motion::PageSlide::default();
        self.set_tab_inner(tab, false, cx);
    }

    pub fn set_tab(&mut self, tab: LoginMethodTab, cx: &mut Context<Self>) {
        self.set_tab_inner(tab, self.active, cx);
    }

    fn set_tab_inner(&mut self, tab: LoginMethodTab, animate_slide: bool, cx: &mut Context<Self>) {
        if animate_slide && self.tab != tab {
            let tab_order = |t: LoginMethodTab| match t {
                LoginMethodTab::CstoaQr => 0,
                LoginMethodTab::ApiKey => 1,
                LoginMethodTab::OAuth => 2,
            };
            let dir = if tab_order(tab) >= tab_order(self.tab) {
                1.0
            } else {
                -1.0
            };
            self.tab_slide.start(dir, cx);
        }
        self.generation += 1;
        self.reset_qr(cx);
        self.tab = tab;
        self.provider_dropdown_open = false;
        self.api_key_error = None;
        let key = self.api_key_input.downgrade();
        let url = self.base_url_input.downgrade();
        let window = self.window;
        cx.spawn(async move |_, cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = key.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.set_masked(true, window, cx);
                });
                let _ = url.update(cx, |input, cx| input.set_value("", window, cx));
            });
        })
        .detach();
        if self.active && tab == LoginMethodTab::CstoaQr {
            self.start_cstoa_qr_flow(cx);
        }
        cx.notify();
    }

    pub fn has_dropdown(&self) -> bool {
        self.provider_dropdown_open
    }

    pub fn focus_primary_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.tab != LoginMethodTab::ApiKey || self.provider_dropdown_open {
            return false;
        }
        self.api_key_input
            .update(cx, |inp, cx| inp.focus(window, cx));
        cx.notify();
        true
    }

    pub fn dismiss_dropdown(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.provider_dropdown_open {
            return false;
        }
        self.provider_dropdown_open = false;
        self.provider_trigger_focus.focus(window, cx);
        cx.notify();
        true
    }

    pub fn dismiss_dropdowns_on_outside_click(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.provider_dropdown_open {
            return false;
        }
        self.provider_dropdown_open = false;
        cx.notify();
        true
    }

    pub fn set_sky_time(&mut self, sky_time_secs: f32) {
        self.sky_time_secs = sky_time_secs;
    }

    pub fn set_enter_transition(&mut self, opacity: f32, dx: gpui::Pixels, dy: gpui::Pixels) {
        self.enter_opacity = opacity.clamp(0.0, 1.0);
        self.enter_dx = dx;
        self.enter_dy = dy;
    }

    fn reset_qr(&mut self, cx: &mut Context<Self>) {
        self.qr_generation += 1;
        // Cancel any active server-side QR flow immediately when leaving or switching tabs
        // so existing CSTOA credentials remain untouched and provider locks release at once.
        if let Some(flow_id) = self.active_qr_flow_id.take() {
            let client = self.client.clone();
            cx.background_executor()
                .spawn(async move {
                    let _ = client
                        .cancel_auth_flow("cstoa".into(), "oauth".into(), Some(flow_id))
                        .await;
                })
                .detach();
        }
        self.qr_matrix = None;
        self.qr_error = None;
        self.qr_expires_at_ms = 0;
        self.qr_loading = false;
        cx.notify();
    }

    pub fn refresh_cstoa_qr_flow(&mut self, cx: &mut Context<Self>) {
        self.reset_qr(cx);
        self.start_cstoa_qr_flow(cx);
    }

    pub fn start_cstoa_qr_flow(&mut self, cx: &mut Context<Self>) {
        if !self.active
            || self.tab != LoginMethodTab::CstoaQr
            || self.qr_running
            || self.qr_matrix.is_some()
            || self.qr_error.is_some()
            || self.qr_expires_at_ms != 0
        {
            return;
        }
        self.qr_running = true;
        self.qr_loading = true;
        cx.notify();

        let client = self.client.clone();
        let generation = self.qr_generation;
        cx.spawn(async move |weak, cx| {
            let started = client
                .start_auth_flow("cstoa".into(), "oauth".into(), None)
                .await;
            let flow_id = started
                .as_ref()
                .ok()
                .and_then(|val| val.get("flowId").or_else(|| val.get("id")))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let started_state = started
                .as_ref()
                .ok()
                .and_then(|val| val.get("state").or_else(|| val.get("status")))
                .and_then(Value::as_str);
            let mut succeeded = matches!(started_state, Some("succeeded" | "completed" | "done"));
            let started_terminal =
                succeeded || matches!(started_state, Some("failed" | "cancelled" | "idle"));
            let live = weak
                .update(cx, |this, cx| {
                    if this.qr_generation != generation {
                        return false;
                    }
                    this.qr_loading = false;
                    match &started {
                        Ok(_) if succeeded => {}
                        Ok(val) if !started_terminal => match parse_cstoa_oauth_payload(val) {
                            Some(parsed) if flow_id.is_some() => {
                                this.active_qr_flow_id = flow_id.clone();
                                this.qr_expires_at_ms = parsed.expires_at_ms;
                                match encode_qr_matrix_offline(&parsed.verification_uri_complete) {
                                    Ok(matrix) => this.qr_matrix = Some(matrix),
                                    Err(err) => this.qr_error = Some(err.into()),
                                }
                            }
                            _ => {
                                let msg = extract_cstoa_oauth_error(val)
                                    .unwrap_or_else(|| "未能获取扫码登录地址，请重试。".into());
                                this.qr_error = Some(msg.into());
                            }
                        },
                        Ok(val) => {
                            let msg = extract_cstoa_oauth_error(val)
                                .unwrap_or_else(|| "未能获取扫码登录地址，请重试。".into());
                            this.qr_error = Some(msg.into());
                        }
                        Err(err) => {
                            this.qr_error = Some(err.to_string().into());
                        }
                    }
                    cx.notify();
                    this.qr_matrix.is_some()
                })
                .unwrap_or(false);

            let mut already_terminal = started_terminal;
            let mut last_remaining_sec: Option<u64> = None;
            if live && let Some(id) = flow_id.as_deref() {
                loop {
                    let current = weak
                        .update(cx, |this, _| {
                            this.qr_generation == generation
                                && this.active_qr_flow_id.as_deref() == Some(id)
                                && now_epoch_ms() < this.qr_expires_at_ms
                        })
                        .unwrap_or(false);
                    if !current {
                        break;
                    }
                    // Retry transient failures until expiry.
                    if let Ok(status) = client
                        .get_auth_flow_status("cstoa".into(), "oauth".into())
                        .await
                    {
                        let is_current = status
                            .get("flowId")
                            .or_else(|| status.get("id"))
                            .and_then(Value::as_str)
                            == Some(id);
                        let state = status
                            .get("state")
                            .or_else(|| status.get("status"))
                            .and_then(Value::as_str);
                        if is_current && matches!(state, Some("succeeded" | "completed" | "done")) {
                            succeeded = true;
                            break;
                        }
                        if !is_current || matches!(state, Some("failed" | "cancelled" | "idle")) {
                            already_terminal = true;
                            let err_msg = is_current
                                .then(|| extract_cstoa_oauth_error(&status))
                                .flatten()
                                .unwrap_or_else(|| "扫码登录已结束，请重试。".into());
                            let _ = weak.update(cx, |this, cx| {
                                if this.qr_generation == generation {
                                    this.active_qr_flow_id = None;
                                    this.qr_matrix = None;
                                    this.qr_expires_at_ms = 0;
                                    this.qr_loading = false;
                                    this.qr_error = Some(err_msg.into());
                                    cx.notify();
                                }
                            });
                            break;
                        }
                    }
                    let window_active = weak
                        .update(cx, |this, cx| {
                            if generation != this.qr_generation {
                                return false;
                            }
                            let active = this
                                .window
                                .update(cx, |_, window, _| {
                                    let vp = window.viewport_size();
                                    window.is_window_active()
                                        && window.is_visible()
                                        && f32::from(vp.width) > 1.0
                                        && f32::from(vp.height) > 1.0
                                })
                                .unwrap_or(false);
                            let rem_sec = this
                                .qr_expires_at_ms
                                .saturating_sub(now_epoch_ms())
                                .div_ceil(1000);
                            if active && last_remaining_sec != Some(rem_sec) {
                                last_remaining_sec = Some(rem_sec);
                                cx.notify();
                            }
                            active
                        })
                        .unwrap_or(false);
                    let poll_interval_ms = if window_active { 250 } else { 1500 };
                    cx.background_executor()
                        .timer(Duration::from_millis(poll_interval_ms))
                        .await;
                }
            }
            if succeeded {
                // Refresh globals concurrently while transitioning immediately to Account view
                // so the UI does not block on a serial remote account/catalog roundtrip.
                let refresh_client = client.clone();
                cx.background_executor()
                    .spawn(async move {
                        let _ = refresh_client.refresh_globals().await;
                    })
                    .detach();
            }
            if !succeeded
                && !already_terminal
                && let Some(id) = flow_id
            {
                let _ = client
                    .cancel_auth_flow("cstoa".into(), "oauth".into(), Some(id))
                    .await;
            }
            let _ = weak.update(cx, |this, cx| {
                this.qr_running = false;
                this.active_qr_flow_id = None;
                if succeeded && this.qr_generation == generation {
                    cx.emit(LoginViewEvent::NavigateToAccount);
                } else if this.qr_generation != generation {
                    this.start_cstoa_qr_flow(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_provider_oauth(&mut self, provider_id: String, cx: &mut Context<Self>) {
        if self.oauth_pending {
            return;
        }
        self.oauth_pending = true;
        let generation = self.generation;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client
                .start_auth_flow(provider_id, "oauth".into(), None)
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.oauth_pending = false;
                if this.generation != generation {
                    return;
                }
                match res {
                    Ok(val) => {
                        if let Some(url) = val
                            .get("authUrl")
                            .and_then(|a| a.get("url"))
                            .or_else(|| val.get("url"))
                            .or_else(|| val.get("verificationUriComplete"))
                            .and_then(Value::as_str)
                        {
                            open_url_in_trusted_browser(url);
                        }
                    }
                    Err(err) => cx.emit(LoginViewEvent::ErrorMessage(err.to_string())),
                }
            });
        })
        .detach();
    }

    fn toggle_provider_menu(
        &mut self,
        choices: &[(String, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving_key || self.dismiss_dropdown(window, cx) {
            return;
        }
        self.provider_index = choices
            .iter()
            .position(|(id, _)| id == &self.selected_provider_id)
            .unwrap_or(0);
        self.provider_scroll.scroll_to_item(self.provider_index);
        self.provider_dropdown_open = true;
        self.provider_menu_focus.focus(window, cx);
        cx.notify();
    }

    fn select_api_key_provider(
        &mut self,
        provider: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving_key {
            return;
        }
        if self.selected_provider_id != provider {
            self.selected_provider_id = provider.to_owned();
            self.api_key_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.set_masked(true, window, cx);
            });
            self.base_url_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.api_key_error = None;
        }
        self.dismiss_dropdown(window, cx);
    }

    fn submit_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving_key || !self.active || self.tab != LoginMethodTab::ApiKey {
            return;
        }
        let text = Zeroizing::new(self.api_key_input.read(cx).text().to_string());
        let raw_key = Zeroizing::new(text.trim().to_owned());
        if raw_key.is_empty() {
            self.api_key_error = Some("请输入有效的 API Key。".into());
            cx.notify();
            return;
        }
        self.dismiss_dropdown(window, cx);
        self.api_key_error = None;
        let base_url = {
            let b = self.base_url_input.read(cx).value().trim().to_owned();
            (!b.is_empty()).then_some(b)
        };
        let provider = self.selected_provider_id.clone();
        self.saving_key = true;
        cx.notify();

        let generation = self.generation;
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let res = client
                .save_provider_api_key(provider, raw_key, base_url)
                .await;
            if res.is_ok() {
                let _ = client.refresh_globals().await;
                let _ = client.refresh_models(None).await;
            }
            let _ = weak.update_in(cx, |this, window, cx| {
                this.saving_key = false;
                cx.notify();
                if this.generation != generation {
                    return;
                }
                match res {
                    Ok(_) => {
                        this.api_key_input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        cx.emit(LoginViewEvent::CredentialsSavedReturn);
                    }
                    Err(ClientError::BadRequest { .. }) => {
                        this.api_key_error =
                            Some("无法保存凭据，请检查 API Key 和服务地址。".into());
                    }
                    Err(_) => cx.emit(LoginViewEvent::ErrorMessage(
                        "保存凭据失败，请重试。".into(),
                    )),
                }
            });
        })
        .detach();
    }

    fn render_cstoa_qr_pane(
        &mut self,
        _mono_family: SharedString,
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.active
            && self.qr_matrix.is_none()
            && !self.qr_running
            && self.qr_error.is_none()
            && self.qr_expires_at_ms == 0
        {
            let ent = cx.entity();
            cx.defer(move |cx| ent.update(cx, |this, cx| this.start_cstoa_qr_flow(cx)));
        }

        let now = now_epoch_ms();
        let expired = self.qr_expires_at_ms > 0 && now >= self.qr_expires_at_ms;
        let rem_secs = if self.qr_expires_at_ms > now {
            Some(self.qr_expires_at_ms.saturating_sub(now).div_ceil(1000))
        } else {
            None
        };

        let has_live_qr = self.qr_matrix.is_some() && !expired;
        let qr_pending =
            self.qr_loading || (self.qr_running && self.qr_error.is_none() && !expired);
        let qr_box = if let Some(ref qr_matrix) = self.qr_matrix
            && !expired
        {
            // Render QR modules directly onto the single `$radius-row` (`8px`) white card (`kzCcS`)
            // via vector `canvas` + `paint_quad` so D3D11 texture-atlas bilinear sampling never
            // produces an inner gray border halo around the QR area in light or dark themes.
            let matrix = Arc::clone(qr_matrix);
            let qr_canvas = gpui::canvas(
                |_, _, _| {},
                move |bounds, _, window, _cx| {
                    let n = matrix.width;
                    if n == 0 {
                        return;
                    }
                    let side = f32::from(bounds.size.width).min(f32::from(bounds.size.height));
                    let scale = window.scale_factor().max(1.0);
                    let origin_x =
                        f32::from(bounds.origin.x) + (f32::from(bounds.size.width) - side) * 0.5;
                    let origin_y =
                        f32::from(bounds.origin.y) + (f32::from(bounds.size.height) - side) * 0.5;
                    let edge = |idx: usize| -> f32 {
                        ((idx as f32 * side * scale) / (n as f32)).round() / scale
                    };
                    let dark_color = gpui::rgb(0x111827);
                    for row in 0..n {
                        let y0 = origin_y + edge(row);
                        let y1 = origin_y + edge(row + 1);
                        let h = (y1 - y0).max(1.0 / scale);
                        let mut col = 0;
                        while col < n {
                            if !matrix.dark[row * n + col] {
                                col += 1;
                                continue;
                            }
                            let start_col = col;
                            while col < n && matrix.dark[row * n + col] {
                                col += 1;
                            }
                            let x0 = origin_x + edge(start_col);
                            let x1 = origin_x + edge(col);
                            let w = (x1 - x0).max(1.0 / scale);
                            window.paint_quad(gpui::fill(
                                gpui::Bounds::new(
                                    gpui::point(px(x0), px(y0)),
                                    gpui::size(px(w), px(h)),
                                ),
                                dark_color,
                            ));
                        }
                    }
                },
            )
            .w(px(144.0))
            .h(px(144.0));

            div()
                .id("cstoa-qr-image-box")
                .test_support()
                .a11y_image("cstoa-qr-image-box", "CSTOA 扫码登录二维码")
                .w(px(172.0))
                .h(px(172.0))
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette.border_subtle)
                .bg(gpui_kit::white())
                .shadow(palette.craft_stack_shadows())
                .flex()
                .items_center()
                .justify_center()
                .child(qr_canvas)
                .into_any_element()
        } else {
            let placeholder_msg = if qr_pending {
                SharedString::from("正在生成本地二维码…")
            } else if expired {
                SharedString::from("二维码已过期")
            } else if let Some(ref err) = self.qr_error {
                err.clone()
            } else {
                SharedString::from("点击下方刷新获取二维码")
            };
            div()
                .id("cstoa-qr-refresh-placeholder")
                .test_support()
                .a11y_status("cstoa-qr-refresh-placeholder", placeholder_msg.clone())
                .w(px(172.0))
                .h(px(172.0))
                .px(space::MD)
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette.border_subtle)
                .bg(palette.bg_surface)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::CAPTION)
                        .text_color(if self.qr_error.is_some() {
                            palette.status_danger
                        } else {
                            palette.text_secondary
                        })
                        .child(placeholder_msg),
                )
                .into_any_element()
        };

        let countdown_text = match rem_secs {
            Some(s) => {
                let mins = s / 60;
                let secs = s % 60;
                SharedString::from(format!("{mins:02}:{secs:02}"))
            }
            None if qr_pending => SharedString::from("加载中"),
            None if expired => SharedString::from("00:00"),
            None => SharedString::from("待刷新"),
        };

        let dot_color = if has_live_qr {
            palette.status_success
        } else if qr_pending {
            palette.status_warning
        } else {
            palette.text_muted
        };

        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(space::MD)
            .child(qr_box)
            // `N1Rfg`: `6×6` status dot + `"等待扫码 ·"` / countdown + `1×12` separator + `$accent-ink` refresh link
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(space::SM)
                    .child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded(radius::FULL)
                            .bg(dot_color),
                    )
                    .children(has_live_qr.then(|| {
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("等待扫码 ·"))
                    }))
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if has_live_qr {
                                palette.text_primary
                            } else {
                                palette.text_secondary
                            })
                            .child(countdown_text),
                    )
                    .child(div().w(px(1.0)).h(px(12.0)).bg(palette.border_strong))
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id("cstoa-qr-refresh-btn")
                            .test_support()
                            .a11y_button("cstoa-qr-refresh-btn", "刷新二维码")
                            .h(height::INLINE)
                            .px(space::XS)
                            .rounded(radius::CONTROL)
                            .text_color(palette.accent_ink)
                            .flex()
                            .items_center()
                            .gap(space::TIGHT)
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.qr_loading {
                                    this.refresh_cstoa_qr_flow(cx);
                                    cx.notify();
                                }
                            }))
                            .child(
                                Icon::new(IconName::RefreshCw)
                                    .size(px(12.0))
                                    .text_color(palette.accent_ink),
                            )
                            .child(cjk_inline_label("刷新")),
                    ),
            )
    }
}

impl Render for LoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let mono_family = theme_global.mono_font_family;
        let scale_factor = window.scale_factor();

        // Underlying left `864×800` absolute sky canvas (`tM7YQ` / `jiQRy`) with `.pen` `856px`
        // sky shader inset (`rtOFG` / `Q3yEX8`) over solid `$bg-workspace` + 9-stop horizontal fade.
        // Keeping the feather mask at full alpha while fading the D3D11 sky surface by `enter_opacity`
        // yields exact pre-composited cross-fade (`p * Final + (1 - p) * bg`) with zero right-edge flash.
        let left_canvas = div()
            .id("login-sky-canvas-864")
            .test_support()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(layout::LOGIN_SHADER_WIDTH)
            .h_full()
            .overflow_hidden()
            .bg(palette.bg_workspace)
            .child(div().w(px(856.0)).h_full().overflow_hidden().child(
                render_sky_background_rounded(
                    SkyPreset::login_for_theme(palette.mode),
                    self.sky_time_secs,
                    self.enter_opacity,
                    [0.0; 4],
                    scale_factor,
                ),
            ))
            .child(render_login_9_stop_fade(&palette));

        let snap = self.client.snapshot();
        let providers = snap
            .auth_providers
            .as_ref()
            .map(|p| p.as_slice())
            .unwrap_or(&[]);

        let mut provider_menu_opt = None;
        let tab_content = match self.tab {
            LoginMethodTab::CstoaQr => self
                .render_cstoa_qr_pane(mono_family, &palette, cx)
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
                            .child("当前暂无其他支持浏览器 OAuth 的提供商。"),
                    );
                } else {
                    for p in oauth_providers {
                        let pid = p.id.clone();
                        let pname = p.name.clone();
                        let btn_id = SharedString::from(format!("login-oauth-btn-{pid}"));
                        let btn_label = SharedString::from(format!("使用 {pname} 授权登录"));
                        col = col.child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id(btn_id.clone())
                                .test_support()
                                .a11y_button(btn_id, btn_label.clone())
                                .w_full()
                                .h(height::FIELD)
                                .px(space::MD)
                                .rounded(radius::CONTROL)
                                .border_1()
                                .border_color(palette.border_strong)
                                .bg(palette.bg_workspace)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .justify_between()
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_provider_oauth(pid.clone(), cx);
                                }))
                                .child(btn_label)
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
                let selected_provider_name = key_providers
                    .iter()
                    .find(|(pid, _)| pid == &self.selected_provider_id)
                    .map(|(_, pname)| pname.clone())
                    .unwrap_or_else(|| self.selected_provider_id.clone());

                let provider_menu_p = motion::surface_presence(
                    "login-apikey-provider-pop-p",
                    self.provider_dropdown_open,
                    motion::POPOVER_OPEN,
                    window,
                    cx,
                );
                let provider_interactive = self.provider_dropdown_open;
                provider_menu_opt = (provider_interactive || provider_menu_p > 0.001).then(|| {
                    let mut list_rows = div().w_full().flex().flex_col().gap(space::MICRO);

                    for (idx, (pid, pname)) in key_providers.iter().enumerate() {
                        let active = self.provider_index == idx;
                        let opt_label = SharedString::from(pname.clone());
                        let base_item = apply_scale(div(), scale::UI)
                            .flex_shrink_0()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if active {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::SM)
                            .child(div().min_w_0().truncate().child(opt_label.clone()))
                            .children(active.then(|| {
                                Icon::new(IconName::Check)
                                    .size(px(14.0))
                                    .text_color(palette.accent_ink)
                            }));
                        let item = if provider_interactive {
                            let pid_click = pid.clone();
                            let opt_id = SharedString::from(format!("login-apikey-provider-{pid}"));
                            let hover_bg = if active {
                                palette.bg_item_active
                            } else {
                                palette.bg_item_hover
                            };
                            base_item
                                .id(opt_id.clone())
                                .test_support()
                                .a11y_listitem(opt_id, opt_label, active)
                                .hover(move |s| s.bg(hover_bg))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_api_key_provider(&pid_click, window, cx);
                                }))
                                .into_any_element()
                        } else {
                            base_item.into_any_element()
                        };
                        list_rows = list_rows.child(item);
                    }

                    let list = if provider_interactive {
                        div()
                            .id("login-apikey-provider-menu-scroll")
                            .track_scroll(&self.provider_scroll)
                            .w_full()
                            .max_h(px(220.0))
                            .overflow_y_scroll()
                            .child(list_rows)
                            .into_any_element()
                    } else {
                        let offset_y = self.provider_scroll.offset().y;
                        div()
                            .w_full()
                            .max_h(px(220.0))
                            .overflow_hidden()
                            .child(div().relative().top(offset_y).child(list_rows))
                            .into_any_element()
                    };

                    let popover_panel = render_frosted_surface(
                        if provider_interactive {
                            "login-apikey-provider-popover"
                        } else {
                            "login-apikey-provider-popover-exit"
                        },
                        FrostedGlassSpec::popover(&palette).with_opacity(provider_menu_p),
                        palette.bg_elevated,
                        transparency_enabled,
                        scale_factor,
                    )
                    .p(space::XS)
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_hairline);

                    let popover_panel = if provider_interactive {
                        popover_panel
                            .test_support()
                            .a11y_dialog("login-apikey-provider-popover", "选择服务提供商")
                            .occlude()
                            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .child(
                                div()
                                    .w_full()
                                    .focus_trap(
                                        "login-apikey-provider-focus",
                                        &self.provider_menu_focus,
                                    )
                                    .on_key_down(cx.listener({
                                        let choices = key_providers.clone();
                                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                                            if crate::settings::move_picker_cursor(
                                                event,
                                                &mut this.provider_index,
                                                choices.len(),
                                                &this.provider_scroll,
                                            ) {
                                                cx.notify();
                                                cx.stop_propagation();
                                            } else if crate::settings::activates_button(event) {
                                                if let Some((id, _)) =
                                                    choices.get(this.provider_index)
                                                {
                                                    this.select_api_key_provider(id, window, cx);
                                                }
                                                cx.stop_propagation();
                                            }
                                        }
                                    }))
                                    .child(list),
                            )
                            .into_any_element()
                    } else {
                        popover_panel.child(list).into_any_element()
                    };

                    div().absolute().top(px(62.0)).left_0().right_0().child(
                        motion::apply_popover_motion(
                            div().rounded(radius::ROW).child(popover_panel),
                            provider_menu_p,
                            -6.0,
                            palette.craft_stack_shadows(),
                        ),
                    )
                });

                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(space::TIGHT)
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("服务提供商")),
                            )
                            .child(
                                apply_scale(div(), scale::UI)
                                    .id("login-apikey-provider-trigger")
                                    .test_support()
                                    .a11y_toggle_button(
                                        "login-apikey-provider-trigger",
                                        format!("服务提供商：{selected_provider_name}"),
                                        self.provider_dropdown_open,
                                    )
                                    .track_focus(&self.provider_trigger_focus)
                                    .tab_index(0)
                                    .on_key_down(cx.listener({
                                        let choices = key_providers.clone();
                                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                                            if crate::settings::activates_button(event) {
                                                this.toggle_provider_menu(&choices, window, cx);
                                                cx.stop_propagation();
                                            }
                                        }
                                    }))
                                    .w_full()
                                    .h(px(36.0))
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_strong)
                                    .bg(if self.provider_dropdown_open {
                                        palette.bg_item_active
                                    } else {
                                        palette.bg_workspace
                                    })
                                    .text_color(palette.text_primary)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(space::SM)
                                    .hover(|s| s.bg(palette.bg_surface))
                                    .capture_any_mouse_down(
                                        crate::shell::keep_popover_open_on_mouse_down,
                                    )
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.toggle_provider_menu(&key_providers, window, cx);
                                    }))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .child(SharedString::from(selected_provider_name)),
                                    )
                                    .child(motion::animated_chevron(
                                        "login-apikey-provider",
                                        self.provider_dropdown_open,
                                        true,
                                        px(14.0),
                                        palette.text_secondary,
                                        window,
                                        cx,
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(space::TIGHT)
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_primary)
                                    .child("API Key"),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(36.0))
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(if self.api_key_error.is_some() {
                                        palette.status_danger
                                    } else {
                                        palette.border_strong
                                    })
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.api_key_input)
                                            .accessibility_id("login-apikey-input")
                                            .content_type(InputContentType::Password)
                                            .disabled(self.saving_key)
                                            .aria_label("API Key 密钥")
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .w_full()
                                            .h_full()
                                            .mask_toggle(),
                                    ),
                            )
                            .children(self.api_key_error.clone().map(|err_msg| {
                                apply_scale(div(), scale::CAPTION)
                                    .id("login-apikey-error-text")
                                    .test_support()
                                    .a11y_status("login-apikey-error-text", err_msg.clone())
                                    .text_color(palette.status_danger)
                                    .child(err_msg)
                            })),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(space::TIGHT)
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("服务地址（可选）")),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(36.0))
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_strong)
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.base_url_input)
                                            .accessibility_id("login-baseurl-input")
                                            .disabled(self.saving_key)
                                            .aria_label("自定义 Base URL")
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .w_full()
                                            .h_full(),
                                    ),
                            ),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("login-apikey-save-btn")
                            .test_support()
                            .a11y_button(
                                "login-apikey-save-btn",
                                if self.saving_key {
                                    "正在保存"
                                } else {
                                    "保存凭据"
                                },
                            )
                            .w_full()
                            .h(px(36.0))
                            .px(space::MD)
                            .rounded(radius::CONTROL)
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

        // Compact switching footer below QR / form (`APIKEY 登录` / `OAuth 登录` / `自定义网关`)
        let mode_footer = div()
            .w_full()
            .pt(space::SM)
            .border_t_1()
            .border_color(palette.border_subtle)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::XS)
            .child(if self.tab == LoginMethodTab::CstoaQr {
                login_compact_switch_btn(
                    "login-tab-apikey",
                    "APIKEY 登录",
                    false,
                    &palette,
                    cx.listener(|this, _, _, cx| {
                        this.set_tab(LoginMethodTab::ApiKey, cx);
                    }),
                )
                .into_any_element()
            } else {
                login_compact_switch_btn(
                    "login-tab-cstoa",
                    "扫码登录",
                    false,
                    &palette,
                    cx.listener(|this, _, _, cx| {
                        this.set_tab(LoginMethodTab::CstoaQr, cx);
                    }),
                )
                .into_any_element()
            })
            .child(if self.tab == LoginMethodTab::OAuth {
                login_compact_switch_btn(
                    "login-tab-apikey",
                    "APIKEY 登录",
                    false,
                    &palette,
                    cx.listener(|this, _, _, cx| {
                        this.set_tab(LoginMethodTab::ApiKey, cx);
                    }),
                )
                .into_any_element()
            } else {
                login_compact_switch_btn(
                    "login-tab-oauth",
                    "OAuth 登录",
                    false,
                    &palette,
                    cx.listener(|this, _, _, cx| {
                        this.set_tab(LoginMethodTab::OAuth, cx);
                    }),
                )
                .into_any_element()
            })
            .child(login_compact_switch_btn(
                "login-goto-custom-gateway-btn",
                "自定义网关",
                false,
                &palette,
                cx.listener(|_, _, _, cx| {
                    cx.emit(LoginViewEvent::NavigateToCustomGatewaySettings);
                }),
            ));

        let (tab_op, tab_dx) = self.tab_slide.sample(window, cx);
        let content_op = (self.enter_opacity * tab_op).clamp(0.0, 1.0);
        let content_dx = self.enter_dx + tab_dx;
        let content_dy = self.enter_dy;

        // Right login main area overlapping smoothly from the fade blend zone rightward
        let right_viewport = div()
            .relative()
            .ml_auto()
            .w(px(560.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .px(space::GROUP)
            .child(
                div()
                    .id("login-viewport-300")
                    .test_support()
                    .relative()
                    .left(content_dx)
                    .top(content_dy)
                    .opacity(content_op)
                    .w(px(300.0))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(space::LG)
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(space::XS)
                            .child(
                                apply_scale(div(), scale::TITLE)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label(match self.tab {
                                        LoginMethodTab::CstoaQr => "CSTOA 扫码登录",
                                        LoginMethodTab::ApiKey => "APIKEY 凭据登录",
                                        LoginMethodTab::OAuth => "OAuth 授权登录",
                                    })),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_secondary)
                                    .child(match self.tab {
                                        LoginMethodTab::CstoaQr => {
                                            "使用手机扫描二维码，在 OA 页面完成授权"
                                        }
                                        LoginMethodTab::ApiKey => {
                                            "各服务凭据独立保存，就绪即可开始诊断"
                                        }
                                        LoginMethodTab::OAuth => "在系统浏览器中完成第三方账号授权",
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(space::LG)
                            .child(tab_content)
                            .child(mode_footer)
                            .children(provider_menu_opt),
                    ),
            );

        div()
            .id("login-view-tm7yq")
            .test_support()
            .a11y_region("login-view", "登录提供商")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(palette.bg_workspace)
            .flex()
            .flex_row()
            .child(left_canvas)
            .child(right_viewport)
    }
}

fn login_compact_switch_btn<F>(
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
        .a11y_button(id, label)
        .h(height::COMPACT)
        .px(space::SM)
        .rounded(radius::CONTROL)
        .bg(if active {
            palette.bg_item_active
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
        .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
        .on_click(on_click)
        .child(cjk_inline_label(label))
}

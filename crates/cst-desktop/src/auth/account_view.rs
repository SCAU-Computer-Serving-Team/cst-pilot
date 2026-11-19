use cst_client::ClientHandle;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius, scale, space,
};
use crate::workbench::model_picker::format_quota_balance;

#[derive(Debug, Clone)]
pub enum AccountViewEvent {
    LoggedOutCstoa,
    ErrorMessage(String),
}

/// Exclusive CSTOA Account View: displays student ID & name, queries remaining credit balance with
/// retry on failure, and supports CSTOA-only logout without clearing other providers' credentials.
pub struct AccountView {
    client: ClientHandle,
    quota_text: Option<SharedString>,
    quota_loading: bool,
    quota_error: Option<SharedString>,
    logging_out: bool,
}

impl EventEmitter<AccountViewEvent> for AccountView {}

impl AccountView {
    pub fn new(client: ClientHandle, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            client,
            quota_text: None,
            quota_loading: false,
            quota_error: None,
            logging_out: false,
        };
        this.refresh_cstoa_quota(true, cx);
        this
    }

    pub fn refresh_cstoa_quota(&mut self, force_refresh: bool, cx: &mut Context<Self>) {
        if self.quota_loading {
            return;
        }
        self.quota_loading = true;
        self.quota_error = None;
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client
                .get_provider_quota("cstoa".into(), force_refresh)
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.quota_loading = false;
                match res {
                    Ok(val) => {
                        this.quota_text = Some(SharedString::from(
                            format_quota_balance(&val).unwrap_or_else(|| "已就绪".to_owned()),
                        ));
                        this.quota_error = None;
                    }
                    Err(err) => {
                        this.quota_error = Some(SharedString::from(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn logout_cstoa_only(&mut self, cx: &mut Context<Self>) {
        if self.logging_out {
            return;
        }
        self.logging_out = true;
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.logout_provider("cstoa".into()).await;
            let _ = client.refresh_globals().await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |this, cx| {
                this.logging_out = false;
                match res {
                    Ok(_) => {
                        cx.emit(AccountViewEvent::LoggedOutCstoa);
                    }
                    Err(err) => {
                        cx.emit(AccountViewEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for AccountView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;

        let snap = self.client.snapshot();
        let account = snap.account.as_ref();
        let name = account
            .and_then(|a| a.profile.name.as_deref())
            .unwrap_or("科协队员");
        let student_id = account
            .and_then(|a| a.profile.student_id.as_deref())
            .unwrap_or("未提供学号");

        div()
            .id("account-view")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(space::GROUP)
            .child(
                div()
                    .w_full()
                    .max_w(layout::PALETTE_MODAL_WIDTH)
                    .p(space::GROUP)
                    .rounded(radius::SPECIAL)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .bg(palette.bg_surface)
                    .flex()
                    .flex_col()
                    .gap(space::LG)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(space::MICRO)
                                    .child(
                                        apply_scale(div(), scale::TITLE)
                                            .text_color(palette.text_primary)
                                            .child(SharedString::from(name.to_owned())),
                                    )
                                    .child(
                                        apply_scale(div(), scale::CAPTION)
                                            .font_family(mono_family.clone())
                                            .text_color(palette.text_secondary)
                                            .child(SharedString::from(format!(
                                                "学号：{student_id}"
                                            ))),
                                    ),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .px(space::SM)
                                    .py(space::MICRO)
                                    .rounded(radius::CONTROL)
                                    .bg(palette.bg_elevated)
                                    .text_color(palette.status_success)
                                    .child(cjk_inline_label("CSTOA 已登录")),
                            ),
                    )
                    // Quota balance row with retry
                    .child(
                        div()
                            .p(space::MD)
                            .rounded(radius::ROW)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_elevated)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::MD)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(space::MICRO)
                                    .child(
                                        apply_scale(div(), scale::CAPTION)
                                            .text_color(palette.text_secondary)
                                            .child("剩余额度点"),
                                    )
                                    .child(
                                        apply_scale(div(), scale::BODY_SEMIBOLD)
                                            .font_family(mono_family)
                                            .text_color(if self.quota_error.is_some() {
                                                palette.status_danger
                                            } else {
                                                palette.text_primary
                                            })
                                            .child(if self.quota_loading {
                                                SharedString::from("查询中…")
                                            } else if let Some(ref err) = self.quota_error {
                                                err.clone()
                                            } else if let Some(ref q) = self.quota_text {
                                                q.clone()
                                            } else {
                                                SharedString::from("--")
                                            }),
                                    ),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .id("account-quota-refresh-btn")
                                    .test_support()
                                    .h(height::CONTROL)
                                    .px(space::SM)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_subtle)
                                    .bg(palette.bg_surface)
                                    .text_color(palette.text_secondary)
                                    .flex()
                                    .items_center()
                                    .gap(space::XS)
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_cstoa_quota(true, cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::RefreshCw)
                                            .size(px(12.0))
                                            .text_color(palette.text_secondary),
                                    )
                                    .child(cjk_inline_label("刷新重试")),
                            ),
                    )
                    // CSTOA-only logout
                    .child(
                        div().flex().justify_end().child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("account-cstoa-logout-btn")
                                .test_support()
                                .h(height::CONTROL)
                                .px(space::MD)
                                .rounded(radius::CONTROL)
                                .border_1()
                                .border_color(palette.status_danger)
                                .text_color(palette.status_danger)
                                .flex()
                                .items_center()
                                .gap(space::XS)
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.logout_cstoa_only(cx);
                                }))
                                .child(
                                    Icon::new(IconName::LogOut)
                                        .size(px(13.0))
                                        .text_color(palette.status_danger),
                                )
                                .child(cjk_inline_label(if self.logging_out {
                                    "正在退出…"
                                } else {
                                    "退出 CSTOA 登录"
                                })),
                        ),
                    ),
            )
    }
}

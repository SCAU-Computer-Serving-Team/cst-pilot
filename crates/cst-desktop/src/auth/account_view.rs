use cst_client::ClientHandle;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius,
    render_subpage_header_xwjof, scale, space,
};
use crate::workbench::model_picker::format_quota_balance;

#[derive(Debug, Clone)]
pub enum AccountViewEvent {
    LoggedOutCstoa,
    ErrorMessage(String),
}

/// Exclusive CSTOA Account View (`cst-pilot-desktop.pen` 「04 · 账号与登录」):
/// Top full-width `44px` `xWJOf` header + `816px` centered column with single-layer `$bg-workspace`
/// `8px` card chaining three `56px` info rows (`c4PWw`), and neutral-border outline danger logout button.
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

        let quota_display = if self.quota_loading {
            SharedString::from("查询中…")
        } else if let Some(ref err) = self.quota_error {
            err.clone()
        } else if let Some(ref q) = self.quota_text {
            q.clone()
        } else {
            SharedString::from("--")
        };

        let info_card = div()
            .w_full()
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_workspace)
            .flex()
            .flex_col()
            // Row 1: 学号 (`56px` `c4PWw`)
            .child(
                div()
                    .w_full()
                    .h(px(56.0))
                    .px(space::LG)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::LG)
                    .child(
                        apply_scale(div(), scale::BODY)
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("学号")),
                    )
                    .child(
                        apply_scale(div(), scale::BODY)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_primary)
                            .child(SharedString::from(student_id.to_owned())),
                    ),
            )
            // Row 2: 姓名 (`56px` `c4PWw`)
            .child(
                div()
                    .w_full()
                    .h(px(56.0))
                    .px(space::LG)
                    .border_t_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::LG)
                    .child(
                        apply_scale(div(), scale::BODY)
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("姓名")),
                    )
                    .child(
                        apply_scale(div(), scale::BODY)
                            .text_color(palette.text_primary)
                            .child(SharedString::from(name.to_owned())),
                    ),
            )
            // Row 3: 剩余额度点 (`56px` `c4PWw`)
            .child(
                div()
                    .w_full()
                    .h(px(56.0))
                    .px(space::LG)
                    .border_t_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::LG)
                    .child(
                        apply_scale(div(), scale::BODY)
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("剩余额度点")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::BODY)
                                    .font_family(mono_family)
                                    .text_color(if self.quota_error.is_some() {
                                        palette.status_danger
                                    } else {
                                        palette.text_primary
                                    })
                                    .child(quota_display),
                            )
                            .child(
                                div()
                                    .id("account-quota-refresh-btn")
                                    .test_support()
                                    .a11y_button("account-quota-refresh-btn", "刷新剩余额度")
                                    .w(height::CONTROL)
                                    .h(height::CONTROL)
                                    .rounded(radius::CONTROL)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_cstoa_quota(true, cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::RefreshCw)
                                            .size(px(14.0))
                                            .text_color(palette.text_secondary),
                                    ),
                            ),
                    ),
            );

        div()
            .id("account-view")
            .test_support()
            .a11y_region("account-view", "账号信息")
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.bg_workspace)
            .child(render_subpage_header_xwjof(
                "account-subpage-header",
                IconName::User,
                "账号信息",
                None,
                &palette,
            ))
            .child(
                div()
                    .id("account-scroll-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .px(space::GROUP)
                    .py(space::GROUP)
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                            .flex()
                            .flex_col()
                            .gap(space::GROUP)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(space::SM)
                                    .child(
                                        apply_scale(div(), scale::BODY_SEMIBOLD)
                                            .text_color(palette.text_primary)
                                            .child(cjk_inline_label("CSTOA 账号")),
                                    )
                                    .child(info_card),
                            )
                            .child(
                                div().flex().items_center().child(
                                    apply_scale(div(), scale::UI_MEDIUM)
                                        .id("account-cstoa-logout-btn")
                                        .test_support()
                                        .a11y_button(
                                            "account-logout-btn",
                                            if self.logging_out {
                                                "正在退出"
                                            } else {
                                                "退出 CSTOA 登录"
                                            },
                                        )
                                        .h(height::CONTROL)
                                        .px(space::MD)
                                        .rounded(radius::CONTROL)
                                        .border_1()
                                        .border_color(palette.border_strong)
                                        .bg(palette.bg_workspace)
                                        .text_color(palette.status_danger)
                                        .flex()
                                        .items_center()
                                        .gap(space::XS)
                                        .hover(|s| s.bg(palette.status_danger_subtle))
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
                    ),
            )
    }
}

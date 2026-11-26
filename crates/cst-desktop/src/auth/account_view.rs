use std::rc::Rc;

use cst_client::ClientHandle;
use gpui::{KeyDownEvent, MouseMoveEvent};
use gpui_kit::assets::IconName;
use gpui_kit::base::{ElementExt as _, FocusTrapElement as _};
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::auth::login_view::LoginMethodTab;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::kbd_focus::{KbdTone, use_dialog_action_nav};
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, motion, radius,
    render_subpage_header_xwjof, scale, space,
};
use crate::workbench::model_picker::format_quota_balance;

#[derive(Debug, Clone)]
pub enum AccountViewEvent {
    NavigateBack,
    OpenLoginTab(LoginMethodTab),
    LoggedOutCstoa,
    ErrorMessage(String),
}

/// Exclusive CSTOA Account View (`cst-pilot-desktop.pen` 「04 · 账号与登录」 `d2XpB`):
/// Top full-width `44px` `xWJOf` header + `816px` centered column with single-layer `$bg-workspace`
/// `8px` card chaining three `56px` info rows (`c4PWw`), `GOWKl` action bar, and `sVucQ` logout confirm dialog.
pub struct AccountView {
    client: ClientHandle,
    quota_text: Option<SharedString>,
    quota_loading: bool,
    quota_error: Option<SharedString>,
    logging_out: bool,
    logout_confirm_open: bool,
    logout_trap_focus: FocusHandle,
    logout_cancel_focus: FocusHandle,
    logout_trigger_focus: FocusHandle,
    logout_confirm_focus: FocusHandle,
    _revision_task: gpui::Task<()>,
    quota_task: Option<gpui::Task<()>>,
    profile_error: Option<SharedString>,
}

impl EventEmitter<AccountViewEvent> for AccountView {}

impl AccountView {
    pub fn new(client: ClientHandle, cx: &mut Context<Self>) -> Self {
        let mut rev_rx = client.subscribe_revisions();
        let revision_task = cx.spawn(async move |weak, cx| {
            let mut previous = None;
            while rev_rx.changed().await.is_ok() {
                let alive = weak
                    .update(cx, |this, cx| {
                        let account = this.client.snapshot().account.clone();
                        if previous != account {
                            previous = account;
                            this.profile_error = None;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        Self {
            client,
            quota_text: None,
            quota_loading: false,
            quota_error: None,
            logging_out: false,
            logout_confirm_open: false,
            logout_trap_focus: cx.focus_handle(),
            logout_cancel_focus: cx.focus_handle().tab_stop(true),
            logout_trigger_focus: cx.focus_handle().tab_stop(true),
            logout_confirm_focus: cx.focus_handle().tab_stop(true),
            _revision_task: revision_task,
            quota_task: None,
            profile_error: None,
        }
    }

    pub fn has_logout_confirm(&self) -> bool {
        self.logout_confirm_open
    }

    pub fn blocks_workspace_shortcuts(&self) -> bool {
        self.logout_confirm_open || self.logging_out
    }

    pub fn leave(&mut self, cx: &mut Context<Self>) {
        self.logout_confirm_open = false;
        self.quota_task.take();
        self.quota_loading = false;
        self.quota_text = None;
        self.quota_error = None;
        self.profile_error = None;
        cx.notify();
    }

    pub fn open_logout_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.logging_out || self.logout_confirm_open {
            return;
        }
        self.logout_confirm_open = true;
        self.logout_cancel_focus.focus(window, cx);
        cx.notify();
    }

    pub fn dismiss_logout_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.logout_confirm_open {
            return false;
        }
        self.logout_confirm_open = false;
        self.logout_trigger_focus.focus(window, cx);
        cx.notify();
        true
    }

    pub fn refresh_cstoa_quota(&mut self, cx: &mut Context<Self>) {
        if self.quota_loading {
            return;
        }
        self.quota_loading = true;
        self.quota_error = None;
        self.profile_error = None;
        cx.notify();

        let client = self.client.clone();
        self.quota_task = Some(cx.spawn(async move |weak, cx| {
            // The account and quota endpoints share the upstream profile cache.
            let account = client.refresh_account().await;
            let res = client.get_provider_quota("cstoa".into(), false).await;
            let _ = weak.update(cx, |this, cx| {
                this.quota_loading = false;
                this.profile_error = account.err().map(|err| err.to_string().into());
                match res {
                    Ok(val) => {
                        this.quota_text = format_quota_balance(&val).map(SharedString::from);
                        if this.quota_text.is_none() {
                            this.quota_error = val
                                .get("reason")
                                .and_then(serde_json::Value::as_str)
                                .map(SharedString::from);
                        }
                    }
                    Err(err) => {
                        this.quota_error = Some(SharedString::from(err.to_string()));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn logout_cstoa_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.logging_out || !self.dismiss_logout_confirm(window, cx) {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;

        let snap = self.client.snapshot();
        let account = snap.account.as_ref();
        let name = account
            .and_then(|a| a.profile.name.as_deref())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(if self.quota_loading {
                "查询中…"
            } else {
                "未提供姓名"
            });
        let student_id = account
            .and_then(|a| a.profile.student_id.as_deref())
            .filter(|id| !id.trim().is_empty())
            .unwrap_or(if self.quota_loading {
                "查询中…"
            } else {
                "未提供学号"
            });
        let profile_error = (!self.quota_loading)
            .then(|| {
                self.profile_error
                    .clone()
                    .or_else(|| {
                        account
                            .and_then(|a| a.profile.reason.as_deref())
                            .filter(|reason| !reason.trim().is_empty())
                            .map(SharedString::from)
                    })
                    .filter(|reason| self.quota_error.as_ref() != Some(reason))
            })
            .flatten();

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
                                        this.refresh_cstoa_quota(cx);
                                    }))
                                    .child(
                                        Icon::new(IconName::RefreshCw)
                                            .size(px(14.0))
                                            .text_color(palette.text_secondary),
                                    ),
                            ),
                    ),
            );

        let info_card = info_card.children(profile_error.map(|err| {
            apply_scale(div(), scale::UI)
                .border_t_1()
                .border_color(palette.border_subtle)
                .p(space::LG)
                .text_color(palette.text_secondary)
                .child(err)
        }));

        let logout_scrim_p = motion::surface_presence(
            "account-logout-scrim-p",
            self.logout_confirm_open,
            motion::MODAL_OPEN,
            window,
            cx,
        );
        let logout_nav = use_dialog_action_nav(
            "account-logout-confirm",
            self.logout_confirm_open,
            0,
            window,
            cx,
        );
        let confirm_modal_opt = self
            .logout_confirm_open
            .then(|| {
                let kbd_style = snap.desktop_settings.keyboard_focus_style;
                let scale_factor = window.scale_factor();
                let entity = cx.entity();
                let ent_cancel = entity.clone();
                let ent_confirm = entity.clone();
                let on_cancel: crate::theme::kbd_focus::DialogActionCallback =
                    Rc::new(move |w, cx| {
                        ent_cancel.update(cx, |this, cx| {
                            this.dismiss_logout_confirm(w, cx);
                        });
                    });
                let on_confirm: crate::theme::kbd_focus::DialogActionCallback =
                    Rc::new(move |w, cx| {
                        ent_confirm.update(cx, |this, cx| {
                            this.logout_cstoa_only(w, cx);
                        });
                    });
                let focuses = [
                    self.logout_cancel_focus.clone(),
                    self.logout_confirm_focus.clone(),
                ];
                logout_nav.clear_tracker(cx);
                let kbd_active = logout_nav.is_keyboard_active(cx);
                let tracker_cancel = logout_nav.tracker(cx);
                let tracker_confirm = logout_nav.tracker(cx);
                let kbd_overlay = logout_nav.render_overlay(
                    kbd_style,
                    &focuses,
                    [KbdTone::Neutral, KbdTone::Danger],
                    false,
                    &palette,
                    window,
                    cx,
                );
                let nav_move = logout_nav.clone();
                let nav_key = logout_nav.clone();
                let nav_click_cancel = logout_nav.clone();
                let nav_click_confirm = logout_nav.clone();
                let click_cancel = Rc::clone(&on_cancel);
                let click_confirm = Rc::clone(&on_confirm);

                let card = render_frosted_surface(
                    "account-logout-confirm-dialog",
                    FrostedGlassSpec::modal(&palette, f32::from(radius::ROW))
                        .with_opacity(logout_scrim_p),
                    palette.bg_elevated,
                    transparency_enabled,
                    scale_factor,
                )
                .test_support()
                .a11y_dialog("account-logout-confirm-dialog", "退出 CSTOA 登录")
                .w(layout::MODAL_STANDARD_WIDTH)
                .p(px(20.0))
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette.border_subtle)
                .shadow(palette.craft_stack_shadows())
                .flex()
                .flex_col()
                .gap(space::MD)
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            Icon::new(IconName::CircleAlert)
                                .size(px(16.0))
                                .text_color(palette.status_danger),
                        )
                        .child(
                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label("确认退出 CSTOA？")),
                        ),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .text_color(palette.text_secondary)
                        .child("退出后将清除本机 CSTOA 登录凭据，可随时重新扫码或通过密钥登录。"),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .pt(space::XS)
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap(space::SM)
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("account-logout-cancel-btn")
                                .test_support()
                                .a11y_button("account-logout-cancel-btn", "取消")
                                .track_focus(&self.logout_cancel_focus)
                                .tab_index(0)
                                .h(height::CONTROL)
                                .px(space::MD)
                                .rounded(radius::CONTROL)
                                .border_1()
                                .border_color(palette.border_strong)
                                .bg(palette.bg_workspace)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .when(!kbd_active, |el| el.hover(|s| s.bg(palette.bg_surface)))
                                .on_click(move |_, window, cx| {
                                    nav_click_cancel.clear_keyboard(window, cx);
                                    click_cancel(window, cx);
                                })
                                .child(cjk_inline_label("取消"))
                                .on_prepaint(move |bounds, _, _| tracker_cancel.set(0, bounds)),
                        )
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("account-logout-confirm-btn")
                                .test_support()
                                .a11y_button("account-logout-confirm-btn", "退出 CSTOA")
                                .track_focus(&self.logout_confirm_focus)
                                .tab_index(0)
                                .h(height::CONTROL)
                                .px(space::MD)
                                .rounded(radius::CONTROL)
                                .bg(palette.status_danger)
                                .text_color(palette.text_on_accent)
                                .flex()
                                .items_center()
                                .when(!kbd_active, |el| {
                                    el.hover(|s| s.bg(palette.status_danger_hover))
                                })
                                .on_click(move |_, window, cx| {
                                    nav_click_confirm.clear_keyboard(window, cx);
                                    click_confirm(window, cx);
                                })
                                .child(cjk_inline_label("退出 CSTOA"))
                                .on_prepaint(move |bounds, _, _| tracker_confirm.set(1, bounds)),
                        )
                        .child(kbd_overlay),
                );

                render_frosted_surface(
                    "account-logout-confirm-scrim",
                    FrostedGlassSpec::workspace_scrim(&palette).with_opacity(logout_scrim_p),
                    palette.bg_scrim,
                    transparency_enabled,
                    scale_factor,
                )
                .rounded(radius::ROW)
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_move(move |ev: &MouseMoveEvent, w, cx| {
                    nav_move.handle_mouse_move(ev, w, cx);
                })
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.dismiss_logout_confirm(window, cx);
                    }),
                )
                .child(
                    div()
                        .tab_stop(false)
                        .on_key_down(move |ev: &KeyDownEvent, w, cx| {
                            if nav_key.handle_key_down(
                                ev,
                                &focuses,
                                false,
                                on_cancel.as_ref(),
                                on_confirm.as_ref(),
                                w,
                                cx,
                            ) {
                                cx.stop_propagation();
                            }
                        })
                        .focus_trap("account-logout-confirm-trap", &self.logout_trap_focus)
                        .child(motion::apply_modal_motion(
                            card,
                            logout_scrim_p,
                            palette.craft_stack_shadows(),
                        )),
                )
            })
            .or_else(|| {
                (logout_scrim_p > 0.001).then(|| {
                    let scale_factor = window.scale_factor();
                    let card = render_frosted_surface(
                        "account-logout-confirm-dialog-exit",
                        FrostedGlassSpec::modal(&palette, f32::from(radius::ROW))
                            .with_opacity(logout_scrim_p),
                        palette.bg_elevated,
                        transparency_enabled,
                        scale_factor,
                    )
                    .w(layout::MODAL_STANDARD_WIDTH)
                    .p(px(20.0))
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                Icon::new(IconName::CircleAlert)
                                    .size(px(16.0))
                                    .text_color(palette.status_danger),
                            )
                            .child(
                                apply_scale(div(), scale::BODY_SEMIBOLD)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("确认退出 CSTOA？")),
                            ),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_secondary)
                            .child(
                                "退出后将清除本机 CSTOA 登录凭据，可随时重新扫码或通过密钥登录。",
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .pt(space::XS)
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .h(height::CONTROL)
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_strong)
                                    .bg(palette.bg_workspace)
                                    .text_color(palette.text_primary)
                                    .flex()
                                    .items_center()
                                    .child(cjk_inline_label("取消")),
                            )
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .h(height::CONTROL)
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .bg(palette.status_danger)
                                    .text_color(palette.text_on_accent)
                                    .flex()
                                    .items_center()
                                    .child(cjk_inline_label("退出 CSTOA")),
                            ),
                    );
                    render_frosted_surface(
                        "account-logout-confirm-scrim-exit",
                        FrostedGlassSpec::workspace_scrim(&palette).with_opacity(logout_scrim_p),
                        palette.bg_scrim,
                        transparency_enabled,
                        scale_factor,
                    )
                    .rounded(radius::ROW)
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(motion::apply_modal_motion(
                        card,
                        logout_scrim_p,
                        palette.craft_stack_shadows(),
                    ))
                })
            });

        div()
            .id("account-view")
            .test_support()
            .a11y_region("account-view", "账号信息")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(render_subpage_header_xwjof(
                "account-subpage-header",
                IconName::User,
                "账号信息",
                None,
                Some(std::rc::Rc::new({
                    let e_back = cx.entity();
                    move |_, cx| {
                        e_back.update(cx, |_, cx| cx.emit(AccountViewEvent::NavigateBack));
                    }
                })),
                &palette,
                window,
                cx,
            ))
            .child(
                div()
                    .id("account-scroll-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .px(layout::SUBPAGE_HORIZONTAL_INSET)
                    .py(space::GROUP)
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                            .id("account-content-column")
                            .test_support()
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
                            // `GOWKl`: action button row (`重新扫码登录` · `APIKEY 登录` · `退出 CSTOA`)
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(space::MD)
                                    .pt(space::XS)
                                    .child(
                                        apply_scale(div(), scale::UI)
                                            .id("account-relogin-qr-btn")
                                            .test_support()
                                            .a11y_button("account-relogin-qr-btn", "重新扫码登录")
                                            .h(height::CONTROL)
                                            .px(space::MD)
                                            .rounded(radius::CONTROL)
                                            .border_1()
                                            .border_color(palette.border_strong)
                                            .bg(palette.bg_workspace)
                                            .text_color(palette.text_primary)
                                            .flex()
                                            .items_center()
                                            .hover(|s| s.bg(palette.bg_surface))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if !this.blocks_workspace_shortcuts() {
                                                    cx.emit(AccountViewEvent::OpenLoginTab(
                                                        LoginMethodTab::CstoaQr,
                                                    ));
                                                }
                                            }))
                                            .child(cjk_inline_label("重新扫码登录")),
                                    )
                                    .child(
                                        apply_scale(div(), scale::UI)
                                            .id("account-relogin-apikey-btn")
                                            .test_support()
                                            .a11y_button(
                                                "account-relogin-apikey-btn",
                                                "APIKEY 登录",
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
                                            .hover(|s| s.bg(palette.bg_surface))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if !this.blocks_workspace_shortcuts() {
                                                    cx.emit(AccountViewEvent::OpenLoginTab(
                                                        LoginMethodTab::ApiKey,
                                                    ));
                                                }
                                            }))
                                            .child(cjk_inline_label("APIKEY 登录")),
                                    )
                                    .child(
                                        apply_scale(div(), scale::UI)
                                            .id("account-cstoa-logout-btn")
                                            .test_support()
                                            .a11y_button(
                                                "account-logout-btn",
                                                if self.logging_out {
                                                    "正在退出"
                                                } else {
                                                    "退出 CSTOA"
                                                },
                                            )
                                            .track_focus(&self.logout_trigger_focus)
                                            .tab_index(0)
                                            .on_key_down(cx.listener(
                                                |this, event: &gpui::KeyDownEvent, window, cx| {
                                                    if crate::settings::activates_button(event) {
                                                        this.open_logout_confirm(window, cx);
                                                        cx.stop_propagation();
                                                    }
                                                },
                                            ))
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
                                            .hover(|s| s.bg(palette.status_danger_subtle))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_logout_confirm(window, cx);
                                            }))
                                            .child(
                                                Icon::new(IconName::LogOut)
                                                    .size(px(16.0))
                                                    .text_color(palette.status_danger),
                                            )
                                            .child(cjk_inline_label(if self.logging_out {
                                                "正在退出…"
                                            } else {
                                                "退出 CSTOA"
                                            })),
                                    ),
                            ),
                    ),
            )
            .children(confirm_modal_opt)
    }
}

pub mod exit_dialog;
pub mod overlays;
pub mod sidebar;
pub mod topbar;
pub mod window_bounds;

use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

use cst_client::{
    ChannelModeDto, ClientHandle, ClientSnapshot, DesktopSettingsPatchDto, ExitCheckReport,
    UpdatePhaseDto, WorkspaceView,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    TestSupportExt, Window, div, px,
};

use crate::background::{
    FrostedGlassSpec, ShaderFrameScheduler, SkyPreset, has_active_hardware_diagnostic,
    render_frosted_surface, render_login_9_stop_fade, render_sky_background,
};
use crate::shell::exit_dialog::render_exit_confirm_dialog;
use crate::shell::overlays::{
    render_top_notice_banner, render_web_active_lock_overlay, render_workspace_error_bar,
};
use crate::shell::sidebar::{
    MAX_RECENT_SIDEBAR_SESSIONS, ShellRoute, SidebarCallbacks, render_sidebar_l081m1,
};
use crate::shell::topbar::{TopbarCallbacks, render_topbar};
use crate::shell::window_bounds::{
    GEOMETRY_DEBOUNCE_MS, NormalBoundsTracker, ResolvedWindowGeometry, collect_app_displays,
};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, FontLoadReport, apply_desktop_theme, apply_scale,
    cjk_inline_label, height, layout, radius, scale, space,
};

gpui_kit::actions!(
    cst_shell,
    [
        ToggleSidebar,
        NewSession,
        OpenSettings,
        DismissTopSurface,
        RequestAppExit
    ]
);

pub const SHELL_KEY_CONTEXT: &str = "CstAppShell";

pub fn register_shell_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-b", ToggleSidebar, Some(SHELL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-n", NewSession, Some(SHELL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSettings, Some(SHELL_KEY_CONTEXT)),
        KeyBinding::new("escape", DismissTopSurface, Some(SHELL_KEY_CONTEXT)),
    ]);
}

struct ExitDialogModalState {
    report: ExitCheckReport,
    exiting: bool,
    prior_focus: Option<FocusHandle>,
}

struct WebSwitchConfirmState {
    prior_focus: Option<FocusHandle>,
}

pub struct AppShell {
    client: ClientHandle,
    snapshot: std::sync::Arc<ClientSnapshot>,
    route: ShellRoute,
    sidebar_collapsed: bool,
    sidebar_batch_mode: bool,
    sidebar_selected_ids: BTreeSet<String>,
    recent_drawer_open: bool,
    recent_drawer_prior_focus: Option<FocusHandle>,
    account_menu_open: bool,
    account_menu_prior_focus: Option<FocusHandle>,
    web_switch_confirm: Option<WebSwitchConfirmState>,
    exit_dialog: Option<ExitDialogModalState>,
    last_web_url: Option<String>,
    workspace_error: Option<SharedString>,
    top_notice: Option<SharedString>,
    shader_scheduler: ShaderFrameScheduler,
    bounds_tracker: NormalBoundsTracker,
    bounds_debounce_task: Option<Task<()>>,
    error_dismiss_task: Option<Task<()>>,
    notice_dismiss_task: Option<Task<()>>,
    allow_immediate_close: bool,
    root_focus: FocusHandle,
    sidebar_collapse_focus: FocusHandle,
    topbar_expand_focus: FocusHandle,
    topbar_recent_focus: FocusHandle,
    sidebar_account_focus: FocusHandle,
    topbar_account_focus: FocusHandle,
    web_lock_trap_focus: FocusHandle,
    web_lock_switch_focus: FocusHandle,
    exit_trap_focus: FocusHandle,
    exit_cancel_focus: FocusHandle,
    exit_confirm_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for AppShell {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.root_focus.clone()
    }
}

impl AppShell {
    pub fn new(
        client: ClientHandle,
        resolved_geometry: &ResolvedWindowGeometry,
        font_report: Option<&FontLoadReport>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_snapshot = client.snapshot();
        let sidebar_collapsed = initial_snapshot.desktop_settings.sidebar_collapsed;
        let _palette = apply_desktop_theme(
            initial_snapshot.desktop_settings.theme,
            Some(window),
            font_report,
            cx,
        );

        let initial_notice = font_report
            .and_then(|r| r.warning.as_ref())
            .map(|w| SharedString::from(w.clone()))
            .or_else(|| {
                initial_snapshot.desktop_settings_corrupt.then(|| {
                    SharedString::from("桌面配置文件格式异常，已保留原文件并使用默认外观。")
                })
            });

        let root_focus = cx.focus_handle();
        let sidebar_collapse_focus = cx.focus_handle();
        let topbar_expand_focus = cx.focus_handle();
        let topbar_recent_focus = cx.focus_handle();
        let sidebar_account_focus = cx.focus_handle();
        let topbar_account_focus = cx.focus_handle();
        let web_lock_trap_focus = cx.focus_handle();
        let web_lock_switch_focus = cx.focus_handle();
        let exit_trap_focus = cx.focus_handle();
        let exit_cancel_focus = cx.focus_handle();
        let exit_confirm_focus = cx.focus_handle();

        let mut subscriptions = Vec::new();

        // Observe system light/dark appearance changes
        subscriptions.push(cx.observe_window_appearance(window, |this, window, cx| {
            apply_desktop_theme(this.snapshot.desktop_settings.theme, Some(window), None, cx);
            cx.notify();
        }));

        // Observe window bounds changes -> 300ms debounced PATCH to `/api/app/settings`
        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.on_window_geometry_changed(window, cx);
        }));

        // Intercept native window close (`X` / `Alt+F4`) to check active tasks & drafts
        let weak_close = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let Some(entity) = weak_close.upgrade() else {
                return true;
            };
            let allow = entity.read(cx).allow_immediate_close;
            if allow {
                return true;
            }
            entity.update(cx, |this, cx| {
                this.request_close_flow(window, cx);
            });
            false
        });

        let shell = Self {
            client: client.clone(),
            snapshot: initial_snapshot,
            route: ShellRoute::Home,
            sidebar_collapsed,
            sidebar_batch_mode: false,
            sidebar_selected_ids: BTreeSet::new(),
            recent_drawer_open: false,
            recent_drawer_prior_focus: None,
            account_menu_open: false,
            account_menu_prior_focus: None,
            web_switch_confirm: None,
            exit_dialog: None,
            last_web_url: None,
            workspace_error: None,
            top_notice: initial_notice,
            shader_scheduler: ShaderFrameScheduler::new(),
            bounds_tracker: NormalBoundsTracker::from_resolved(resolved_geometry),
            bounds_debounce_task: None,
            error_dismiss_task: None,
            notice_dismiss_task: None,
            allow_immediate_close: false,
            root_focus: root_focus.clone(),
            sidebar_collapse_focus,
            topbar_expand_focus,
            topbar_recent_focus,
            sidebar_account_focus,
            topbar_account_focus,
            web_lock_trap_focus,
            web_lock_switch_focus,
            exit_trap_focus,
            exit_cancel_focus,
            exit_confirm_focus,
            _subscriptions: subscriptions,
        };

        root_focus.focus(window, cx);
        shell.spawn_client_revision_listener(window, cx);
        shell.spawn_wakeup_listener(window, cx);
        shell
    }

    pub fn route(&self) -> ShellRoute {
        self.route
    }

    pub fn set_route(&mut self, route: ShellRoute, cx: &mut Context<Self>) {
        self.route = route;
        cx.notify();
    }

    pub fn is_sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    pub fn has_exit_dialog(&self) -> bool {
        self.exit_dialog.is_some()
    }

    pub fn show_workspace_error(
        &mut self,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.workspace_error = Some(message.into());
        self.error_dismiss_task = Some(cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = weak.update(cx, |this, cx| {
                this.workspace_error = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn show_top_notice(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.top_notice = Some(message.into());
        self.notice_dismiss_task = Some(cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(3500))
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.top_notice = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn spawn_client_revision_listener(&self, window: &mut Window, cx: &mut Context<Self>) {
        let mut rev_rx = self.client.subscribe_revisions();
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            while rev_rx.changed().await.is_ok() {
                let latest_snap = client.snapshot();
                let keep_going = weak.update_in(cx, |this, window, cx| {
                    let prev_mode = this.snapshot.channel_mode;
                    let prev_theme = this.snapshot.desktop_settings.theme;
                    this.snapshot = latest_snap;
                    this.sidebar_collapsed = this.snapshot.desktop_settings.sidebar_collapsed;

                    if prev_theme != this.snapshot.desktop_settings.theme {
                        apply_desktop_theme(
                            this.snapshot.desktop_settings.theme,
                            Some(window),
                            None,
                            cx,
                        );
                    }
                    if prev_mode != ChannelModeDto::WebActive
                        && this.snapshot.channel_mode == ChannelModeDto::WebActive
                    {
                        this.web_lock_switch_focus.focus(window, cx);
                    }
                    cx.notify();
                });
                if keep_going.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn spawn_wakeup_listener(&self, window: &mut Window, cx: &mut Context<Self>) {
        let mut wakeup_rx = self.client.subscribe_wakeup();
        cx.spawn_in(window, async move |weak, cx| {
            while wakeup_rx.changed().await.is_ok() {
                let ok = weak.update_in(cx, |_, window, _| {
                    window.activate_window();
                });
                if ok.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_window_geometry_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let vp = window.viewport_size();
        let is_min = vp.width <= px(1.0) || vp.height <= px(1.0);
        let bounds = window.bounds();
        let center_x = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
        let center_y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
        let matched_hint = collect_app_displays(cx)
            .into_iter()
            .find(|d| {
                center_x >= d.work_x
                    && center_x <= d.work_x + d.work_width
                    && center_y >= d.work_y
                    && center_y <= d.work_y + d.work_height
            })
            .map(|d| d.hint);

        if self
            .bounds_tracker
            .observe(bounds, window.is_maximized(), is_min, matched_hint)
        {
            let client = self.client.clone();
            self.bounds_debounce_task = Some(cx.spawn(async move |weak, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(GEOMETRY_DEBOUNCE_MS))
                    .await;
                let maybe_patch = weak
                    .update(cx, |this, _| this.bounds_tracker.take_pending_patch())
                    .ok()
                    .flatten();
                if let Some(patch) = maybe_patch {
                    let client_bg = client.clone();
                    let _ = cx
                        .background_executor()
                        .spawn(async move {
                            let _ = client_bg.patch_desktop_settings(patch).await;
                        })
                        .await;
                }
            }));
        }
    }

    pub fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next_collapsed = !self.sidebar_collapsed;
        self.sidebar_collapsed = next_collapsed;
        self.recent_drawer_open = false;
        if next_collapsed {
            self.topbar_expand_focus.focus(window, cx);
        } else {
            self.sidebar_collapse_focus.focus(window, cx);
        }
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client
                        .patch_desktop_settings(DesktopSettingsPatchDto {
                            sidebar_collapsed: Some(next_collapsed),
                            ..DesktopSettingsPatchDto::default()
                        })
                        .await
                })
                .await;
            if let Err(err) = res {
                let _ = weak.update(cx, |this, cx| {
                    this.show_workspace_error(err.to_string(), cx);
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// Closes ONLY the single topmost open surface and restores focus within 1 frame.
    pub fn dismiss_topmost_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(dialog) = self.exit_dialog.take() {
            if let Some(prev) = dialog.prior_focus {
                prev.focus(window, cx);
            } else {
                self.root_focus.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if let Some(confirm) = self.web_switch_confirm.take() {
            if let Some(prev) = confirm.prior_focus {
                prev.focus(window, cx);
            } else {
                self.root_focus.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if self.account_menu_open {
            self.account_menu_open = false;
            if let Some(prev) = self.account_menu_prior_focus.take() {
                prev.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if self.recent_drawer_open {
            self.recent_drawer_open = false;
            if let Some(prev) = self.recent_drawer_prior_focus.take() {
                prev.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if self.sidebar_batch_mode {
            self.sidebar_batch_mode = false;
            self.sidebar_selected_ids.clear();
            cx.notify();
            return true;
        }
        false
    }

    pub fn request_close_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.exit_dialog.is_some() {
            return;
        }
        let prior_focus = window.focused(cx);
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let check_res = cx
                .background_executor()
                .spawn({
                    let c = client.clone();
                    async move { c.check_exit().await }
                })
                .await;

            match check_res {
                Ok(report) if report.requires_confirmation() => {
                    let _ = weak.update_in(cx, |this, window, cx| {
                        this.exit_dialog = Some(ExitDialogModalState {
                            report,
                            exiting: false,
                            prior_focus,
                        });
                        // Safe default focus on `"取消"`!
                        this.exit_cancel_focus.focus(window, cx);
                        cx.notify();
                    });
                }
                _ => {
                    let _ = weak.update_in(cx, |this, window, cx| {
                        this.execute_final_shutdown(window, cx);
                    });
                }
            }
        })
        .detach();
    }

    fn execute_final_shutdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref mut d) = self.exit_dialog {
            d.exiting = true;
            cx.notify();
        }
        let pending_patch = self.bounds_tracker.take_pending_patch();
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let _ = client
                .spawn_tokio({
                    let c = client.clone();
                    async move {
                        if let Some(patch) = pending_patch {
                            let _ = tokio::time::timeout(
                                Duration::from_millis(200),
                                c.patch_desktop_settings(patch),
                            )
                            .await;
                        }
                        let _ = c.shutdown_and_exit().await;
                        Ok(())
                    }
                })
                .await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.allow_immediate_close = true;
                window.remove_window();
                cx.quit();
            });
        })
        .detach();
    }

    fn trigger_open_web(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !force && self.client.check_web_switch().requires_confirmation() {
            self.web_switch_confirm = Some(WebSwitchConfirmState {
                prior_focus: window.focused(cx),
            });
            cx.notify();
            return;
        }
        self.web_switch_confirm = None;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.open_web().await })
                .await;
            let _ = weak.update(cx, |this, cx| match res {
                Ok(opened) => {
                    this.last_web_url = Some(opened.url.clone());
                    open_url_in_trusted_browser(&opened.url);
                    cx.notify();
                }
                Err(err) => {
                    this.show_workspace_error(err.to_string(), cx);
                }
            });
        })
        .detach();
    }

    fn trigger_close_web(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.close_web().await })
                .await;
            let _ = weak.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    this.root_focus.focus(window, cx);
                    cx.notify();
                }
                Err(err) => {
                    this.show_workspace_error(err.to_string(), cx);
                }
            });
        })
        .detach();
    }

    fn activate_session_by_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.route = ShellRoute::Session;
        self.recent_drawer_open = false;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.activate_session(Some(session_id)).await })
                .await;
            if let Err(err) = res {
                let _ = weak.update(cx, |this, cx| {
                    this.show_workspace_error(err.to_string(), cx);
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn render_workspace_body(
        &self,
        sky_time: f32,
        palette: &DesktopPalette,
    ) -> gpui_kit::AnyElement {
        match self.route {
            ShellRoute::Home => div()
                .id("workspace-home-view")
                .test_support()
                .relative()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(render_sky_background(
                            SkyPreset::home_for_theme(palette.mode),
                            sky_time,
                            1.0,
                        )),
                )
                .child(
                    div()
                        .relative()
                        .size_full()
                        .px(space::GROUP)
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w_full()
                                .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(space::LG)
                                .child(
                                    apply_scale(div(), scale::HERO)
                                        .text_color(palette.text_primary)
                                        .child("有什么需要诊断或修复？"),
                                )
                                .child(
                                    apply_scale(div(), scale::UI)
                                        .text_color(palette.text_secondary)
                                        .child("支持硬件巡检、蓝屏与日志排查、启动项分析及命令清单交付"),
                                ),
                        ),
                )
                .into_any_element(),
            ShellRoute::Login => div()
                .id("workspace-login-view")
                .test_support()
                .flex()
                .flex_row()
                .items_stretch()
                .size_full()
                .bg(palette.bg_workspace)
                .child(
                    div()
                        .relative()
                        .w(layout::LOGIN_SHADER_WIDTH)
                        .h_full()
                        .flex_shrink_0()
                        .overflow_hidden()
                        .child(render_sky_background(
                            SkyPreset::login_for_theme(palette.mode),
                            sky_time,
                            1.0,
                        ))
                        .child(render_login_9_stop_fade(palette)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .px(space::GROUP)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(px(300.0))
                                .flex()
                                .flex_col()
                                .gap(space::MD)
                                .child(
                                    apply_scale(div(), scale::TITLE)
                                        .text_color(palette.text_primary)
                                        .child("连接模型服务"),
                                )
                                .child(
                                    apply_scale(div(), scale::UI)
                                        .text_color(palette.text_secondary)
                                        .child("支持 CSTOA 扫码授权、OAuth 与 APIKEY 服务配置。"),
                                ),
                        ),
                )
                .into_any_element(),
            other => {
                let (id, heading) = match other {
                    ShellRoute::Session => ("workspace-session-view", "对话工作台"),
                    ShellRoute::AllSessions => ("workspace-all-sessions-view", "全部会话"),
                    ShellRoute::Settings => ("workspace-settings-view", "设置"),
                    ShellRoute::Account => ("workspace-account-view", "专属账号"),
                    ShellRoute::Home | ShellRoute::Login => unreachable!(),
                };
                div()
                    .id(id)
                    .test_support()
                    .size_full()
                    .bg(palette.bg_workspace)
                    .px(space::GROUP)
                    .py(space::GROUP)
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                            .child(
                                apply_scale(div(), scale::TITLE)
                                    .text_color(palette.text_primary)
                                    .child(heading),
                            ),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_recent_drawer_popover(
        &self,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        entity: &Entity<Self>,
    ) -> impl IntoElement {
        let recent = if self.snapshot.sessions.len() > MAX_RECENT_SIDEBAR_SESSIONS {
            &self.snapshot.sessions[..MAX_RECENT_SIDEBAR_SESSIONS]
        } else {
            &self.snapshot.sessions[..]
        };

        let mut rows = div()
            .id("recent-drawer-rows")
            .relative()
            .max_h(px(320.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(space::MICRO);

        if recent.is_empty() {
            rows = rows.child(
                apply_scale(div(), scale::UI)
                    .px(space::SM)
                    .py(space::MD)
                    .text_color(palette.text_muted)
                    .child("暂无历史对话"),
            );
        } else {
            for sess in recent {
                let sid = sess.id.clone();
                let title = if sess.title.trim().is_empty() {
                    SharedString::from("新对话")
                } else {
                    SharedString::from(sess.title.clone())
                };
                let ent = entity.clone();
                rows = rows.child(
                    apply_scale(div(), scale::UI)
                        .id(SharedString::from(format!("drawer-session-{sid}")))
                        .test_support()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, _, cx| {
                            let target = sid.clone();
                            ent.update(cx, |this, cx| this.activate_session_by_id(target, cx));
                        })
                        .child(div().w_full().truncate().child(cjk_inline_label(title))),
                );
            }
        }

        let ent_all = entity.clone();
        div().absolute().top(px(44.0)).left(px(108.0)).child(
            render_frosted_surface(
                "recent-sessions-drawer",
                FrostedGlassSpec::popover_h2p2z(palette),
                palette.bg_elevated,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .w(px(264.0))
            .p(space::SM)
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_hairline)
            .flex()
            .flex_col()
            .gap(space::XS)
            .child(rows)
            .child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id("drawer-open-all-sessions-btn")
                    .test_support()
                    .relative()
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.accent_solid)
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, _, cx| {
                        ent_all.update(cx, |this, cx| {
                            this.recent_drawer_open = false;
                            this.route = ShellRoute::AllSessions;
                            cx.notify();
                        });
                    })
                    .child(cjk_inline_label("查看全部会话")),
            ),
        )
    }

    fn render_account_popover(
        &self,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        entity: &Entity<Self>,
    ) -> impl IntoElement {
        let logged_in = self.snapshot.account.as_ref().is_some_and(|a| a.signed_in);
        let ent_auth = entity.clone();
        let ent_set = entity.clone();
        let ent_exit = entity.clone();

        let pos_div = if self.sidebar_collapsed {
            div().absolute().top(px(44.0)).right(px(136.0))
        } else {
            div().absolute().bottom(px(52.0)).left(space::SM)
        };

        pos_div.child(
            render_frosted_surface(
                "account-popover-menu",
                FrostedGlassSpec::popover_h2p2z(palette),
                palette.bg_elevated,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .w(px(216.0))
            .p(space::XS)
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_hairline)
            .flex()
            .flex_col()
            .gap(space::MICRO)
            .child(
                apply_scale(div(), scale::UI)
                    .id("account-menu-profile-btn")
                    .test_support()
                    .relative()
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, _, cx| {
                        ent_auth.update(cx, |this, cx| {
                            this.account_menu_open = false;
                            this.route = if logged_in {
                                ShellRoute::Account
                            } else {
                                ShellRoute::Login
                            };
                            cx.notify();
                        });
                    })
                    .child(
                        Icon::new(IconName::User)
                            .size(px(14.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label(if logged_in {
                        "专属账号"
                    } else {
                        "登录模型服务"
                    })),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("account-menu-settings-btn")
                    .test_support()
                    .relative()
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, _, cx| {
                        ent_set.update(cx, |this, cx| {
                            this.account_menu_open = false;
                            this.route = ShellRoute::Settings;
                            cx.notify();
                        });
                    })
                    .child(
                        Icon::new(IconName::Settings)
                            .size(px(14.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label("设置")),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("account-menu-exit-btn")
                    .test_support()
                    .relative()
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.status_danger)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, window, cx| {
                        ent_exit.update(cx, |this, cx| {
                            this.account_menu_open = false;
                            this.request_close_flow(window, cx);
                        });
                    })
                    .child(
                        Icon::new(IconName::LogOut)
                            .size(px(14.0))
                            .text_color(palette.status_danger),
                    )
                    .child(cjk_inline_label("退出 CST Pilot")),
            ),
        )
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();

        let web_locked = self.snapshot.channel_mode == ChannelModeDto::WebActive;
        let hw_diag_active = has_active_hardware_diagnostic(&self.snapshot);
        let sky_time = self.shader_scheduler.sample_for_window(
            self.route.uses_sky_shader(),
            web_locked,
            hw_diag_active,
            cx.reduce_motion(),
            window,
        );

        let (active_session_id, active_view) = self
            .client
            .with_workspace(|ws| (ws.active_session_id().map(str::to_owned), ws.active_view()));

        let (account_label, account_initial) = match self.snapshot.account.as_ref() {
            Some(acc) if acc.signed_in => {
                let name = acc
                    .profile
                    .name
                    .as_deref()
                    .or(acc.profile.student_id.as_deref())
                    .unwrap_or("已登录");
                let init = name
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "U".to_owned());
                (
                    SharedString::from(name.to_owned()),
                    SharedString::from(init),
                )
            }
            _ => (SharedString::from("未登录账号"), SharedString::from("C")),
        };

        let update_available = self.snapshot.update_state.as_ref().is_some_and(|u| {
            matches!(
                u.status.phase,
                UpdatePhaseDto::Available | UpdatePhaseDto::Preparing | UpdatePhaseDto::Ready
            )
        });

        let entity = cx.entity();

        let topbar_el = {
            let e1 = entity.clone();
            let e2 = entity.clone();
            let e3 = entity.clone();
            let e4 = entity.clone();
            let e5 = entity.clone();
            let e6 = entity.clone();
            let e7 = entity.clone();
            let e8 = entity.clone();
            let e9 = entity.clone();
            render_topbar(
                self.sidebar_collapsed,
                active_view,
                window.is_maximized(),
                update_available,
                account_initial.clone(),
                &self.topbar_expand_focus,
                &self.topbar_recent_focus,
                &self.topbar_account_focus,
                &palette,
                TopbarCallbacks {
                    on_expand_sidebar: Rc::new(move |w, cx| {
                        e1.update(cx, |this, cx| this.toggle_sidebar(w, cx));
                    }),
                    on_toggle_recent_drawer: Rc::new(move |w, cx| {
                        e2.update(cx, |this, cx| {
                            this.recent_drawer_open = !this.recent_drawer_open;
                            if this.recent_drawer_open {
                                this.recent_drawer_prior_focus = w.focused(cx);
                            }
                            cx.notify();
                        });
                    }),
                    on_new_session: Rc::new(move |_, cx| {
                        e3.update(cx, |this, cx| {
                            this.route = ShellRoute::Home;
                            let c = this.client.clone();
                            cx.background_executor()
                                .spawn(async move {
                                    let _ = c.activate_session(None).await;
                                })
                                .detach();
                            cx.notify();
                        });
                    }),
                    on_open_dashboard: Rc::new(move |_, cx| {
                        e4.update(cx, |this, cx| {
                            this.show_top_notice("本地用量仪表盘将在后续版本开放。", cx);
                        });
                    }),
                    on_select_view: Rc::new(move |view, _, cx| {
                        e5.update(cx, |this, cx| {
                            if view == WorkspaceView::Tui {
                                this.show_top_notice("窗口内置终端视图将在阶段 10 开放。", cx);
                            } else {
                                this.client
                                    .with_workspace_mut(|ws| ws.switch_view(view, None));
                                cx.notify();
                            }
                        });
                    }),
                    on_open_web: Rc::new(move |w, cx| {
                        e6.update(cx, |this, cx| this.trigger_open_web(false, w, cx));
                    }),
                    on_toggle_account_menu: Rc::new(move |w, cx| {
                        e7.update(cx, |this, cx| {
                            this.account_menu_open = !this.account_menu_open;
                            if this.account_menu_open {
                                this.account_menu_prior_focus = w.focused(cx);
                            }
                            cx.notify();
                        });
                    }),
                    on_open_settings: Rc::new(move |_, cx| {
                        e8.update(cx, |this, cx| {
                            this.route = ShellRoute::Settings;
                            cx.notify();
                        });
                    }),
                    on_minimize_window: Rc::new(|w, _| w.minimize_window()),
                    on_zoom_window: Rc::new(|w, _| w.zoom_window()),
                    on_request_close: Rc::new(move |w, cx| {
                        e9.update(cx, |this, cx| this.request_close_flow(w, cx));
                    }),
                },
            )
        };

        let mut workspace_card = div()
            .id("workspace-container")
            .test_support()
            .relative()
            .size_full()
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_workspace)
            .overflow_hidden()
            .child(self.render_workspace_body(sky_time, &palette));

        if let Some(ref err_msg) = self.workspace_error {
            workspace_card = workspace_card.child(render_workspace_error_bar(
                err_msg.clone(),
                &palette,
                transparency_enabled,
                scale_factor,
            ));
        }

        // Frame strips (`8px` `$bg-shell` around workspace)
        let framed_workspace = if self.sidebar_collapsed {
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .pl(layout::SHELL_FRAME_INSET)
                .pr(layout::SHELL_FRAME_INSET)
                .pb(layout::SHELL_FRAME_INSET)
                .bg(palette.bg_shell)
                .child(workspace_card)
        } else {
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .pr(layout::SHELL_FRAME_INSET)
                .pb(layout::SHELL_FRAME_INSET)
                .bg(palette.bg_shell)
                .child(workspace_card)
        };

        let right_column = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(topbar_el)
            .child(framed_workspace);

        let mut main_layout = div()
            .flex()
            .flex_row()
            .items_stretch()
            .size_full()
            .bg(palette.bg_shell);

        if !self.sidebar_collapsed {
            let s1 = entity.clone();
            let s2 = entity.clone();
            let s3 = entity.clone();
            let s4 = entity.clone();
            let s5 = entity.clone();
            let s6 = entity.clone();
            let s7 = entity.clone();
            let s8 = entity.clone();
            let s9 = entity.clone();
            let s10 = entity.clone();
            let s11 = entity.clone();

            main_layout = main_layout.child(render_sidebar_l081m1(
                self.route,
                active_session_id.as_deref(),
                &self.snapshot.sessions,
                self.sidebar_batch_mode,
                &self.sidebar_selected_ids,
                account_label,
                account_initial,
                &self.sidebar_collapse_focus,
                &self.sidebar_account_focus,
                &palette,
                SidebarCallbacks {
                    on_collapse: Rc::new(move |w, cx| {
                        s1.update(cx, |this, cx| this.toggle_sidebar(w, cx));
                    }),
                    on_new_session: Rc::new(move |_, cx| {
                        s2.update(cx, |this, cx| {
                            this.route = ShellRoute::Home;
                            let c = this.client.clone();
                            cx.background_executor()
                                .spawn(async move {
                                    let _ = c.activate_session(None).await;
                                })
                                .detach();
                            cx.notify();
                        });
                    }),
                    on_open_all_sessions: Rc::new(move |_, cx| {
                        s3.update(cx, |this, cx| {
                            this.route = ShellRoute::AllSessions;
                            cx.notify();
                        });
                    }),
                    on_open_dashboard: Rc::new(move |_, cx| {
                        s4.update(cx, |this, cx| {
                            this.show_top_notice("本地用量仪表盘将在后续版本开放。", cx);
                        });
                    }),
                    on_select_session: Rc::new(move |sid, _, cx| {
                        s5.update(cx, |this, cx| this.activate_session_by_id(sid, cx));
                    }),
                    on_toggle_batch_mode: Rc::new(move |_, cx| {
                        s6.update(cx, |this, cx| {
                            this.sidebar_batch_mode = !this.sidebar_batch_mode;
                            this.sidebar_selected_ids.clear();
                            cx.notify();
                        });
                    }),
                    on_toggle_select_all: Rc::new(move |_, cx| {
                        s7.update(cx, |this, cx| {
                            let recent_ids: Vec<String> = this
                                .snapshot
                                .sessions
                                .iter()
                                .take(MAX_RECENT_SIDEBAR_SESSIONS)
                                .map(|s| s.id.clone())
                                .collect();
                            if recent_ids
                                .iter()
                                .all(|id| this.sidebar_selected_ids.contains(id))
                            {
                                this.sidebar_selected_ids.clear();
                            } else {
                                this.sidebar_selected_ids = recent_ids.into_iter().collect();
                            }
                            cx.notify();
                        });
                    }),
                    on_batch_delete: Rc::new(move |_, cx| {
                        s8.update(cx, |this, cx| {
                            let targets: Vec<String> =
                                this.sidebar_selected_ids.iter().cloned().collect();
                            if targets.is_empty() {
                                return;
                            }
                            this.sidebar_selected_ids.clear();
                            this.sidebar_batch_mode = false;
                            let client = this.client.clone();
                            cx.spawn(async move |weak, cx| {
                                let mut failed = None;
                                for sid in targets {
                                    let c = client.clone();
                                    let res = cx
                                        .background_executor()
                                        .spawn(async move { c.delete_session(sid).await })
                                        .await;
                                    if let Err(err) = res {
                                        failed = Some(err.to_string());
                                    }
                                }
                                let c_ref = client.clone();
                                let _ = cx
                                    .background_executor()
                                    .spawn(async move { c_ref.refresh_globals().await })
                                    .await;
                                if let Some(msg) = failed {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.show_workspace_error(msg, cx);
                                    });
                                }
                            })
                            .detach();
                            cx.notify();
                        });
                    }),
                    on_toggle_session_checked: Rc::new(move |sid, _, cx| {
                        s9.update(cx, |this, cx| {
                            if !this.sidebar_selected_ids.remove(&sid) {
                                this.sidebar_selected_ids.insert(sid);
                            }
                            cx.notify();
                        });
                    }),
                    on_toggle_account_menu: Rc::new(move |w, cx| {
                        s10.update(cx, |this, cx| {
                            this.account_menu_open = !this.account_menu_open;
                            if this.account_menu_open {
                                this.account_menu_prior_focus = w.focused(cx);
                            }
                            cx.notify();
                        });
                    }),
                    on_open_settings: Rc::new(move |_, cx| {
                        s11.update(cx, |this, cx| {
                            this.route = ShellRoute::Settings;
                            cx.notify();
                        });
                    }),
                },
            ));
        }

        main_layout = main_layout.child(right_column);

        let mut root = div()
            .id("cst-app-shell")
            .test_support()
            .key_context(SHELL_KEY_CONTEXT)
            .track_focus(&self.root_focus)
            .relative()
            .size_full()
            .font_family(theme_global.ui_font_family)
            .text_color(palette.text_primary)
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewSession, _, cx| {
                this.route = ShellRoute::Home;
                let c = this.client.clone();
                cx.background_executor()
                    .spawn(async move {
                        let _ = c.activate_session(None).await;
                    })
                    .detach();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.route = ShellRoute::Settings;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &DismissTopSurface, window, cx| {
                let _ = this.dismiss_topmost_surface(window, cx);
            }))
            .on_action(cx.listener(|this, _: &RequestAppExit, window, cx| {
                this.request_close_flow(window, cx);
            }))
            .child(main_layout);

        if self.recent_drawer_open && self.sidebar_collapsed {
            root = root.child(self.render_recent_drawer_popover(
                &palette,
                transparency_enabled,
                scale_factor,
                &entity,
            ));
        }

        if self.account_menu_open {
            root = root.child(self.render_account_popover(
                &palette,
                transparency_enabled,
                scale_factor,
                &entity,
            ));
        }

        if let Some(ref notice) = self.top_notice {
            root = root.child(render_top_notice_banner(
                notice.clone(),
                &palette,
                transparency_enabled,
                scale_factor,
            ));
        }

        // Unsent draft confirmation before switching to Web mode
        if self.web_switch_confirm.is_some() {
            let ent_cancel = entity.clone();
            let ent_proceed = entity.clone();
            root = root.child(
                render_frosted_surface(
                    "web-switch-confirm-scrim",
                    FrostedGlassSpec::scrim_dz0fp(&palette),
                    palette.bg_scrim,
                    transparency_enabled,
                    scale_factor,
                )
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(
                    render_frosted_surface(
                        "web-switch-confirm-card",
                        FrostedGlassSpec::modal_k3x72(&palette, 8.0),
                        palette.bg_modal,
                        transparency_enabled,
                        scale_factor,
                    )
                    .test_support()
                    .w(layout::MODAL_STANDARD_WIDTH)
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .p(space::GROUP)
                    .flex()
                    .flex_col()
                    .gap(space::LG)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::BODY_SEMIBOLD)
                                    .text_color(palette.text_primary)
                                    .child("切换到网页端？"),
                            )
                            .child(
                                apply_scale(div(), scale::UI)
                                    .text_color(palette.text_secondary)
                                    .child("当前存在未发送草稿或待确认消息。切换至网页端期间桌面输入框将暂时锁定。"),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex()
                            .justify_end()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .id("web-switch-cancel-btn")
                                    .test_support()
                                    .h(height::CONTROL)
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_strong)
                                    .bg(palette.bg_elevated)
                                    .flex()
                                    .items_center()
                                    .on_click(move |_, w, cx| {
                                        ent_cancel.update(cx, |this, cx| {
                                            let _ = this.dismiss_topmost_surface(w, cx);
                                        });
                                    })
                                    .child(cjk_inline_label("取消")),
                            )
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .id("web-switch-confirm-btn")
                                    .test_support()
                                    .h(height::CONTROL)
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .bg(palette.accent_solid)
                                    .text_color(palette.text_on_accent)
                                    .flex()
                                    .items_center()
                                    .on_click(move |_, w, cx| {
                                        ent_proceed.update(cx, |this, cx| {
                                            this.trigger_open_web(true, w, cx);
                                        });
                                    })
                                    .child(cjk_inline_label("继续切换")),
                            ),
                    ),
                ),
            );
        }

        // `WebActive` takeover lock (`WO6NQ`)
        if web_locked {
            let ent_open = entity.clone();
            let ent_back = entity.clone();
            root = root.child(render_web_active_lock_overlay(
                &palette,
                transparency_enabled,
                scale_factor,
                &self.web_lock_trap_focus,
                &self.web_lock_switch_focus,
                move |w, cx| {
                    ent_open.update(cx, |this, cx| {
                        if let Some(ref url) = this.last_web_url {
                            open_url_in_trusted_browser(url);
                        } else {
                            this.trigger_open_web(true, w, cx);
                        }
                    });
                },
                move |w, cx| {
                    ent_back.update(cx, |this, cx| this.trigger_close_web(w, cx));
                },
            ));
        }

        // Exit confirmation dialog (`pFooK`) — sits topmost so user can always safely exit even during `WebActive`
        if let Some(ref dialog) = self.exit_dialog {
            let ent_cancel = entity.clone();
            let ent_confirm = entity.clone();
            root = root.child(render_exit_confirm_dialog(
                &dialog.report,
                dialog.exiting,
                &palette,
                transparency_enabled,
                scale_factor,
                &self.exit_trap_focus,
                &self.exit_cancel_focus,
                &self.exit_confirm_focus,
                move |w, cx| {
                    ent_cancel.update(cx, |this, cx| {
                        let _ = this.dismiss_topmost_surface(w, cx);
                    });
                },
                move |w, cx| {
                    ent_confirm.update(cx, |this, cx| this.execute_final_shutdown(w, cx));
                },
            ));
        }

        root
    }
}

fn open_url_in_trusted_browser(url: &str) {
    #[cfg(windows)]
    {
        if (url.starts_with("http://127.0.0.1:") || url.starts_with("https://"))
            && let Ok(win_dir) = cst_os::windows_directory()
        {
            let _ = std::process::Command::new(win_dir.join("explorer.exe"))
                .arg(url)
                .spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

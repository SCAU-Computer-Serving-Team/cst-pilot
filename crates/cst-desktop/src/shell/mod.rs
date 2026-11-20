pub mod exit_dialog;
pub mod overlays;
pub mod sidebar;
pub mod topbar;
pub mod window_bounds;

use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cst_client::{
    ChannelModeDto, ClientError, ClientHandle, ClientSnapshot, DesktopSettingsPatchDto,
    ExitCheckReport, ThemeModeDto, UpdatePhaseDto, WorkspaceView,
};
use gpui::{Animation, AnimationExt as _};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, TestSupportExt, Window, WindowControlArea, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::auth::account_view::{AccountView, AccountViewEvent};
use crate::auth::login_view::{LoginView, LoginViewEvent};
use crate::auth::missing_key::{MissingKeyModalState, render_missing_key_dialog};
use crate::background::{
    FrostedGlassSpec, ShaderFrameScheduler, has_active_hardware_diagnostic, render_frosted_surface,
};
use crate::palette::{CommandPaletteEvent, CommandPaletteModal, PaletteCommandTarget};
use crate::sessions::all_sessions::{AllSessionsEvent, AllSessionsView};
use crate::sessions::branch_tree::{BranchTreeEvent, BranchTreeView};
use crate::sessions::export_view::{ExportView, ExportViewEvent};
use crate::settings::update_dialog::{UpdateDialogState, render_update_dialog};
use crate::settings::{SettingsCategory, SettingsView, SettingsViewEvent};
use crate::shell::exit_dialog::render_exit_confirm_dialog;
use crate::shell::overlays::{
    BannerCountdownState, render_top_notice_banner, render_web_active_lock_overlay,
    render_workspace_error_bar,
};
use crate::shell::sidebar::{
    MAX_RECENT_SIDEBAR_SESSIONS, ShellRoute, SidebarCallbacks, filter_recent_sidebar_sessions,
    render_sidebar_l081m1,
};
use crate::shell::topbar::{TopbarCallbacks, render_topbar, render_window_caption_controls};
use crate::shell::window_bounds::{
    GEOMETRY_DEBOUNCE_MS, NormalBoundsTracker, ResolvedWindowGeometry, collect_app_displays,
};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, FontLoadReport, apply_desktop_theme, apply_scale,
    cjk_inline_label, height, layout, motion, radius, scale, space,
};
use crate::workbench::composer::SlashNavigationTarget;
use crate::workbench::{WorkbenchEvent, WorkbenchView};

fn viewport_is_narrow(window: &Window) -> Option<bool> {
    let viewport = window.viewport_size();
    (viewport.width > px(1.0) && viewport.height > px(1.0))
        .then(|| viewport.width < px(layout::COMPACT_SIDEBAR_BREAKPOINT_WIDTH))
}

gpui_kit::actions!(
    cst_shell,
    [
        ToggleSidebar,
        NewSession,
        OpenSettings,
        ToggleCommandPalette,
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
        KeyBinding::new("ctrl-k", ToggleCommandPalette, Some(SHELL_KEY_CONTEXT)),
        KeyBinding::new("escape", DismissTopSurface, Some(SHELL_KEY_CONTEXT)),
    ]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSubView {
    #[default]
    Chat,
    Tree,
    Export,
}

struct ExitDialogModalState {
    report: ExitCheckReport,
    exiting: bool,
    prior_focus: Option<FocusHandle>,
}

struct WebSwitchConfirmState {
    prior_focus: Option<FocusHandle>,
}

struct RenameSessionModalState {
    session_id: String,
    input: Entity<InputState>,
    prior_focus: Option<FocusHandle>,
}

pub struct AppShell {
    client: ClientHandle,
    snapshot: std::sync::Arc<ClientSnapshot>,
    theme_preference: ThemeModeDto,
    route: ShellRoute,
    session_subview: SessionSubView,
    workbench: Entity<WorkbenchView>,
    all_sessions_view: Entity<AllSessionsView>,
    branch_tree_view: Entity<BranchTreeView>,
    export_view: Entity<ExportView>,
    settings_view: Entity<SettingsView>,
    login_view: Entity<LoginView>,
    account_view: Entity<AccountView>,
    sidebar_search_input: Entity<InputState>,
    command_palette: Option<Entity<CommandPaletteModal>>,
    command_palette_sub: Option<Subscription>,
    missing_key_modal: Option<MissingKeyModalState>,
    pending_return_session_id: Option<Option<String>>,
    startup_missing_key_checked: bool,
    update_dialog: UpdateDialogState,
    last_prompted_update_version: Option<String>,
    branch_summarizing_sessions: BTreeSet<String>,
    session_subview_prior_focus: Option<FocusHandle>,
    sidebar_collapsed: bool,
    auto_collapsed_narrow: bool,
    sidebar_viewport_narrow: bool,
    sidebar_save_task: Option<Task<()>>,
    sidebar_pending_save: Option<bool>,
    sidebar_batch_mode: bool,
    sidebar_selected_ids: BTreeSet<String>,
    sidebar_context_menu: Option<(String, Point<Pixels>)>,
    sidebar_menu_prior_focus: Option<FocusHandle>,
    sidebar_menu_focus: FocusHandle,
    rename_modal: Option<RenameSessionModalState>,
    rename_trap_focus: FocusHandle,
    recent_drawer_open: bool,
    recent_drawer_prior_focus: Option<FocusHandle>,
    account_menu_open: bool,
    account_menu_prior_focus: Option<FocusHandle>,
    web_switch_confirm: Option<WebSwitchConfirmState>,
    exit_dialog: Option<ExitDialogModalState>,
    exit_check_pending: bool,
    shutting_down: bool,
    web_transition_pending: bool,
    web_lock_prior_focus: Option<FocusHandle>,
    missing_key_trap_focus: FocusHandle,
    missing_key_cancel_focus: FocusHandle,
    update_trap_focus: FocusHandle,
    update_dismiss_focus: FocusHandle,
    last_web_url: Option<String>,
    workspace_error: Option<SharedString>,
    top_notice: Option<SharedString>,
    error_banner_timer: BannerCountdownState,
    notice_banner_timer: BannerCountdownState,
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
        let initial_narrow = viewport_is_narrow(window)
            .unwrap_or(resolved_geometry.width < layout::COMPACT_SIDEBAR_BREAKPOINT_WIDTH);
        let auto_collapsed_narrow =
            initial_narrow && !initial_snapshot.desktop_settings.sidebar_collapsed;
        let sidebar_collapsed =
            initial_snapshot.desktop_settings.sidebar_collapsed || auto_collapsed_narrow;
        let theme_preference = initial_snapshot.desktop_settings.theme;
        let _palette = apply_desktop_theme(theme_preference, Some(window), font_report, cx);

        let initial_notice = font_report
            .and_then(|r| r.warning.as_ref())
            .map(|w| SharedString::from(w.clone()))
            .or_else(|| {
                initial_snapshot.desktop_settings_corrupt.then(|| {
                    SharedString::from("桌面配置文件格式异常，已保留原文件并使用默认外观。")
                })
            });

        let root_focus = cx.focus_handle();
        let sidebar_collapse_focus = cx.focus_handle().tab_stop(true);
        let topbar_expand_focus = cx.focus_handle().tab_stop(true);
        let topbar_recent_focus = cx.focus_handle().tab_stop(true);
        let sidebar_account_focus = cx.focus_handle();
        let topbar_account_focus = cx.focus_handle().tab_stop(true);
        let web_lock_trap_focus = cx.focus_handle();
        let web_lock_switch_focus = cx.focus_handle().tab_stop(true);
        let exit_trap_focus = cx.focus_handle();
        let exit_cancel_focus = cx.focus_handle().tab_stop(true);
        let exit_confirm_focus = cx.focus_handle().tab_stop(true);

        let workbench = cx.new(|cx| WorkbenchView::new(client.clone(), window, cx));
        let all_sessions_view = cx.new(|cx| AllSessionsView::new(client.clone(), cx));
        let branch_tree_view =
            cx.new(|cx| BranchTreeView::new(client.clone(), String::new(), window, cx));
        let export_view = cx.new(|cx| ExportView::new(client.clone(), String::new(), cx));
        let settings_view = cx.new(|cx| SettingsView::new(client.clone(), window, cx));
        let login_view = cx.new(|cx| LoginView::new(client.clone(), window, cx));
        let account_view = cx.new(|cx| AccountView::new(client.clone(), cx));
        let sidebar_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索"));

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&workbench, window, Self::on_workbench_event));
        subscriptions.push(cx.subscribe_in(
            &all_sessions_view,
            window,
            Self::on_all_sessions_event,
        ));
        subscriptions.push(cx.observe(&all_sessions_view, |_, _, cx| cx.notify()));
        subscriptions.push(cx.subscribe_in(&branch_tree_view, window, Self::on_branch_tree_event));
        subscriptions.push(cx.subscribe_in(&export_view, window, Self::on_export_view_event));
        subscriptions.push(cx.subscribe_in(&settings_view, window, Self::on_settings_view_event));
        subscriptions.push(cx.subscribe_in(&login_view, window, Self::on_login_view_event));
        subscriptions.push(cx.subscribe_in(&account_view, window, Self::on_account_view_event));
        subscriptions.push(cx.subscribe_in(
            &sidebar_search_input,
            window,
            |_, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));

        // Observe system light/dark appearance changes
        subscriptions.push(cx.observe_window_appearance(window, |this, window, cx| {
            apply_desktop_theme(this.theme_preference, Some(window), None, cx);
            cx.notify();
        }));

        // Observe window bounds changes -> 300ms debounced PATCH to `/api/app/settings`
        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.on_window_geometry_changed(window, cx);
        }));
        let weak_shell = cx.entity().downgrade();
        subscriptions.push(
            window.observe_window_visibility(move |visibility, window, cx| {
                if visibility.is_visible()
                    && let Some(shell) = weak_shell.upgrade()
                {
                    shell.update(cx, |this, cx| this.on_window_geometry_changed(window, cx));
                }
            }),
        );

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

        let mut shell = Self {
            client,
            snapshot: initial_snapshot,
            theme_preference,
            route: ShellRoute::Home,
            session_subview: SessionSubView::Chat,
            workbench,
            all_sessions_view,
            branch_tree_view,
            export_view,
            settings_view,
            login_view,
            account_view,
            sidebar_search_input,
            command_palette: None,
            command_palette_sub: None,
            missing_key_modal: None,
            pending_return_session_id: None,
            startup_missing_key_checked: false,
            update_dialog: UpdateDialogState::default(),
            last_prompted_update_version: None,
            branch_summarizing_sessions: BTreeSet::new(),
            session_subview_prior_focus: None,
            sidebar_collapsed,
            auto_collapsed_narrow,
            sidebar_viewport_narrow: initial_narrow,
            sidebar_save_task: None,
            sidebar_pending_save: None,
            sidebar_batch_mode: false,
            sidebar_selected_ids: BTreeSet::new(),
            sidebar_context_menu: None,
            sidebar_menu_prior_focus: None,
            sidebar_menu_focus: cx.focus_handle(),
            rename_modal: None,
            rename_trap_focus: cx.focus_handle(),
            recent_drawer_open: false,
            recent_drawer_prior_focus: None,
            account_menu_open: false,
            account_menu_prior_focus: None,
            web_switch_confirm: None,
            exit_dialog: None,
            exit_check_pending: false,
            shutting_down: false,
            web_transition_pending: false,
            web_lock_prior_focus: None,
            missing_key_trap_focus: cx.focus_handle(),
            missing_key_cancel_focus: cx.focus_handle().tab_stop(true),
            update_trap_focus: cx.focus_handle(),
            update_dismiss_focus: cx.focus_handle().tab_stop(true),
            last_web_url: None,
            workspace_error: None,
            top_notice: None,
            error_banner_timer: BannerCountdownState::new(),
            notice_banner_timer: BannerCountdownState::new(),
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

        // Seed the tracker even if the platform does not emit a bounds callback at startup.
        // Geometry stabilization must not consume the first user-driven resize.
        shell.bounds_tracker.observe(
            window.bounds(),
            resolved_geometry.maximized || window.is_maximized(),
            viewport_is_narrow(window).is_none(),
            resolved_geometry.display_hint.clone(),
        );

        if shell.snapshot.channel_mode == ChannelModeDto::WebActive {
            shell.web_lock_prior_focus = Some(root_focus);
            shell.web_lock_switch_focus.focus(window, cx);
        } else {
            root_focus.focus(window, cx);
        }
        if let Some(notice_msg) = initial_notice {
            shell.show_top_notice(notice_msg, cx);
        }
        shell.maybe_check_startup_missing_key_and_updates(window, cx);
        shell.spawn_client_revision_listener(window, cx);
        shell.spawn_wakeup_listener(window, cx);
        shell
    }

    pub fn route(&self) -> ShellRoute {
        self.route
    }

    pub fn session_subview(&self) -> SessionSubView {
        self.session_subview
    }

    pub fn workbench(&self) -> &Entity<WorkbenchView> {
        &self.workbench
    }

    pub fn all_sessions_view(&self) -> &Entity<AllSessionsView> {
        &self.all_sessions_view
    }

    pub fn branch_tree_view(&self) -> &Entity<BranchTreeView> {
        &self.branch_tree_view
    }

    pub fn export_view(&self) -> &Entity<ExportView> {
        &self.export_view
    }

    pub fn settings_view(&self) -> &Entity<SettingsView> {
        &self.settings_view
    }

    pub fn login_view(&self) -> &Entity<LoginView> {
        &self.login_view
    }

    pub fn account_view(&self) -> &Entity<AccountView> {
        &self.account_view
    }

    pub fn command_palette(&self) -> Option<&Entity<CommandPaletteModal>> {
        self.command_palette.as_ref()
    }

    pub fn set_route(&mut self, route: ShellRoute, cx: &mut Context<Self>) {
        self.set_workspace_route(route, SessionSubView::Chat, cx);
    }

    fn set_workspace_route(
        &mut self,
        route: ShellRoute,
        subview: SessionSubView,
        cx: &mut Context<Self>,
    ) {
        if self.route != route {
            self.account_menu_open = false;
            self.account_menu_prior_focus = None;
            if self.route == ShellRoute::Login || route == ShellRoute::Login {
                self.login_view.update(cx, |view, cx| {
                    view.set_active(route == ShellRoute::Login, cx)
                });
            }
            if self.route == ShellRoute::Home {
                self.workbench.update(cx, |wb, _| wb.mark_left_home_route());
            }
            if self.route == ShellRoute::Settings {
                self.settings_view.update(cx, SettingsView::leave);
            }
            if self.route == ShellRoute::Account {
                self.account_view.update(cx, AccountView::leave);
            }
            if route == ShellRoute::Account {
                self.account_view
                    .update(cx, |view, cx| view.refresh_cstoa_quota(true, cx));
            }
        }
        if self.route != route || self.session_subview != subview {
            if self.route == ShellRoute::AllSessions {
                self.all_sessions_view
                    .update(cx, AllSessionsView::reset_surface);
            }
            if self.route == ShellRoute::Session {
                match self.session_subview {
                    SessionSubView::Tree => self
                        .branch_tree_view
                        .update(cx, |view, _| view.close_view()),
                    SessionSubView::Export => self.export_view.update(cx, |view, _| view.stop()),
                    SessionSubView::Chat => {}
                }
            }
            self.rename_modal = None;
            self.sidebar_context_menu = None;
            self.sidebar_menu_prior_focus = None;
            self.sidebar_batch_mode = false;
            self.sidebar_selected_ids.clear();
            self.session_subview_prior_focus = None;
        }
        self.route = route;
        self.session_subview = subview;
        cx.notify();
    }

    fn return_to_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prior = self.session_subview_prior_focus.take();
        self.set_route(ShellRoute::Session, cx);
        self.restore_surface_focus(prior, window, cx);
    }

    pub fn is_sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    pub fn has_exit_dialog(&self) -> bool {
        self.exit_dialog.is_some()
    }

    pub fn workbench_welcome_stagger_done(&self, cx: &App) -> bool {
        self.workbench.read(cx).is_welcome_stagger_done()
    }

    pub fn open_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_dialog.open || self.shutting_down {
            return;
        }
        self.update_dialog.open = true;
        self.update_dialog.busy_error = None;
        self.update_dialog.prior_focus = window.focused(cx);
        if self.exit_dialog.is_none()
            && self.snapshot.channel_mode != ChannelModeDto::WebActive
            && self.web_switch_confirm.is_none()
        {
            self.update_dismiss_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn update_handoff_active(&self) -> bool {
        self.update_dialog.handoff_locked
            || self.update_dialog.launch_pending
            || (self.update_dialog.open
                && self
                    .snapshot
                    .update_state
                    .as_ref()
                    .is_some_and(|update| update.status.phase == UpdatePhaseDto::Launching))
    }

    fn close_account_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.account_menu_open = false;
        if let Some(prev) = self.account_menu_prior_focus.take() {
            prev.focus(window, cx);
        }
        cx.notify();
    }

    fn blocks_workspace_shortcuts(&self, cx: &App) -> bool {
        self.shutting_down
            || self.exit_dialog.is_some()
            || self.update_dialog.open
            || self.missing_key_modal.is_some()
            || self.web_switch_confirm.is_some()
            || self.command_palette.is_some()
            || self.rename_modal.is_some()
            || (self.route == ShellRoute::AllSessions
                && self.all_sessions_view.read(cx).has_rename_modal())
            || (self.route == ShellRoute::Session
                && self.session_subview == SessionSubView::Tree
                && self.branch_tree_view.read(cx).has_summary_modal())
            || self.account_menu_open
            || (self.route == ShellRoute::Login && self.login_view.read(cx).has_dropdown())
            || (self.route == ShellRoute::Settings && self.settings_view.read(cx).has_dropdown(cx))
            || (self.route == ShellRoute::Account
                && self.account_view.read(cx).blocks_workspace_shortcuts())
            || self.snapshot.channel_mode == ChannelModeDto::WebActive
    }

    fn restore_surface_focus(
        &self,
        prior: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keep = |trap: &FocusHandle, default: &FocusHandle| {
            prior
                .as_ref()
                .filter(|focus| trap.contains(focus, window))
                .unwrap_or(default)
                .clone()
        };
        let focus = if self.exit_dialog.is_some() {
            keep(&self.exit_trap_focus, &self.exit_cancel_focus)
        } else if self.snapshot.channel_mode == ChannelModeDto::WebActive {
            keep(&self.web_lock_trap_focus, &self.web_lock_switch_focus)
        } else if self.update_dialog.open {
            if self.update_handoff_active() {
                self.update_trap_focus.clone()
            } else {
                keep(&self.update_trap_focus, &self.update_dismiss_focus)
            }
        } else if self.missing_key_modal.is_some() {
            keep(&self.missing_key_trap_focus, &self.missing_key_cancel_focus)
        } else {
            prior
                .filter(|focus| self.root_focus.contains(focus, window))
                .unwrap_or_else(|| self.root_focus.clone())
        };
        focus.focus(window, cx);
    }

    fn focus_missing_key(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.exit_dialog.is_none()
            && !self.update_dialog.open
            && self.snapshot.channel_mode != ChannelModeDto::WebActive
            && self.web_switch_confirm.is_none()
        {
            self.missing_key_cancel_focus.focus(window, cx);
        }
    }

    fn schedule_error_dismiss_after(
        &mut self,
        gen_id: u64,
        wait: Duration,
        cx: &mut Context<Self>,
    ) {
        self.error_dismiss_task = Some(cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(wait).await;
            let _ = weak.update(cx, |this, cx| {
                if this.error_banner_timer.should_dismiss(gen_id) {
                    this.workspace_error = None;
                    this.error_dismiss_task = None;
                    cx.notify();
                }
            });
        }));
    }

    fn schedule_notice_dismiss_after(
        &mut self,
        gen_id: u64,
        wait: Duration,
        cx: &mut Context<Self>,
    ) {
        self.notice_dismiss_task = Some(cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(wait).await;
            let _ = weak.update(cx, |this, cx| {
                if this.notice_banner_timer.should_dismiss(gen_id) {
                    this.top_notice = None;
                    this.notice_dismiss_task = None;
                    cx.notify();
                }
            });
        }));
    }

    fn set_banner_hovered(&mut self, is_error: bool, hovered: bool, cx: &mut Context<Self>) {
        let now = Instant::now();
        if is_error {
            if let Some((gen_id, wait)) = self.error_banner_timer.set_hovered(hovered, now) {
                self.schedule_error_dismiss_after(gen_id, wait, cx);
            } else if hovered {
                self.error_dismiss_task = None;
            }
        } else if let Some((gen_id, wait)) = self.notice_banner_timer.set_hovered(hovered, now) {
            self.schedule_notice_dismiss_after(gen_id, wait, cx);
        } else if hovered {
            self.notice_dismiss_task = None;
        }
    }

    pub fn show_workspace_error(
        &mut self,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.workspace_error = Some(message.into());
        let (gen_id, wait) = self.error_banner_timer.start(Instant::now());
        self.schedule_error_dismiss_after(gen_id, wait, cx);
        cx.notify();
    }

    pub fn show_top_notice(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.top_notice = Some(message.into());
        let (gen_id, wait) = self.notice_banner_timer.start(Instant::now());
        self.schedule_notice_dismiss_after(gen_id, wait, cx);
        cx.notify();
    }

    fn cycle_theme_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next_mode = match self.theme_preference {
            ThemeModeDto::System => ThemeModeDto::Light,
            ThemeModeDto::Light => ThemeModeDto::Dark,
            ThemeModeDto::Dark => ThemeModeDto::System,
        };
        self.theme_preference = next_mode;
        apply_desktop_theme(next_mode, Some(window), None, cx);
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client
                        .patch_desktop_settings(DesktopSettingsPatchDto {
                            theme: Some(next_mode),
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

    fn maybe_check_startup_missing_key_and_updates(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 1. Startup 24h shared missing-key prompt check when entering Home with 0 ready models
        if !self.startup_missing_key_checked
            && self.route == ShellRoute::Home
            && let Some(models) = self.snapshot.models.as_ref()
        {
            self.startup_missing_key_checked = true;
            if !models.any_model_configured() {
                let client = self.client.clone();
                cx.spawn_in(window, async move |weak, cx| {
                    if let Ok(claimed) = client.claim_missing_key_prompt().await
                        && claimed.claimed
                    {
                        let _ = weak.update_in(cx, |this, window, cx| {
                            if this.missing_key_modal.is_none()
                                && this.route == ShellRoute::Home
                                && this
                                    .snapshot
                                    .models
                                    .as_ref()
                                    .is_some_and(|m| !m.any_model_configured())
                            {
                                this.missing_key_modal = Some(MissingKeyModalState {
                                    provider_label: None,
                                    return_session_id: None,
                                    prior_focus: window.focused(cx),
                                });
                                this.focus_missing_key(window, cx);
                                cx.notify();
                            }
                        });
                    }
                })
                .detach();
            }
        }

        // 2. Auto-surface UpdateDialog when a new `Available` version arrives, or auto-launch when `Ready`
        if let Some(ref us) = self.snapshot.update_state {
            match us.status.phase {
                UpdatePhaseDto::Available => {
                    let ver = us.status.version.clone().unwrap_or_default();
                    if !ver.is_empty() && self.last_prompted_update_version.as_deref() != Some(&ver)
                    {
                        self.last_prompted_update_version = Some(ver);
                        self.open_update_dialog(window, cx);
                    }
                }
                UpdatePhaseDto::Ready if self.update_dialog.auto_launch_when_ready => {
                    self.trigger_update_launch_handoff(window, cx);
                }
                _ => {}
            }
        }
    }

    fn spawn_client_revision_listener(&self, window: &mut Window, cx: &mut Context<Self>) {
        let mut rev_rx = self.client.subscribe_revisions();
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            while rev_rx.changed().await.is_ok() {
                let latest_snap = client.snapshot();
                let keep_going = weak.update_in(cx, |this, window, cx| {
                    let prev_mode = this.snapshot.channel_mode;
                    let prev_theme = this.theme_preference;
                    let prev_update_phase =
                        this.snapshot.update_state.as_ref().map(|u| u.status.phase);
                    let prev_pref_collapsed = this.snapshot.desktop_settings.sidebar_collapsed;
                    let was_corrupt = this.snapshot.desktop_settings_corrupt;
                    this.snapshot = latest_snap;
                    let pref_collapsed = this.snapshot.desktop_settings.sidebar_collapsed;
                    // A revision can arrive while SSE is offline, e.g. from an HTTP request.
                    // Ignore older echoes while a local manual preference is being saved.
                    let local_pref_collapsed =
                        this.sidebar_collapsed && !this.auto_collapsed_narrow;
                    if pref_collapsed != prev_pref_collapsed
                        && pref_collapsed != local_pref_collapsed
                        && this.sidebar_save_task.is_none()
                    {
                        this.auto_collapsed_narrow =
                            !pref_collapsed && this.sidebar_viewport_narrow;
                        this.set_sidebar_collapsed(
                            pref_collapsed || this.auto_collapsed_narrow,
                            window,
                            cx,
                        );
                    }
                    if this.snapshot.desktop_settings_corrupt && !was_corrupt {
                        this.show_top_notice("桌面配置文件格式异常，已保留原文件。", cx);
                    }
                    this.theme_preference = this.snapshot.desktop_settings.theme;

                    if prev_theme != this.theme_preference {
                        apply_desktop_theme(this.theme_preference, Some(window), None, cx);
                    }
                    if prev_mode != this.snapshot.channel_mode {
                        if this.snapshot.channel_mode == ChannelModeDto::WebActive {
                            if this.exit_dialog.is_none() {
                                this.web_lock_prior_focus = window.focused(cx);
                                this.web_lock_switch_focus.focus(window, cx);
                            }
                        } else if prev_mode == ChannelModeDto::WebActive {
                            let prior = this.web_lock_prior_focus.take();
                            if this.exit_dialog.is_none() {
                                this.restore_surface_focus(prior, window, cx);
                            }
                        }
                    }
                    this.maybe_check_startup_missing_key_and_updates(window, cx);
                    let phase = this.snapshot.update_state.as_ref().map(|u| u.status.phase);
                    if prev_update_phase != phase
                        && this.update_dialog.open
                        && this.exit_dialog.is_none()
                        && this.web_switch_confirm.is_none()
                        && this.snapshot.channel_mode != ChannelModeDto::WebActive
                    {
                        this.restore_surface_focus(None, window, cx);
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
        let Some(is_narrow) = viewport_is_narrow(window).filter(|_| window.is_visible()) else {
            return;
        };
        // Work only at breakpoint crossings. A manual expansion in a narrow window
        // survives moves, resizes within the same range and minimize/restore.
        if is_narrow != self.sidebar_viewport_narrow {
            self.sidebar_viewport_narrow = is_narrow;
            if is_narrow && !self.sidebar_collapsed {
                self.auto_collapsed_narrow = true;
                self.set_sidebar_collapsed(true, window, cx);
            } else if !is_narrow && self.auto_collapsed_narrow {
                self.auto_collapsed_narrow = false;
                self.set_sidebar_collapsed(false, window, cx);
            }
        }
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
            .observe(bounds, window.is_maximized(), false, matched_hint)
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

    pub fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_none() && self.blocks_workspace_shortcuts(cx) {
            return;
        }
        if let Some(palette_ent) = self.command_palette.take() {
            self.command_palette_sub = None;
            if let Some(pf) = palette_ent.read(cx).prior_focus() {
                pf.focus(window, cx);
            }
            cx.notify();
            return;
        }
        let prior = window.focused(cx);
        let sessions = self.snapshot.sessions.as_ref().clone();
        let has_active_session = self.route == ShellRoute::Session
            && self
                .client
                .with_workspace(|ws| ws.active_session_id().is_some());
        let modal =
            cx.new(|cx| CommandPaletteModal::new(sessions, has_active_session, prior, window, cx));
        let sub = cx.subscribe_in(&modal, window, Self::on_command_palette_event);
        self.command_palette = Some(modal);
        self.command_palette_sub = Some(sub);
        cx.notify();
    }

    fn on_command_palette_event(
        &mut self,
        palette_ent: &Entity<CommandPaletteModal>,
        event: &CommandPaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.as_ref() != Some(palette_ent)
            || self.exit_dialog.is_some()
            || self.update_dialog.open
            || self.missing_key_modal.is_some()
            || self.web_switch_confirm.is_some()
            || self.snapshot.channel_mode == ChannelModeDto::WebActive
        {
            return;
        }
        self.command_palette = None;
        self.command_palette_sub = None;
        self.restore_surface_focus(palette_ent.read(cx).prior_focus(), window, cx);
        if let CommandPaletteEvent::Execute(target) = event {
            self.execute_palette_target(target.clone(), window, cx);
        }
        cx.notify();
    }

    fn execute_palette_target(
        &mut self,
        target: PaletteCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active_sid = self
            .client
            .with_workspace(|ws| ws.active_session_id().map(str::to_owned));
        match target {
            PaletteCommandTarget::SlashExport => {
                if let Some(sid) = active_sid {
                    self.open_session_export(sid, window, cx);
                } else {
                    self.show_top_notice("请先进入一个会话后再执行导出。", cx);
                }
            }
            PaletteCommandTarget::SlashTree => {
                if let Some(sid) = active_sid {
                    self.open_session_tree(sid, window, cx);
                } else {
                    self.show_top_notice("请先进入一个会话后再查看分支树。", cx);
                }
            }
            PaletteCommandTarget::SlashCompact => {
                if let Some(sid) = active_sid {
                    let client = self.client.clone();
                    cx.spawn(async move |weak, cx| {
                        let res = client.compact_session(sid.clone(), None).await;
                        let _ = client.activate_session(Some(sid)).await;
                        let _ = weak.update(cx, |this, cx| match res {
                            Ok(_) => this.show_top_notice("会话上下文已完成压缩。", cx),
                            Err(e) => this.show_workspace_error(e.to_string(), cx),
                        });
                    })
                    .detach();
                } else {
                    self.show_top_notice("请先进入一个会话后再压缩上下文。", cx);
                }
            }
            PaletteCommandTarget::SlashFork => {
                if let Some(sid) = active_sid {
                    let latest_user_eid = self.snapshot.loaded_sessions.get(&sid).and_then(|s| {
                        s.messages.iter().rev().find_map(|m| {
                            let msg = m.get("message").unwrap_or(m);
                            (msg.get("role").and_then(serde_json::Value::as_str) == Some("user"))
                                .then(|| {
                                    m.get("id")
                                        .or_else(|| msg.get("id"))
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned)
                                })
                                .flatten()
                        })
                    });
                    if let Some(eid) = latest_user_eid {
                        let client = self.client.clone();
                        cx.spawn(async move |weak, cx| {
                            let res = client.fork_session(sid, eid).await;
                            let _ = client.refresh_globals().await;
                            let _ = weak.update(cx, |this, cx| match res {
                                Ok(new_sid) if !new_sid.is_empty() => {
                                    this.activate_session_by_id(new_sid, cx);
                                }
                                Ok(_) => {}
                                Err(e) => this.show_workspace_error(e.to_string(), cx),
                            });
                        })
                        .detach();
                    }
                } else {
                    self.show_top_notice("请先进入一个会话后再派生新分支。", cx);
                }
            }
            PaletteCommandTarget::OpenSession(sid) => {
                self.activate_session_by_id(sid, cx);
            }
            PaletteCommandTarget::NewSession => {
                self.set_route(ShellRoute::Home, cx);
                self.sidebar_batch_mode = false;
                self.sidebar_selected_ids.clear();
                let c = self.client.clone();
                cx.background_executor()
                    .spawn(async move {
                        let _ = c.activate_session(None).await;
                    })
                    .detach();
                cx.notify();
            }
            PaletteCommandTarget::OpenSettings => {
                self.set_route(ShellRoute::Settings, cx);
                cx.notify();
            }
            PaletteCommandTarget::OpenWeb => {
                self.trigger_open_web(false, window, cx);
            }
        }
    }

    fn open_session_tree(&mut self, sid: String, window: &mut Window, cx: &mut Context<Self>) {
        let prior = window.focused(cx);
        self.set_workspace_route(ShellRoute::Session, SessionSubView::Tree, cx);
        self.session_subview_prior_focus = prior;
        self.branch_tree_view.update(cx, |tv, cx| {
            tv.set_session_id(sid, window, cx);
            tv.focus_handle(cx).focus(window, cx);
        });
    }

    fn open_session_export(&mut self, sid: String, window: &mut Window, cx: &mut Context<Self>) {
        let prior = window.focused(cx);
        self.set_workspace_route(ShellRoute::Session, SessionSubView::Export, cx);
        self.session_subview_prior_focus = prior;
        self.export_view
            .update(cx, |view, cx| view.set_session_id(sid, cx));
        self.root_focus.focus(window, cx);
    }

    fn dismiss_sidebar_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.sidebar_context_menu.take().is_none() {
            return false;
        }
        let prior = self
            .sidebar_menu_prior_focus
            .take()
            .or_else(|| Some(self.sidebar_collapse_focus.clone()));
        self.restore_surface_focus(prior, window, cx);
        cx.notify();
        true
    }

    fn close_sidebar_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(modal) = self.rename_modal.take() else {
            return false;
        };
        self.restore_surface_focus(modal.prior_focus, window, cx);
        cx.notify();
        true
    }

    fn submit_sidebar_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = &self.rename_modal else {
            return;
        };
        if crate::sessions::input_is_composing(&modal.input, window, cx) {
            return;
        }
        let target = modal.session_id.clone();
        let title = modal.input.read(cx).value().trim().to_owned();
        if self
            .all_sessions_view
            .update(cx, |view, cx| view.rename_session(target, title, cx))
        {
            self.close_sidebar_rename(window, cx);
        }
    }

    fn set_sidebar_collapsed(
        &mut self,
        collapsed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if collapsed == self.sidebar_collapsed {
            return;
        }
        self.sidebar_collapsed = collapsed;
        let focus_disappears = if collapsed {
            let search_focus = self.sidebar_search_input.read(cx).focus_handle(cx);
            self.sidebar_collapse_focus.is_focused(window)
                || self.sidebar_account_focus.is_focused(window)
                || search_focus.contains_focused(window, cx)
                || self.sidebar_menu_focus.contains_focused(window, cx)
        } else {
            self.topbar_expand_focus.is_focused(window)
                || self.topbar_recent_focus.is_focused(window)
                || self.topbar_account_focus.is_focused(window)
        };
        self.recent_drawer_open = false;
        self.recent_drawer_prior_focus = None;
        self.sidebar_context_menu = None;
        self.sidebar_menu_prior_focus = None;
        self.sidebar_batch_mode = false;
        self.sidebar_selected_ids.clear();
        if focus_disappears && self.route != ShellRoute::Login {
            self.sidebar_toggle_focus().focus(window, cx);
        }
        cx.notify();
    }

    fn sidebar_toggle_focus(&self) -> &FocusHandle {
        if self.sidebar_collapsed {
            &self.topbar_expand_focus
        } else {
            &self.sidebar_collapse_focus
        }
    }

    pub fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next_collapsed = !self.sidebar_collapsed;
        self.auto_collapsed_narrow = false;
        self.set_sidebar_collapsed(next_collapsed, window, cx);
        if self.route != ShellRoute::Login {
            self.sidebar_toggle_focus().focus(window, cx);
        }

        // Serialize writes to the same setting. Coalesce intervening clicks while
        // a PATCH is outstanding, and keep the newest local choice on screen.
        self.sidebar_pending_save = Some(next_collapsed);
        if self.sidebar_save_task.is_some() {
            return;
        }
        let client = self.client.clone();
        self.sidebar_save_task = Some(cx.spawn(async move |weak, cx| {
            loop {
                let Ok(Some(collapsed)) =
                    weak.update(cx, |this, _| this.sidebar_pending_save.take())
                else {
                    break;
                };
                let result = client
                    .patch_desktop_settings(DesktopSettingsPatchDto {
                        sidebar_collapsed: Some(collapsed),
                        ..DesktopSettingsPatchDto::default()
                    })
                    .await;
                let done = weak.update(cx, |this, cx| {
                    if this.sidebar_pending_save.is_some() {
                        return false;
                    }
                    this.sidebar_save_task = None;
                    if let Err(err) = result {
                        this.show_workspace_error(err.to_string(), cx);
                    }
                    true
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
        }));
    }

    /// Closes ONLY the single topmost open surface and restores focus within 1 frame.
    pub fn dismiss_topmost_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.shutting_down {
            return true;
        }
        if let Some(dialog) = self.exit_dialog.take() {
            self.restore_surface_focus(dialog.prior_focus, window, cx);
            cx.notify();
            return true;
        }
        if self.snapshot.channel_mode == ChannelModeDto::WebActive || self.update_handoff_active() {
            return true;
        }
        if let Some(confirm) = self.web_switch_confirm.take() {
            self.restore_surface_focus(confirm.prior_focus, window, cx);
            cx.notify();
            return true;
        }
        if self.update_dialog.open {
            self.update_dialog.open = false;
            self.update_dialog.auto_launch_when_ready = false;
            self.update_dialog.busy_error = None;
            let prior = self.update_dialog.prior_focus.take();
            self.restore_surface_focus(prior, window, cx);
            cx.notify();
            return true;
        }
        if let Some(mk) = self.missing_key_modal.take() {
            self.restore_surface_focus(mk.prior_focus, window, cx);
            cx.notify();
            return true;
        }
        if self.close_sidebar_rename(window, cx) {
            return true;
        }
        if let Some(palette_ent) = self.command_palette.take() {
            self.command_palette_sub = None;
            if let Some(pf) = palette_ent.read(cx).prior_focus() {
                pf.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        if self.account_menu_open {
            self.close_account_menu(window, cx);
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
        if self.dismiss_sidebar_menu(window, cx) {
            return true;
        }
        if self.route == ShellRoute::AllSessions
            && self
                .all_sessions_view
                .update(cx, |av, cx| av.dismiss_batch_mode(window, cx))
        {
            return true;
        }
        if self.route == ShellRoute::Session
            && self.session_subview == SessionSubView::Tree
            && self
                .branch_tree_view
                .update(cx, |view, cx| view.dismiss_summary(window, cx))
        {
            return true;
        }
        if self.sidebar_batch_mode {
            self.sidebar_batch_mode = false;
            self.sidebar_selected_ids.clear();
            self.sidebar_collapse_focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.route == ShellRoute::Session && self.session_subview != SessionSubView::Chat {
            self.return_to_chat(window, cx);
            return true;
        }
        if self.route == ShellRoute::Account
            && self
                .account_view
                .update(cx, |av, cx| av.dismiss_logout_confirm(window, cx))
        {
            return true;
        }
        if self.route == ShellRoute::Login
            && self
                .login_view
                .update(cx, |lv, cx| lv.dismiss_dropdown(window, cx))
        {
            return true;
        }
        if self.route == ShellRoute::Settings
            && self
                .settings_view
                .update(cx, |sv, cx| sv.dismiss_topmost_surface(window, cx))
        {
            return true;
        }
        if matches!(self.route, ShellRoute::Home | ShellRoute::Session)
            && self
                .workbench
                .update(cx, |wb, cx| wb.dismiss_topmost_surface(window, cx))
        {
            return true;
        }
        if !self.sidebar_collapsed && !self.sidebar_search_input.read(cx).value().is_empty() {
            self.sidebar_search_input
                .update(cx, |st, cx| st.set_value("", window, cx));
            cx.notify();
            return true;
        }
        false
    }

    fn trigger_update_prepare(&mut self, cx: &mut Context<Self>) {
        if !self.update_dialog.open
            || self.update_dialog.request_pending
            || self.update_handoff_active()
        {
            return;
        }
        self.update_dialog.request_pending = true;
        self.update_dialog.busy_error = None;
        self.update_dialog.auto_launch_when_ready = true;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.prepare_update().await;
            let _ = client.refresh_globals().await;
            let _ = weak.update(cx, |this, cx| {
                this.update_dialog.request_pending = false;
                if let Err(err) = res {
                    this.update_dialog.auto_launch_when_ready = false;
                    this.update_dialog.busy_error = Some(SharedString::from(err.to_string()));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_update_cancel(&mut self, cx: &mut Context<Self>) {
        if self.update_dialog.request_pending || self.update_handoff_active() {
            return;
        }
        self.update_dialog.request_pending = true;
        self.update_dialog.auto_launch_when_ready = false;
        self.update_dialog.busy_error = None;
        let op_id = self
            .snapshot
            .update_state
            .as_ref()
            .and_then(|u| u.status.operation_id.clone());
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.cancel_update(op_id.as_deref()).await;
            let _ = client.refresh_globals().await;
            let _ = weak.update(cx, |this, cx| {
                this.update_dialog.request_pending = false;
                if let Err(err) = res {
                    this.show_workspace_error(err.to_string(), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_update_launch_handoff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.update_dialog.open
            || self.exit_check_pending
            || self.exit_dialog.is_some()
            || self.shutting_down
            || self.update_handoff_active()
            || self.snapshot.channel_mode == ChannelModeDto::WebActive
            || self.web_switch_confirm.is_some()
        {
            return;
        }
        self.update_dialog.auto_launch_when_ready = false;
        self.update_dialog.launch_pending = true;
        self.update_dialog.busy_error = None;
        self.update_trap_focus.focus(window, cx);
        cx.notify();
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let res = client.launch_update("desktop").await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.update_dialog.launch_pending = false;
                match res {
                    Ok(_) => {
                        // Receipt received: lock UI with `"正在重启完成更新…"` and immediately close window & quit!
                        this.update_dialog.open = true;
                        this.update_dialog.handoff_locked = true;
                        this.allow_immediate_close = true;
                        cx.notify();
                        window.remove_window();
                        cx.quit();
                    }
                    Err(ClientError::Conflict { message, .. }) => {
                        this.update_dialog.open = true;
                        this.update_dialog.busy_error = Some(SharedString::from(message));
                        this.restore_surface_focus(None, window, cx);
                        cx.notify();
                    }
                    Err(err) => {
                        this.update_dialog.open = true;
                        this.update_dialog.busy_error = Some(SharedString::from(err.to_string()));
                        this.restore_surface_focus(None, window, cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn request_close_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.exit_dialog.is_some()
            || self.exit_check_pending
            || self.shutting_down
            || self.update_handoff_active()
        {
            return;
        }
        self.exit_check_pending = true;
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let check_res = cx
                .background_executor()
                .spawn({
                    let c = client.clone();
                    async move { c.check_exit().await }
                })
                .await;

            let report = check_res.unwrap_or_else(|_| {
                // A failed lifecycle query must not bypass confirmation for local drafts.
                let local = client.check_web_switch();
                ExitCheckReport {
                    running_sessions: 0,
                    pending_server_inbox: 0,
                    active_exports: 0,
                    unresolved_local_outbox: local.unresolved_local_outbox,
                    unsent_drafts: local.unsent_drafts,
                }
            });
            let _ = weak.update_in(cx, |this, window, cx| {
                this.exit_check_pending = false;
                if report.requires_confirmation() {
                    this.exit_dialog = Some(ExitDialogModalState {
                        report,
                        exiting: false,
                        prior_focus: window.focused(cx),
                    });
                    this.exit_cancel_focus.focus(window, cx);
                    cx.notify();
                } else {
                    this.execute_final_shutdown(window, cx);
                }
            });
        })
        .detach();
    }

    fn execute_final_shutdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shutting_down || self.update_handoff_active() {
            return;
        }
        self.shutting_down = true;
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
        if self.web_transition_pending || self.shutting_down || self.update_handoff_active() {
            return;
        }
        if !force && self.client.check_web_switch().requires_confirmation() {
            if self.web_switch_confirm.is_some() {
                return;
            }
            self.web_switch_confirm = Some(WebSwitchConfirmState {
                prior_focus: window.focused(cx),
            });
            cx.notify();
            return;
        }
        if let Some(confirm) = self.web_switch_confirm.take() {
            self.restore_surface_focus(confirm.prior_focus, window, cx);
            cx.notify();
        }
        self.web_transition_pending = true;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.open_web().await })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.web_transition_pending = false;
                match res {
                    Ok(opened) => {
                        this.last_web_url = Some(opened.url.clone());
                        open_url_in_trusted_browser(&opened.url);
                        cx.notify();
                    }
                    Err(err) => this.show_workspace_error(err.to_string(), cx),
                }
            });
        })
        .detach();
    }

    fn trigger_close_web(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.web_transition_pending
            || self.shutting_down
            || self.snapshot.channel_mode != ChannelModeDto::WebActive
        {
            return;
        }
        self.web_transition_pending = true;
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.close_web().await })
                .await;
            let _ = weak.update_in(cx, |this, _, cx| {
                this.web_transition_pending = false;
                if let Err(err) = res {
                    this.show_workspace_error(err.to_string(), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn activate_session_by_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.set_route(ShellRoute::Session, cx);
        self.recent_drawer_open = false;
        self.sidebar_context_menu = None;
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

    fn return_after_credentials_saved(&mut self, cx: &mut Context<Self>) {
        if let Some(target_opt) = self.pending_return_session_id.take() {
            match target_opt {
                Some(sid) => {
                    // Return to original session WITHOUT auto-resending!
                    self.activate_session_by_id(sid, cx);
                }
                None => {
                    self.set_route(ShellRoute::Home, cx);
                    cx.notify();
                }
            }
        }
    }

    fn on_workbench_event(
        &mut self,
        _wb: &Entity<WorkbenchView>,
        event: &WorkbenchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkbenchEvent::ActivatedSession(sid) => {
                self.activate_session_by_id(sid.clone(), cx);
            }
            WorkbenchEvent::MissingCredentials { provider } => {
                let active_sid = self
                    .client
                    .with_workspace(|ws| ws.active_session_id().map(str::to_owned));
                if self.missing_key_modal.is_none() {
                    self.missing_key_modal = Some(MissingKeyModalState {
                        provider_label: Some(provider.clone()),
                        return_session_id: active_sid,
                        prior_focus: window.focused(cx),
                    });
                    self.focus_missing_key(window, cx);
                    cx.notify();
                }
            }
            WorkbenchEvent::OpenSlashRoute(target) => {
                let active_sid = self
                    .client
                    .with_workspace(|ws| ws.active_session_id().map(str::to_owned));
                match target {
                    SlashNavigationTarget::Tree => {
                        if let Some(sid) = active_sid {
                            self.open_session_tree(sid, window, cx);
                        }
                    }
                    SlashNavigationTarget::Export => {
                        if let Some(sid) = active_sid {
                            self.open_session_export(sid, window, cx);
                        }
                    }
                    SlashNavigationTarget::ForkLatest => {}
                }
            }
            WorkbenchEvent::OpenAccountPage => {
                self.set_route(ShellRoute::Account, cx);
                cx.notify();
            }
            WorkbenchEvent::WorkspaceError(msg) => {
                self.show_workspace_error(msg.clone(), cx);
            }
            WorkbenchEvent::TopNotice(msg) => {
                self.show_top_notice(msg.clone(), cx);
            }
        }
    }

    fn on_all_sessions_event(
        &mut self,
        _av: &Entity<AllSessionsView>,
        event: &AllSessionsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AllSessionsEvent::OpenSession(sid) => {
                self.activate_session_by_id(sid.clone(), cx);
                self.root_focus.focus(window, cx);
            }
            AllSessionsEvent::SessionDeleted(sid) => {
                if self
                    .client
                    .with_workspace(|ws| ws.active_session_id() == Some(sid.as_str()))
                {
                    if self.route == ShellRoute::Session {
                        self.set_route(ShellRoute::Home, cx);
                        self.root_focus.focus(window, cx);
                    }
                    let client = self.client.clone();
                    let sid = sid.clone();
                    cx.spawn(async move |_, _| {
                        if client.with_workspace(|ws| ws.active_session_id() == Some(sid.as_str()))
                        {
                            let _ = client.activate_session(None).await;
                        }
                    })
                    .detach();
                }
            }
            AllSessionsEvent::Notice(msg) => {
                self.show_top_notice(msg.clone(), cx);
            }
            AllSessionsEvent::ErrorMessage(msg) => {
                self.show_workspace_error(msg.clone(), cx);
            }
        }
    }

    fn on_branch_tree_event(
        &mut self,
        _tv: &Entity<BranchTreeView>,
        event: &BranchTreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            BranchTreeEvent::BackToChat => {
                if self.route == ShellRoute::Session && self.session_subview == SessionSubView::Tree
                {
                    self.return_to_chat(window, cx);
                }
            }
            BranchTreeEvent::SwitchToForkedSession(sid) => {
                self.activate_session_by_id(sid.clone(), cx);
                self.root_focus.focus(window, cx);
            }
            BranchTreeEvent::PrefillComposer { session_id, text } => {
                if self.route == ShellRoute::Session
                    && self.session_subview == SessionSubView::Chat
                    && self
                        .client
                        .with_workspace(|ws| ws.active_session_id() == Some(session_id.as_str()))
                {
                    self.workbench
                        .update(cx, |wb, cx| wb.prefill_session_composer(text, window, cx));
                }
            }
            BranchTreeEvent::SummarizingStarted { session_id } => {
                self.branch_summarizing_sessions.insert(session_id.clone());
                self.return_to_chat(window, cx);
            }
            BranchTreeEvent::SummarizingFinished { session_id } => {
                self.branch_summarizing_sessions.remove(session_id);
                cx.notify();
            }
            BranchTreeEvent::ErrorMessage(msg) => {
                self.show_workspace_error(msg.clone(), cx);
            }
        }
    }

    fn on_export_view_event(
        &mut self,
        _ev: &Entity<ExportView>,
        event: &ExportViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ExportViewEvent::BackToChat => self.return_to_chat(window, cx),
            ExportViewEvent::Notice(msg) => {
                self.show_top_notice(msg.clone(), cx);
            }
            ExportViewEvent::ErrorMessage(msg) => {
                self.show_workspace_error(msg.clone(), cx);
            }
        }
    }

    fn on_settings_view_event(
        &mut self,
        _sv: &Entity<SettingsView>,
        event: &SettingsViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.route != ShellRoute::Settings {
            return;
        }
        match event {
            SettingsViewEvent::OpenUpdateDialog => {
                self.open_update_dialog(window, cx);
            }
            SettingsViewEvent::CancelUpdate => {
                self.trigger_update_cancel(cx);
            }
            SettingsViewEvent::OpenLoginOrAccount { signed_in } => {
                self.set_route(
                    if *signed_in {
                        ShellRoute::Account
                    } else {
                        ShellRoute::Login
                    },
                    cx,
                );
                cx.notify();
            }
            SettingsViewEvent::CredentialsSaved => {
                self.return_after_credentials_saved(cx);
            }
            SettingsViewEvent::Notice(msg) => {
                self.show_top_notice(msg.clone(), cx);
            }
            SettingsViewEvent::ErrorMessage(msg) => {
                self.show_workspace_error(msg.clone(), cx);
            }
        }
    }

    fn on_login_view_event(
        &mut self,
        _lv: &Entity<LoginView>,
        event: &LoginViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.route != ShellRoute::Login {
            return;
        }
        match event {
            LoginViewEvent::NavigateToAccount => {
                self.set_route(ShellRoute::Account, cx);
                cx.notify();
            }
            LoginViewEvent::NavigateToCustomGatewaySettings => {
                self.set_route(ShellRoute::Settings, cx);
                self.settings_view
                    .update(cx, |sv, cx| sv.open_custom_gateway_creator(window, cx));
                cx.notify();
            }
            LoginViewEvent::CredentialsSavedReturn => {
                if self.pending_return_session_id.is_some() {
                    self.return_after_credentials_saved(cx);
                } else {
                    self.set_route(ShellRoute::Home, cx);
                    cx.notify();
                }
            }
            LoginViewEvent::ErrorMessage(msg) => {
                self.show_workspace_error(msg.clone(), cx);
            }
        }
    }

    fn on_account_view_event(
        &mut self,
        _av: &Entity<AccountView>,
        event: &AccountViewEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.route != ShellRoute::Account {
            return;
        }
        match event {
            AccountViewEvent::OpenLoginTab(tab) => {
                let target_tab = *tab;
                self.set_route(ShellRoute::Login, cx);
                self.login_view
                    .update(cx, |lv, cx| lv.set_tab(target_tab, cx));
                cx.notify();
            }
            AccountViewEvent::LoggedOutCstoa => {
                self.show_top_notice("已退出 CSTOA 登录，其余模型服务凭据保持不变。", cx);
                self.set_route(ShellRoute::Login, cx);
                cx.notify();
            }
            AccountViewEvent::ErrorMessage(msg) => {
                self.show_workspace_error(msg.clone(), cx);
            }
        }
    }

    fn render_workspace_body(
        &self,
        palette: &DesktopPalette,
        entity: &Entity<Self>,
    ) -> gpui_kit::AnyElement {
        match self.route {
            ShellRoute::Home => self.workbench.clone().into_any_element(),
            ShellRoute::Session => match self.session_subview {
                SessionSubView::Tree => self.branch_tree_view.clone().into_any_element(),
                SessionSubView::Export => self.export_view.clone().into_any_element(),
                SessionSubView::Chat => {
                    let active_session_id = self
                        .client
                        .with_workspace(|ws| ws.active_session_id().map(str::to_owned));
                    if let Some(sum_sid) = active_session_id
                        .filter(|sid| self.branch_summarizing_sessions.contains(sid))
                    {
                        let sid_abort = sum_sid.clone();
                        let ent_abort = entity.clone();
                        div()
                            .size_full()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("branch-summarizing-banner")
                                    .test_support()
                                    .a11y_status(
                                        "branch-summarizing-banner",
                                        "正在生成所选分支的上下文总结",
                                    )
                                    .w_full()
                                    .h(height::CONTROL)
                                    .px(space::GROUP)
                                    .bg(palette.bg_surface)
                                    .border_b_1()
                                    .border_color(palette.border_subtle)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        apply_scale(div(), scale::CAPTION_MEDIUM)
                                            .text_color(palette.accent_solid)
                                            .child("正在生成所选分支的上下文总结…"),
                                    )
                                    .child(
                                        apply_scale(div(), scale::CAPTION_MEDIUM)
                                            .id("branch-summarizing-cancel-btn")
                                            .test_support()
                                            .a11y_button(
                                                "branch-summarizing-cancel-btn",
                                                "取消总结",
                                            )
                                            .h(height::INLINE)
                                            .px(space::SM)
                                            .rounded(radius::CONTROL)
                                            .text_color(palette.status_danger)
                                            .flex()
                                            .items_center()
                                            .on_click(move |_, _, cx| {
                                                let s = sid_abort.clone();
                                                ent_abort.update(cx, |this, cx| {
                                                    this.branch_summarizing_sessions.remove(&s);
                                                    let c = this.client.clone();
                                                    cx.spawn(async move |_, _| {
                                                        let _ = c.abort_session(s).await;
                                                    })
                                                    .detach();
                                                    cx.notify();
                                                });
                                            })
                                            .child(cjk_inline_label("取消总结")),
                                    ),
                            )
                            .child(div().flex_1().min_h_0().child(self.workbench.clone()))
                            .into_any_element()
                    } else {
                        self.workbench.clone().into_any_element()
                    }
                }
            },
            ShellRoute::AllSessions => self.all_sessions_view.clone().into_any_element(),
            ShellRoute::Settings => self.settings_view.clone().into_any_element(),
            ShellRoute::Login => self.login_view.clone().into_any_element(),
            ShellRoute::Account => self.account_view.clone().into_any_element(),
        }
    }

    /// Context menu `UWt8w` (`200px` wide, `4px` padding, `8px` radius, `H2p2z` frosted glass).
    #[allow(clippy::too_many_arguments)]
    fn render_sidebar_context_menu(
        &self,
        sid: &str,
        anchor_pos: Point<Pixels>,
        viewport_size: gpui_kit::Size<Pixels>,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        entity: &Entity<Self>,
    ) -> impl IntoElement {
        let sid_rename = sid.to_owned();
        let sid_del = sid.to_owned();
        let ent_rename = entity.clone();
        let ent_manage = entity.clone();
        let ent_del = entity.clone();
        let ent_backdrop = entity.clone();

        let est_h = if self.sidebar_batch_mode { 40.0 } else { 116.0 };
        let raw_x = f32::from(anchor_pos.x);
        let raw_y = f32::from(anchor_pos.y);
        let max_x = (f32::from(viewport_size.width) - 208.0).max(8.0);
        let max_y = (f32::from(viewport_size.height) - est_h - 8.0).max(8.0);
        let menu_x = if raw_x <= 0.5 && raw_y <= 0.5 {
            84.0
        } else {
            raw_x.clamp(8.0, max_x)
        };
        let menu_y = if raw_x <= 0.5 && raw_y <= 0.5 {
            160.0
        } else {
            raw_y.clamp(8.0, max_y)
        };

        let mut menu_box = render_frosted_surface(
            "sidebar-session-context-menu",
            FrostedGlassSpec::popover_h2p2z(palette),
            palette.bg_elevated,
            transparency_enabled,
            scale_factor,
        )
        .test_support()
        .role(gpui::Role::Menu)
        .accessibility_id("sidebar-session-context-menu")
        .aria_label("会话右键菜单")
        .w(px(200.0))
        .p(space::XS)
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_hairline)
        .flex()
        .flex_col()
        .gap(space::MICRO)
        .track_focus(&self.sidebar_menu_focus)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());

        // In batch mode, right-click menu converges to single `"删除"`!
        if !self.sidebar_batch_mode {
            menu_box = menu_box
                .child(
                    apply_scale(div(), scale::UI)
                        .id("sidebar-ctx-rename")
                        .test_support()
                        .a11y_menuitem("sidebar-ctx-rename", "重命名")
                        .relative()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, window, cx| {
                            let target = sid_rename.clone();
                            ent_rename.update(cx, |this, cx| {
                                let prior = this
                                    .sidebar_menu_prior_focus
                                    .take()
                                    .or_else(|| Some(this.sidebar_collapse_focus.clone()));
                                this.sidebar_context_menu = None;
                                let Some(session) = this
                                    .all_sessions_view
                                    .read(cx)
                                    .visible_sessions()
                                    .into_iter()
                                    .find(|session| session.id == target)
                                else {
                                    this.restore_surface_focus(prior, window, cx);
                                    cx.notify();
                                    return;
                                };
                                let input = cx.new(|cx| {
                                    let mut st =
                                        InputState::new(window, cx).placeholder("输入新会话标题…");
                                    st.set_value(session.title, window, cx);
                                    st.focus(window, cx);
                                    st
                                });
                                this.rename_modal = Some(RenameSessionModalState {
                                    session_id: target,
                                    input,
                                    prior_focus: prior,
                                });
                                cx.notify();
                            });
                        })
                        .child(
                            Icon::new(IconName::Pencil)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("重命名")),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .id("sidebar-ctx-manage")
                        .test_support()
                        .a11y_menuitem("sidebar-ctx-manage", "管理")
                        .relative()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .hover(|s| s.bg(palette.bg_item_active))
                        .on_click(move |_, window, cx| {
                            ent_manage.update(cx, |this, cx| {
                                this.dismiss_sidebar_menu(window, cx);
                                this.sidebar_batch_mode = true;
                                this.sidebar_selected_ids.clear();
                                cx.notify();
                            });
                        })
                        .child(
                            Icon::new(IconName::ListChecks)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("管理")),
                )
                .child(
                    div()
                        .relative()
                        .px(space::XS)
                        .py(space::MICRO)
                        .child(div().w_full().h(px(1.0)).bg(palette.border_subtle)),
                );
        }

        menu_box = menu_box.child(
            apply_scale(div(), scale::UI)
                .id("sidebar-ctx-delete")
                .test_support()
                .a11y_menuitem("sidebar-ctx-delete", "删除")
                .relative()
                .h(height::CONTROL)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .text_color(palette.status_danger)
                .flex()
                .items_center()
                .gap(space::SM)
                .hover(|s| s.bg(palette.status_danger_subtle))
                .on_click(move |_, window, cx| {
                    ent_del.update(cx, |this, cx| {
                        this.dismiss_sidebar_menu(window, cx);
                        this.all_sessions_view.update(cx, |view, cx| {
                            view.delete_sessions_optimistic(vec![sid_del.clone()], cx)
                        });
                    });
                })
                .child(
                    Icon::new(IconName::Trash)
                        .size(px(16.0))
                        .text_color(palette.status_danger),
                )
                .child(cjk_inline_label("删除")),
        );

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                ent_backdrop.update(cx, |this, cx| {
                    this.dismiss_sidebar_menu(window, cx);
                });
            })
            .child(div().absolute().top(px(menu_y)).left(px(menu_x)).child(
                motion::animate_popover_enter(
                    "sidebar-ctx-menu-anim",
                    menu_box,
                    palette.craft_stack_shadows(),
                    -4.0,
                ),
            ))
    }

    fn render_recent_drawer_popover(
        &self,
        active_session_id: Option<&str>,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        entity: &Entity<Self>,
    ) -> impl IntoElement {
        let mut sorted: Vec<_> = self.snapshot.sessions.as_ref().clone();
        sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        if sorted.len() > MAX_RECENT_SIDEBAR_SESSIONS {
            sorted.truncate(MAX_RECENT_SIDEBAR_SESSIONS);
        }

        let mut rows = div()
            .id("recent-drawer-rows")
            .relative()
            .max_h(px(320.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(space::MICRO);

        if sorted.is_empty() {
            rows = rows.child(
                apply_scale(div(), scale::UI)
                    .px(space::SM)
                    .py(space::MD)
                    .text_color(palette.text_muted)
                    .child("暂无历史对话"),
            );
        } else {
            for sess in &sorted {
                let sid = sess.id.clone();
                let is_active = matches!(self.route, ShellRoute::Session)
                    && active_session_id == Some(sid.as_str());
                let title = if sess.title.trim().is_empty() {
                    SharedString::from("新对话")
                } else {
                    SharedString::from(sess.title.clone())
                };
                let drawer_row_id = SharedString::from(format!("drawer-session-{sid}"));
                let ent = entity.clone();
                rows = rows.child(
                    apply_scale(
                        div(),
                        if is_active {
                            scale::UI_MEDIUM
                        } else {
                            scale::UI
                        },
                    )
                    .id(drawer_row_id.clone())
                    .test_support()
                    .a11y_listitem(drawer_row_id, title.clone(), is_active)
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .bg(if is_active {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .text_color(if is_active {
                        palette.text_primary
                    } else {
                        palette.text_secondary
                    })
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                    .on_click(move |_, _, cx| {
                        let target = sid.clone();
                        ent.update(cx, |this, cx| this.activate_session_by_id(target, cx));
                    })
                    .child(div().w_full().truncate().child(cjk_inline_label(title))),
                );
            }
        }

        let ent_all = entity.clone();
        let ent_backdrop = entity.clone();

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                ent_backdrop.update(cx, |this, cx| {
                    this.recent_drawer_open = false;
                    cx.notify();
                });
            })
            .child(
                div().absolute().top(px(44.0)).left(px(148.0)).child(
                    motion::animate_popover_enter(
                        "recent-drawer-anim",
                        render_frosted_surface(
                            "recent-sessions-drawer",
                            FrostedGlassSpec::popover_h2p2z(palette),
                            palette.bg_elevated,
                            transparency_enabled,
                            scale_factor,
                        )
                        .test_support()
                        .a11y_dialog("recent-sessions-drawer", "最近会话抽屉")
                        .w(px(264.0))
                        .p(space::XS)
                        .rounded(radius::ROW)
                        .border_1()
                        .border_color(palette.border_hairline)
                        .flex()
                        .flex_col()
                        .gap(space::MICRO)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(rows)
                        .child(
                            div()
                                .relative()
                                .px(space::XS)
                                .py(space::MICRO)
                                .child(div().w_full().h(px(1.0)).bg(palette.border_subtle)),
                        )
                        .child(
                            apply_scale(div(), scale::UI)
                                .id("drawer-open-all-sessions-btn")
                                .test_support()
                                .a11y_button("drawer-open-all-sessions-btn", "查看全部会话")
                                .relative()
                                .h(height::CONTROL)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .hover(|s| s.bg(palette.bg_item_active))
                                .on_click(move |_, _, cx| {
                                    ent_all.update(cx, |this, cx| {
                                        this.recent_drawer_open = false;
                                        this.set_route(ShellRoute::AllSessions, cx);
                                        cx.notify();
                                    });
                                })
                                .child(
                                    Icon::new(IconName::RotateCcwClock)
                                        .size(px(16.0))
                                        .text_color(palette.text_secondary),
                                )
                                .child(cjk_inline_label("查看全部会话")),
                        ),
                        palette.craft_stack_shadows(),
                        -6.0,
                    ),
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
        let ent_theme = entity.clone();
        let ent_exit = entity.clone();
        let ent_backdrop = entity.clone();

        let (theme_icon, theme_label) = match self.theme_preference {
            ThemeModeDto::System => (IconName::SunMoon, "跟随系统"),
            ThemeModeDto::Light => (IconName::Sun, "浅色"),
            ThemeModeDto::Dark => (IconName::Moon, "深色"),
        };

        let pos_div = if self.sidebar_collapsed {
            div().absolute().top(px(44.0)).right(px(136.0))
        } else {
            div().absolute().bottom(px(50.0)).left(space::LG)
        };

        let account_identity = self
            .snapshot
            .account
            .as_ref()
            .filter(|a| a.signed_in)
            .map(|a| {
                let name = a.profile.name.as_deref().unwrap_or("科协队员");
                match a.profile.student_id.as_deref().filter(|s| !s.is_empty()) {
                    Some(sid) => format!("{name} · {sid}"),
                    None => name.to_owned(),
                }
            });

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                ent_backdrop.update(cx, |this, cx| {
                    this.close_account_menu(window, cx);
                });
            })
            .child(
                pos_div.child(motion::animate_popover_enter(
                    "account-popover-anim",
                    render_frosted_surface(
                        "account-popover-menu",
                        FrostedGlassSpec::popover_h2p2z(palette),
                        palette.bg_elevated,
                        transparency_enabled,
                        scale_factor,
                    )
                    .test_support()
                    .role(gpui::Role::Menu)
                    .accessibility_id("account-popover-menu")
                    .aria_label("账号与偏好菜单")
                    .w(px(224.0))
                    .p(space::XS)
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_hairline)
                    .flex()
                    .flex_col()
                    .gap(space::MICRO)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(account_identity.map(|identity_label| {
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("account-menu-identity-header")
                            .test_support()
                            .w_full()
                            .px(space::SM)
                            .py(space::XS)
                            .text_color(palette.text_primary)
                            .truncate()
                            .child(cjk_inline_label(identity_label))
                    }))
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("account-menu-profile-btn")
                            .test_support()
                            .a11y_menuitem(
                                "account-menu-profile-btn",
                                if logged_in {
                                    "专属账号"
                                } else {
                                    "登录模型服务"
                                },
                            )
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .hover(|s| s.bg(palette.bg_item_active))
                            .on_click(move |_, _, cx| {
                                ent_auth.update(cx, |this, cx| {
                                    this.account_menu_open = false;
                                    this.set_route(
                                        if logged_in {
                                            ShellRoute::Account
                                        } else {
                                            ShellRoute::Login
                                        },
                                        cx,
                                    );
                                    cx.notify();
                                });
                            })
                            .child(
                                Icon::new(IconName::User)
                                    .size(px(15.0))
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
                            .a11y_menuitem("account-menu-settings-btn", "设置")
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .hover(|s| s.bg(palette.bg_item_active))
                            .on_click(move |_, _, cx| {
                                ent_set.update(cx, |this, cx| {
                                    this.account_menu_open = false;
                                    this.set_route(ShellRoute::Settings, cx);
                                    cx.notify();
                                });
                            })
                            .child(
                                Icon::new(IconName::Settings)
                                    .size(px(15.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("设置")),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("account-menu-theme-btn")
                            .test_support()
                            .a11y_menuitem("account-menu-theme-btn", format!("主题：{theme_label}"))
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::SM)
                            .hover(|s| s.bg(palette.bg_item_active))
                            .on_click(move |_, window, cx| {
                                ent_theme.update(cx, |this, cx| {
                                    this.cycle_theme_mode(window, cx);
                                });
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(space::SM)
                                    .child(
                                        Icon::new(theme_icon)
                                            .size(px(15.0))
                                            .text_color(palette.text_secondary),
                                    )
                                    .child(cjk_inline_label("主题")),
                            )
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_muted)
                                    .child(cjk_inline_label(theme_label)),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .px(space::XS)
                            .py(space::MICRO)
                            .child(div().w_full().h(px(1.0)).bg(palette.border_subtle)),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("account-menu-exit-btn")
                            .test_support()
                            .a11y_menuitem("account-menu-exit-btn", "退出 CST Pilot")
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.status_danger)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .hover(|s| s.bg(palette.status_danger_subtle))
                            .on_click(move |_, window, cx| {
                                ent_exit.update(cx, |this, cx| {
                                    this.close_account_menu(window, cx);
                                    this.request_close_flow(window, cx);
                                });
                            })
                            .child(
                                Icon::new(IconName::LogOut)
                                    .size(px(15.0))
                                    .text_color(palette.status_danger),
                            )
                            .child(cjk_inline_label("退出 CST Pilot")),
                    ),
                    palette.craft_stack_shadows(),
                    if self.sidebar_collapsed { -6.0 } else { 6.0 },
                )),
            )
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();
        let viewport_size = window.viewport_size();

        let sidebar_sessions = self.all_sessions_view.read(cx).visible_sessions();
        self.sidebar_selected_ids
            .retain(|id| sidebar_sessions.iter().any(|session| &session.id == id));
        if self
            .sidebar_context_menu
            .as_ref()
            .is_some_and(|(id, _)| !sidebar_sessions.iter().any(|session| &session.id == id))
        {
            self.dismiss_sidebar_menu(window, cx);
        }
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

        let wb_session_id = match self.route {
            ShellRoute::Session => active_session_id.clone(),
            _ => None,
        };
        self.workbench.update(cx, |wb, cx| {
            wb.sync_route_and_sky(wb_session_id, sky_time, window, cx);
        });
        if self.route == ShellRoute::Login {
            self.login_view
                .update(cx, |lv, _| lv.set_sky_time(sky_time));
        }
        if let Some(ref cp) = self.command_palette {
            let sess_vec = self.snapshot.sessions.as_ref().clone();
            let has_act = self.route == ShellRoute::Session && active_session_id.is_some();
            cp.update(cx, |m, _| m.sync_context(sess_vec, has_act));
        }

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

        let entity = cx.entity();

        let main_layout = if self.route == ShellRoute::Login {
            // Full-window bleed mode for `ShellRoute::Login` (`1280×800` `tM7YQ` / `jiQRy`):
            // hides sidebar & regular `GElS8` topbar so left `864px` sky shader + right login viewport breathe freely.
            let e_close = entity.clone();
            let e_back = entity.clone();

            let mut login_shell = div()
                .relative()
                .size_full()
                .bg(palette.bg_workspace)
                .child(self.login_view.clone())
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(layout::TOPBAR_HEIGHT)
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .id("login-topbar-drag-region")
                                .window_control_area(WindowControlArea::Drag)
                                .flex_1()
                                .h_full(),
                        )
                        .child(render_window_caption_controls(
                            window.is_maximized(),
                            &palette,
                            Rc::new(|w, _| w.minimize_window()),
                            Rc::new(|w, _| w.zoom_window()),
                            Rc::new(move |w, cx| {
                                e_close.update(cx, |this, cx| this.request_close_flow(w, cx));
                            }),
                        )),
                )
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("login-window-back-btn")
                        .test_support()
                        .a11y_button("login-window-back-btn", "返回工作台")
                        .tab_index(0)
                        .absolute()
                        .left(space::SM)
                        .top(space::TIGHT)
                        .h(height::COMPACT)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .gap(space::XS)
                        .text_color(palette.text_primary)
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(move |_, _, cx| {
                            e_back.update(cx, |this, cx| {
                                this.set_route(ShellRoute::Home, cx);
                                cx.notify();
                            });
                        })
                        .child(Icon::new(IconName::ArrowLeft).size(px(14.0)))
                        .child(cjk_inline_label("返回")),
                );

            if let Some(ref err_msg) = self.workspace_error {
                let ent_hover = entity.clone();
                login_shell = login_shell.child(render_workspace_error_bar(
                    err_msg.clone(),
                    self.error_banner_timer.generation(),
                    &palette,
                    transparency_enabled,
                    scale_factor,
                    move |hovered, _, cx| {
                        let hov = *hovered;
                        ent_hover.update(cx, |this, cx| this.set_banner_hovered(true, hov, cx));
                    },
                ));
            } else if let Some(ref notice) = self.top_notice {
                let ent_hover = entity.clone();
                login_shell = login_shell.child(render_top_notice_banner(
                    notice.clone(),
                    self.notice_banner_timer.generation(),
                    &palette,
                    transparency_enabled,
                    scale_factor,
                    move |hovered, _, cx| {
                        let hov = *hovered;
                        ent_hover.update(cx, |this, cx| this.set_banner_hovered(false, hov, cx));
                    },
                ));
            }
            login_shell
        } else {
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
                    self.recent_drawer_open,
                    active_view,
                    window.is_maximized(),
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
                                this.set_route(ShellRoute::Home, cx);
                                this.sidebar_batch_mode = false;
                                this.sidebar_selected_ids.clear();
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
                                this.sidebar_batch_mode = false;
                                this.sidebar_selected_ids.clear();
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
                                this.set_route(ShellRoute::Settings, cx);
                                cx.notify();
                            });
                        }),
                        on_minimize_window: Rc::new(|w, _| w.minimize_window()),
                        on_zoom_window: Rc::new(|w, _| w.zoom_window()),
                        on_request_close: Rc::new(move |w, cx| {
                            e9.update(cx, |this, cx| this.request_close_flow(w, cx));
                        }),
                    },
                    window,
                    cx,
                )
            };

            let mut workspace_card = div()
                .id("workspace-container")
                .test_support()
                .a11y_region("workspace-container", "工作区主视图")
                .relative()
                .size_full()
                .rounded(radius::ROW)
                .bg(palette.bg_workspace)
                .shadow(palette.workspace_shadows())
                .overflow_hidden()
                .child(self.render_workspace_body(&palette, &entity));

            if let Some(ref err_msg) = self.workspace_error {
                let ent_hover = entity.clone();
                workspace_card = workspace_card.child(render_workspace_error_bar(
                    err_msg.clone(),
                    self.error_banner_timer.generation(),
                    &palette,
                    transparency_enabled,
                    scale_factor,
                    move |hovered, _, cx| {
                        let hov = *hovered;
                        ent_hover.update(cx, |this, cx| this.set_banner_hovered(true, hov, cx));
                    },
                ));
            } else if let Some(ref notice) = self.top_notice {
                let ent_hover = entity.clone();
                workspace_card = workspace_card.child(render_top_notice_banner(
                    notice.clone(),
                    self.notice_banner_timer.generation(),
                    &palette,
                    transparency_enabled,
                    scale_factor,
                    move |hovered, _, cx| {
                        let hov = *hovered;
                        ent_hover.update(cx, |this, cx| this.set_banner_hovered(false, hov, cx));
                    },
                ));
            }

            // Non-interactive 1px inner border ring painted above rounded sky shader surfaces
            workspace_card = workspace_card.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_subtle),
            );

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

            let mut row_layout = div()
                .flex()
                .flex_row()
                .items_stretch()
                .size_full()
                .bg(palette.bg_shell);

            if !self.sidebar_collapsed {
                let search_query = self.sidebar_search_input.read(cx).value().to_string();
                let s1 = entity.clone();
                let s2 = entity.clone();
                let s_clr = entity.clone();
                let s_pal = entity.clone();
                let s3 = entity.clone();
                let s4 = entity.clone();
                let s5 = entity.clone();
                let s5_menu = entity.clone();
                let s7 = entity.clone();
                let s8 = entity.clone();
                let s9 = entity.clone();
                let s10 = entity.clone();
                let s11 = entity.clone();

                row_layout = row_layout.child(
                    div()
                        .h_full()
                        .flex()
                        .flex_shrink_0()
                        .child(render_sidebar_l081m1(
                            self.route,
                            active_session_id.as_deref(),
                            &sidebar_sessions,
                            window,
                            cx,
                            &self.sidebar_search_input,
                            &search_query,
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
                                        this.set_route(ShellRoute::Home, cx);
                                        this.sidebar_batch_mode = false;
                                        this.sidebar_selected_ids.clear();
                                        let c = this.client.clone();
                                        cx.background_executor()
                                            .spawn(async move {
                                                let _ = c.activate_session(None).await;
                                            })
                                            .detach();
                                        cx.notify();
                                    });
                                }),
                                on_clear_search: Rc::new(move |w, cx| {
                                    s_clr.update(cx, |this, cx| {
                                        this.sidebar_search_input.update(cx, |st, cx| {
                                            st.set_value("", w, cx);
                                            st.focus(w, cx);
                                        });
                                        cx.notify();
                                    });
                                }),
                                on_open_palette: Rc::new(move |w, cx| {
                                    s_pal.update(cx, |this, cx| this.toggle_command_palette(w, cx));
                                }),
                                on_open_all_sessions: Rc::new(move |window, cx| {
                                    s3.update(cx, |this, cx| {
                                        this.sidebar_batch_mode = false;
                                        this.sidebar_selected_ids.clear();
                                        this.set_route(ShellRoute::AllSessions, cx);
                                        this.all_sessions_view
                                            .update(cx, |view, cx| view.focus(window, cx));
                                    });
                                }),
                                on_open_dashboard: Rc::new(move |_, cx| {
                                    s4.update(cx, |this, cx| {
                                        this.sidebar_batch_mode = false;
                                        this.sidebar_selected_ids.clear();
                                        this.show_top_notice(
                                            "本地用量仪表盘将在后续版本开放。",
                                            cx,
                                        );
                                    });
                                }),
                                on_select_session: Rc::new(move |sid, window, cx| {
                                    s5.update(cx, |this, cx| {
                                        this.activate_session_by_id(sid, cx);
                                        this.root_focus.focus(window, cx);
                                    });
                                }),
                                on_open_session_menu: Rc::new(move |sid, pos, window, cx| {
                                    s5_menu.update(cx, |this, cx| {
                                        if this.sidebar_context_menu.is_none() {
                                            this.sidebar_menu_prior_focus = window.focused(cx);
                                        }
                                        this.sidebar_context_menu = Some((sid, pos));
                                        this.sidebar_menu_focus.focus(window, cx);
                                        cx.notify();
                                    });
                                }),
                                on_toggle_select_all: Rc::new(move |_, cx| {
                                    s7.update(cx, |this, cx| {
                                        let q =
                                            this.sidebar_search_input.read(cx).value().to_string();
                                        let recent_ids: Vec<String> =
                                            filter_recent_sidebar_sessions(
                                                &this.all_sessions_view.read(cx).visible_sessions(),
                                                &q,
                                            )
                                            .into_iter()
                                            .map(|s| s.id)
                                            .collect();
                                        if !recent_ids.is_empty()
                                            && recent_ids
                                                .iter()
                                                .all(|id| this.sidebar_selected_ids.contains(id))
                                        {
                                            this.sidebar_selected_ids.clear();
                                        } else {
                                            this.sidebar_selected_ids =
                                                recent_ids.into_iter().collect();
                                        }
                                        cx.notify();
                                    });
                                }),
                                on_batch_delete: Rc::new(move |window, cx| {
                                    s8.update(cx, |this, cx| {
                                        if !this.sidebar_batch_mode
                                            || this.sidebar_selected_ids.is_empty()
                                        {
                                            return;
                                        }
                                        let targets =
                                            std::mem::take(&mut this.sidebar_selected_ids)
                                                .into_iter()
                                                .collect();
                                        this.sidebar_batch_mode = false;
                                        this.sidebar_collapse_focus.focus(window, cx);
                                        this.all_sessions_view.update(cx, |view, cx| {
                                            view.delete_sessions_optimistic(targets, cx)
                                        });
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
                                        this.set_route(ShellRoute::Settings, cx);
                                        cx.notify();
                                    });
                                }),
                            },
                        ))
                        .with_animation(
                            "sidebar-drawer-enter",
                            Animation::new(motion::NORMAL).with_easing(motion::ease_out()),
                            |el, d| {
                                let t = d.clamp(0.0, 1.0);
                                el.opacity(0.01 + 0.99 * t).ml(px(-24.0 * (1.0 - t)))
                            },
                        ),
                );
            }

            row_layout.child(right_column)
        };

        let mut root = div()
            .id("cst-app-shell")
            .test_support()
            .role(gpui::Role::Application)
            .accessibility_id("cst-app-shell")
            .aria_label("CST Pilot")
            .key_context(SHELL_KEY_CONTEXT)
            .track_focus(&self.root_focus)
            .relative()
            .size_full()
            .font_family(theme_global.ui_font_family)
            .text_color(palette.text_primary)
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                if this.blocks_workspace_shortcuts(cx) {
                    return;
                }
                this.toggle_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewSession, _, cx| {
                if this.blocks_workspace_shortcuts(cx) {
                    return;
                }
                this.set_route(ShellRoute::Home, cx);
                this.sidebar_batch_mode = false;
                this.sidebar_selected_ids.clear();
                let c = this.client.clone();
                cx.background_executor()
                    .spawn(async move {
                        let _ = c.activate_session(None).await;
                    })
                    .detach();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                if this.blocks_workspace_shortcuts(cx) {
                    return;
                }
                this.set_route(ShellRoute::Settings, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DismissTopSurface, window, cx| {
                let _ = this.dismiss_topmost_surface(window, cx);
            }))
            .on_action(cx.listener(|this, _: &RequestAppExit, window, cx| {
                this.request_close_flow(window, cx);
            }))
            .child(main_layout);

        if self.route != ShellRoute::Login {
            if let Some((ref menu_sid, menu_pos)) = self.sidebar_context_menu {
                root = root.child(self.render_sidebar_context_menu(
                    menu_sid,
                    menu_pos,
                    viewport_size,
                    &palette,
                    transparency_enabled,
                    scale_factor,
                    &entity,
                ));
            }

            if self.recent_drawer_open && self.sidebar_collapsed {
                root = root.child(self.render_recent_drawer_popover(
                    active_session_id.as_deref(),
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
        }

        // `Ctrl+K` `520px` Command Palette (`I70mz`)
        if let Some(ref cp_modal) = self.command_palette {
            root = root.child(cp_modal.clone());
        }

        // Rename Session Modal (`384px`, `r: 8px`, `p: 20px`, `gap: 12px`)
        if let Some(ref rm) = self.rename_modal {
            let ent_cancel = entity.clone();
            root = root.child(
                render_frosted_surface(
                    "rename-session-scrim",
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
                .child(motion::animate_modal_enter(
                    "rename-session-modal-anim",
                    render_frosted_surface(
                        "rename-session-modal",
                        FrostedGlassSpec::modal_k3x72(&palette, 8.0),
                        palette.bg_modal,
                        transparency_enabled,
                        scale_factor,
                    )
                    .test_support()
                    .a11y_dialog("rename-session-modal", "重命名对话")
                    .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                        if let Some(modal) = &this.rename_modal {
                            crate::sessions::consume_composing_escape(&modal.input, window, cx);
                        }
                    }))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        if event.keystroke.key == "enter"
                            && !event.is_held
                            && this.rename_modal.as_ref().is_some_and(|modal| {
                                modal.input.read(cx).focus_handle(cx).is_focused(window)
                            })
                        {
                            this.submit_sidebar_rename(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .w(layout::MODAL_STANDARD_WIDTH)
                    .p(px(20.0))
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .relative()
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("重命名对话")),
                    )
                    .child(
                        div().relative().child(
                            Input::new(&rm.input)
                                .accessibility_id("rename-session-input")
                                .aria_label("新会话标题"),
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
                                    .id("rename-session-cancel-btn")
                                    .test_support()
                                    .a11y_button("rename-session-cancel-btn", "取消")
                                    .tab_index(0)
                                    .h(height::CONTROL)
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_strong)
                                    .bg(palette.bg_elevated)
                                    .flex()
                                    .items_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(move |_, w, cx| {
                                        ent_cancel.update(cx, |this, cx| {
                                            this.close_sidebar_rename(w, cx);
                                        });
                                    })
                                    .child(cjk_inline_label("取消")),
                            )
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .id("rename-session-confirm-btn")
                                    .test_support()
                                    .a11y_button("rename-session-confirm-btn", "保存")
                                    .tab_index(0)
                                    .h(height::CONTROL)
                                    .px(space::LG)
                                    .rounded(radius::CONTROL)
                                    .bg(palette.accent_solid)
                                    .text_color(palette.text_on_accent)
                                    .flex()
                                    .items_center()
                                    .hover(|s| s.bg(palette.accent_hover))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_sidebar_rename(window, cx)
                                    }))
                                    .child(cjk_inline_label("保存")),
                            ),
                    )
                    .tab_stop(false)
                    .focus_trap("sidebar-rename-focus-trap", &self.rename_trap_focus),
                    palette.craft_stack_shadows(),
                )),
            );
        }

        // `384px` Missing Key Dialog (`e7fEty`)
        if let Some(ref mk_state) = self.missing_key_modal {
            let ent_cancel = entity.clone();
            let ent_cfg = entity.clone();
            root = root.child(render_missing_key_dialog(
                mk_state,
                &palette,
                transparency_enabled,
                scale_factor,
                &self.missing_key_trap_focus,
                &self.missing_key_cancel_focus,
                move |w, cx| {
                    ent_cancel.update(cx, |this, cx| {
                        let _ = this.dismiss_topmost_surface(w, cx);
                    });
                },
                move |w, cx| {
                    ent_cfg.update(cx, |this, cx| {
                        let Some(taken) = this.missing_key_modal.take() else {
                            return;
                        };
                        this.pending_return_session_id = Some(taken.return_session_id);
                        this.root_focus.focus(w, cx);
                        this.set_route(ShellRoute::Settings, cx);
                        this.settings_view.update(cx, |sv, cx| {
                            sv.set_category(SettingsCategory::Providers, cx);
                        });
                        cx.notify();
                    });
                },
            ));
        }

        // `384px` Software Update Dialog (`kOm7J` / `TqiZo`)
        if self.update_dialog.open || self.update_dialog.handoff_locked {
            let ent_later = entity.clone();
            let ent_prep = entity.clone();
            let ent_canc = entity.clone();
            let ent_lnch = entity.clone();
            root = root.child(render_update_dialog(
                &self.update_dialog,
                self.snapshot.update_state.as_deref(),
                mono_family,
                &palette,
                transparency_enabled,
                scale_factor,
                &self.update_trap_focus,
                &self.update_dismiss_focus,
                move |w, cx| {
                    ent_later.update(cx, |this, cx| {
                        let _ = this.dismiss_topmost_surface(w, cx);
                    });
                },
                move |_, cx| {
                    ent_prep.update(cx, |this, cx| this.trigger_update_prepare(cx));
                },
                move |_, cx| {
                    ent_canc.update(cx, |this, cx| this.trigger_update_cancel(cx));
                },
                move |w, cx| {
                    ent_lnch.update(cx, |this, cx| this.trigger_update_launch_handoff(w, cx));
                },
            ));
        }

        // Unsent draft confirmation before switching to Web mode (`384px`, `r: 8px`, `p: 20px`, `gap: 12px`)
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
                .child(motion::animate_modal_enter(
                    "web-switch-confirm-anim",
                    render_frosted_surface(
                        "web-switch-confirm-card",
                        FrostedGlassSpec::modal_k3x72(&palette, 8.0),
                        palette.bg_modal,
                        transparency_enabled,
                        scale_factor,
                    )
                    .test_support()
                    .a11y_dialog("web-switch-confirm-card", "切换到网页端确认")
                    .w(layout::MODAL_STANDARD_WIDTH)
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .p(px(20.0))
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .gap(space::SM)
                            .child(
                                apply_scale(div(), scale::BODY_SEMIBOLD)
                                    .text_color(palette.text_primary)
                                    .child(cjk_inline_label("切换到网页端？")),
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
                                    .a11y_button("web-switch-cancel-btn", "取消")
                                    .h(height::CONTROL)
                                    .px(space::MD)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_strong)
                                    .bg(palette.bg_elevated)
                                    .flex()
                                    .items_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
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
                                    .a11y_button("web-switch-confirm-btn", "继续切换")
                                    .h(height::CONTROL)
                                    .px(space::LG)
                                    .rounded(radius::CONTROL)
                                    .bg(palette.accent_solid)
                                    .text_color(palette.text_on_accent)
                                    .flex()
                                    .items_center()
                                    .hover(|s| s.bg(palette.accent_hover))
                                    .on_click(move |_, w, cx| {
                                        ent_proceed.update(cx, |this, cx| {
                                            this.trigger_open_web(true, w, cx);
                                        });
                                    })
                                    .child(cjk_inline_label("继续切换")),
                            ),
                    ),
                    palette.craft_stack_shadows(),
                )),
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

pub fn open_url_in_trusted_browser(url: &str) {
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

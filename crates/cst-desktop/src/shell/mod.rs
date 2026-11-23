pub mod exit_dialog;
mod menus;
pub mod overlays;
pub mod sidebar;
pub mod topbar;
pub mod window_bounds;

use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

use cst_client::{
    ChannelModeDto, ClientError, ClientHandle, ClientSnapshot, DesktopSettingsPatchDto,
    ExitCheckReport, ThemeModeDto, UpdatePhaseDto, WorkspaceView,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::base::motion::{Transition, TransitionState, transition};
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
    FrostedGlassSpec, ShaderFrameScheduler, SkyPreset, has_active_hardware_diagnostic,
    render_frosted_surface, render_sky_background_rounded,
};
use crate::palette::{CommandPaletteEvent, CommandPaletteModal, PaletteCommandTarget};
use crate::sessions::all_sessions::{AllSessionsEvent, AllSessionsView};
use crate::sessions::branch_tree::{BranchTreeEvent, BranchTreeView};
use crate::sessions::export_view::{ExportView, ExportViewEvent};
use crate::settings::update_dialog::{UpdateDialogState, render_update_dialog};
use crate::settings::{SettingsCategory, SettingsView, SettingsViewEvent};
use crate::shell::exit_dialog::render_exit_confirm_dialog;
use crate::shell::overlays::{
    BannerCountdownState, BannerDeadline, render_top_notice_banner, render_web_active_lock_overlay,
    render_workspace_error_bar,
};
use crate::shell::sidebar::{
    ShellRoute, SidebarCallbacks, filter_recent_sidebar_sessions, render_sidebar,
    render_sidebar_exiting,
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
use crate::workbench::{WorkbenchEvent, WorkbenchView, render_home_brand_arcs};

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
    route_history: Vec<(ShellRoute, SessionSubView, Option<String>)>,
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
    recent_btn_anchor_x: Rc<std::cell::Cell<f32>>,
    topbar_session_more_open: bool,
    topbar_session_more_prior_focus: Option<FocusHandle>,
    topbar_session_more_anchor_x: Rc<std::cell::Cell<f32>>,
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
    workspace_error: Option<SharedString>,
    top_notice: Option<SharedString>,
    error_banner_timer: BannerCountdownState,
    notice_banner_timer: BannerCountdownState,
    shader_scheduler: ShaderFrameScheduler,
    bounds_tracker: NormalBoundsTracker,
    cached_display_hint: Option<(gpui::DisplayId, String)>,
    bounds_dirty_generation: u64,
    bounds_debounce_task: Option<Task<()>>,
    error_dismiss_task: Option<Task<()>>,
    notice_dismiss_task: Option<Task<()>>,
    allow_immediate_close: bool,
    root_focus: FocusHandle,
    sidebar_collapse_focus: FocusHandle,
    topbar_expand_focus: FocusHandle,
    topbar_recent_focus: FocusHandle,
    topbar_session_more_focus: FocusHandle,
    sidebar_account_focus: FocusHandle,
    sidebar_settings_focus: FocusHandle,
    topbar_account_focus: FocusHandle,
    topbar_settings_focus: FocusHandle,
    web_lock_trap_focus: FocusHandle,
    web_lock_switch_focus: FocusHandle,
    exit_trap_focus: FocusHandle,
    exit_cancel_focus: FocusHandle,
    exit_confirm_focus: FocusHandle,
    tooltip_overlay: Entity<motion::DesktopTooltips>,
    route_transition: motion::RouteTransition,
    last_sky_time: f32,
    login_return_route: ShellRoute,
    sidebar_presence: TransitionState<f32>,
    account_presence: TransitionState<f32>,
    sidebar_menu_presence: TransitionState<f32>,
    last_sidebar_menu: Option<(String, Point<Pixels>)>,
    banner_presence: TransitionState<f32>,
    last_banner: Option<(bool, SharedString)>,
    tooltip_session: Option<String>,
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
        let topbar_session_more_focus = cx.focus_handle().tab_stop(true);
        let sidebar_account_focus = cx.focus_handle();
        let sidebar_settings_focus = cx.focus_handle().tab_stop(true);
        let topbar_account_focus = cx.focus_handle().tab_stop(true);
        let topbar_settings_focus = cx.focus_handle().tab_stop(true);
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

        if cx.has_global::<crate::OwnedClientGlobal>() {
            subscriptions.push(cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    crate::sync_system_reduce_motion(cx);
                }
            }));
        }

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

        let tooltip_overlay = motion::DesktopTooltips::install(window, cx);

        let mut shell = Self {
            client,
            snapshot: initial_snapshot,
            theme_preference,
            route: ShellRoute::Home,
            session_subview: SessionSubView::Chat,
            route_history: Vec::new(),
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
            recent_btn_anchor_x: Rc::new(std::cell::Cell::new(134.0)),
            topbar_session_more_open: false,
            topbar_session_more_prior_focus: None,
            topbar_session_more_anchor_x: Rc::new(std::cell::Cell::new(220.0)),
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
            workspace_error: None,
            top_notice: None,
            error_banner_timer: BannerCountdownState::new(),
            notice_banner_timer: BannerCountdownState::new(),
            shader_scheduler: ShaderFrameScheduler::new(),
            bounds_tracker: NormalBoundsTracker::from_resolved(resolved_geometry),
            cached_display_hint: resolved_geometry
                .display_id
                .zip(resolved_geometry.display_hint.clone()),
            bounds_dirty_generation: 0,
            bounds_debounce_task: None,
            error_dismiss_task: None,
            notice_dismiss_task: None,
            allow_immediate_close: false,
            root_focus: root_focus.clone(),
            sidebar_collapse_focus,
            topbar_expand_focus,
            topbar_recent_focus,
            topbar_session_more_focus,
            sidebar_account_focus,
            sidebar_settings_focus,
            topbar_account_focus,
            topbar_settings_focus,
            web_lock_trap_focus,
            web_lock_switch_focus,
            exit_trap_focus,
            exit_cancel_focus,
            exit_confirm_focus,
            tooltip_overlay,
            route_transition: motion::RouteTransition::default(),
            last_sky_time: 0.0,
            login_return_route: ShellRoute::Home,
            sidebar_presence: TransitionState::default(),
            account_presence: TransitionState::default(),
            sidebar_menu_presence: TransitionState::default(),
            last_sidebar_menu: None,
            banner_presence: TransitionState::default(),
            last_banner: None,
            tooltip_session: None,
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
        self.set_workspace_route_inner(route, subview, true, None, cx);
    }

    fn set_workspace_route_inner(
        &mut self,
        route: ShellRoute,
        subview: SessionSubView,
        record_history: bool,
        forced_transition: Option<motion::RouteTransitionKind>,
        cx: &mut Context<Self>,
    ) {
        if self.route != route || self.session_subview != subview {
            #[cfg(feature = "test-support")]
            crate::ux_probe::mark_input("route_switch");
            if record_history && self.route != ShellRoute::Login && route != ShellRoute::Login {
                let session_id = match (self.route, self.session_subview) {
                    (ShellRoute::Session, SessionSubView::Tree) => {
                        Some(self.branch_tree_view.read(cx).session_id().to_owned())
                    }
                    (ShellRoute::Session, SessionSubView::Export) => {
                        Some(self.export_view.read(cx).session_id().to_owned())
                    }
                    _ => None,
                };
                let prev = (self.route, self.session_subview, session_id);
                if self.route_history.last() != Some(&prev) {
                    self.route_history.push(prev);
                    if self.route_history.len() > 24 {
                        self.route_history.remove(0);
                    }
                }
            }
            let kind = forced_transition.unwrap_or_else(|| {
                match (self.route, self.session_subview, route, subview) {
                    (ShellRoute::Home, _, dst, _) if dst != ShellRoute::Login => {
                        motion::RouteTransitionKind::HomeToWorkspace
                    }
                    (src, _, ShellRoute::Home, _) if src != ShellRoute::Login => {
                        motion::RouteTransitionKind::WorkspaceToHome
                    }
                    (
                        ShellRoute::Session,
                        SessionSubView::Chat,
                        ShellRoute::Session,
                        SessionSubView::Tree | SessionSubView::Export,
                    )
                    | (
                        ShellRoute::Session,
                        SessionSubView::Chat,
                        ShellRoute::Settings | ShellRoute::AllSessions | ShellRoute::Account,
                        _,
                    )
                    | (ShellRoute::Settings, _, ShellRoute::Account, _) => {
                        motion::RouteTransitionKind::SlideForward
                    }
                    (
                        ShellRoute::Session,
                        SessionSubView::Tree | SessionSubView::Export,
                        ShellRoute::Session,
                        SessionSubView::Chat,
                    ) => motion::RouteTransitionKind::SlideBackward,
                    _ => motion::RouteTransitionKind::CrossFade,
                }
            });
            self.route_transition.start(kind, self.last_sky_time, cx);
            self.reset_account_menu();
            self.tooltip_overlay
                .update(cx, motion::DesktopTooltips::dismiss);
        }
        if self.route != route {
            if self.route == ShellRoute::Login || route == ShellRoute::Login {
                if route == ShellRoute::Login && self.route != ShellRoute::Login {
                    self.login_return_route = self.route;
                }
                self.sidebar_presence = TransitionState::default();
                self.set_banner_hovered(true, self.error_banner_timer.generation(), false, cx);
                self.set_banner_hovered(false, self.notice_banner_timer.generation(), false, cx);
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
                    .update(cx, |view, cx| view.refresh_cstoa_quota(cx));
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
            self.recent_drawer_open = false;
            self.recent_drawer_prior_focus = None;
            self.topbar_session_more_open = false;
            self.topbar_session_more_prior_focus = None;
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
        self.set_workspace_route_inner(
            ShellRoute::Session,
            SessionSubView::Chat,
            false,
            Some(motion::RouteTransitionKind::SlideBackward),
            cx,
        );
        self.restore_surface_focus(prior, window, cx);
    }

    fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_active_session = self
            .client
            .with_workspace(|ws| ws.active_session_id().is_some());
        let mut target = None;
        while let Some((prev_route, prev_subview, session_id)) = self.route_history.pop() {
            if prev_route == self.route && prev_subview == self.session_subview {
                continue;
            }
            if let Some(id) = &session_id {
                if !self
                    .snapshot
                    .sessions
                    .iter()
                    .any(|session| session.id == *id)
                {
                    continue;
                }
            } else if prev_route == ShellRoute::Session && !has_active_session {
                continue;
            }
            target = Some((prev_route, prev_subview, session_id));
            break;
        }
        let fallback = if has_active_session {
            (ShellRoute::Session, SessionSubView::Chat, None)
        } else {
            (ShellRoute::Home, SessionSubView::Chat, None)
        };
        let (dest_route, dest_subview, session_id) = target.unwrap_or(fallback);
        let transition_kind = if dest_route == ShellRoute::Home {
            motion::RouteTransitionKind::WorkspaceToHome
        } else {
            motion::RouteTransitionKind::SlideBackward
        };
        self.set_workspace_route_inner(dest_route, dest_subview, false, Some(transition_kind), cx);
        self.root_focus.focus(window, cx);
        if let Some(id) = session_id {
            self.initialize_session_subview(dest_subview, id, window, cx);
        }
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

    fn close_recent_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recent_drawer_open = false;
        let prior = self.recent_drawer_prior_focus.take();
        self.restore_surface_focus(prior, window, cx);
        cx.notify();
    }

    fn close_topbar_session_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.topbar_session_more_open = false;
        let prior = self.topbar_session_more_prior_focus.take();
        self.restore_surface_focus(prior, window, cx);
        cx.notify();
    }

    fn reset_account_menu(&mut self) {
        self.account_menu_open = false;
        self.account_menu_prior_focus = None;
        self.account_presence = TransitionState::default();
    }

    fn toggle_account_menu(
        &mut self,
        from_settings: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.account_menu_open {
            self.close_account_menu(window, cx);
        } else if !self.blocks_transient_surfaces(cx) {
            let trigger = match (self.sidebar_collapsed, from_settings) {
                (true, true) => &self.topbar_settings_focus,
                (true, false) => &self.topbar_account_focus,
                (false, true) => &self.sidebar_settings_focus,
                (false, false) => &self.sidebar_account_focus,
            };
            self.account_menu_prior_focus = Some(trigger.clone());
            trigger.focus(window, cx);
            self.account_menu_open = true;
            self.tooltip_overlay
                .update(cx, motion::DesktopTooltips::dismiss);
            cx.notify();
        }
    }

    fn close_account_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.account_menu_open = false;
        let prior = self.account_menu_prior_focus.take();
        self.restore_surface_focus(prior, window, cx);
        cx.notify();
    }

    fn blocks_workspace_shortcuts(&self, cx: &App) -> bool {
        self.account_menu_open || self.blocks_transient_surfaces(cx)
    }

    fn blocks_transient_surfaces(&self, cx: &App) -> bool {
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
    fn schedule_banner_dismiss(
        &mut self,
        is_error: bool,
        deadline: BannerDeadline,
        cx: &mut Context<Self>,
    ) {
        let task = cx.spawn(async move |weak, cx| {
            let executor = cx.background_executor();
            let wait = deadline.remaining_at(executor.now());
            executor.timer(wait).await;
            let _ = weak.update(cx, |this, cx| {
                let (timer, task, message) = if is_error {
                    (
                        &mut this.error_banner_timer,
                        &mut this.error_dismiss_task,
                        &mut this.workspace_error,
                    )
                } else {
                    (
                        &mut this.notice_banner_timer,
                        &mut this.notice_dismiss_task,
                        &mut this.top_notice,
                    )
                };
                if timer.expire(deadline, cx.background_executor().now()) {
                    *message = None;
                    *task = None;
                    cx.notify();
                }
            });
        });
        if is_error {
            self.error_dismiss_task = Some(task);
        } else {
            self.notice_dismiss_task = Some(task);
        }
    }

    fn set_banner_hovered(
        &mut self,
        is_error: bool,
        generation: u64,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if !is_error && self.workspace_error.is_some() {
            return;
        }
        let (timer, task) = if is_error {
            (&mut self.error_banner_timer, &mut self.error_dismiss_task)
        } else {
            (&mut self.notice_banner_timer, &mut self.notice_dismiss_task)
        };
        if timer.generation() != generation || timer.is_hovered() == hovered {
            return;
        }
        if let Some(deadline) = timer.set_hovered(hovered, cx.background_executor().now()) {
            self.schedule_banner_dismiss(is_error, deadline, cx);
        } else if timer.is_hovered() {
            *task = None;
        }
    }

    pub fn show_workspace_error(
        &mut self,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        // The error replaces the notice hitbox. Unmounting it emits no leave.
        self.set_banner_hovered(false, self.notice_banner_timer.generation(), false, cx);
        self.workspace_error = Some(message.into());
        let deadline = self
            .error_banner_timer
            .start(cx.background_executor().now());
        self.schedule_banner_dismiss(true, deadline, cx);
        cx.notify();
    }

    pub fn show_top_notice(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.top_notice = Some(message.into());
        let deadline = self
            .notice_banner_timer
            .start(cx.background_executor().now());
        self.schedule_banner_dismiss(false, deadline, cx);
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
        fn opt_arc_ptr_eq<T>(a: &Option<std::sync::Arc<T>>, b: &Option<std::sync::Arc<T>>) -> bool {
            match (a, b) {
                (None, None) => true,
                (Some(lhs), Some(rhs)) => std::sync::Arc::ptr_eq(lhs, rhs),
                _ => false,
            }
        }

        let mut rev_rx = self.client.subscribe_revisions();
        let client = self.client.clone();
        cx.spawn_in(window, async move |weak, cx| {
            while rev_rx.changed().await.is_ok() {
                let latest_snap = client.snapshot();
                let keep_going = weak.update_in(cx, |this, window, cx| {
                    let prev_snap = std::sync::Arc::clone(&this.snapshot);
                    let shell_globals_changed = prev_snap.connected != latest_snap.connected
                        || prev_snap.reconnecting != latest_snap.reconnecting
                        || prev_snap.channel_mode != latest_snap.channel_mode
                        || prev_snap.version != latest_snap.version
                        || prev_snap.desktop_settings != latest_snap.desktop_settings
                        || prev_snap.desktop_settings_corrupt
                            != latest_snap.desktop_settings_corrupt
                        || !std::sync::Arc::ptr_eq(&prev_snap.commands, &latest_snap.commands)
                        || !std::sync::Arc::ptr_eq(&prev_snap.sessions, &latest_snap.sessions)
                        || !opt_arc_ptr_eq(&prev_snap.models, &latest_snap.models)
                        || !opt_arc_ptr_eq(&prev_snap.auth_providers, &latest_snap.auth_providers)
                        || !opt_arc_ptr_eq(&prev_snap.account, &latest_snap.account)
                        || !opt_arc_ptr_eq(&prev_snap.update_state, &latest_snap.update_state)
                        || !opt_arc_ptr_eq(
                            &prev_snap.last_oauth_event,
                            &latest_snap.last_oauth_event,
                        );

                    let prev_mode = prev_snap.channel_mode;
                    let prev_theme = this.theme_preference;
                    let prev_update_phase = prev_snap.update_state.as_ref().map(|u| u.status.phase);
                    let prev_pref_collapsed = prev_snap.desktop_settings.sidebar_collapsed;
                    let was_corrupt = prev_snap.desktop_settings_corrupt;
                    this.snapshot = latest_snap;

                    // Fast path for live session streaming: when only `loaded_sessions` updated
                    // while viewing an active chat, notify only `MessageListView` (and
                    // `ComposerView` if queue/running/context changed) instead of repainting the
                    // entire `AppShell` sidebar, topbar, and overlays on every frame.
                    if !shell_globals_changed
                        && this.route == ShellRoute::Session
                        && this.session_subview == SessionSubView::Chat
                    {
                        let active_sid = this
                            .client
                            .with_workspace(|ws| ws.active_session_id().map(str::to_owned));
                        if this.workbench.read(cx).bound_session_id() != active_sid.as_deref() {
                            cx.notify();
                            return;
                        }
                        let composer_changed = active_sid.as_deref().is_some_and(|sid| {
                            let prev_s = prev_snap.loaded_sessions.get(sid);
                            let next_s = this.snapshot.loaded_sessions.get(sid);
                            match (prev_s, next_s) {
                                (Some(a), Some(b)) => {
                                    a.running != b.running
                                        || a.queue != b.queue
                                        || a.ui != b.ui
                                        || a.context_usage != b.context_usage
                                }
                                (None, None) => false,
                                _ => true,
                            }
                        });
                        let wb = this.workbench.read(cx);
                        let msg_list = wb.message_list().clone();
                        let sess_comp = composer_changed.then(|| wb.session_composer().clone());
                        msg_list.update(cx, |_, cx| cx.notify());
                        if let Some(comp) = sess_comp {
                            comp.update(cx, |_, cx| cx.notify());
                        }
                        return;
                    }

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
                            this.set_banner_hovered(
                                true,
                                this.error_banner_timer.generation(),
                                false,
                                cx,
                            );
                            this.set_banner_hovered(
                                false,
                                this.notice_banner_timer.generation(),
                                false,
                                cx,
                            );
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
            self.sidebar_presence = TransitionState::default();
            let account_open = self.account_menu_open;
            self.reset_account_menu();
            if account_open
                && self.route != ShellRoute::Login
                && !self.blocks_transient_surfaces(cx)
            {
                self.sidebar_toggle_focus().focus(window, cx);
            }

            if is_narrow && !self.sidebar_collapsed {
                self.auto_collapsed_narrow = true;
                self.set_sidebar_collapsed(true, window, cx);
            } else if !is_narrow && self.auto_collapsed_narrow {
                self.auto_collapsed_narrow = false;
                self.set_sidebar_collapsed(false, window, cx);
            }
        }
        let bounds = window.bounds();
        let current_display_id = window.display(cx).map(|d| d.id());
        let matched_hint = match (current_display_id, &self.cached_display_hint) {
            (Some(id), Some((cached_id, hint))) if id == *cached_id => Some(hint.clone()),
            _ => {
                let center_x = f32::from(bounds.origin.x) + f32::from(bounds.size.width) * 0.5;
                let center_y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) * 0.5;
                let resolved = collect_app_displays(cx)
                    .into_iter()
                    .find(|d| {
                        center_x >= d.work_x
                            && center_x <= d.work_x + d.work_width
                            && center_y >= d.work_y
                            && center_y <= d.work_y + d.work_height
                    })
                    .map(|d| d.hint);
                if let (Some(id), Some(hint)) = (current_display_id, resolved.as_ref()) {
                    self.cached_display_hint = Some((id, hint.clone()));
                }
                resolved
            }
        };

        if self
            .bounds_tracker
            .observe(bounds, window.is_maximized(), false, matched_hint)
        {
            self.bounds_dirty_generation = self.bounds_dirty_generation.wrapping_add(1);
            if self.bounds_debounce_task.is_none() {
                let client = self.client.clone();
                self.bounds_debounce_task = Some(cx.spawn(async move |weak, cx| {
                    loop {
                        let Ok(seen_generation) =
                            weak.update(cx, |this, _| this.bounds_dirty_generation)
                        else {
                            return;
                        };
                        cx.background_executor()
                            .timer(Duration::from_millis(GEOMETRY_DEBOUNCE_MS))
                            .await;
                        let Ok(outcome) = weak.update(cx, |this, _| {
                            if this.bounds_dirty_generation != seen_generation {
                                None
                            } else {
                                Some(this.bounds_tracker.take_pending_patch())
                            }
                        }) else {
                            return;
                        };
                        let Some(maybe_patch) = outcome else {
                            continue;
                        };
                        if let Some(patch) = maybe_patch {
                            let client_bg = client.clone();
                            let _ = cx
                                .background_executor()
                                .spawn(async move {
                                    let _ = client_bg.patch_desktop_settings(patch).await;
                                })
                                .await;
                        }
                        let Ok(should_continue) = weak.update(cx, |this, _| {
                            if this.bounds_dirty_generation != seen_generation {
                                true
                            } else {
                                this.bounds_debounce_task = None;
                                false
                            }
                        }) else {
                            return;
                        };
                        if !should_continue {
                            return;
                        }
                    }
                }));
            }
        }
    }

    pub fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_none() && self.blocks_workspace_shortcuts(cx) {
            return;
        }
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("command_palette");
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
                        let res = client.compact_session(sid, None).await;
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
                    let Some(sess) = self.snapshot.loaded_sessions.get(&sid) else {
                        return;
                    };
                    if sess.running {
                        self.show_top_notice("请等待当前回复完成后再分支会话。", cx);
                        return;
                    }
                    if let Some(eid) =
                        crate::workbench::message_list::latest_forkable_entry_id(sess)
                    {
                        let client = self.client.clone();
                        cx.spawn(async move |weak, cx| {
                            let res = client.fork_session(sid, eid).await;
                            let _ = client.refresh_globals().await;
                            let _ = weak.update(cx, |this, cx| match res {
                                Ok(new_sid) if !new_sid.is_empty() => {
                                    this.activate_session_by_id(new_sid, cx);
                                    this.show_top_notice("已从当前节点分支新会话", cx);
                                }
                                Ok(_) => {}
                                Err(e) => this.show_workspace_error(e.to_string(), cx),
                            });
                        })
                        .detach();
                    } else {
                        self.show_top_notice("当前会话尚无可用于分支的已完成回复。", cx);
                    }
                } else {
                    self.show_top_notice("请先进入一个会话后再分支新会话。", cx);
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
        self.initialize_session_subview(SessionSubView::Tree, sid, window, cx);
    }

    fn open_session_export(&mut self, sid: String, window: &mut Window, cx: &mut Context<Self>) {
        let prior = window.focused(cx);
        self.set_workspace_route(ShellRoute::Session, SessionSubView::Export, cx);
        self.session_subview_prior_focus = prior;
        self.initialize_session_subview(SessionSubView::Export, sid, window, cx);
    }

    fn initialize_session_subview(
        &mut self,
        subview: SessionSubView,
        sid: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match subview {
            SessionSubView::Tree => self.branch_tree_view.update(cx, |view, cx| {
                view.set_session_id(sid, window, cx);
                view.focus_handle(cx).focus(window, cx);
            }),
            SessionSubView::Export => {
                self.export_view
                    .update(cx, |view, cx| view.set_session_id(sid, cx));
                self.root_focus.focus(window, cx);
            }
            SessionSubView::Chat => {}
        }
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
        self.reset_account_menu();
        self.tooltip_overlay
            .update(cx, motion::DesktopTooltips::dismiss);
        let focus_disappears = if collapsed {
            let search_focus = self.sidebar_search_input.read(cx).focus_handle(cx);
            self.sidebar_collapse_focus.is_focused(window)
                || self.sidebar_account_focus.is_focused(window)
                || self.sidebar_settings_focus.is_focused(window)
                || search_focus.contains_focused(window, cx)
                || self.sidebar_menu_focus.contains_focused(window, cx)
        } else {
            self.topbar_expand_focus.is_focused(window)
                || self.topbar_recent_focus.is_focused(window)
                || self.topbar_session_more_focus.is_focused(window)
                || self.topbar_account_focus.is_focused(window)
                || self.topbar_settings_focus.is_focused(window)
        };
        self.recent_drawer_open = false;
        self.recent_drawer_prior_focus = None;
        self.topbar_session_more_open = false;
        self.topbar_session_more_prior_focus = None;
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
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("toggle_sidebar");
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
        if self.topbar_session_more_open {
            self.close_topbar_session_more(window, cx);
            return true;
        }
        if self.recent_drawer_open {
            self.close_recent_drawer(window, cx);
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
        if !self.sidebar_collapsed
            && self
                .sidebar_search_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            if self.sidebar_search_input.read(cx).value().is_empty() {
                self.sidebar_toggle_focus().focus(window, cx);
            } else {
                self.sidebar_search_input
                    .update(cx, |st, cx| st.set_value("", window, cx));
            }
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
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("session_switch");
        self.set_route(ShellRoute::Session, cx);
        self.recent_drawer_open = false;
        self.recent_drawer_prior_focus = None;
        self.sidebar_context_menu = None;
        let activation = self.client.begin_activate_session(Some(session_id.clone()));
        cx.spawn(async move |weak, cx| {
            if let Err(err) = activation.await {
                let _ = weak.update(cx, |this, cx| {
                    if this
                        .client
                        .with_workspace(|ws| ws.active_session_id() == Some(session_id.as_str()))
                    {
                        this.show_workspace_error(err.to_string(), cx);
                    }
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
            AllSessionsEvent::NavigateBack => {
                self.navigate_back(window, cx);
            }
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
            SettingsViewEvent::NavigateBack => {
                self.navigate_back(window, cx);
            }
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.route != ShellRoute::Account {
            return;
        }
        match event {
            AccountViewEvent::NavigateBack => {
                self.navigate_back(window, cx);
            }
            AccountViewEvent::OpenLoginTab(tab) => {
                let target_tab = *tab;
                self.set_route(ShellRoute::Login, cx);
                self.login_view
                    .update(cx, |lv, cx| lv.set_tab(target_tab, cx));
                cx.notify();
            }
            AccountViewEvent::LoggedOutCstoa => {
                self.show_top_notice("已退出 计维CSTOA 登录，其余提供商凭据保持不变。", cx);
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
                                    .rounded_t(radius::ROW)
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

    fn render_active_or_exiting_banner(
        &mut self,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        entity: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let active = if let Some(ref err_msg) = self.workspace_error {
            Some((true, err_msg.clone(), self.error_banner_timer.generation()))
        } else if let Some(ref notice) = self.top_notice {
            Some((false, notice.clone(), self.notice_banner_timer.generation()))
        } else {
            None
        };
        let is_open = active.is_some();
        if let Some((is_error, ref msg, _)) = active {
            self.last_banner = Some((is_error, msg.clone()));
        }
        let progress = self
            .banner_presence
            .sample(
                if is_open { 1.0_f32 } else { 0.0 },
                Transition::new(if is_open {
                    motion::NORMAL
                } else {
                    motion::SURFACE_CLOSE
                })
                .ease(motion::ease_out()),
                window,
                cx,
            )
            .value
            .clamp(0.0, 1.0);
        if !is_open && progress <= 0.001 {
            self.last_banner = None;
            return None;
        }
        let (is_error, msg, generation) = match active {
            Some((is_err, msg, banner_gen)) => (is_err, msg, banner_gen),
            None => {
                let (is_err, msg) = self.last_banner.clone()?;
                (is_err, msg, 0)
            }
        };
        let ent_hover = entity.clone();
        if is_error {
            Some(
                render_workspace_error_bar(
                    is_open,
                    progress,
                    msg,
                    palette,
                    transparency_enabled,
                    scale_factor,
                    move |hovered, _, cx| {
                        ent_hover.update(cx, |this, cx| {
                            this.set_banner_hovered(true, generation, *hovered, cx)
                        });
                    },
                )
                .into_any_element(),
            )
        } else {
            Some(
                render_top_notice_banner(
                    is_open,
                    progress,
                    msg,
                    palette,
                    transparency_enabled,
                    scale_factor,
                    move |hovered, _, cx| {
                        ent_hover.update(cx, |this, cx| {
                            this.set_banner_hovered(false, generation, *hovered, cx)
                        });
                    },
                )
                .into_any_element(),
            )
        }
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "test-support")]
        let frame_started = crate::ux_probe::is_enabled().then(std::time::Instant::now);
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
        let active_chat =
            self.route == ShellRoute::Session && self.session_subview == SessionSubView::Chat;
        let transient_blocked = self.blocks_transient_surfaces(cx)
            || (active_chat && self.workbench.read(cx).has_modal());
        if transient_blocked {
            self.reset_account_menu();
        }
        let tooltip_blocked = transient_blocked || self.account_menu_open;
        self.tooltip_overlay
            .update(cx, |layer, cx| layer.set_blocked(tooltip_blocked, cx));
        if web_locked {
            self.sidebar_presence = TransitionState::default();
        }
        let hw_diag_active = has_active_hardware_diagnostic(&self.snapshot);
        let sky_time = self.shader_scheduler.sample_for_window(
            self.route.uses_sky_shader(),
            web_locked,
            hw_diag_active,
            cx.reduce_motion(),
            window,
        );
        self.last_sky_time = sky_time;

        let route_sample = self.route_transition.sample(active_chat, window, cx);

        let (active_session_id, active_view) = self
            .client
            .with_workspace(|ws| (ws.active_session_id().map(str::to_owned), ws.active_view()));

        if self.tooltip_session != active_session_id {
            self.tooltip_session.clone_from(&active_session_id);
            self.tooltip_overlay
                .update(cx, motion::DesktopTooltips::dismiss);
        }
        let wb_session_id = match self.route {
            ShellRoute::Session => active_session_id.clone(),
            _ => None,
        };
        self.workbench.update(cx, |wb, cx| {
            wb.set_sidebar_collapsed(self.sidebar_collapsed);
            wb.set_route_transition_samples(route_sample.home_enter, route_sample.chat_enter);
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
                .overflow_hidden()
                .bg(palette.bg_workspace)
                .child(
                    div()
                        .relative()
                        .size_full()
                        .opacity(route_sample.container_opacity)
                        .left(route_sample.container_dx)
                        .top(route_sample.container_dy)
                        .child(self.login_view.clone()),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(layout::TOPBAR_HEIGHT)
                        .pl(space::SM)
                        .flex()
                        .items_center()
                        .child(
                            apply_scale(div(), scale::UI_MEDIUM)
                                .id("login-window-back-btn")
                                .test_support()
                                .a11y_button("login-window-back-btn", "返回工作台")
                                .tab_index(0)
                                .h(height::COMPACT)
                                .px(space::SM)
                                .flex_shrink_0()
                                .rounded(radius::CONTROL)
                                .flex()
                                .items_center()
                                .gap(space::XS)
                                .text_color(palette.text_primary)
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(move |_, _, cx| {
                                    e_back.update(cx, |this, cx| {
                                        let dest = match this.login_return_route {
                                            ShellRoute::Login => ShellRoute::Home,
                                            other => other,
                                        };
                                        this.set_route(dest, cx);
                                        cx.notify();
                                    });
                                })
                                .child(Icon::new(IconName::ArrowLeft).size(px(14.0)))
                                .child(cjk_inline_label("返回")),
                        )
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
                            window,
                            cx,
                        )),
                );

            if let Some(banner_el) = self.render_active_or_exiting_banner(
                &palette,
                transparency_enabled,
                scale_factor,
                &entity,
                window,
                cx,
            ) {
                login_shell = login_shell.child(banner_el);
            }
            login_shell
        } else {
            let sidebar_open = !self.sidebar_collapsed;
            let sidebar_progress = self
                .sidebar_presence
                .sample(
                    if sidebar_open { 1.0_f32 } else { 0.0 },
                    Transition::new(if sidebar_open {
                        motion::NORMAL
                    } else {
                        motion::SURFACE_CLOSE
                    })
                    .ease(motion::ease_out()),
                    window,
                    cx,
                )
                .value
                .clamp(0.0, 1.0);
            let collapsed_progress = (1.0 - sidebar_progress).clamp(0.0, 1.0);

            let topbar_el = {
                let e1 = entity.clone();
                let e2 = entity.clone();
                let e2_more = entity.clone();
                let e3 = entity.clone();
                let e4 = entity.clone();
                let e5 = entity.clone();
                let e6 = entity.clone();
                let e7 = entity.clone();
                let e8 = entity.clone();
                let e9 = entity.clone();
                let show_topbar_session_more = active_chat && active_session_id.is_some();
                let collapsed_session_title: Option<String> = if active_chat {
                    active_session_id.as_ref().map(|sid| {
                        self.snapshot
                            .sessions
                            .iter()
                            .find(|s| &s.id == sid)
                            .map(|s| {
                                if s.title.trim().is_empty() {
                                    "新对话".to_owned()
                                } else {
                                    s.title.clone()
                                }
                            })
                            .unwrap_or_else(|| "当前对话".to_owned())
                    })
                } else {
                    None
                };
                render_topbar(
                    self.sidebar_collapsed,
                    collapsed_progress,
                    self.recent_drawer_open,
                    show_topbar_session_more,
                    self.topbar_session_more_open,
                    collapsed_session_title.as_deref(),
                    Rc::clone(&self.recent_btn_anchor_x),
                    Rc::clone(&self.topbar_session_more_anchor_x),
                    active_view,
                    window.is_maximized(),
                    account_initial.clone(),
                    &self.topbar_expand_focus,
                    &self.topbar_recent_focus,
                    &self.topbar_session_more_focus,
                    &self.topbar_account_focus,
                    &self.topbar_settings_focus,
                    &palette,
                    TopbarCallbacks {
                        on_expand_sidebar: Rc::new(move |w, cx| {
                            e1.update(cx, |this, cx| this.toggle_sidebar(w, cx));
                        }),
                        on_toggle_recent_drawer: Rc::new(move |w, cx| {
                            e2.update(cx, |this, cx| {
                                #[cfg(feature = "test-support")]
                                crate::ux_probe::mark_input("recent_drawer");
                                if this.recent_drawer_open {
                                    this.close_recent_drawer(w, cx);
                                } else {
                                    if this.account_menu_open {
                                        this.close_account_menu(w, cx);
                                    }
                                    if this.topbar_session_more_open {
                                        this.topbar_session_more_open = false;
                                        this.topbar_session_more_prior_focus = None;
                                    }
                                    this.recent_drawer_open = true;
                                    this.recent_drawer_prior_focus =
                                        Some(this.topbar_recent_focus.clone());
                                    this.topbar_recent_focus.focus(w, cx);
                                    cx.notify();
                                }
                            });
                        }),
                        on_toggle_session_more: Rc::new(move |w, cx| {
                            e2_more.update(cx, |this, cx| {
                                if this.topbar_session_more_open {
                                    this.close_topbar_session_more(w, cx);
                                } else {
                                    if this.account_menu_open {
                                        this.close_account_menu(w, cx);
                                    }
                                    if this.recent_drawer_open {
                                        this.recent_drawer_open = false;
                                        this.recent_drawer_prior_focus = None;
                                    }
                                    this.topbar_session_more_open = true;
                                    this.topbar_session_more_prior_focus =
                                        Some(this.topbar_session_more_focus.clone());
                                    this.topbar_session_more_focus.focus(w, cx);
                                    cx.notify();
                                }
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
                            e7.update(cx, |this, cx| this.toggle_account_menu(false, w, cx));
                        }),
                        on_open_settings: Rc::new(move |w, cx| {
                            e8.update(cx, |this, cx| this.toggle_account_menu(true, w, cx));
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
                .child(
                    div()
                        .relative()
                        .size_full()
                        .opacity(route_sample.container_opacity)
                        .left(route_sample.container_dx)
                        .top(route_sample.container_dy)
                        .child(self.render_workspace_body(&palette, &entity)),
                );

            if let Some(curtain) = route_sample.home_exit_curtain {
                let curtain_layer = div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h_full()
                            .opacity(1.0 - curtain.sky_progress)
                            .child(render_sky_background_rounded(
                                SkyPreset::home_for_theme(palette.mode),
                                curtain.frozen_sky_time,
                                1.0,
                                [f32::from(radius::ROW); 4],
                                scale_factor,
                            )),
                    )
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .px(space::GROUP)
                            .pb(space::SM)
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(600.0))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .child(
                                        div()
                                            .w_full()
                                            .h(px(200.0))
                                            .relative()
                                            .flex()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .relative()
                                                    .w_full()
                                                    .h_full()
                                                    .top(curtain.mark_dy)
                                                    .opacity(curtain.mark_opacity)
                                                    .child(render_home_brand_arcs()),
                                            )
                                            .child(
                                                apply_scale(div(), scale::HERO)
                                                    .absolute()
                                                    .top(px(125.0) + curtain.greeting_dy)
                                                    .left_0()
                                                    .right_0()
                                                    .opacity(curtain.greeting_opacity)
                                                    .flex()
                                                    .justify_center()
                                                    .text_color(palette.text_welcome)
                                                    .child("我该如何协助您？"),
                                            ),
                                    )
                                    .child(div().w_full().h(px(88.0))),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(17.0) - curtain.footer_dy)
                            .opacity(curtain.footer_opacity)
                            .flex()
                            .justify_center()
                            .child(
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_secondary)
                                    .child("@cst-pilot"),
                            ),
                    );
                workspace_card = workspace_card.child(curtain_layer);
            }

            if let Some(banner_el) = self.render_active_or_exiting_banner(
                &palette,
                transparency_enabled,
                scale_factor,
                &entity,
                window,
                cx,
            ) {
                workspace_card = workspace_card.child(banner_el);
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
            #[cfg(feature = "test-support")]
            if let Some(layer) = crate::ux_probe::bounds_probe_layer("workspace", None) {
                workspace_card = workspace_card.child(layer);
            }

            let framed_workspace = div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .pl(layout::SHELL_FRAME_INSET * collapsed_progress)
                .pr(layout::SHELL_FRAME_INSET)
                .pb(layout::SHELL_FRAME_INSET)
                .bg(palette.bg_shell)
                .child(workspace_card);

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

            if sidebar_open {
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

                let sidebar_el = render_sidebar(
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
                    self.account_menu_open,
                    &self.sidebar_collapse_focus,
                    &self.sidebar_account_focus,
                    &self.sidebar_settings_focus,
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
                                this.show_top_notice("本地用量仪表盘将在后续版本开放。", cx);
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
                                let q = this.sidebar_search_input.read(cx).value().to_string();
                                let recent_ids: Vec<String> = filter_recent_sidebar_sessions(
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
                                    this.sidebar_selected_ids = recent_ids.into_iter().collect();
                                }
                                cx.notify();
                            });
                        }),
                        on_batch_delete: Rc::new(move |window, cx| {
                            s8.update(cx, |this, cx| {
                                if !this.sidebar_batch_mode || this.sidebar_selected_ids.is_empty()
                                {
                                    return;
                                }
                                let targets = std::mem::take(&mut this.sidebar_selected_ids)
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
                            s10.update(cx, |this, cx| this.toggle_account_menu(false, w, cx));
                        }),
                        on_open_settings: Rc::new(move |w, cx| {
                            s11.update(cx, |this, cx| this.toggle_account_menu(true, w, cx));
                        }),
                    },
                );

                let animated_w = layout::SIDEBAR_WIDTH * sidebar_progress;
                row_layout = row_layout.child(
                    div()
                        .h_full()
                        .w(animated_w)
                        .overflow_hidden()
                        .flex()
                        .flex_shrink_0()
                        .opacity(0.01 + 0.99 * sidebar_progress)
                        .child(
                            div()
                                .h_full()
                                .flex_shrink_0()
                                .ml(px(-24.0 * (1.0 - sidebar_progress)))
                                .child(sidebar_el),
                        ),
                );
            } else if sidebar_progress > 0.001 {
                // Visual-only 150ms exit: preserves real sidebar layout while interactive & a11y nodes unmount on frame 1.
                let search_query = self.sidebar_search_input.read(cx).value().to_string();
                let sidebar_exit_el = render_sidebar_exiting(
                    self.route,
                    active_session_id.as_deref(),
                    &sidebar_sessions,
                    &search_query,
                    self.sidebar_batch_mode,
                    &self.sidebar_selected_ids,
                    account_label,
                    account_initial,
                    &palette,
                );
                row_layout = row_layout.child(
                    div()
                        .h_full()
                        .w(layout::SIDEBAR_WIDTH * sidebar_progress)
                        .overflow_hidden()
                        .flex()
                        .flex_shrink_0()
                        .opacity(0.01 + 0.99 * sidebar_progress)
                        .child(
                            div()
                                .h_full()
                                .flex_shrink_0()
                                .ml(px(-24.0 * (1.0 - sidebar_progress)))
                                .child(sidebar_exit_el),
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
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                crate::sessions::consume_composing_escape(&this.sidebar_search_input, window, cx);
            }))
            .on_action(cx.listener(|this, _: &DismissTopSurface, window, cx| {
                let _ = this.dismiss_topmost_surface(window, cx);
            }))
            .on_action(cx.listener(|this, _: &RequestAppExit, window, cx| {
                this.request_close_flow(window, cx);
            }))
            .child(main_layout);

        if self.route != ShellRoute::Login {
            let ctx_open = self.sidebar_context_menu.is_some();
            if let Some(ref active_ctx) = self.sidebar_context_menu {
                self.last_sidebar_menu = Some(active_ctx.clone());
            }
            let ctx_progress = self
                .sidebar_menu_presence
                .sample(
                    if ctx_open { 1.0_f32 } else { 0.0 },
                    Transition::new(if ctx_open {
                        motion::POPOVER_OPEN
                    } else {
                        motion::SURFACE_CLOSE
                    })
                    .ease(motion::ease_out()),
                    window,
                    cx,
                )
                .value
                .clamp(0.0, 1.0);
            if ctx_open || ctx_progress > 0.001 {
                if let Some((menu_sid, menu_pos)) = self
                    .sidebar_context_menu
                    .as_ref()
                    .or(self.last_sidebar_menu.as_ref())
                {
                    root = root.child(self.render_sidebar_context_menu(
                        ctx_open,
                        ctx_progress,
                        menu_sid,
                        *menu_pos,
                        viewport_size,
                        &palette,
                        transparency_enabled,
                        scale_factor,
                        &entity,
                    ));
                }
            } else {
                self.last_sidebar_menu = None;
            }

            if self.sidebar_collapsed && !web_locked {
                let open = self.recent_drawer_open;
                let progress = transition(
                    "shell-recent-drawer-presence",
                    if open { 1.0_f32 } else { 0.0 },
                    Transition::new(if open {
                        motion::POPOVER_OPEN
                    } else {
                        motion::SURFACE_CLOSE
                    })
                    .ease(motion::ease_out()),
                    window,
                    cx,
                );
                if open || progress > 0.0 {
                    root = root.child(self.render_recent_drawer_popover(
                        progress,
                        active_session_id.as_deref(),
                        &palette,
                        transparency_enabled,
                        scale_factor,
                        &entity,
                    ));
                }

                if let Some(ref sid) = active_session_id
                    && active_chat
                {
                    let more_open = self.topbar_session_more_open;
                    let more_progress = transition(
                        "shell-topbar-session-more-presence",
                        if more_open { 1.0_f32 } else { 0.0 },
                        Transition::new(if more_open {
                            motion::POPOVER_OPEN
                        } else {
                            motion::SURFACE_CLOSE
                        })
                        .ease(motion::ease_out()),
                        window,
                        cx,
                    );
                    if more_open || more_progress > 0.001 {
                        root = root.child(self.render_topbar_session_more_popover(
                            more_open,
                            more_progress,
                            sid,
                            &palette,
                            transparency_enabled,
                            scale_factor,
                            &entity,
                        ));
                    }
                }
            }

            let account_open = self.account_menu_open;
            let account_progress = self
                .account_presence
                .sample(
                    if account_open { 1.0_f32 } else { 0.0 },
                    Transition::new(if account_open {
                        motion::POPOVER_OPEN
                    } else {
                        motion::SURFACE_CLOSE
                    })
                    .ease(motion::ease_out()),
                    window,
                    cx,
                )
                .value
                .clamp(0.0, 1.0);
            if account_open || account_progress > 0.001 {
                root = root.child(self.render_account_popover(
                    account_open,
                    account_progress,
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
        let rename_scrim_p = motion::surface_presence(
            "rename-session-scrim-p",
            self.rename_modal.is_some(),
            motion::MODAL_OPEN,
            window,
            cx,
        );
        if let Some(ref rm) = self.rename_modal {
            let ent_cancel = entity.clone();
            root = root.child(
                render_frosted_surface(
                    "rename-session-scrim",
                    FrostedGlassSpec::scrim(&palette).with_opacity(rename_scrim_p),
                    palette.bg_scrim,
                    transparency_enabled,
                    scale_factor,
                )
                .opacity(0.01 + 0.99 * rename_scrim_p)
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
                        FrostedGlassSpec::modal(&palette, 8.0).with_opacity(rename_scrim_p),
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
        let missing_key_p = motion::surface_presence(
            "missing-key-scrim-p",
            self.missing_key_modal.is_some(),
            motion::MODAL_OPEN,
            window,
            cx,
        );
        if let Some(ref mk_state) = self.missing_key_modal {
            let ent_cancel = entity.clone();
            let ent_cfg = entity.clone();
            root = root.child(render_missing_key_dialog(
                mk_state,
                missing_key_p,
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
        let update_dialog_open = self.update_dialog.open || self.update_dialog.handoff_locked;
        let update_dialog_p = motion::surface_presence(
            "update-dialog-scrim-p",
            update_dialog_open,
            motion::MODAL_OPEN,
            window,
            cx,
        );
        if update_dialog_open {
            let ent_later = entity.clone();
            let ent_prep = entity.clone();
            let ent_canc = entity.clone();
            let ent_lnch = entity.clone();
            root = root.child(render_update_dialog(
                update_dialog_p,
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
        let web_confirm_p = motion::surface_presence(
            "web-switch-confirm-scrim-p",
            self.web_switch_confirm.is_some(),
            motion::MODAL_OPEN,
            window,
            cx,
        );
        if self.web_switch_confirm.is_some() {
            let ent_cancel = entity.clone();
            let ent_proceed = entity.clone();
            root = root.child(
                render_frosted_surface(
                    "web-switch-confirm-scrim",
                    FrostedGlassSpec::scrim(&palette).with_opacity(web_confirm_p),
                    palette.bg_scrim,
                    transparency_enabled,
                    scale_factor,
                )
                .opacity(0.01 + 0.99 * web_confirm_p)
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
                        FrostedGlassSpec::modal(&palette, 8.0).with_opacity(web_confirm_p),
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
        let web_lock_p = motion::surface_presence(
            "web-active-scrim-p",
            web_locked,
            motion::MODAL_OPEN,
            window,
            cx,
        );
        if web_locked {
            let ent_open = entity.clone();
            let ent_back = entity.clone();
            root = root.child(render_web_active_lock_overlay(
                web_lock_p,
                &palette,
                transparency_enabled,
                scale_factor,
                &self.web_lock_trap_focus,
                &self.web_lock_switch_focus,
                move |w, cx| {
                    ent_open.update(cx, |this, cx| {
                        // Always mint a fresh single-use ticket when re-opening browser in WebActive.
                        this.trigger_open_web(true, w, cx);
                    });
                },
                move |w, cx| {
                    ent_back.update(cx, |this, cx| this.trigger_close_web(w, cx));
                },
            ));
        }

        // Exit confirmation dialog (`pFooK`) — sits topmost so user can always safely exit even during `WebActive`
        let exit_dialog_p = motion::surface_presence(
            "exit-dialog-scrim-p",
            self.exit_dialog.is_some(),
            motion::MODAL_OPEN,
            window,
            cx,
        );
        if let Some(ref dialog) = self.exit_dialog {
            let ent_cancel = entity.clone();
            let ent_confirm = entity.clone();
            root = root.child(render_exit_confirm_dialog(
                &dialog.report,
                dialog.exiting,
                exit_dialog_p,
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

        root = root.child(self.tooltip_overlay.clone());

        #[cfg(feature = "test-support")]
        if let Some(started) = frame_started {
            let route_name = match self.route {
                ShellRoute::Home => "home",
                ShellRoute::Session => match self.session_subview {
                    SessionSubView::Chat => "session:chat",
                    SessionSubView::Tree => "session:tree",
                    SessionSubView::Export => "session:export",
                },
                ShellRoute::AllSessions => "all_sessions",
                ShellRoute::Settings => "settings",
                ShellRoute::Account => "account",
                ShellRoute::Login => "login",
            };
            crate::ux_probe::record_render(
                started,
                crate::ux_probe::ShellProbeState {
                    route: route_name.to_owned(),
                    active_session_id,
                    viewport_width: f32::from(viewport_size.width),
                    viewport_height: f32::from(viewport_size.height),
                    scale_factor,
                    sidebar_collapsed: self.sidebar_collapsed,
                    window_active: window.is_window_active(),
                    reduce_motion: cx.reduce_motion(),
                    transparency_enabled,
                    web_active: web_locked,
                    forced_warp: crate::ux_probe::forced_warp_enabled(),
                },
            );
            if let Some(layer) = crate::ux_probe::bounds_probe_layer("shell", Some(started)) {
                root = root.child(layer);
            }
        }

        root
    }
}

pub fn open_url_in_trusted_browser(url: &str) {
    #[cfg(windows)]
    {
        if url.starts_with("http://127.0.0.1:") || url.starts_with("https://") {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::UI::Shell::ShellExecuteW;
            use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
            use windows::core::PCWSTR;
            let verb: Vec<u16> = OsStr::new("open").encode_wide().chain([0]).collect();
            let target: Vec<u16> = OsStr::new(url).encode_wide().chain([0]).collect();
            let launched = unsafe {
                let res = ShellExecuteW(
                    None,
                    PCWSTR(verb.as_ptr()),
                    PCWSTR(target.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
                (res.0 as isize) > 32
            };
            if !launched && let Ok(win_dir) = cst_os::windows_directory() {
                let _ = std::process::Command::new(win_dir.join("explorer.exe"))
                    .arg(url)
                    .spawn();
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

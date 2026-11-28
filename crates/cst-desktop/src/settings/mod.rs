pub mod general_tab;
pub mod preferences_tab;
pub mod providers_tab;
pub mod shortcuts_tab;
pub mod update_dialog;

use std::cell::Cell;
use std::rc::Rc;

use cst_client::{
    ClientHandle, DesktopSettingsPatchDto, KeyboardFocusStyleDto, ThemeModeDto, UpdatePhaseDto,
};
use gpui::{FocusHandle, KeyDownEvent, MouseMoveEvent, Pixels, Point, ScrollHandle, point};
use gpui_kit::assets::IconName;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::input::Escape;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, TestSupportExt, Window,
    div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::settings::general_tab::{GeneralTabKbdProps, render_general_tab};
use crate::settings::preferences_tab::{PreferencesTabEvent, PreferencesTabView};
use crate::settings::providers_tab::{ProvidersTabEvent, ProvidersTabView};
use crate::settings::shortcuts_tab::render_shortcuts_tab;
use crate::theme::kbd_focus::{
    DialogNavStep, KbdDynamicBoundsTracker, pointer_moved_past_threshold, resolve_dialog_nav_key,
    scroll_dynamic_target_into_view, scroll_step_y,
};
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, motion,
    render_subpage_header_xwjof, scale, space,
};

pub(crate) fn activates_button(event: &gpui::KeyDownEvent) -> bool {
    !event.is_held
        && event.keystroke.modifiers == gpui::Modifiers::default()
        && matches!(event.keystroke.key.as_str(), "enter" | "space")
}

pub(crate) fn move_picker_cursor(
    event: &gpui::KeyDownEvent,
    index: &mut usize,
    count: usize,
    scroll: &gpui::ScrollHandle,
) -> bool {
    if count == 0 || event.keystroke.modifiers != gpui::Modifiers::default() {
        return false;
    }
    let last = count - 1;
    *index = match event.keystroke.key.as_str() {
        "up" => (*index).min(last).saturating_sub(1),
        "down" => (*index + 1).min(last),
        "home" => 0,
        "end" => last,
        _ => return false,
    };
    scroll.scroll_to_item(*index);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsCategory {
    #[default]
    General,
    Providers,
    Preferences,
    Shortcuts,
}

impl SettingsCategory {
    pub const ALL: [Self; 4] = [
        Self::General,
        Self::Providers,
        Self::Preferences,
        Self::Shortcuts,
    ];

    pub fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Providers => 1,
            Self::Preferences => 2,
            Self::Shortcuts => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "常规",
            Self::Providers => "提供商",
            Self::Preferences => "模型偏好",
            Self::Shortcuts => "快捷键",
        }
    }

    pub fn a11y_id(self) -> &'static str {
        match self {
            Self::General => "settings-tab-general",
            Self::Providers => "settings-tab-providers",
            Self::Preferences => "settings-tab-preferences",
            Self::Shortcuts => "settings-tab-shortcuts",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SettingsViewEvent {
    NavigateBack,
    ThemeChanged(ThemeModeDto),
    OpenUpdateDialog,
    CancelUpdate,
    OpenLoginOrAccount { signed_in: bool },
    CredentialsSaved,
    Notice(String),
    ErrorMessage(String),
}

/// 4-Tab Settings View (`cst-pilot-desktop.pen` 「03 · 设置」 master `VPWAF`):
/// Top full-width `44px` `xWJOf` subpage header + `816px` center column with `40px` tabbar (`2px` `$accent-solid` indicator at `bottom: -1px`).
pub struct SettingsView {
    client: ClientHandle,
    category: SettingsCategory,
    page_slide: motion::PageSlide,
    body_scroll: ScrollHandle,
    tab_rects: Rc<[Cell<(f32, f32)>; 4]>,
    tab_bar_focus: FocusHandle,
    tab_kbd_active: bool,
    suppress_focus_kbd_for: Option<FocusHandle>,
    tab_last_pointer: Option<Point<Pixels>>,
    general_focus: FocusHandle,
    general_row_idx: usize,
    general_kbd_active: bool,
    general_last_pointer: Option<Point<Pixels>>,
    general_tracker: KbdDynamicBoundsTracker,
    shortcuts_focus: FocusHandle,
    shortcuts_kbd_active: bool,
    providers_tab: Entity<ProvidersTabView>,
    preferences_tab: Entity<PreferencesTabView>,
    optimistic_theme: Option<ThemeModeDto>,
    optimistic_keyboard_style: Option<KeyboardFocusStyleDto>,
    keyboard_style_saving: bool,
    pending_keyboard_style: Option<KeyboardFocusStyleDto>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsViewEvent> for SettingsView {}

impl SettingsView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let body_scroll = ScrollHandle::new();
        let providers_tab =
            cx.new(|cx| ProvidersTabView::new(client.clone(), body_scroll.clone(), window, cx));
        let preferences_tab =
            cx.new(|cx| PreferencesTabView::new(client.clone(), body_scroll.clone(), window, cx));

        let tab_bar_focus = cx.focus_handle().tab_stop(true);
        let general_focus = cx.focus_handle().tab_stop(true);
        let shortcuts_focus = cx.focus_handle().tab_stop(true);

        let mut subs = Vec::new();
        subs.push(
            cx.subscribe_in(&providers_tab, window, |this, _, ev, window, cx| match ev {
                ProvidersTabEvent::OpenLoginOrAccount { signed_in } => {
                    cx.emit(SettingsViewEvent::OpenLoginOrAccount {
                        signed_in: *signed_in,
                    });
                }
                ProvidersTabEvent::CredentialsSaved => {
                    cx.emit(SettingsViewEvent::CredentialsSaved);
                }
                ProvidersTabEvent::ReturnToTabBar => {
                    this.focus_tab_bar_from_content(window, cx);
                }
                ProvidersTabEvent::Notice(msg) => {
                    cx.emit(SettingsViewEvent::Notice(msg.clone()));
                }
                ProvidersTabEvent::ErrorMessage(msg) => {
                    cx.emit(SettingsViewEvent::ErrorMessage(msg.clone()));
                }
            }),
        );
        subs.push(cx.subscribe_in(
            &preferences_tab,
            window,
            |this, _, ev, window, cx| match ev {
                PreferencesTabEvent::ReturnToTabBar => {
                    this.focus_tab_bar_from_content(window, cx);
                }
                PreferencesTabEvent::Notice(msg) => {
                    cx.emit(SettingsViewEvent::Notice(msg.clone()));
                }
                PreferencesTabEvent::ErrorMessage(msg) => {
                    cx.emit(SettingsViewEvent::ErrorMessage(msg.clone()));
                }
            },
        ));
        subs.push(cx.on_focus(&tab_bar_focus, window, |this, window, cx| {
            let suppressed = this
                .suppress_focus_kbd_for
                .take()
                .is_some_and(|h| h == this.tab_bar_focus);
            if !suppressed {
                this.tab_kbd_active = true;
                this.tab_last_pointer = Some(window.mouse_position());
            }
            cx.notify();
        }));
        subs.push(cx.on_blur(&tab_bar_focus, window, |this, _, cx| {
            if this.tab_kbd_active {
                this.tab_kbd_active = false;
                cx.notify();
            }
        }));
        subs.push(cx.on_focus(&general_focus, window, |this, window, cx| {
            let suppressed = this
                .suppress_focus_kbd_for
                .take()
                .is_some_and(|h| h == this.general_focus);
            if !suppressed {
                this.general_kbd_active = true;
                this.general_last_pointer = Some(window.mouse_position());
            }
            cx.notify();
        }));
        subs.push(cx.on_blur(&general_focus, window, |this, _, cx| {
            if this.general_kbd_active {
                this.general_kbd_active = false;
                cx.notify();
            }
        }));
        subs.push(cx.on_focus(&shortcuts_focus, window, |this, _, cx| {
            let suppressed = this
                .suppress_focus_kbd_for
                .take()
                .is_some_and(|h| h == this.shortcuts_focus);
            if !suppressed {
                this.shortcuts_kbd_active = true;
            }
            cx.notify();
        }));
        subs.push(cx.on_blur(&shortcuts_focus, window, |this, _, cx| {
            if this.shortcuts_kbd_active {
                this.shortcuts_kbd_active = false;
                cx.notify();
            }
        }));

        Self {
            client,
            category: SettingsCategory::General,
            page_slide: motion::PageSlide::default(),
            body_scroll,
            tab_rects: Rc::new(Default::default()),
            tab_bar_focus,
            tab_kbd_active: false,
            suppress_focus_kbd_for: None,
            tab_last_pointer: None,
            general_focus,
            general_row_idx: 0,
            general_kbd_active: false,
            general_last_pointer: None,
            general_tracker: KbdDynamicBoundsTracker::new(),
            shortcuts_focus,
            shortcuts_kbd_active: false,
            providers_tab,
            preferences_tab,
            optimistic_theme: None,
            optimistic_keyboard_style: None,
            keyboard_style_saving: false,
            pending_keyboard_style: None,
            _subscriptions: subs,
        }
    }

    pub fn category(&self) -> SettingsCategory {
        self.category
    }

    pub fn is_tab_kbd_active(&self) -> bool {
        self.tab_kbd_active
    }

    pub fn is_general_kbd_active(&self) -> bool {
        self.general_kbd_active
    }

    pub fn general_row_index(&self) -> usize {
        self.general_row_idx
    }

    pub fn body_scroll_offset_y(&self) -> f32 {
        f32::from(self.body_scroll.offset().y)
    }

    pub fn providers_tab(&self) -> &Entity<ProvidersTabView> {
        &self.providers_tab
    }

    pub fn preferences_tab(&self) -> &Entity<PreferencesTabView> {
        &self.preferences_tab
    }

    pub fn focus_on_enter(
        &mut self,
        from_keyboard: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.body_scroll.set_offset(point(px(0.0), px(0.0)));
        self.general_kbd_active = false;
        self.shortcuts_kbd_active = false;
        self.suppress_focus_kbd_for = (!from_keyboard && !self.tab_bar_focus.is_focused(window))
            .then(|| self.tab_bar_focus.clone());
        self.tab_kbd_active = from_keyboard;
        self.tab_last_pointer = from_keyboard.then(|| window.mouse_position());
        self.tab_bar_focus.focus(window, cx);
        cx.notify();
    }

    pub fn focus_on_return(
        &mut self,
        from_keyboard: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.category == SettingsCategory::Providers {
            self.providers_tab
                .update(cx, |pt, cx| pt.focus_return_row(from_keyboard, window, cx));
            cx.notify();
            return;
        }
        self.focus_on_enter(from_keyboard, window, cx);
    }

    fn focus_tab_bar_from_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.general_kbd_active = false;
        self.shortcuts_kbd_active = false;
        self.suppress_focus_kbd_for = None;
        self.tab_kbd_active = true;
        self.tab_last_pointer = Some(window.mouse_position());
        self.body_scroll.set_offset(point(px(0.0), px(0.0)));
        self.tab_bar_focus.focus(window, cx);
        cx.notify();
    }

    pub fn is_any_input_composing(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        match self.category {
            SettingsCategory::Providers => self
                .providers_tab
                .update(cx, |tab, cx| tab.is_any_input_composing(window, cx)),
            SettingsCategory::Preferences => self
                .preferences_tab
                .update(cx, |tab, cx| tab.is_any_input_composing(window, cx)),
            SettingsCategory::General | SettingsCategory::Shortcuts => false,
        }
    }

    pub fn consume_composing_escape(&self, window: &mut Window, cx: &mut gpui::App) {
        match self.category {
            SettingsCategory::Providers => {
                self.providers_tab
                    .update(cx, |tab, cx| tab.consume_composing_escape(window, cx));
            }
            SettingsCategory::Preferences => {
                self.preferences_tab
                    .update(cx, |tab, cx| tab.consume_composing_escape(window, cx));
            }
            SettingsCategory::General | SettingsCategory::Shortcuts => {}
        }
    }

    fn select_theme(&mut self, theme: ThemeModeDto, cx: &mut Context<Self>) {
        self.optimistic_theme = Some(theme);
        cx.emit(SettingsViewEvent::ThemeChanged(theme));
        cx.notify();
    }

    pub fn clear_optimistic_theme(&mut self, cx: &mut Context<Self>) {
        if self.optimistic_theme.take().is_some() {
            cx.notify();
        }
    }

    fn save_keyboard_style(&mut self, style: KeyboardFocusStyleDto, cx: &mut Context<Self>) {
        self.optimistic_keyboard_style = Some(style);
        cx.notify();
        if self.keyboard_style_saving {
            self.pending_keyboard_style = Some(style);
            return;
        }
        self.keyboard_style_saving = true;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let result = client
                .patch_desktop_settings(DesktopSettingsPatchDto {
                    keyboard_focus_style: Some(style),
                    ..DesktopSettingsPatchDto::default()
                })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.keyboard_style_saving = false;
                if let Some(next) = this.pending_keyboard_style.take() {
                    this.save_keyboard_style(next, cx);
                } else {
                    if let Err(err) = result {
                        this.optimistic_keyboard_style = None;
                        cx.emit(SettingsViewEvent::ErrorMessage(err.to_string()));
                    } else if this.optimistic_keyboard_style == Some(style) {
                        this.optimistic_keyboard_style = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn set_category(&mut self, category: SettingsCategory, cx: &mut Context<Self>) {
        if self.category == category {
            return;
        }
        match (self.category, category) {
            (SettingsCategory::Providers, SettingsCategory::Preferences) => {
                if let Some(scope) = self.providers_tab.read(cx).current_scope() {
                    self.preferences_tab
                        .update(cx, |tab, cx| tab.mirror_external_scope(scope, cx));
                }
            }
            (SettingsCategory::Preferences, SettingsCategory::Providers) => {
                if let Some(scope) = self.preferences_tab.read(cx).current_scope() {
                    self.providers_tab
                        .update(cx, |tab, cx| tab.mirror_external_scope(scope, cx));
                }
            }
            _ => {}
        }
        #[cfg(feature = "test-support")]
        crate::ux_probe::mark_input("settings_tab");
        // Only dismiss transient popovers when switching between settings tabs; preserve any
        // in-progress custom provider form so returning to `提供商` never discards unsaved edits.
        self.dismiss_click_outside_dropdowns(cx);
        self.body_scroll.set_offset(point(px(0.0), px(0.0)));
        let old_idx = self.category.index();
        let new_idx = category.index();
        self.page_slide
            .start(if new_idx > old_idx { 1.0 } else { -1.0 }, cx);
        self.category = category;
        cx.notify();
    }

    pub fn open_custom_gateway_creator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_category(SettingsCategory::Providers, cx);
        self.providers_tab
            .update(cx, |pt, cx| pt.open_new_custom_provider(window, cx));
        cx.notify();
    }

    pub fn leave(&mut self, cx: &mut Context<Self>) {
        self.page_slide = motion::PageSlide::default();
        self.suppress_focus_kbd_for = None;
        self.general_tracker.cancel_pending_scroll();
        self.close_tab_surfaces(cx);
    }

    fn close_tab_surfaces(&mut self, cx: &mut Context<Self>) {
        self.providers_tab.update(cx, ProvidersTabView::leave);
        self.preferences_tab.update(cx, PreferencesTabView::leave);
    }

    pub fn dismiss_dropdowns_on_scroll(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let a = self
            .preferences_tab
            .update(cx, |pt, cx| pt.dismiss_dropdowns_on_scroll(window, cx));
        let b = self
            .providers_tab
            .update(cx, |pt, cx| pt.dismiss_dropdowns_on_scroll(window, cx));
        a || b
    }

    pub fn has_dropdown(&self, cx: &gpui::App) -> bool {
        match self.category {
            SettingsCategory::Preferences => self.preferences_tab.read(cx).has_dropdown(),
            SettingsCategory::Providers => self.providers_tab.read(cx).has_dropdown(),
            _ => false,
        }
    }

    pub fn dismiss_topmost_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.category {
            SettingsCategory::Preferences => self
                .preferences_tab
                .update(cx, |pt, cx| pt.dismiss_topmost(window, cx)),
            SettingsCategory::Providers => self
                .providers_tab
                .update(cx, |pt, cx| pt.dismiss_topmost(window, cx)),
            _ => false,
        }
    }

    fn enter_active_category_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_kbd_active = false;
        self.suppress_focus_kbd_for = None;
        match self.category {
            SettingsCategory::General => {
                self.general_row_idx = 0;
                self.general_kbd_active = true;
                self.general_last_pointer = Some(window.mouse_position());
                self.general_focus.focus(window, cx);
            }
            SettingsCategory::Providers => {
                self.providers_tab
                    .update(cx, |pt, cx| pt.focus_from_tabbar(window, cx));
            }
            SettingsCategory::Preferences => {
                self.preferences_tab
                    .update(cx, |pt, cx| pt.focus_from_tabbar(window, cx));
            }
            SettingsCategory::Shortcuts => {
                self.shortcuts_kbd_active = true;
                self.shortcuts_focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn handle_tab_bar_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.tab_bar_focus.is_focused(window) {
            return;
        }
        if event.keystroke.modifiers == gpui::Modifiers::default()
            && event.keystroke.key.as_str() == "down"
        {
            self.enter_active_category_content(window, cx);
            cx.stop_propagation();
            return;
        }
        let cur = self.category.index();
        match resolve_dialog_nav_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers,
            event.is_held,
            cur,
            SettingsCategory::ALL.len(),
        ) {
            DialogNavStep::Move(next) => {
                self.tab_kbd_active = true;
                self.tab_last_pointer = Some(window.mouse_position());
                self.set_category(SettingsCategory::ALL[next], cx);
                cx.stop_propagation();
            }
            DialogNavStep::Activate(_) => {
                self.enter_active_category_content(window, cx);
                cx.stop_propagation();
            }
            DialogNavStep::TabActivated => {
                self.tab_kbd_active = true;
                self.tab_last_pointer = Some(window.mouse_position());
                cx.notify();
            }
            DialogNavStep::Ignore => {}
        }
    }

    fn handle_shortcuts_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.shortcuts_focus.is_focused(window)
            || event.keystroke.modifiers != gpui::Modifiers::default()
        {
            return;
        }
        let vp_h = f32::from(self.body_scroll.bounds().size.height).max(240.0);
        let page_step = (vp_h - 64.0).max(120.0);
        match event.keystroke.key.as_str() {
            "up" => {
                if f32::from(self.body_scroll.offset().y) >= -1.0 {
                    self.focus_tab_bar_from_content(window, cx);
                } else {
                    scroll_step_y(&self.body_scroll, 48.0);
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "down" => {
                let _ = scroll_step_y(&self.body_scroll, -48.0);
                cx.stop_propagation();
                cx.notify();
            }
            "pageup" => {
                if f32::from(self.body_scroll.offset().y) >= -1.0 {
                    self.focus_tab_bar_from_content(window, cx);
                } else {
                    scroll_step_y(&self.body_scroll, page_step);
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "pagedown" => {
                let _ = scroll_step_y(&self.body_scroll, -page_step);
                cx.stop_propagation();
                cx.notify();
            }
            "home" => {
                self.body_scroll.set_offset(point(px(0.0), px(0.0)));
                cx.stop_propagation();
                cx.notify();
            }
            "end" => {
                let max_y = f32::from(self.body_scroll.max_offset().y).max(0.0);
                self.body_scroll.set_offset(point(px(0.0), px(-max_y)));
                cx.stop_propagation();
                cx.notify();
            }
            _ => {}
        }
    }

    fn trigger_update_check_or_open(&mut self, cx: &mut Context<Self>) {
        let cur_status = self.client.snapshot().update_state.clone();
        let cur_phase = cur_status.as_ref().map(|u| u.status.phase);
        let manual_upgrade_required = cur_status
            .as_ref()
            .and_then(|u| u.status.error_kind.as_deref())
            == Some("manual_upgrade_required");
        if manual_upgrade_required
            || matches!(
                cur_phase,
                Some(
                    UpdatePhaseDto::Unavailable
                        | UpdatePhaseDto::Launching
                        | UpdatePhaseDto::ExitRequired
                )
            )
        {
            return;
        }
        if matches!(
            cur_phase,
            Some(UpdatePhaseDto::Available | UpdatePhaseDto::Ready)
        ) {
            cx.emit(SettingsViewEvent::OpenUpdateDialog);
            return;
        }
        let c = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = c.check_update().await;
            let _ = c.refresh_globals().await;
            let _ = weak.update(cx, |_, cx| {
                match res {
                    Ok(st)
                        if matches!(
                            st.phase,
                            UpdatePhaseDto::Available | UpdatePhaseDto::Ready
                        ) =>
                    {
                        cx.emit(SettingsViewEvent::OpenUpdateDialog);
                    }
                    Ok(st) if st.phase == UpdatePhaseDto::Failed => {
                        let msg = if st.message.is_empty() {
                            "检查更新失败，请稍后重试。".to_owned()
                        } else {
                            st.message
                        };
                        cx.emit(SettingsViewEvent::ErrorMessage(msg));
                    }
                    Ok(st) if st.phase == UpdatePhaseDto::Current => {
                        let msg = if st.message.is_empty() {
                            "当前已是最新版本。".to_owned()
                        } else {
                            st.message
                        };
                        cx.emit(SettingsViewEvent::Notice(msg));
                    }
                    Ok(_) => {}
                    Err(err) => {
                        cx.emit(SettingsViewEvent::ErrorMessage(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn handle_general_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers != gpui::Modifiers::default() {
            return;
        }
        let snap = self.client.snapshot();
        let phase = snap
            .update_state
            .as_ref()
            .map(|u| u.status.phase)
            .unwrap_or(UpdatePhaseDto::Idle);
        let manual_upgrade_required = phase == UpdatePhaseDto::Failed
            && snap
                .update_state
                .as_ref()
                .and_then(|u| u.status.error_kind.as_deref())
                == Some("manual_upgrade_required");
        let update_actionable = !matches!(
            phase,
            UpdatePhaseDto::Unavailable
                | UpdatePhaseDto::Checking
                | UpdatePhaseDto::Launching
                | UpdatePhaseDto::ExitRequired
        ) && !manual_upgrade_required;
        let max_row = if update_actionable { 2 } else { 1 };
        let themes = [
            ThemeModeDto::System,
            ThemeModeDto::Light,
            ThemeModeDto::Dark,
        ];
        match event.keystroke.key.as_str() {
            "up" => {
                if self.general_row_idx == 0 {
                    self.focus_tab_bar_from_content(window, cx);
                } else {
                    self.general_row_idx = self.general_row_idx.saturating_sub(1);
                    self.general_kbd_active = true;
                    self.general_last_pointer = Some(window.mouse_position());
                    self.scroll_general_row_into_view(&snap.desktop_settings, window);
                }
                cx.stop_propagation();
                cx.notify();
            }
            "down" => {
                self.general_row_idx = (self.general_row_idx + 1).min(max_row);
                self.general_kbd_active = true;
                self.general_last_pointer = Some(window.mouse_position());
                self.scroll_general_row_into_view(&snap.desktop_settings, window);
                cx.stop_propagation();
                cx.notify();
            }
            "home" => {
                self.general_row_idx = 0;
                self.general_kbd_active = true;
                self.general_last_pointer = Some(window.mouse_position());
                self.body_scroll.set_offset(point(px(0.0), px(0.0)));
                cx.stop_propagation();
                cx.notify();
            }
            "end" => {
                self.general_row_idx = max_row;
                self.general_kbd_active = true;
                self.general_last_pointer = Some(window.mouse_position());
                self.scroll_general_row_into_view(&snap.desktop_settings, window);
                cx.stop_propagation();
                cx.notify();
            }
            "left" | "right" => {
                let forward = event.keystroke.key.as_str() == "right";
                self.general_kbd_active = true;
                self.general_last_pointer = Some(window.mouse_position());
                match self.general_row_idx {
                    0 => {
                        let effective_theme =
                            self.optimistic_theme.unwrap_or(snap.desktop_settings.theme);
                        let cur = themes
                            .iter()
                            .position(|&t| t == effective_theme)
                            .unwrap_or(0);
                        let next = if forward {
                            (cur + 1).min(themes.len() - 1)
                        } else {
                            cur.saturating_sub(1)
                        };
                        if next != cur {
                            self.select_theme(themes[next], cx);
                        }
                    }
                    1 => {
                        let effective_style = self
                            .optimistic_keyboard_style
                            .unwrap_or(snap.desktop_settings.keyboard_focus_style);
                        let next = if forward {
                            KeyboardFocusStyleDto::Rail
                        } else {
                            KeyboardFocusStyleDto::Brackets
                        };
                        if next != effective_style {
                            self.save_keyboard_style(next, cx);
                        }
                    }
                    _ => {}
                }
                cx.stop_propagation();
                cx.notify();
            }
            "enter" | "space" if !event.is_held => {
                self.general_kbd_active = true;
                self.general_last_pointer = Some(window.mouse_position());
                match self.general_row_idx {
                    0 => {
                        let effective_theme =
                            self.optimistic_theme.unwrap_or(snap.desktop_settings.theme);
                        let cur = themes
                            .iter()
                            .position(|&t| t == effective_theme)
                            .unwrap_or(0);
                        let next = (cur + 1) % themes.len();
                        self.select_theme(themes[next], cx);
                    }
                    1 => {
                        let effective_style = self
                            .optimistic_keyboard_style
                            .unwrap_or(snap.desktop_settings.keyboard_focus_style);
                        let next = match effective_style {
                            KeyboardFocusStyleDto::Brackets => KeyboardFocusStyleDto::Rail,
                            KeyboardFocusStyleDto::Rail => KeyboardFocusStyleDto::Brackets,
                        };
                        self.save_keyboard_style(next, cx);
                    }
                    2 if update_actionable => {
                        if phase == UpdatePhaseDto::Preparing {
                            cx.emit(SettingsViewEvent::CancelUpdate);
                        } else {
                            self.trigger_update_check_or_open(cx);
                        }
                    }
                    _ => {}
                }
                cx.stop_propagation();
                cx.notify();
            }
            _ => {}
        }
    }

    fn scroll_general_row_into_view(
        &self,
        desktop_settings: &cst_client::DesktopSettingsDto,
        window: &mut Window,
    ) {
        let key = match self.general_row_idx {
            0 => match self.optimistic_theme.unwrap_or(desktop_settings.theme) {
                ThemeModeDto::System => "gen-theme-system",
                ThemeModeDto::Light => "gen-theme-light",
                ThemeModeDto::Dark => "gen-theme-dark",
            },
            1 => match self
                .optimistic_keyboard_style
                .unwrap_or(desktop_settings.keyboard_focus_style)
            {
                KeyboardFocusStyleDto::Brackets => "gen-kbd-brackets",
                KeyboardFocusStyleDto::Rail => "gen-kbd-rail",
            },
            _ => "gen-update",
        };
        scroll_dynamic_target_into_view(
            &self.body_scroll,
            &self.general_tracker,
            key,
            16.0,
            window,
        );
    }

    pub fn dismiss_click_outside_dropdowns(&mut self, cx: &mut Context<Self>) -> bool {
        let mut closed = false;
        closed |= self
            .preferences_tab
            .update(cx, |pt, cx| pt.dismiss_dropdowns_on_outside_click(cx));
        closed |= self
            .providers_tab
            .update(cx, |pt, cx| pt.dismiss_click_outside_dropdowns(cx));
        closed
    }

    pub fn focus_primary_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.category {
            SettingsCategory::Preferences => self
                .preferences_tab
                .update(cx, |pt, cx| pt.focus_primary_input(window, cx)),
            SettingsCategory::Providers => self
                .providers_tab
                .update(cx, |pt, cx| pt.focus_primary_input(window, cx)),
            SettingsCategory::General | SettingsCategory::Shortcuts => false,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let snap = self.client.snapshot();
        if self.optimistic_theme == Some(snap.desktop_settings.theme) {
            self.optimistic_theme = None;
        }
        if !self.keyboard_style_saving
            && self.optimistic_keyboard_style == Some(snap.desktop_settings.keyboard_focus_style)
        {
            self.optimistic_keyboard_style = None;
        }
        let mut effective_desktop_settings = snap.desktop_settings.clone();
        if let Some(theme) = self.optimistic_theme {
            effective_desktop_settings.theme = theme;
        }
        if let Some(style) = self.optimistic_keyboard_style {
            effective_desktop_settings.keyboard_focus_style = style;
        }
        let kbd_style = effective_desktop_settings.keyboard_focus_style;

        let rects = [
            self.tab_rects[0].get(),
            self.tab_rects[1].get(),
            self.tab_rects[2].get(),
            self.tab_rects[3].get(),
        ];
        let tabs_measured = rects.iter().all(|&(_, w)| w > 0.0);
        let active_rect = rects[self.category.index()];
        let rects_capture = Rc::clone(&self.tab_rects);

        let mut tab_bar = div()
            .on_children_prepainted(move |bounds, window, _| {
                // Four in-flow tabs, optionally followed by the absolute indicator.
                let [first, second, third, fourth, ..] = bounds.as_slice() else {
                    return;
                };
                let mut changed = false;
                for (rect, tab) in rects_capture.iter().zip([first, second, third, fourth]) {
                    let next = (
                        f32::from(tab.origin.x - first.origin.x),
                        f32::from(tab.size.width),
                    );
                    changed |= rect.replace(next) != next;
                }
                if changed {
                    window.request_animation_frame();
                }
            })
            .id("settings-tabbar-vpwaf")
            .test_support()
            .role(gpui_kit::Role::TabList)
            .accessibility_id("settings-tabbar-vpwaf")
            .aria_label("设置分类标签栏")
            .track_focus(&self.tab_bar_focus)
            .relative()
            .w_full()
            .h(height::FIELD)
            .border_b_1()
            .border_color(palette.border_subtle)
            .flex()
            .items_center()
            .gap(space::GROUP)
            .on_key_down(cx.listener(Self::handle_tab_bar_key))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.tab_kbd_active
                    && pointer_moved_past_threshold(&mut this.tab_last_pointer, ev.position)
                {
                    this.tab_kbd_active = false;
                    cx.notify();
                }
            }));

        let tab_kbd_active = self.tab_kbd_active && self.tab_bar_focus.is_focused(window);
        for cat in SettingsCategory::ALL {
            let active = self.category == cat;
            let text_el = if active {
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(cat.label()))
            } else {
                apply_scale(div(), scale::BODY)
                    .text_color(palette.text_secondary)
                    .hover(move |s| {
                        if tab_kbd_active {
                            s
                        } else {
                            s.text_color(palette.text_primary)
                        }
                    })
                    .child(cjk_inline_label(cat.label()))
            };

            tab_bar = tab_bar.child(
                div()
                    .id(SharedString::from(format!("settings-tab-{:?}", cat)))
                    .test_support()
                    .a11y_tab(cat.a11y_id(), cat.label(), active)
                    .relative()
                    .h_full()
                    .flex()
                    .items_center()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab_kbd_active = false;
                        this.set_category(cat, cx);
                    }))
                    .child(text_el)
                    .children((active && !tabs_measured).then(|| {
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(-1.0))
                            .h(px(2.0))
                            .bg(palette.accent_solid)
                    })),
            );
        }

        if tabs_measured {
            let ind_x = transition(
                "settings-tab-ind-x",
                active_rect.0,
                Transition::new(motion::NORMAL).ease(motion::ease_in_out()),
                window,
                cx,
            );
            let ind_w = transition(
                "settings-tab-ind-w",
                active_rect.1,
                Transition::new(motion::NORMAL).ease(motion::ease_in_out()),
                window,
                cx,
            );
            tab_bar = tab_bar.child(
                div()
                    .absolute()
                    .left(px(ind_x))
                    .bottom(px(-1.0))
                    .w(px(ind_w))
                    .h(px(2.0))
                    .bg(palette.accent_solid),
            );
        }

        let entity = cx.entity();
        let e_theme = entity.clone();
        let e_kbd_style = entity.clone();
        let e_check = entity.clone();
        let e_cancel = entity.clone();
        let e_gen_key = entity.clone();
        let e_gen_move = entity.clone();

        let content_el = match self.category {
            SettingsCategory::General => render_general_tab(
                &effective_desktop_settings,
                &snap.version,
                snap.update_state.as_deref(),
                mono_family,
                &palette,
                GeneralTabKbdProps {
                    focus_handle: self.general_focus.clone(),
                    kbd_active: self.general_kbd_active && self.general_focus.is_focused(window),
                    row_idx: self.general_row_idx,
                    tracker: self.general_tracker.clone(),
                    on_key_down: Rc::new(move |ev, w, cx| {
                        e_gen_key.update(cx, |this, cx| this.handle_general_key(ev, w, cx));
                    }),
                    on_mouse_move: Rc::new(move |ev, _w, cx| {
                        e_gen_move.update(cx, |this, cx| {
                            if this.general_kbd_active
                                && pointer_moved_past_threshold(
                                    &mut this.general_last_pointer,
                                    ev.position,
                                )
                            {
                                this.general_kbd_active = false;
                                cx.notify();
                            }
                        });
                    }),
                },
                window,
                cx,
                Rc::new(move |theme: ThemeModeDto, _, cx| {
                    e_theme.update(cx, |this, cx| {
                        this.general_kbd_active = false;
                        this.select_theme(theme, cx);
                    });
                }),
                Rc::new(move |style: KeyboardFocusStyleDto, _, cx| {
                    e_kbd_style.update(cx, |this, cx| {
                        this.general_kbd_active = false;
                        this.save_keyboard_style(style, cx);
                    });
                }),
                Rc::new(move |_, cx| {
                    e_check.update(cx, |this, cx| {
                        this.general_kbd_active = false;
                        this.trigger_update_check_or_open(cx);
                    });
                }),
                Rc::new(move |_, cx| {
                    e_cancel.update(cx, |this, cx| {
                        this.general_kbd_active = false;
                        cx.emit(SettingsViewEvent::CancelUpdate);
                    });
                }),
            )
            .into_any_element(),
            SettingsCategory::Providers => self.providers_tab.clone().into_any_element(),
            SettingsCategory::Preferences => self.preferences_tab.clone().into_any_element(),
            SettingsCategory::Shortcuts => {
                let e_short_key = entity.clone();
                render_shortcuts_tab(
                    mono_family,
                    &palette,
                    &self.shortcuts_focus,
                    self.shortcuts_kbd_active && self.shortcuts_focus.is_focused(window),
                    kbd_style,
                    Rc::new(move |ev, w, cx| {
                        e_short_key.update(cx, |this, cx| this.handle_shortcuts_key(ev, w, cx));
                    }),
                )
                .into_any_element()
            }
        };

        let (opacity, offset) = self.page_slide.sample(window, cx);
        let content_wrap = div()
            .relative()
            .left(offset)
            .w_full()
            .opacity(opacity)
            .child(content_el);

        let e_back = entity.clone();
        div()
            .id("settings-view-vpwaf")
            .test_support()
            .a11y_region("settings-view", "设置")
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.consume_composing_escape(window, cx);
            }))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key.as_str() == "tab"
                    && !ev.keystroke.modifiers.control
                    && !ev.keystroke.modifiers.alt
                    && !ev.keystroke.modifiers.platform
                {
                    if this.is_any_input_composing(window, cx) {
                        cx.stop_propagation();
                        window.prevent_default();
                    } else {
                        this.suppress_focus_kbd_for = None;
                    }
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.tab_kbd_active
                    && pointer_moved_past_threshold(&mut this.tab_last_pointer, ev.position)
                {
                    this.tab_kbd_active = false;
                    cx.notify();
                }
            }))
            .child(render_subpage_header_xwjof(
                "settings-subpage-header",
                IconName::Settings,
                "设置",
                None,
                Some(Rc::new(move |_, cx| {
                    e_back.update(cx, |_, cx| cx.emit(SettingsViewEvent::NavigateBack));
                })),
                &palette,
                window,
                cx,
            ))
            .child(
                div()
                    .id("settings-scroll-body")
                    .track_scroll(&self.body_scroll)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .on_scroll(cx.listener(|this, _, window, cx| {
                        this.dismiss_dropdowns_on_scroll(window, cx);
                    }))
                    .px(layout::SUBPAGE_HORIZONTAL_INSET)
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
                            .gap(px(20.0))
                            .child(tab_bar)
                            .child(content_wrap),
                    ),
            )
    }
}

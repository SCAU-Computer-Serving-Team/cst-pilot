pub(super) mod scope;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cst_client::{ClientHandle, ModelChoiceDto, ModelsSnapshot};
use gpui::{
    KeyDownEvent, MouseMoveEvent, Pixels, Point, ScrollStrategy, UniformListScrollHandle,
    uniform_list,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::{ElementExt as _, FocusTrapElement as _};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::kbd_focus::{
    KbdDynamicBoundsTracker, KbdDynamicSpec, KbdMenuStep, KbdRailPlacement,
    pointer_moved_past_threshold, render_gliding_dynamic_indicator, resolve_menu_nav_key,
    scroll_dynamic_target_into_view,
};
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};

use self::scope::ModelScope;

#[derive(Debug, Clone)]
pub enum PreferencesTabEvent {
    Notice(String),
    ErrorMessage(String),
    ReturnToTabBar,
}

#[derive(Clone)]
enum PrefRowItem {
    Enabled {
        ordered_idx: usize,
        full_id: String,
        provider: String,
        name: String,
    },
    Unchecked {
        full_id: String,
        provider: String,
        name: String,
    },
}

const PREF_LIST_ROW_HEIGHT_PX: f32 = 48.0;
const PREF_LIST_MAX_VISIBLE_ROWS: usize = 8;
const PREF_COLLAPSED_MODEL_LIMIT: usize = 5;

pub struct PreferencesTabView {
    client: ClientHandle,
    search_input: Entity<InputState>,
    provider_filter: Option<String>,
    default_dropdown_open: bool,
    filter_dropdown_open: bool,
    models_expanded: bool,
    enabled_scope: ModelScope,
    confirmed_scope: ModelScope,
    last_ready_models: Vec<ModelChoiceDto>,
    optimistic_default: Option<(String, String)>,
    confirmed_default: Option<(String, String)>,
    selected_row_id: Option<String>,
    visible_row_ids: Vec<String>,
    visible_enabled_indices: Vec<usize>,
    visible_filter_key: Option<(Option<String>, String)>,
    observed_models: Option<Arc<ModelsSnapshot>>,
    scope_saving: bool,
    scope_pending: bool,
    scope_revision: u64,
    default_saving: bool,
    pending_default: Option<(String, String)>,
    default_revision: u64,
    refreshing_catalog: bool,
    body_scroll: gpui::ScrollHandle,
    can_collapse_models: bool,
    pending_scroll_to_index: Option<usize>,
    default_trigger_focus: FocusHandle,
    filter_trigger_focus: FocusHandle,
    select_all_focus: FocusHandle,
    refresh_btn_focus: FocusHandle,
    expand_btn_focus: FocusHandle,
    controls_kbd_active: bool,
    suppress_controls_focus_kbd_for: Option<FocusHandle>,
    controls_last_pointer: Option<Point<Pixels>>,
    controls_tracker: KbdDynamicBoundsTracker,
    picker_focus: FocusHandle,
    picker_index: usize,
    picker_keyboard_nav: bool,
    picker_last_pointer: Option<Point<Pixels>>,
    picker_tracker: KbdDynamicBoundsTracker,
    picker_scroll: gpui::ScrollHandle,
    default_picker_scroll: UniformListScrollHandle,
    list_scroll: UniformListScrollHandle,
    list_kbd_active: bool,
    list_last_pointer: Option<Point<Pixels>>,
    list_tracker: KbdDynamicBoundsTracker,
    _subscriptions: Vec<Subscription>,
    focus_handle: FocusHandle,
}

impl EventEmitter<PreferencesTabEvent> for PreferencesTabView {}

impl Focusable for PreferencesTabView {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PreferencesTabView {
    pub fn new(
        client: ClientHandle,
        body_scroll: gpui::ScrollHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索模型…"));
        let mut subscriptions = vec![cx.subscribe(
            &search_input,
            |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    cx.notify();
                }
                InputEvent::Focus => {
                    this.dismiss_dropdowns_on_outside_click(cx);
                }
                _ => {}
            },
        )];
        let default_trigger_focus = cx.focus_handle().tab_stop(true);
        let filter_trigger_focus = cx.focus_handle().tab_stop(true);
        let select_all_focus = cx.focus_handle().tab_stop(true);
        let refresh_btn_focus = cx.focus_handle().tab_stop(true);
        let expand_btn_focus = cx.focus_handle().tab_stop(true);
        for handle in [
            &default_trigger_focus,
            &filter_trigger_focus,
            &select_all_focus,
            &refresh_btn_focus,
            &expand_btn_focus,
        ] {
            let expected = handle.clone();
            subscriptions.push(cx.on_focus(handle, window, move |this, window, cx| {
                let suppressed = this
                    .suppress_controls_focus_kbd_for
                    .take()
                    .is_some_and(|h| h == expected);
                if suppressed {
                    return;
                }
                this.controls_kbd_active = true;
                this.list_kbd_active = false;
                this.controls_last_pointer = Some(window.mouse_position());
                cx.notify();
            }));
        }
        Self {
            client,
            search_input,
            provider_filter: None,
            default_dropdown_open: false,
            filter_dropdown_open: false,
            models_expanded: false,
            enabled_scope: ModelScope::default(),
            confirmed_scope: ModelScope::default(),
            last_ready_models: Vec::new(),
            optimistic_default: None,
            confirmed_default: None,
            selected_row_id: None,
            visible_row_ids: Vec::new(),
            visible_enabled_indices: Vec::new(),
            visible_filter_key: None,
            observed_models: None,
            scope_saving: false,
            scope_pending: false,
            scope_revision: 0,
            default_saving: false,
            pending_default: None,
            default_revision: 0,
            refreshing_catalog: false,
            body_scroll,
            can_collapse_models: false,
            pending_scroll_to_index: None,
            default_trigger_focus,
            filter_trigger_focus,
            select_all_focus,
            refresh_btn_focus,
            expand_btn_focus,
            controls_kbd_active: false,
            suppress_controls_focus_kbd_for: None,
            controls_last_pointer: None,
            controls_tracker: KbdDynamicBoundsTracker::new(),
            picker_focus: cx.focus_handle(),
            picker_index: 0,
            picker_keyboard_nav: false,
            picker_last_pointer: None,
            picker_tracker: KbdDynamicBoundsTracker::new(),
            picker_scroll: gpui::ScrollHandle::new(),
            default_picker_scroll: UniformListScrollHandle::new(),
            list_scroll: UniformListScrollHandle::new(),
            list_kbd_active: false,
            list_last_pointer: None,
            list_tracker: KbdDynamicBoundsTracker::new(),
            _subscriptions: subscriptions,
            focus_handle: cx.focus_handle().tab_stop(true),
        }
    }

    pub fn has_dropdown(&self) -> bool {
        self.default_dropdown_open || self.filter_dropdown_open
    }

    pub fn is_any_input_composing(&self, window: &mut Window, cx: &mut gpui_kit::App) -> bool {
        crate::sessions::input_is_composing(&self.search_input, window, cx)
    }

    pub fn consume_composing_escape(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        crate::sessions::consume_composing_escape(&self.search_input, window, cx);
    }

    fn focus_control_silently(
        &mut self,
        handle: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !handle.is_focused(window) {
            self.suppress_controls_focus_kbd_for = Some(handle.clone());
            handle.focus(window, cx);
        }
    }

    pub fn focus_primary_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.has_dropdown() {
            return false;
        }
        self.controls_kbd_active = false;
        self.list_kbd_active = false;
        self.search_input
            .update(cx, |inp, cx| inp.focus(window, cx));
        cx.notify();
        true
    }

    pub fn focus_from_tabbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.controls_kbd_active = true;
        self.list_kbd_active = false;
        self.controls_last_pointer = Some(window.mouse_position());
        self.default_trigger_focus.focus(window, cx);
        scroll_dynamic_target_into_view(
            &self.body_scroll,
            &self.controls_tracker,
            "pref-default-trigger",
            16.0,
            window,
        );
        cx.notify();
    }

    pub fn leave(&mut self, cx: &mut Context<Self>) {
        self.default_dropdown_open = false;
        self.filter_dropdown_open = false;
        self.picker_keyboard_nav = false;
        self.controls_kbd_active = false;
        self.suppress_controls_focus_kbd_for = None;
        self.list_kbd_active = false;
        self.models_expanded = false;
        self.controls_tracker.cancel_pending_scroll();
        self.list_tracker.cancel_pending_scroll();
        cx.notify();
    }

    pub fn dismiss_topmost(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.has_dropdown() {
            let was_kbd = self.picker_keyboard_nav;
            self.dismiss_dropdowns(window, cx);
            if was_kbd {
                self.controls_kbd_active = true;
                self.controls_last_pointer = Some(window.mouse_position());
                cx.notify();
            }
            return true;
        }
        if self
            .search_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            if !self.search_input.read(cx).value().is_empty() {
                self.search_input
                    .update(cx, |i, cx| i.set_value("", window, cx));
            } else if !self.visible_row_ids.is_empty() {
                let valid = self
                    .selected_row_id
                    .as_deref()
                    .and_then(|id| self.visible_row_ids.iter().position(|v| v == id))
                    .unwrap_or(0);
                self.focus_list_at(valid, window, cx);
            } else {
                self.controls_kbd_active = true;
                self.list_kbd_active = false;
                self.controls_last_pointer = Some(window.mouse_position());
                self.default_trigger_focus.focus(window, cx);
            }
            cx.notify();
            return true;
        }
        false
    }

    fn active_controls_kbd_spec(&self, window: &Window) -> Option<KbdDynamicSpec> {
        if !self.controls_kbd_active || self.has_dropdown() {
            return None;
        }
        if self.default_trigger_focus.is_focused(window) {
            return Some(KbdDynamicSpec::control(
                "pref-default-trigger",
                KbdRailPlacement::BottomCenter,
            ));
        }
        if self.select_all_focus.is_focused(window) {
            return Some(KbdDynamicSpec::control(
                "pref-select-all",
                KbdRailPlacement::BottomCenter,
            ));
        }
        if self.refresh_btn_focus.is_focused(window) {
            return Some(KbdDynamicSpec::control(
                "pref-refresh",
                KbdRailPlacement::BottomCenter,
            ));
        }
        if self.filter_trigger_focus.is_focused(window) {
            return Some(KbdDynamicSpec::control(
                "pref-filter-trigger",
                KbdRailPlacement::BottomCenter,
            ));
        }
        if self.expand_btn_focus.is_focused(window) {
            return Some(KbdDynamicSpec::hairline_row("pref-expand-btn"));
        }
        None
    }

    fn focus_list_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible_row_ids.is_empty() {
            return;
        }
        let clamped = index.min(self.visible_row_ids.len() - 1);
        self.selected_row_id = self.visible_row_ids.get(clamped).cloned();
        self.controls_kbd_active = false;
        self.list_kbd_active = true;
        self.list_last_pointer = Some(window.mouse_position());
        self.focus_handle.focus(window, cx);
        self.list_scroll
            .scroll_to_item(clamped, ScrollStrategy::Nearest);
        self.list_tracker
            .request_scroll(&self.body_scroll, format!("pref-row-{clamped}"), 16.0);
        cx.notify();
    }

    pub fn dismiss_dropdowns(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.has_dropdown() {
            return false;
        }
        let was_kbd = self.picker_keyboard_nav;
        let trigger = if self.default_dropdown_open {
            self.default_trigger_focus.clone()
        } else {
            self.filter_trigger_focus.clone()
        };
        if was_kbd {
            self.suppress_controls_focus_kbd_for = None;
            trigger.focus(window, cx);
        } else {
            self.focus_control_silently(&trigger, window, cx);
        }
        self.dismiss_dropdowns_on_outside_click(cx);
        if was_kbd {
            self.controls_kbd_active = true;
            self.controls_last_pointer = Some(window.mouse_position());
            cx.notify();
        }
        true
    }

    pub fn dismiss_dropdowns_on_scroll(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.has_dropdown() {
            return false;
        }
        let held_focus = self.picker_focus.is_focused(window);
        let trigger = if self.default_dropdown_open {
            self.default_trigger_focus.clone()
        } else {
            self.filter_trigger_focus.clone()
        };
        self.dismiss_dropdowns_on_outside_click(cx);
        if held_focus {
            self.focus_control_silently(&trigger, window, cx);
        }
        true
    }

    pub fn dismiss_dropdowns_on_outside_click(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.has_dropdown() {
            return false;
        }
        self.default_dropdown_open = false;
        self.filter_dropdown_open = false;
        self.picker_keyboard_nav = false;
        cx.notify();
        true
    }

    fn toggle_dropdown(
        &mut self,
        default_model: bool,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if (default_model && self.default_dropdown_open)
            || (!default_model && self.filter_dropdown_open)
        {
            self.dismiss_dropdowns(window, cx);
            return;
        }
        self.default_dropdown_open = default_model;
        self.filter_dropdown_open = !default_model;
        self.picker_index = index;
        self.picker_keyboard_nav = false;
        if default_model {
            self.default_picker_scroll = UniformListScrollHandle::new();
            self.default_picker_scroll
                .scroll_to_item(index, ScrollStrategy::Nearest);
        } else {
            self.picker_scroll = gpui::ScrollHandle::new();
            self.picker_scroll.scroll_to_item(index);
        }
        self.picker_focus.focus(window, cx);
        cx.notify();
    }

    fn enabled_ready_pairs_from_snapshot(
        &self,
        models: Option<&ModelsSnapshot>,
    ) -> Vec<(String, String)> {
        let ready_source: Vec<&ModelChoiceDto> = models
            .into_iter()
            .flat_map(|m| &m.models)
            .filter(|m| m.credential_ready && m.catalog_present)
            .collect();
        let ready_keys: HashSet<(&str, &str)> = if !ready_source.is_empty() {
            ready_source
                .iter()
                .map(|m| (m.provider.as_str(), m.id.as_str()))
                .collect()
        } else {
            self.last_ready_models
                .iter()
                .map(|m| (m.provider.as_str(), m.id.as_str()))
                .collect()
        };
        self.enabled_scope
            .model_rows()
            .filter_map(|(_, full_id)| {
                let (prov, mid) = full_id.split_once('/')?;
                ready_keys
                    .contains(&(prov, mid))
                    .then(|| (prov.to_owned(), mid.to_owned()))
            })
            .collect()
    }

    fn sync_from_snapshot(&mut self) {
        if self.scope_saving || self.scope_pending {
            return;
        }
        let snap = self.client.snapshot();
        let Some(models) = snap.models.as_ref() else {
            return;
        };
        if self
            .observed_models
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, models))
        {
            return;
        }
        let has_ready = models
            .models
            .iter()
            .any(|m| m.credential_ready && m.catalog_present);
        if self.refreshing_catalog && !has_ready && !self.last_ready_models.is_empty() {
            // Ignore transient empty snapshots emitted mid-refresh to prevent list flicker.
            return;
        }
        self.observed_models = Some(models.clone());
        let next_scope = ModelScope::from_snapshot(models);
        self.confirmed_scope = next_scope.clone();
        if next_scope != self.enabled_scope {
            self.enabled_scope = next_scope;
        }
        if has_ready {
            self.last_ready_models = models
                .models
                .iter()
                .filter(|m| m.credential_ready && m.catalog_present)
                .cloned()
                .collect();
        } else if !self.refreshing_catalog {
            self.last_ready_models.clear();
        }
        if !self.default_saving && self.pending_default.is_none() {
            self.confirmed_default = models
                .default_model
                .as_ref()
                .map(|d| (d.provider.clone(), d.id.clone()));
            if let Some((ref opt_p, ref opt_m)) = self.optimistic_default
                && models
                    .default_model
                    .as_ref()
                    .is_some_and(|d| &d.provider == opt_p && &d.id == opt_m)
            {
                self.optimistic_default = None;
            } else if !self.refreshing_catalog {
                self.optimistic_default = None;
            }
        }
    }

    fn compute_auto_default(
        &self,
        snap_models: Option<&ModelsSnapshot>,
    ) -> Option<(String, String)> {
        let enabled_pairs = self.enabled_ready_pairs_from_snapshot(snap_models);
        let current_default = self.optimistic_default.clone().or_else(|| {
            snap_models
                .and_then(|m| m.default_model.as_ref())
                .map(|d| (d.provider.clone(), d.id.clone()))
        });
        match (current_default, enabled_pairs.first()) {
            (Some(cur), Some(first)) if !enabled_pairs.contains(&cur) => Some(first.clone()),
            (None, Some(first)) => Some(first.clone()),
            _ => None,
        }
    }

    pub(super) fn current_scope(&self) -> Option<ModelScope> {
        (self.observed_models.is_some() || self.scope_saving || self.scope_pending)
            .then(|| self.enabled_scope.clone())
    }

    pub(super) fn mirror_external_scope(&mut self, scope: ModelScope, cx: &mut Context<Self>) {
        if self.scope_saving || self.scope_pending {
            return;
        }
        if self.enabled_scope != scope {
            self.enabled_scope = scope;
            cx.notify();
        }
    }

    fn enqueue_scope_save(&mut self, cx: &mut Context<Self>) {
        let snap = self.client.snapshot();
        if !self.default_saving
            && self.pending_default.is_none()
            && let Some(next_def) = self.compute_auto_default(snap.models.as_deref())
        {
            self.optimistic_default = Some(next_def);
            self.default_revision = self.default_revision.wrapping_add(1);
        }
        self.scope_revision = self.scope_revision.wrapping_add(1);
        self.scope_pending = true;
        cx.notify();

        if self.scope_saving {
            return;
        }
        self.scope_saving = true;
        let client = self.client.clone();

        cx.spawn(async move |weak, cx| {
            loop {
                let step = weak.update(cx, |this, _| {
                    if !this.scope_pending {
                        return None;
                    }
                    this.scope_pending = false;
                    let rev = this.scope_revision;
                    let def_rev = this.default_revision;
                    let scope_snapshot = this.enabled_scope.clone();
                    let patterns = scope_snapshot.patterns();
                    let snap = this.client.snapshot();
                    let auto_default = if this.default_saving || this.pending_default.is_some() {
                        None
                    } else {
                        this.compute_auto_default(snap.models.as_deref())
                            .or_else(|| {
                                let cur = this.optimistic_default.as_ref()?;
                                let snap_matches = snap
                                    .models
                                    .as_ref()
                                    .and_then(|m| m.default_model.as_ref())
                                    .is_some_and(|d| d.provider == cur.0 && d.id == cur.1);
                                (!snap_matches).then(|| cur.clone())
                            })
                    };
                    Some((rev, def_rev, scope_snapshot, patterns, auto_default))
                });
                let Ok(Some((rev, def_rev, saved_scope, patterns, auto_default))) = step else {
                    let _ = weak.update(cx, |this, cx| {
                        this.scope_saving = false;
                        cx.notify();
                    });
                    break;
                };

                let res = client.set_explicit_model_scope(patterns).await;
                let should_sync_default = res.is_ok()
                    && weak
                        .update(cx, |this, _| {
                            !this.default_saving
                                && this.pending_default.is_none()
                                && this.default_revision == def_rev
                        })
                        .unwrap_or(false);
                let default_res = if should_sync_default {
                    if let Some((prov, mid)) = auto_default.clone() {
                        Some(client.set_default_model(prov, mid).await)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let action = weak
                    .update(cx, |this, cx| {
                        if res.is_ok() {
                            this.confirmed_scope = saved_scope;
                            if let Some(ref def) = auto_default
                                && matches!(default_res, Some(Ok(_)))
                                && !this.default_saving
                                && this.pending_default.is_none()
                                && this.default_revision == def_rev
                            {
                                this.confirmed_default = Some(def.clone());
                            }
                        }
                        if this.scope_pending || this.scope_revision != rev {
                            return 1u8; // continue immediately to flush newer edits
                        }
                        if let Err(err) = res {
                            this.enabled_scope = this.confirmed_scope.clone();
                            if !this.default_saving && this.pending_default.is_none() {
                                this.optimistic_default = this.confirmed_default.clone();
                            }
                            this.scope_saving = false;
                            cx.emit(PreferencesTabEvent::ErrorMessage(format!(
                                "保存启用模型范围失败，已自动回滚：{err}"
                            )));
                            cx.notify();
                            return 0u8; // stop after rollback
                        }
                        if let Some(Err(err)) = default_res {
                            if !this.default_saving && this.pending_default.is_none() {
                                this.optimistic_default = this.confirmed_default.clone();
                            }
                            cx.emit(PreferencesTabEvent::ErrorMessage(format!(
                                "同步默认模型失败：{err}"
                            )));
                        }
                        2u8 // queue drained cleanly, perform single final refresh
                    })
                    .unwrap_or(0);

                if action == 1 {
                    continue;
                }
                if action == 0 {
                    break;
                }

                let refreshed = client.refresh_models(None).await.is_ok();
                let again = weak
                    .update(cx, |this, cx| {
                        if this.scope_pending || this.scope_revision != rev {
                            return true;
                        }
                        this.scope_saving = false;
                        if refreshed {
                            this.sync_from_snapshot();
                        }
                        cx.notify();
                        false
                    })
                    .unwrap_or(false);
                if !again {
                    break;
                }
            }
        })
        .detach();
    }

    fn toggle_model_enabled(&mut self, full_id: String, cx: &mut Context<Self>) {
        self.enabled_scope.toggle(full_id);
        self.enqueue_scope_save(cx);
    }

    fn select_all_visible(&mut self, visible_full_ids: Vec<String>, cx: &mut Context<Self>) {
        if self.enabled_scope.select(visible_full_ids) {
            self.enqueue_scope_save(cx);
        }
    }

    fn clear_visible(&mut self, visible_full_ids: Vec<String>, cx: &mut Context<Self>) {
        if self
            .enabled_scope
            .clear(visible_full_ids.iter().map(String::as_str))
        {
            self.enqueue_scope_save(cx);
        }
    }

    fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        if self.refreshing_catalog {
            return;
        }
        self.refreshing_catalog = true;
        cx.notify();
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.refresh_models_catalog().await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |this, cx| {
                this.refreshing_catalog = false;
                this.sync_from_snapshot();
                match res {
                    Ok(_) => cx.emit(PreferencesTabEvent::Notice("模型列表已刷新。".into())),
                    Err(err) => cx.emit(PreferencesTabEvent::ErrorMessage(err.to_string())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn move_enabled_row(&mut self, idx: usize, delta: isize, cx: &mut Context<Self>) {
        let Some(position) = self.visible_enabled_indices.iter().position(|&i| i == idx) else {
            return;
        };
        let Some(target_position) = position.checked_add_signed(delta) else {
            return;
        };
        let Some(&target) = self.visible_enabled_indices.get(target_position) else {
            return;
        };
        let selected = self.enabled_scope.model_id_at(idx).map(str::to_owned);
        if !self.enabled_scope.swap(idx, target) {
            return;
        }
        self.selected_row_id = selected;
        self.list_scroll
            .scroll_to_item(target_position, ScrollStrategy::Nearest);
        self.list_tracker.request_scroll(
            &self.body_scroll,
            format!("pref-row-{target_position}"),
            16.0,
        );
        self.enqueue_scope_save(cx);
    }

    fn set_default_model(
        &mut self,
        provider: String,
        model_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dismiss_dropdowns(window, cx) {
            return;
        }
        self.optimistic_default = Some((provider.clone(), model_id.clone()));
        self.pending_default = Some((provider, model_id));
        self.default_revision = self.default_revision.wrapping_add(1);
        cx.notify();

        if self.default_saving {
            return;
        }
        self.default_saving = true;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            loop {
                let next = weak.update(cx, |this, _| {
                    let target = this.pending_default.take()?;
                    Some((this.default_revision, target))
                });
                let Ok(Some((rev, (prov, mid)))) = next else {
                    let _ = weak.update(cx, |this, cx| {
                        this.default_saving = false;
                        cx.notify();
                    });
                    break;
                };

                let res = client.set_default_model(prov.clone(), mid.clone()).await;
                let action = weak
                    .update(cx, |this, cx| {
                        if res.is_ok() {
                            this.confirmed_default = Some((prov, mid));
                        }
                        if this.pending_default.is_some() || this.default_revision != rev {
                            return 1u8;
                        }
                        if let Err(err) = res {
                            if !this.scope_saving && !this.scope_pending {
                                this.optimistic_default = this.confirmed_default.clone();
                            }
                            this.default_saving = false;
                            cx.emit(PreferencesTabEvent::ErrorMessage(err.to_string()));
                            cx.notify();
                            return 0u8;
                        }
                        2u8
                    })
                    .unwrap_or(0);

                if action == 1 {
                    continue;
                }
                if action == 0 {
                    break;
                }

                let refreshed = client.refresh_models(None).await.is_ok();
                let again = weak
                    .update(cx, |this, cx| {
                        if this.pending_default.is_some() || this.default_revision != rev {
                            return true;
                        }
                        this.default_saving = false;
                        if refreshed {
                            this.sync_from_snapshot();
                        }
                        cx.notify();
                        false
                    })
                    .unwrap_or(false);
                if !again {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_dropdown() || !self.focus_handle.is_focused(window) {
            return;
        }
        let ks = &event.keystroke;
        if self.visible_row_ids.is_empty() {
            if ks.modifiers == gpui::Modifiers::default() && ks.key.as_str() == "up" {
                self.list_kbd_active = false;
                self.controls_kbd_active = false;
                self.search_input.update(cx, |i, cx| i.focus(window, cx));
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let cur_vis_idx = self
            .selected_row_id
            .as_deref()
            .and_then(|id| self.visible_row_ids.iter().position(|x| x == id))
            .unwrap_or(0);
        if ks.modifiers == gpui::Modifiers::default() {
            if ks.key.as_str() == "up" && cur_vis_idx == 0 {
                self.list_kbd_active = false;
                self.controls_kbd_active = false;
                self.search_input.update(cx, |i, cx| i.focus(window, cx));
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if ks.key.as_str() == "down"
                && cur_vis_idx + 1 == self.visible_row_ids.len()
                && self.can_collapse_models
            {
                self.list_kbd_active = false;
                self.controls_kbd_active = true;
                self.controls_last_pointer = Some(window.mouse_position());
                self.expand_btn_focus.focus(window, cx);
                scroll_dynamic_target_into_view(
                    &self.body_scroll,
                    &self.controls_tracker,
                    "pref-expand-btn",
                    16.0,
                    window,
                );
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        match resolve_menu_nav_key(
            ks.key.as_str(),
            ks.modifiers,
            event.is_held,
            cur_vis_idx,
            self.visible_row_ids.len(),
            false,
        ) {
            KbdMenuStep::Move(next) => {
                self.selected_row_id = self.visible_row_ids.get(next).cloned();
                self.controls_kbd_active = false;
                self.list_kbd_active = true;
                self.list_last_pointer = Some(window.mouse_position());
                self.list_scroll
                    .scroll_to_item(next, ScrollStrategy::Nearest);
                self.list_tracker.request_scroll(
                    &self.body_scroll,
                    format!("pref-row-{next}"),
                    16.0,
                );
                cx.stop_propagation();
                cx.notify();
            }
            KbdMenuStep::Activate(idx) => {
                if let Some(fid) = self.visible_row_ids.get(idx).cloned() {
                    self.selected_row_id = Some(fid.clone());
                    self.controls_kbd_active = false;
                    self.list_kbd_active = true;
                    self.list_last_pointer = Some(window.mouse_position());
                    self.toggle_model_enabled(fid, cx);
                    cx.stop_propagation();
                }
            }
            KbdMenuStep::ReorderUp | KbdMenuStep::ReorderDown => {
                let Some(idx) = self.visible_enabled_indices.iter().copied().find(|&i| {
                    self.enabled_scope.model_id_at(i) == self.selected_row_id.as_deref()
                }) else {
                    return;
                };
                self.controls_kbd_active = false;
                self.list_kbd_active = true;
                self.list_last_pointer = Some(window.mouse_position());
                let delta = if ks.key.as_str() == "up" { -1 } else { 1 };
                self.move_enabled_row(idx, delta, cx);
                cx.stop_propagation();
            }
            KbdMenuStep::SubRight | KbdMenuStep::BackLeft | KbdMenuStep::Ignore => {}
        }
    }
}

impl Render for PreferencesTabView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_from_snapshot();
        self.picker_tracker.clear();
        self.list_tracker.clear();
        self.controls_tracker.clear();
        let controls_kbd = self.controls_kbd_active;
        let tr_default = self.controls_tracker.clone();
        let tr_filter = self.controls_tracker.clone();
        let tr_select_all = self.controls_tracker.clone();
        let tr_refresh = self.controls_tracker.clone();
        let tr_expand = self.controls_tracker.clone();
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let snap = self.client.snapshot();
        let kbd_style = snap.desktop_settings.keyboard_focus_style;
        let models_snap = snap.models.as_ref();

        let snap_ready: Vec<&ModelChoiceDto> = models_snap
            .into_iter()
            .flat_map(|m| &m.models)
            .filter(|m| m.credential_ready && m.catalog_present)
            .collect();
        let ready_models: Vec<&ModelChoiceDto> = if snap_ready.is_empty()
            && self.refreshing_catalog
            && !self.last_ready_models.is_empty()
        {
            self.last_ready_models.iter().collect()
        } else {
            snap_ready
        };
        let mut ready_by_key: HashMap<(&str, &str), &ModelChoiceDto> =
            HashMap::with_capacity(ready_models.len());
        for m in &ready_models {
            ready_by_key.insert((m.provider.as_str(), m.id.as_str()), *m);
        }

        // Default model choices strictly match the enabled models below in the same order.
        let enabled_ready_models: Vec<&ModelChoiceDto> = self
            .enabled_scope
            .model_rows()
            .filter_map(|(_, full_id)| {
                let (prov, mid) = full_id.split_once('/')?;
                ready_by_key.get(&(prov, mid)).copied()
            })
            .collect();

        let requested_default: Option<(&str, &str)> = self
            .optimistic_default
            .as_ref()
            .map(|(p, m)| (p.as_str(), m.as_str()))
            .or_else(|| {
                models_snap
                    .and_then(|m| m.default_model.as_ref())
                    .map(|d| (d.provider.as_str(), d.id.as_str()))
            });
        let effective_default: Option<&ModelChoiceDto> = requested_default
            .and_then(|(prov, mid)| {
                enabled_ready_models
                    .iter()
                    .copied()
                    .find(|m| m.provider == prov && m.id == mid)
            })
            .or_else(|| enabled_ready_models.first().copied());

        let default_label = effective_default
            .map(|m| {
                if m.name.trim().is_empty() {
                    m.id.clone()
                } else {
                    m.name.clone()
                }
            })
            .unwrap_or_else(|| "未选择默认模型".to_owned());
        let default_provider = effective_default
            .map(|m| m.provider.clone())
            .unwrap_or_default();

        // 1. Frameless 48px hairline row for 新会话默认模型 (`lTWwV` / `psK3f`)
        let choices: Vec<_> = enabled_ready_models
            .iter()
            .map(|m| (m.provider.clone(), m.id.clone()))
            .collect();
        let default_index = effective_default
            .and_then(|d| {
                choices
                    .iter()
                    .position(|(p, m)| p == &d.provider && m == &d.id)
            })
            .unwrap_or(0);
        let mut dropdown_menu_opt = None;
        if self.default_dropdown_open {
            let picker_kbd = self.picker_keyboard_nav;
            let active_pop_spec = (picker_kbd && !choices.is_empty()).then(|| {
                KbdDynamicSpec::row(format!("def-{}", self.picker_index.min(choices.len() - 1)))
            });
            let items = if enabled_ready_models.is_empty() {
                div()
                    .id("pref-default-model-scroll")
                    .w_full()
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .p(space::SM)
                            .text_color(palette.text_muted)
                            .child(if ready_models.is_empty() {
                                "暂无已登录服务的可用模型"
                            } else {
                                "请先在下方启用模型"
                            }),
                    )
                    .into_any_element()
            } else {
                let default_rows: Arc<Vec<(String, String, String)>> = Arc::new(
                    enabled_ready_models
                        .iter()
                        .map(|m| {
                            let label = if m.name.trim().is_empty() {
                                m.id.clone()
                            } else {
                                m.name.clone()
                            };
                            (m.provider.clone(), m.id.clone(), label)
                        })
                        .collect(),
                );
                let row_count = default_rows.len();
                let list_h = px((row_count as f32 * 34.0).min(280.0));
                let current_def_pair =
                    effective_default.map(|d| (d.provider.clone(), d.id.clone()));
                uniform_list(
                    "pref-default-model-scroll",
                    row_count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|idx| {
                                let (prov, mid, mname) = &default_rows[idx];
                                let is_selected = current_def_pair
                                    .as_ref()
                                    .is_some_and(|(p, m)| p == prov && m == mid);
                                let prov_c = prov.clone();
                                let mid_c = mid.clone();
                                let tr = this.picker_tracker.clone();
                                let key = SharedString::from(format!("def-{idx}"));
                                let kbd_nav = this.picker_keyboard_nav;
                                let opt_id =
                                    SharedString::from(format!("pref-default-model-{prov}-{mid}"));
                                let mname_label = SharedString::from(mname.clone());
                                let prov_label = SharedString::from(prov.clone());
                                let opt_aria = SharedString::from(format!("{prov} · {mname}"));
                                div().w_full().h(px(34.0)).py(px(1.0)).child(
                                    apply_scale(div(), scale::UI)
                                        .id(opt_id.clone())
                                        .test_support()
                                        .a11y_listitem(opt_id, opt_aria, is_selected)
                                        .flex_shrink_0()
                                        .w_full()
                                        .h(height::CONTROL)
                                        .px(px(10.0))
                                        .rounded(radius::CONTROL)
                                        .text_color(palette.text_primary)
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(space::SM)
                                        .hover(move |s| {
                                            if kbd_nav {
                                                s
                                            } else {
                                                s.bg(palette.bg_item_hover)
                                            }
                                        })
                                        .active(|s| s.bg(palette.bg_active_press))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.set_default_model(
                                                prov_c.clone(),
                                                mid_c.clone(),
                                                window,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        }))
                                        .on_prepaint(move |bounds, _, _| {
                                            tr.set(key.clone(), bounds)
                                        })
                                        .child(
                                            div().flex_1().min_w_0().truncate().child(mname_label),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(space::TIGHT)
                                                .flex_shrink_0()
                                                .child(
                                                    apply_scale(div(), scale::CAPTION)
                                                        .text_color(palette.text_muted)
                                                        .child(prov_label),
                                                )
                                                .children(is_selected.then(|| {
                                                    Icon::new(IconName::Check)
                                                        .size(px(13.0))
                                                        .text_color(palette.accent_ink)
                                                })),
                                        ),
                                )
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.default_picker_scroll)
                .w_full()
                .h(list_h)
                .into_any_element()
            };
            let menu = motion::animate_popover_enter(
                "pref-default-model-pop-anim",
                div()
                    .id("pref-default-model-popover")
                    .test_support()
                    .a11y_dialog("pref-default-model-popover", "选择默认模型")
                    .relative()
                    .w(px(320.0))
                    .p(space::XS)
                    .rounded(radius::ROW)
                    .bg(palette.bg_glass)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .occlude()
                    .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                        if this.picker_keyboard_nav
                            && pointer_moved_past_threshold(
                                &mut this.picker_last_pointer,
                                ev.position,
                            )
                        {
                            this.picker_keyboard_nav = false;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .focus_trap("pref-default-model-focus", &self.picker_focus)
                            .on_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, window, cx| {
                                    if !choices.is_empty()
                                        && event.keystroke.modifiers == gpui::Modifiers::default()
                                    {
                                        let cur = this.picker_index.min(choices.len() - 1);
                                        let moved = match event.keystroke.key.as_str() {
                                            "down" => {
                                                this.picker_index = (cur + 1) % choices.len();
                                                true
                                            }
                                            "up" => {
                                                this.picker_index =
                                                    cur.checked_sub(1).unwrap_or(choices.len() - 1);
                                                true
                                            }
                                            "home" => {
                                                this.picker_index = 0;
                                                true
                                            }
                                            "end" => {
                                                this.picker_index = choices.len() - 1;
                                                true
                                            }
                                            _ => false,
                                        };
                                        if moved {
                                            this.picker_keyboard_nav = true;
                                            this.picker_last_pointer =
                                                Some(window.mouse_position());
                                            this.default_picker_scroll.scroll_to_item(
                                                this.picker_index,
                                                ScrollStrategy::Nearest,
                                            );
                                            cx.notify();
                                            cx.stop_propagation();
                                            return;
                                        }
                                    }
                                    if super::activates_button(event) {
                                        if !choices.is_empty() {
                                            let idx = this.picker_index.min(choices.len() - 1);
                                            this.picker_index = idx;
                                            if let Some((provider, model)) = choices.get(idx) {
                                                this.set_default_model(
                                                    provider.clone(),
                                                    model.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                        }
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .child(items),
                    )
                    .child(render_gliding_dynamic_indicator(
                        "pref-default-pop",
                        kbd_style,
                        active_pop_spec,
                        self.picker_tracker.clone(),
                        &palette,
                        window,
                        cx,
                    )),
                palette.flat_popover_shadows(),
                -6.0,
            );
            dropdown_menu_opt = Some(
                gpui::deferred(div().absolute().top(px(46.0)).right(px(-10.0)).child(menu))
                    .with_priority(20),
            );
        }

        let default_row = div()
            .w_full()
            .h(px(48.0))
            .border_b_1()
            .border_color(palette.border_subtle)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::LG)
            .child(
                apply_scale(div(), scale::BODY)
                    .text_color(palette.text_primary)
                    .child("新会话默认模型"),
            )
            .child(
                div()
                    .id("pref-default-model-trigger")
                    .test_support()
                    .a11y_toggle_button(
                        "pref-default-model-trigger",
                        format!("新会话默认模型：{default_label}"),
                        self.default_dropdown_open,
                    )
                    .track_focus(&self.default_trigger_focus)
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let alt_down = !event.is_held
                            && event.keystroke.key.as_str() == "down"
                            && event.keystroke.modifiers.alt
                            && !event.keystroke.modifiers.control
                            && !event.keystroke.modifiers.shift
                            && !event.keystroke.modifiers.platform;
                        if alt_down {
                            this.toggle_dropdown(true, default_index, window, cx);
                            this.picker_keyboard_nav = true;
                            this.picker_last_pointer = Some(window.mouse_position());
                            cx.stop_propagation();
                        } else if event.keystroke.modifiers == gpui::Modifiers::default() {
                            match event.keystroke.key.as_str() {
                                "up" => {
                                    this.controls_kbd_active = false;
                                    cx.emit(PreferencesTabEvent::ReturnToTabBar);
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                                "down" => {
                                    this.controls_kbd_active = false;
                                    this.list_kbd_active = false;
                                    this.search_input.update(cx, |i, cx| i.focus(window, cx));
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                                _ => {}
                            }
                        }
                    }))
                    .h(height::COMPACT)
                    .px(px(10.0))
                    .mr(px(-10.0))
                    .rounded(radius::CONTROL)
                    .bg(if self.default_dropdown_open {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .flex()
                    .items_center()
                    .gap(space::TIGHT)
                    .hover(move |s| {
                        if controls_kbd {
                            s
                        } else {
                            s.bg(palette.bg_item_hover)
                        }
                    })
                    .active(|s| s.bg(palette.bg_active_press))
                    .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                        let from_kbd = matches!(ev, gpui::ClickEvent::Keyboard(_));
                        this.controls_kbd_active = from_kbd;
                        this.controls_last_pointer = from_kbd.then(|| window.mouse_position());
                        this.toggle_dropdown(true, default_index, window, cx);
                        if from_kbd && this.default_dropdown_open {
                            this.picker_keyboard_nav = true;
                            this.picker_last_pointer = Some(window.mouse_position());
                            cx.notify();
                        }
                    }))
                    .on_prepaint(move |bounds, _, _| tr_default.set("pref-default-trigger", bounds))
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .max_w(px(280.0))
                            .truncate()
                            .text_color(palette.text_primary)
                            .child(SharedString::from(default_label)),
                    )
                    .children((!default_provider.is_empty()).then(|| {
                        apply_scale(div(), scale::CAPTION)
                            .pt(px(2.0))
                            .text_color(palette.text_muted)
                            .child(SharedString::from(default_provider))
                    }))
                    .child(div().ml(px(2.0)).child(motion::animated_chevron(
                        "pref-default-model",
                        self.default_dropdown_open,
                        true,
                        px(13.0),
                        palette.text_secondary,
                        window,
                        cx,
                    ))),
            );

        // 2. Provider filter dropdown & frameless 40px search bar
        let mut providers_seen = Vec::new();
        let mut providers_seen_set = HashSet::new();
        for m in &ready_models {
            if providers_seen_set.insert(m.provider.as_str()) {
                providers_seen.push(m.provider.clone());
            }
        }
        let active_provider_filter = self
            .provider_filter
            .as_deref()
            .filter(|pf| providers_seen_set.contains(*pf));
        let filter_label = active_provider_filter.unwrap_or("全部提供商").to_owned();

        let filter_index = providers_seen
            .iter()
            .position(|p| Some(p.as_str()) == active_provider_filter)
            .map_or(0, |index| index + 1);
        let filter_menu_opt = self.filter_dropdown_open.then(|| {
            let choices = providers_seen.clone();
            let kbd_nav = self.picker_keyboard_nav;
            let active_filter_spec = kbd_nav.then(|| {
                KbdDynamicSpec::row(format!("flt-{}", self.picker_index.min(choices.len())))
            });
            let tr_all = self.picker_tracker.clone();
            let mut items = div()
                .id("pref-filter-scroll")
                .track_scroll(&self.picker_scroll)
                .w_full()
                .max_h(px(260.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(space::MICRO)
                .child(
                    apply_scale(div(), scale::UI)
                        .id("pref-filter-all")
                        .test_support()
                        .a11y_listitem(
                            "pref-filter-all",
                            "全部提供商",
                            active_provider_filter.is_none(),
                        )
                        .flex_shrink_0()
                        .h(height::CONTROL)
                        .px(px(10.0))
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::SM)
                        .hover(move |s| {
                            if kbd_nav {
                                s
                            } else {
                                s.bg(palette.bg_item_hover)
                            }
                        })
                        .active(|s| s.bg(palette.bg_active_press))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.provider_filter = None;
                            this.dismiss_dropdowns(window, cx);
                        }))
                        .on_prepaint(move |bounds, _, _| tr_all.set("flt-0", bounds))
                        .child(cjk_inline_label("全部提供商"))
                        .children(active_provider_filter.is_none().then(|| {
                            Icon::new(IconName::Check)
                                .size(px(13.0))
                                .text_color(palette.accent_ink)
                        })),
                );

            for (index, prov) in providers_seen.iter().enumerate() {
                let is_selected = active_provider_filter == Some(prov.as_str());
                let p_click = prov.clone();
                let tr = self.picker_tracker.clone();
                let key = SharedString::from(format!("flt-{}", index + 1));
                let filter_id = SharedString::from(format!("pref-filter-{prov}"));
                let prov_label = SharedString::from(prov.clone());
                items = items.child(
                    apply_scale(div(), scale::UI)
                        .id(filter_id.clone())
                        .test_support()
                        .a11y_listitem(filter_id, prov_label.clone(), is_selected)
                        .flex_shrink_0()
                        .h(height::CONTROL)
                        .px(px(10.0))
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::SM)
                        .hover(move |s| {
                            if kbd_nav {
                                s
                            } else {
                                s.bg(palette.bg_item_hover)
                            }
                        })
                        .active(|s| s.bg(palette.bg_active_press))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.provider_filter = Some(p_click.clone());
                            this.dismiss_dropdowns(window, cx);
                        }))
                        .on_prepaint(move |bounds, _, _| tr.set(key.clone(), bounds))
                        .child(div().flex_1().min_w_0().truncate().child(prov_label))
                        .children(is_selected.then(|| {
                            Icon::new(IconName::Check)
                                .size(px(13.0))
                                .text_color(palette.accent_ink)
                        })),
                );
            }

            gpui::deferred(
                div().absolute().top(px(42.0)).right(px(-10.0)).child(
                    motion::animate_popover_enter(
                        "pref-filter-pop-anim",
                        div()
                            .id("pref-filter-popover")
                            .test_support()
                            .a11y_dialog("pref-filter-popover", "按提供商筛选")
                            .relative()
                            .w(px(220.0))
                            .p(space::XS)
                            .rounded(radius::ROW)
                            .bg(palette.bg_glass)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .occlude()
                            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                                if this.picker_keyboard_nav
                                    && pointer_moved_past_threshold(
                                        &mut this.picker_last_pointer,
                                        ev.position,
                                    )
                                {
                                    this.picker_keyboard_nav = false;
                                    cx.notify();
                                }
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .focus_trap("pref-filter-focus", &self.picker_focus)
                                    .on_key_down(cx.listener(
                                        move |this, event: &KeyDownEvent, window, cx| {
                                            if super::move_picker_cursor(
                                                event,
                                                &mut this.picker_index,
                                                choices.len() + 1,
                                                &this.picker_scroll,
                                            ) {
                                                this.picker_keyboard_nav = true;
                                                this.picker_last_pointer =
                                                    Some(window.mouse_position());
                                                cx.notify();
                                                cx.stop_propagation();
                                            } else if super::activates_button(event) {
                                                let clamped = this.picker_index.min(choices.len());
                                                this.picker_index = clamped;
                                                this.provider_filter = clamped
                                                    .checked_sub(1)
                                                    .and_then(|ix| choices.get(ix))
                                                    .cloned();
                                                this.dismiss_dropdowns(window, cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    ))
                                    .child(items),
                            )
                            .child(render_gliding_dynamic_indicator(
                                "pref-filter-pop",
                                kbd_style,
                                active_filter_spec,
                                self.picker_tracker.clone(),
                                &palette,
                                window,
                                cx,
                            )),
                        palette.flat_popover_shadows(),
                        -6.0,
                    ),
                ),
            )
            .with_priority(20)
        });

        let search_q = self.search_input.read(cx).value().trim().to_lowercase();
        let filter_key = (active_provider_filter.map(str::to_owned), search_q.clone());
        if self.visible_filter_key.as_ref() != Some(&filter_key) {
            self.visible_filter_key = Some(filter_key);
            self.list_scroll = UniformListScrollHandle::new();
        }
        let matches_query = |prov: &str, mid: &str, mname: &str| -> bool {
            if let Some(pf) = active_provider_filter
                && prov != pf
            {
                return false;
            }
            if !search_q.is_empty() {
                let hay_id = mid.to_lowercase();
                let hay_nm = mname.to_lowercase();
                let hay_pr = prov.to_lowercase();
                return hay_id.contains(&search_q)
                    || hay_nm.contains(&search_q)
                    || hay_pr.contains(&search_q);
            }
            true
        };

        let mut visible_rows: Vec<PrefRowItem> = Vec::with_capacity(ready_models.len());
        let mut visible_full_ids: Vec<String> = Vec::with_capacity(ready_models.len());
        let mut enabled_ready_keys: HashSet<(&str, &str)> =
            HashSet::with_capacity(ready_models.len());
        let mut enabled_ready_count = 0usize;

        let mut visible_enabled_indices = Vec::new();
        for (ordered_idx, full_id) in self.enabled_scope.model_rows() {
            let Some((prov, mid)) = full_id.split_once('/') else {
                continue;
            };
            let Some(m) = ready_by_key.get(&(prov, mid)).copied() else {
                continue;
            };
            enabled_ready_keys.insert((prov, mid));
            enabled_ready_count += 1;
            let mname = if m.name.trim().is_empty() {
                m.id.as_str()
            } else {
                m.name.as_str()
            };
            if matches_query(prov, mid, mname) {
                visible_full_ids.push(full_id.to_owned());
                visible_enabled_indices.push(ordered_idx);
                visible_rows.push(PrefRowItem::Enabled {
                    ordered_idx,
                    full_id: full_id.to_owned(),
                    provider: prov.to_owned(),
                    name: mname.to_owned(),
                });
            }
        }
        for m in &ready_models {
            if enabled_ready_keys.contains(&(m.provider.as_str(), m.id.as_str())) {
                continue;
            }
            let mname = if m.name.trim().is_empty() {
                m.id.as_str()
            } else {
                m.name.as_str()
            };
            if matches_query(&m.provider, &m.id, mname) {
                let full_id = format!("{}/{}", m.provider, m.id);
                visible_full_ids.push(full_id.clone());
                visible_rows.push(PrefRowItem::Unchecked {
                    full_id,
                    provider: m.provider.clone(),
                    name: mname.to_owned(),
                });
            }
        }

        let total_matching_rows = visible_rows.len();
        let filtering_active = active_provider_filter.is_some() || !search_q.is_empty();
        let can_collapse_models =
            !filtering_active && total_matching_rows > PREF_COLLAPSED_MODEL_LIMIT;
        let display_row_count = if can_collapse_models && !self.models_expanded {
            PREF_COLLAPSED_MODEL_LIMIT
        } else {
            total_matching_rows
        };

        // Merged single 全选 / 清空 toggle per finalized lTWwV design
        let all_visible_enabled =
            !visible_full_ids.is_empty() && visible_enabled_indices.len() == visible_full_ids.len();
        let vis_for_toggle = visible_full_ids.clone();

        // Restrict keyboard/button reorder boundaries to the currently rendered preview slice
        // so Alt+Up/Down never pushes the focused row out of the collapsed preview.
        self.can_collapse_models = can_collapse_models;
        self.visible_row_ids = visible_full_ids
            .iter()
            .take(display_row_count)
            .cloned()
            .collect();
        self.visible_enabled_indices = visible_enabled_indices
            .into_iter()
            .take(display_row_count)
            .collect();
        if let Some(scroll_idx) = self.pending_scroll_to_index.take()
            && !self.visible_row_ids.is_empty()
        {
            let clamped = scroll_idx.min(self.visible_row_ids.len() - 1);
            self.selected_row_id = self.visible_row_ids.get(clamped).cloned();
            self.list_scroll
                .scroll_to_item(clamped, ScrollStrategy::Nearest);
            self.list_tracker.request_scroll(
                &self.body_scroll,
                format!("pref-row-{clamped}"),
                16.0,
            );
        }
        if self
            .selected_row_id
            .as_deref()
            .is_none_or(|id| !self.visible_row_ids.iter().any(|v| v == id))
        {
            self.selected_row_id = self.visible_row_ids.first().cloned();
        }

        let search_focused = self
            .search_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let search_has_text = !search_q.is_empty();
        let search_focus_ent = self.search_input.clone();

        let search_filter_bar = div()
            .relative()
            .w_full()
            .h(px(40.0))
            .border_b_1()
            .border_color(if search_focused {
                palette.accent_solid
            } else {
                palette.border_subtle
            })
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                div()
                    .id("preferences-search-box")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                        if this
                            .search_input
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window)
                            && !crate::sessions::input_is_composing(&this.search_input, window, cx)
                            && !this.visible_row_ids.is_empty()
                        {
                            this.focus_list_at(0, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                        if this
                            .search_input
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window)
                            && !crate::sessions::input_is_composing(&this.search_input, window, cx)
                        {
                            this.controls_kbd_active = true;
                            this.list_kbd_active = false;
                            this.controls_last_pointer = Some(window.mouse_position());
                            this.default_trigger_focus.focus(window, cx);
                            scroll_dynamic_target_into_view(
                                &this.body_scroll,
                                &this.controls_tracker,
                                "pref-default-trigger",
                                16.0,
                                window,
                            );
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .on_click(move |_, w, cx| {
                        search_focus_ent.focus_handle(cx).focus(w, cx);
                    })
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(14.0))
                            .text_color(palette.text_muted),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search_input)
                                .accessibility_id("preferences-search-input")
                                .aria_label("搜索模型")
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .p_0(),
                        ),
                    )
                    .children(search_has_text.then(|| {
                        div()
                            .id("preferences-search-clear-btn")
                            .w(px(20.0))
                            .h(px(20.0))
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.search_input.update(cx, |i, cx| {
                                    i.set_value("", w, cx);
                                    i.focus(w, cx);
                                });
                                cx.notify();
                                cx.stop_propagation();
                            }))
                            .child(
                                Icon::new(IconName::X)
                                    .size(px(13.0))
                                    .text_color(palette.text_muted),
                            )
                    })),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("pref-filter-dropdown-btn")
                    .test_support()
                    .a11y_toggle_button(
                        "pref-filter-dropdown-btn",
                        format!("筛选提供商：{filter_label}"),
                        self.filter_dropdown_open,
                    )
                    .track_focus(&self.filter_trigger_focus)
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let alt_down = !event.is_held
                            && event.keystroke.key.as_str() == "down"
                            && event.keystroke.modifiers.alt
                            && !event.keystroke.modifiers.control
                            && !event.keystroke.modifiers.shift
                            && !event.keystroke.modifiers.platform;
                        if alt_down {
                            this.toggle_dropdown(false, filter_index, window, cx);
                            this.picker_keyboard_nav = true;
                            this.picker_last_pointer = Some(window.mouse_position());
                            cx.stop_propagation();
                        } else if event.keystroke.modifiers == gpui::Modifiers::default() {
                            match event.keystroke.key.as_str() {
                                "up" => {
                                    this.controls_kbd_active = true;
                                    this.list_kbd_active = false;
                                    this.controls_last_pointer = Some(window.mouse_position());
                                    this.default_trigger_focus.focus(window, cx);
                                    scroll_dynamic_target_into_view(
                                        &this.body_scroll,
                                        &this.controls_tracker,
                                        "pref-default-trigger",
                                        16.0,
                                        window,
                                    );
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                                "down" if !this.visible_row_ids.is_empty() => {
                                    this.focus_list_at(0, window, cx);
                                    cx.stop_propagation();
                                }
                                _ => {}
                            }
                        }
                    }))
                    .h(height::COMPACT)
                    .px(px(10.0))
                    .mr(px(-10.0))
                    .rounded(radius::CONTROL)
                    .bg(if self.filter_dropdown_open {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .text_color(if self.filter_dropdown_open {
                        palette.text_primary
                    } else {
                        palette.text_secondary
                    })
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .hover(move |s| {
                        if controls_kbd {
                            s
                        } else {
                            s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                        }
                    })
                    .active(|s| s.bg(palette.bg_active_press))
                    .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                        let from_kbd = matches!(ev, gpui::ClickEvent::Keyboard(_));
                        this.controls_kbd_active = from_kbd;
                        this.controls_last_pointer = from_kbd.then(|| window.mouse_position());
                        this.toggle_dropdown(false, filter_index, window, cx);
                        if from_kbd && this.filter_dropdown_open {
                            this.picker_keyboard_nav = true;
                            this.picker_last_pointer = Some(window.mouse_position());
                            cx.notify();
                        }
                    }))
                    .on_prepaint(move |bounds, _, _| tr_filter.set("pref-filter-trigger", bounds))
                    .child(
                        div()
                            .max_w(px(160.0))
                            .truncate()
                            .child(SharedString::from(filter_label)),
                    )
                    .child(motion::animated_chevron(
                        "pref-filter",
                        self.filter_dropdown_open,
                        true,
                        px(13.0),
                        palette.text_secondary,
                        window,
                        cx,
                    )),
            )
            .children(filter_menu_opt);

        let section_header = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child("启用模型"),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .pt(px(2.0))
                            .text_color(palette.text_muted)
                            .child(SharedString::from(format!(
                                "{enabled_ready_count} / {}",
                                ready_models.len()
                            ))),
                    ),
            )
            .child(
                div()
                    .mr(px(-10.0))
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child(
                        apply_scale(div(), scale::UI)
                            .id(if all_visible_enabled {
                                "pref-clear-all-btn"
                            } else {
                                "pref-select-all-btn"
                            })
                            .test_support()
                            .a11y_button(
                                if all_visible_enabled {
                                    "pref-clear-all-btn"
                                } else {
                                    "pref-select-all-btn"
                                },
                                if all_visible_enabled {
                                    "清空可见模型"
                                } else {
                                    "全选可见模型"
                                },
                            )
                            .track_focus(&self.select_all_focus)
                            .h(height::COMPACT)
                            .px(px(10.0))
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .hover(move |s| {
                                if controls_kbd {
                                    s
                                } else {
                                    s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                                }
                            })
                            .active(|s| s.bg(palette.bg_active_press))
                            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                                if ev.keystroke.modifiers == gpui::Modifiers::default() {
                                    match ev.keystroke.key.as_str() {
                                        "right" => {
                                            this.controls_kbd_active = true;
                                            this.controls_last_pointer =
                                                Some(window.mouse_position());
                                            this.refresh_btn_focus.focus(window, cx);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                        "up" => {
                                            this.controls_kbd_active = true;
                                            this.controls_last_pointer =
                                                Some(window.mouse_position());
                                            this.default_trigger_focus.focus(window, cx);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                        "down" => {
                                            this.controls_kbd_active = false;
                                            this.search_input
                                                .update(cx, |i, cx| i.focus(window, cx));
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                        _ => {}
                                    }
                                }
                            }))
                            .on_click(cx.listener(
                                move |this, ev: &gpui::ClickEvent, window, cx| {
                                    let from_kbd = matches!(ev, gpui::ClickEvent::Keyboard(_));
                                    this.controls_kbd_active = from_kbd;
                                    this.controls_last_pointer =
                                        from_kbd.then(|| window.mouse_position());
                                    if all_visible_enabled {
                                        this.clear_visible(vis_for_toggle.clone(), cx);
                                    } else {
                                        this.select_all_visible(vis_for_toggle.clone(), cx);
                                    }
                                },
                            ))
                            .on_prepaint(move |bounds, _, _| {
                                tr_select_all.set("pref-select-all", bounds)
                            })
                            .child(cjk_inline_label(if all_visible_enabled {
                                "清空"
                            } else {
                                "全选"
                            })),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("pref-refresh-btn")
                            .test_support()
                            .a11y_button("pref-refresh-btn", "刷新模型列表")
                            .track_focus(&self.refresh_btn_focus)
                            .h(height::COMPACT)
                            .px(px(10.0))
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .hover(move |s| {
                                if controls_kbd {
                                    s
                                } else {
                                    s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                                }
                            })
                            .active(|s| s.bg(palette.bg_active_press))
                            .opacity(if self.refreshing_catalog { 0.6 } else { 1.0 })
                            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                                if ev.keystroke.modifiers == gpui::Modifiers::default() {
                                    match ev.keystroke.key.as_str() {
                                        "left" => {
                                            this.controls_kbd_active = true;
                                            this.controls_last_pointer =
                                                Some(window.mouse_position());
                                            this.select_all_focus.focus(window, cx);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                        "up" => {
                                            this.controls_kbd_active = true;
                                            this.controls_last_pointer =
                                                Some(window.mouse_position());
                                            this.default_trigger_focus.focus(window, cx);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                        "down" => {
                                            this.controls_kbd_active = true;
                                            this.controls_last_pointer =
                                                Some(window.mouse_position());
                                            this.filter_trigger_focus.focus(window, cx);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                        _ => {}
                                    }
                                }
                            }))
                            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, window, cx| {
                                let from_kbd = matches!(ev, gpui::ClickEvent::Keyboard(_));
                                this.controls_kbd_active = from_kbd;
                                this.controls_last_pointer =
                                    from_kbd.then(|| window.mouse_position());
                                this.refresh_catalog(cx);
                            }))
                            .on_prepaint(move |bounds, _, _| tr_refresh.set("pref-refresh", bounds))
                            .child(cjk_inline_label(if self.refreshing_catalog {
                                "刷新中…"
                            } else {
                                "刷新"
                            })),
                    ),
            );

        // 3. Frameless 48px hairline model rows (collapsed to 5 rows by default per `lTWwV`)
        let list_focused = self.focus_handle.is_focused(window);
        let active_list_spec = (list_focused && self.list_kbd_active)
            .then(|| {
                self.selected_row_id.as_deref().and_then(|id| {
                    self.visible_row_ids
                        .iter()
                        .position(|x| x == id)
                        .map(|idx| KbdDynamicSpec::hairline_row(format!("pref-row-{idx}")))
                })
            })
            .flatten();

        let mut list_body = div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.list_kbd_active
                    && pointer_moved_past_threshold(&mut this.list_last_pointer, ev.position)
                {
                    this.list_kbd_active = false;
                    cx.notify();
                }
            }));

        if display_row_count == 0 {
            list_body = list_body.child(
                apply_scale(div(), scale::UI)
                    .py(space::LG)
                    .text_color(palette.text_muted)
                    .child(if ready_models.is_empty() {
                        "暂无已登录提供商的可用模型，请先在「提供商」中登录"
                    } else {
                        "暂无匹配的模型项"
                    }),
            );
        } else {
            let flip_scope = SharedString::from(format!("{:?}:{search_q}", active_provider_filter));
            let rows_arc = Arc::new(visible_rows);
            let viewport_h = px((display_row_count.min(PREF_LIST_MAX_VISIBLE_ROWS) as f32)
                * PREF_LIST_ROW_HEIGHT_PX);
            list_body = list_body.child(
                uniform_list(
                    "pref-models-virtual-list",
                    display_row_count,
                    cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                        range
                            .map(|vis_idx| {
                                let tr = this.list_tracker.clone();
                                let row_track_key =
                                    SharedString::from(format!("pref-row-{vis_idx}"));
                                let kbd_active = this.list_kbd_active || this.controls_kbd_active;
                                match &rows_arc[vis_idx] {
                                    PrefRowItem::Enabled {
                                        ordered_idx,
                                        full_id,
                                        provider,
                                        name,
                                    } => {
                                        let idx = *ordered_idx;
                                        let can_move_up = vis_idx > 0;
                                        let can_move_down =
                                            vis_idx + 1 < this.visible_enabled_indices.len();
                                        let fid_chk = full_id.clone();
                                        let fid_select = full_id.clone();
                                        let selected =
                                            this.selected_row_id.as_ref() == Some(full_id);
                                        let row_id =
                                            SharedString::from(format!("pref-enabled-row-{idx}"));
                                        let chk_id = SharedString::from(format!(
                                            "pref-toggle-enabled-{idx}"
                                        ));
                                        let up_id =
                                            SharedString::from(format!("pref-move-up-{idx}"));
                                        let down_id =
                                            SharedString::from(format!("pref-move-down-{idx}"));
                                        let flip_y = motion::flip_row_offset_y_with_duration(
                                            "pref-enabled-models",
                                            full_id,
                                            &flip_scope,
                                            px(PREF_LIST_ROW_HEIGHT_PX) * vis_idx as f32,
                                            motion::FAST,
                                            window,
                                            cx,
                                        );
                                        let (hovered, hover_state) = motion::hairline_hover_state(
                                            format!("pref-enabled-{full_id}"),
                                            window,
                                            cx,
                                        );
                                        let hairline_color = if kbd_active {
                                            palette.border_subtle
                                        } else {
                                            motion::hairline_hover_color(
                                                format!("pref-enabled-{full_id}"),
                                                hovered,
                                                palette.border_subtle,
                                                palette.accent_solid,
                                                window,
                                                cx,
                                            )
                                        };
                                        div()
                                            .id(row_id.clone())
                                            .test_support()
                                            .relative()
                                            .top(flip_y)
                                            .a11y_listitem(
                                                row_id,
                                                format!("{name}（{provider}）"),
                                                selected,
                                            )
                                            .w_full()
                                            .h(px(PREF_LIST_ROW_HEIGHT_PX))
                                            .border_b_1()
                                            .border_color(hairline_color)
                                            .flex()
                                            .items_center()
                                            .gap(space::MD)
                                            .active(|s| s.border_color(palette.accent_hover))
                                            .on_hover(move |&is_hovered, _, cx| {
                                                motion::PointerFlag::set(
                                                    &hover_state,
                                                    is_hovered,
                                                    cx,
                                                );
                                            })
                                            .on_click(cx.listener(move |this, _, w, cx| {
                                                this.selected_row_id = Some(fid_select.clone());
                                                this.list_kbd_active = false;
                                                this.focus_handle.focus(w, cx);
                                                cx.notify();
                                            }))
                                            .on_prepaint(move |bounds, _, _| {
                                                tr.set(row_track_key.clone(), bounds);
                                            })
                                            .child({
                                                let chk_mark = motion::animated_checkbox_mark(
                                                    format!("pref-chk-{full_id}"),
                                                    true,
                                                    px(11.0),
                                                    palette.text_on_accent,
                                                    window,
                                                    cx,
                                                );
                                                div()
                                                    .id(chk_id.clone())
                                                    .test_support()
                                                    .a11y_checkbox(
                                                        chk_id,
                                                        format!("启用模型：{name}"),
                                                        true,
                                                    )
                                                    .w(px(16.0))
                                                    .h(px(16.0))
                                                    .rounded(radius::CONTROL)
                                                    .border_1()
                                                    .border_color(palette.accent_solid)
                                                    .bg(palette.accent_solid)
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .flex_shrink_0()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.list_kbd_active = false;
                                                        this.toggle_model_enabled(
                                                            fid_chk.clone(),
                                                            cx,
                                                        );
                                                        cx.stop_propagation();
                                                    }))
                                                    .children(chk_mark)
                                            })
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex()
                                                    .items_center()
                                                    .gap(space::MD)
                                                    .child(
                                                        apply_scale(div(), scale::BODY)
                                                            .truncate()
                                                            .text_color(palette.text_primary)
                                                            .child(SharedString::from(
                                                                name.clone(),
                                                            )),
                                                    )
                                                    .child(
                                                        apply_scale(div(), scale::CAPTION)
                                                            .pt(px(2.0))
                                                            .flex_shrink_0()
                                                            .text_color(palette.text_muted)
                                                            .child(SharedString::from(
                                                                provider.clone(),
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_shrink_0()
                                                    .flex()
                                                    .items_center()
                                                    .gap(space::MICRO)
                                                    .child(
                                                        div()
                                                            .id(up_id.clone())
                                                            .test_support()
                                                            .a11y_button(
                                                                up_id,
                                                                format!("上移模型：{name}"),
                                                            )
                                                            .w(height::COMPACT)
                                                            .h(height::COMPACT)
                                                            .rounded(radius::CONTROL)
                                                            .opacity(if can_move_up {
                                                                1.0
                                                            } else {
                                                                0.35
                                                            })
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .hover(move |s| {
                                                                if can_move_up && !kbd_active {
                                                                    s.bg(palette.bg_item_hover)
                                                                } else {
                                                                    s
                                                                }
                                                            })
                                                            .active(move |s| {
                                                                if can_move_up {
                                                                    s.bg(palette.bg_active_press)
                                                                } else {
                                                                    s
                                                                }
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.list_kbd_active = false;
                                                                    if can_move_up {
                                                                        this.move_enabled_row(
                                                                            idx, -1, cx,
                                                                        );
                                                                    }
                                                                    cx.stop_propagation();
                                                                },
                                                            ))
                                                            .child(
                                                                Icon::new(IconName::ArrowUp)
                                                                    .size(px(13.0))
                                                                    .text_color(
                                                                        palette.text_secondary,
                                                                    ),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .id(down_id.clone())
                                                            .test_support()
                                                            .a11y_button(
                                                                down_id,
                                                                format!("下移模型：{name}"),
                                                            )
                                                            .w(height::COMPACT)
                                                            .h(height::COMPACT)
                                                            .rounded(radius::CONTROL)
                                                            .opacity(if can_move_down {
                                                                1.0
                                                            } else {
                                                                0.35
                                                            })
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .hover(move |s| {
                                                                if can_move_down && !kbd_active {
                                                                    s.bg(palette.bg_item_hover)
                                                                } else {
                                                                    s
                                                                }
                                                            })
                                                            .active(move |s| {
                                                                if can_move_down {
                                                                    s.bg(palette.bg_active_press)
                                                                } else {
                                                                    s
                                                                }
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.list_kbd_active = false;
                                                                    if can_move_down {
                                                                        this.move_enabled_row(
                                                                            idx, 1, cx,
                                                                        );
                                                                    }
                                                                    cx.stop_propagation();
                                                                },
                                                            ))
                                                            .child(
                                                                Icon::new(IconName::ArrowDown)
                                                                    .size(px(13.0))
                                                                    .text_color(
                                                                        palette.text_secondary,
                                                                    ),
                                                            ),
                                                    ),
                                            )
                                            .into_any_element()
                                    }
                                    PrefRowItem::Unchecked {
                                        full_id,
                                        provider,
                                        name,
                                    } => {
                                        let fid_add = full_id.clone();
                                        let unchk_id =
                                            SharedString::from(format!("pref-unchecked-{full_id}"));
                                        let (hovered, hover_state) = motion::hairline_hover_state(
                                            format!("pref-unchecked-{full_id}"),
                                            window,
                                            cx,
                                        );
                                        let hairline_color = if kbd_active {
                                            palette.border_subtle
                                        } else {
                                            motion::hairline_hover_color(
                                                format!("pref-unchecked-{full_id}"),
                                                hovered,
                                                palette.border_subtle,
                                                palette.accent_solid,
                                                window,
                                                cx,
                                            )
                                        };
                                        div()
                                            .id(unchk_id.clone())
                                            .test_support()
                                            .a11y_checkbox(
                                                unchk_id,
                                                format!("启用模型：{name}（{provider}）"),
                                                false,
                                            )
                                            .w_full()
                                            .h(px(PREF_LIST_ROW_HEIGHT_PX))
                                            .border_b_1()
                                            .border_color(hairline_color)
                                            .flex()
                                            .items_center()
                                            .gap(space::MD)
                                            .active(|s| s.border_color(palette.accent_hover))
                                            .on_hover(move |&is_hovered, _, cx| {
                                                motion::PointerFlag::set(
                                                    &hover_state,
                                                    is_hovered,
                                                    cx,
                                                );
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.list_kbd_active = false;
                                                this.toggle_model_enabled(fid_add.clone(), cx);
                                            }))
                                            .on_prepaint(move |bounds, _, _| {
                                                tr.set(row_track_key.clone(), bounds);
                                            })
                                            .child({
                                                let unchk_mark = motion::animated_checkbox_mark(
                                                    format!("pref-chk-{full_id}"),
                                                    false,
                                                    px(11.0),
                                                    palette.text_on_accent,
                                                    window,
                                                    cx,
                                                );
                                                div()
                                                    .w(px(16.0))
                                                    .h(px(16.0))
                                                    .rounded(radius::CONTROL)
                                                    .border_1()
                                                    .border_color(palette.border_strong)
                                                    .bg(palette.bg_workspace)
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .flex_shrink_0()
                                                    .children(unchk_mark)
                                            })
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex()
                                                    .items_center()
                                                    .gap(space::MD)
                                                    .child(
                                                        apply_scale(div(), scale::BODY)
                                                            .truncate()
                                                            .text_color(palette.text_secondary)
                                                            .child(SharedString::from(
                                                                name.clone(),
                                                            )),
                                                    )
                                                    .child(
                                                        apply_scale(div(), scale::CAPTION)
                                                            .pt(px(2.0))
                                                            .flex_shrink_0()
                                                            .text_color(palette.text_muted)
                                                            .child(SharedString::from(
                                                                provider.clone(),
                                                            )),
                                                    ),
                                            )
                                            .into_any_element()
                                    }
                                }
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.list_scroll)
                .w_full()
                .h(viewport_h),
            );
            if can_collapse_models {
                let expanded = self.models_expanded;
                let toggle_label = if expanded {
                    SharedString::from("收起模型列表")
                } else {
                    SharedString::from(format!("查看全部模型（{total_matching_rows}）"))
                };
                let expand_kbd_focused = self.expand_btn_focus.is_focused(window) && controls_kbd;
                let (expand_hovered, expand_hover_state) =
                    motion::hairline_hover_state("pref-expand-models", window, cx);
                let expand_t = motion::hairline_hover_progress(
                    "pref-expand-models",
                    expand_hovered && !controls_kbd && !self.list_kbd_active,
                    window,
                    cx,
                );
                let any_kbd = controls_kbd || self.list_kbd_active;
                let expand_border = if any_kbd {
                    palette.border_subtle
                } else {
                    motion::lerp_hsla(palette.border_subtle, palette.accent_solid, expand_t)
                };
                let expand_fg = if expand_kbd_focused {
                    palette.text_primary
                } else if any_kbd {
                    palette.text_secondary
                } else {
                    motion::lerp_hsla(palette.text_secondary, palette.text_primary, expand_t)
                };
                list_body = list_body.child(
                    apply_scale(div(), scale::UI)
                        .id("pref-toggle-expand-models-btn")
                        .test_support()
                        .a11y_button("pref-toggle-expand-models-btn", toggle_label.clone())
                        .track_focus(&self.expand_btn_focus)
                        .w_full()
                        .h(height::HEADER)
                        .border_b_1()
                        .border_color(expand_border)
                        .text_color(expand_fg)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .active(|s| {
                            s.border_color(palette.accent_hover)
                                .text_color(palette.text_primary)
                        })
                        .on_hover(move |&is_hovered, _, cx| {
                            motion::PointerFlag::set(&expand_hover_state, is_hovered, cx);
                        })
                        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                            if ev.keystroke.modifiers == gpui::Modifiers::default()
                                && ev.keystroke.key.as_str() == "up"
                                && !this.visible_row_ids.is_empty()
                            {
                                this.focus_list_at(this.visible_row_ids.len() - 1, window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .on_click(cx.listener(|this, ev: &gpui::ClickEvent, window, cx| {
                            let from_kbd = matches!(ev, gpui::ClickEvent::Keyboard(_));
                            let expanding = !this.models_expanded;
                            this.models_expanded = expanding;
                            this.list_scroll = UniformListScrollHandle::new();
                            if from_kbd && expanding {
                                this.pending_scroll_to_index = Some(PREF_COLLAPSED_MODEL_LIMIT);
                                this.controls_kbd_active = false;
                                this.list_kbd_active = true;
                                this.list_last_pointer = Some(window.mouse_position());
                                this.focus_handle.focus(window, cx);
                            } else {
                                this.controls_kbd_active = from_kbd;
                                this.list_kbd_active = false;
                                this.controls_last_pointer =
                                    from_kbd.then(|| window.mouse_position());
                            }
                            cx.notify();
                        }))
                        .on_prepaint(move |bounds, _, _| tr_expand.set("pref-expand-btn", bounds))
                        .child(cjk_inline_label(toggle_label))
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(14.0))
                            .text_color(expand_fg),
                        ),
                );
            }
        }

        list_body = list_body.child(render_gliding_dynamic_indicator(
            "pref-models-list",
            kbd_style,
            active_list_spec,
            self.list_tracker.clone(),
            &palette,
            window,
            cx,
        ));

        div()
            .id("settings-preferences-tab")
            .test_support()
            .a11y_region("settings-preferences-tab", "模型偏好设置")
            .relative()
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key.as_str() == "tab"
                    && !ev.keystroke.modifiers.control
                    && !ev.keystroke.modifiers.alt
                    && !ev.keystroke.modifiers.platform
                {
                    this.controls_kbd_active = true;
                    this.controls_last_pointer = Some(window.mouse_position());
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.controls_kbd_active
                    && pointer_moved_past_threshold(&mut this.controls_last_pointer, ev.position)
                {
                    this.controls_kbd_active = false;
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(Self::on_key_down))
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(default_row)
                    .children(dropdown_menu_opt),
            )
            .child(
                div()
                    .mt(px(32.0))
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(section_header)
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .child(search_filter_bar)
                            .child(list_body),
                    ),
            )
            .child(render_gliding_dynamic_indicator(
                "pref-controls",
                kbd_style,
                self.active_controls_kbd_spec(window),
                self.controls_tracker.clone(),
                &palette,
                window,
                cx,
            ))
    }
}

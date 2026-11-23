mod scope;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cst_client::{ClientHandle, ModelChoiceDto, ModelsSnapshot};
use gpui::{KeyDownEvent, ScrollStrategy, UniformListScrollHandle, uniform_list};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};

use self::scope::ModelScope;

#[derive(Debug, Clone)]
pub enum PreferencesTabEvent {
    Notice(String),
    ErrorMessage(String),
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

const PREF_LIST_ROW_HEIGHT_PX: f32 = 56.0;
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
    last_ready_models: Vec<ModelChoiceDto>,
    optimistic_default: Option<(String, String)>,
    selected_row_id: Option<String>,
    visible_enabled_indices: Vec<usize>,
    visible_filter_key: Option<(Option<String>, String)>,
    observed_models: Option<Arc<ModelsSnapshot>>,
    saving_scope: bool,
    saving_default: bool,
    refreshing_catalog: bool,
    default_trigger_focus: FocusHandle,
    filter_trigger_focus: FocusHandle,
    picker_focus: FocusHandle,
    picker_index: usize,
    picker_keyboard_nav: bool,
    picker_scroll: gpui::ScrollHandle,
    default_picker_scroll: UniformListScrollHandle,
    list_scroll: UniformListScrollHandle,
    _search_subscription: Subscription,
    focus_handle: FocusHandle,
}

impl EventEmitter<PreferencesTabEvent> for PreferencesTabView {}

impl Focusable for PreferencesTabView {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PreferencesTabView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索模型…"));
        let search_subscription =
            cx.subscribe(
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
            );
        Self {
            client,
            search_input,
            provider_filter: None,
            default_dropdown_open: false,
            filter_dropdown_open: false,
            models_expanded: false,
            enabled_scope: ModelScope::default(),
            last_ready_models: Vec::new(),
            optimistic_default: None,
            selected_row_id: None,
            visible_enabled_indices: Vec::new(),
            visible_filter_key: None,
            observed_models: None,
            saving_scope: false,
            saving_default: false,
            refreshing_catalog: false,
            default_trigger_focus: cx.focus_handle().tab_stop(true),
            filter_trigger_focus: cx.focus_handle().tab_stop(true),
            picker_focus: cx.focus_handle(),
            picker_index: 0,
            picker_keyboard_nav: false,
            picker_scroll: gpui::ScrollHandle::new(),
            default_picker_scroll: UniformListScrollHandle::new(),
            list_scroll: UniformListScrollHandle::new(),
            _search_subscription: search_subscription,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn has_dropdown(&self) -> bool {
        self.default_dropdown_open || self.filter_dropdown_open
    }

    pub fn leave(&mut self, cx: &mut Context<Self>) {
        self.default_dropdown_open = false;
        self.filter_dropdown_open = false;
        self.picker_keyboard_nav = false;
        self.models_expanded = false;
        cx.notify();
    }

    pub fn dismiss_dropdowns(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.has_dropdown() {
            return false;
        }
        if self.default_dropdown_open {
            self.default_trigger_focus.focus(window, cx);
        } else {
            self.filter_trigger_focus.focus(window, cx);
        }
        self.leave(cx);
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
        if default_model && self.saving_default {
            return;
        }
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
        if self.saving_scope {
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
        if !self.saving_default {
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

    fn save_scoped_with_rollback(&mut self, previous: ModelScope, cx: &mut Context<Self>) {
        self.saving_scope = true;
        let next = self.enabled_scope.patterns();
        let snap = self.client.snapshot();
        let enabled_pairs = self.enabled_ready_pairs_from_snapshot(snap.models.as_deref());
        let current_default = self.optimistic_default.clone().or_else(|| {
            snap.models
                .as_ref()
                .and_then(|m| m.default_model.as_ref())
                .map(|d| (d.provider.clone(), d.id.clone()))
        });
        let auto_next_default = match (current_default, enabled_pairs.first()) {
            (Some(cur), Some(first)) if !enabled_pairs.contains(&cur) => Some(first.clone()),
            (None, Some(first)) => Some(first.clone()),
            _ => None,
        };
        let previous_optimistic = self.optimistic_default.clone();
        if let Some(ref next_def) = auto_next_default {
            self.optimistic_default = Some(next_def.clone());
        }
        let client = self.client.clone();

        cx.spawn(async move |weak, cx| {
            let res = client.set_explicit_model_scope(next).await;
            let default_res = if res.is_ok() {
                if let Some((prov, mid)) = auto_next_default {
                    Some(client.set_default_model(prov, mid).await)
                } else {
                    None
                }
            } else {
                None
            };
            let refreshed = client.refresh_models(None).await.is_ok();
            let _ = weak.update(cx, |this, cx| {
                this.saving_scope = false;
                if let Err(err) = res {
                    this.enabled_scope = previous;
                    this.optimistic_default = previous_optimistic;
                    cx.emit(PreferencesTabEvent::ErrorMessage(format!(
                        "保存启用模型范围失败，已自动回滚：{err}"
                    )));
                } else if let Some(Err(err)) = default_res {
                    this.optimistic_default = previous_optimistic;
                    cx.emit(PreferencesTabEvent::ErrorMessage(format!(
                        "同步默认模型失败：{err}"
                    )));
                }
                if refreshed {
                    this.sync_from_snapshot();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_model_enabled(&mut self, full_id: String, cx: &mut Context<Self>) {
        if self.saving_scope {
            return;
        }
        let prev = self.enabled_scope.clone();
        self.enabled_scope.toggle(full_id);
        cx.notify();
        self.save_scoped_with_rollback(prev, cx);
    }

    fn select_all_visible(&mut self, visible_full_ids: Vec<String>, cx: &mut Context<Self>) {
        if self.saving_scope {
            return;
        }
        let prev = self.enabled_scope.clone();
        if self.enabled_scope.select(visible_full_ids) {
            cx.notify();
            self.save_scoped_with_rollback(prev, cx);
        }
    }

    fn clear_visible(&mut self, visible_full_ids: Vec<String>, cx: &mut Context<Self>) {
        if self.saving_scope {
            return;
        }
        let prev = self.enabled_scope.clone();
        if self
            .enabled_scope
            .clear(visible_full_ids.iter().map(String::as_str))
        {
            cx.notify();
            self.save_scoped_with_rollback(prev, cx);
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
        if self.saving_scope {
            return;
        }
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
        let prev = self.enabled_scope.clone();
        if !self.enabled_scope.swap(idx, target) {
            return;
        }
        self.selected_row_id = selected;
        self.list_scroll
            .scroll_to_item(target_position, ScrollStrategy::Nearest);
        cx.notify();
        self.save_scoped_with_rollback(prev, cx);
    }

    fn set_default_model(
        &mut self,
        provider: String,
        model_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving_default || !self.dismiss_dropdowns(window, cx) {
            return;
        }
        self.saving_default = true;
        let previous_optimistic = self.optimistic_default.clone();
        self.optimistic_default = Some((provider.clone(), model_id.clone()));
        cx.notify();
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.set_default_model(provider, model_id).await;
            let refreshed = client.refresh_models(None).await.is_ok();
            let _ = weak.update(cx, |this, cx| {
                this.saving_default = false;
                if let Err(err) = res {
                    this.optimistic_default = previous_optimistic;
                    cx.emit(PreferencesTabEvent::ErrorMessage(err.to_string()));
                } else if refreshed {
                    this.sync_from_snapshot();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if self.has_dropdown() {
            return;
        }
        if ks.modifiers.alt && self.focus_handle.is_focused(window) && !self.saving_scope {
            let Some(idx) = self
                .visible_enabled_indices
                .iter()
                .copied()
                .find(|&idx| self.enabled_scope.model_id_at(idx) == self.selected_row_id.as_deref())
                .or_else(|| self.visible_enabled_indices.first().copied())
            else {
                return;
            };
            match ks.key.as_str() {
                "up" => {
                    self.move_enabled_row(idx, -1, cx);
                    cx.stop_propagation();
                }
                "down" => {
                    self.move_enabled_row(idx, 1, cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
        }
    }
}

impl Render for PreferencesTabView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_from_snapshot();
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let snap = self.client.snapshot();
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

        // 1. Default model `72px` card with `208×32px` custom dropdown trigger + frosted popover (`H2p2z`)
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
                let list_h = px((row_count as f32 * 32.0).min(280.0));
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
                                let is_kbd_hi =
                                    this.picker_keyboard_nav && idx == this.picker_index;
                                let prov_c = prov.clone();
                                let mid_c = mid.clone();
                                let opt_id =
                                    SharedString::from(format!("pref-default-model-{prov}-{mid}"));
                                let mname_label = SharedString::from(mname.clone());
                                let prov_label = SharedString::from(prov.clone());
                                let opt_aria = SharedString::from(format!("{prov} · {mname}"));
                                apply_scale(div(), scale::UI)
                                    .id(opt_id.clone())
                                    .test_support()
                                    .a11y_listitem(opt_id, opt_aria, is_selected)
                                    .flex_shrink_0()
                                    .w_full()
                                    .h(height::CONTROL)
                                    .px(space::SM)
                                    .rounded(radius::CONTROL)
                                    .bg(if is_kbd_hi {
                                        palette.bg_item_hover
                                    } else {
                                        gpui_kit::transparent_black()
                                    })
                                    .text_color(palette.text_primary)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(space::SM)
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_default_model(
                                            prov_c.clone(),
                                            mid_c.clone(),
                                            window,
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    }))
                                    .child(div().flex_1().min_w_0().truncate().child(mname_label))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(space::XS)
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
                    .w(px(300.0))
                    .p(space::XS)
                    .rounded(radius::ROW)
                    .bg(palette.bg_elevated)
                    .border_1()
                    .border_color(palette.border_subtle)
                    .occlude()
                    .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .w_full()
                            .focus_trap("pref-default-model-focus", &self.picker_focus)
                            .on_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, window, cx| {
                                    if !choices.is_empty() {
                                        let moved = match event.keystroke.key.as_str() {
                                            "down" => {
                                                this.picker_index =
                                                    (this.picker_index + 1) % choices.len();
                                                true
                                            }
                                            "up" => {
                                                this.picker_index = this
                                                    .picker_index
                                                    .checked_sub(1)
                                                    .unwrap_or(choices.len() - 1);
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
                                        if let Some((provider, model)) =
                                            choices.get(this.picker_index)
                                        {
                                            this.set_default_model(
                                                provider.clone(),
                                                model.clone(),
                                                window,
                                                cx,
                                            );
                                        }
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .child(items),
                    ),
                palette.flat_popover_shadows(),
                -6.0,
            );
            dropdown_menu_opt = Some(div().absolute().top(px(60.0)).right(space::LG).child(menu));
        }

        let default_card = div()
            .w_full()
            .h(px(72.0))
            .px(space::LG)
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_workspace)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::LG)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::MICRO)
                    .min_w_0()
                    .child(
                        apply_scale(div(), scale::BODY)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("默认模型")),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("用于新会话，三端同步")),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("pref-default-model-trigger")
                    .test_support()
                    .a11y_toggle_button(
                        "pref-default-model-trigger",
                        format!("默认模型：{default_label}"),
                        self.default_dropdown_open,
                    )
                    .track_focus(&self.default_trigger_focus)
                    .tab_index(0)
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        if super::activates_button(event) {
                            this.toggle_dropdown(true, default_index, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .w(px(208.0))
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(if self.default_dropdown_open {
                        palette.bg_item_active
                    } else {
                        palette.bg_workspace
                    })
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::XS)
                    .hover(|s| s.bg(palette.bg_surface))
                    .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_dropdown(true, default_index, window, cx);
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(default_label)),
                    )
                    .child(motion::animated_chevron(
                        "pref-default-model",
                        self.default_dropdown_open,
                        true,
                        px(14.0),
                        palette.text_secondary,
                        window,
                        cx,
                    )),
            );

        // 2. Search + Provider filter `PHFud` dropdown (`zjoqr`) + `"全选 / 清空 / 刷新"` ghost buttons (`I8Og18`)
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
        let filter_label = active_provider_filter.unwrap_or("全部").to_owned();

        let filter_index = providers_seen
            .iter()
            .position(|p| Some(p.as_str()) == active_provider_filter)
            .map_or(0, |index| index + 1);
        let filter_menu_opt = self.filter_dropdown_open.then(|| {
            let choices = providers_seen.clone();
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
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .bg(if self.picker_keyboard_nav && self.picker_index == 0 {
                            palette.bg_item_hover
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::SM)
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.provider_filter = None;
                            this.dismiss_dropdowns(window, cx);
                        }))
                        .child(cjk_inline_label("全部"))
                        .children(active_provider_filter.is_none().then(|| {
                            Icon::new(IconName::Check)
                                .size(px(13.0))
                                .text_color(palette.accent_ink)
                        })),
                );

            for (index, prov) in providers_seen.iter().enumerate() {
                let is_selected = active_provider_filter == Some(prov.as_str());
                let is_kbd_hi = self.picker_keyboard_nav && self.picker_index == index + 1;
                let p_click = prov.clone();
                let filter_id = SharedString::from(format!("pref-filter-{prov}"));
                let prov_label = SharedString::from(prov.clone());
                items = items.child(
                    apply_scale(div(), scale::UI)
                        .id(filter_id.clone())
                        .test_support()
                        .a11y_listitem(filter_id, prov_label.clone(), is_selected)
                        .flex_shrink_0()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .bg(if is_kbd_hi {
                            palette.bg_item_hover
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::SM)
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.provider_filter = Some(p_click.clone());
                            this.dismiss_dropdowns(window, cx);
                        }))
                        .child(div().flex_1().min_w_0().truncate().child(prov_label))
                        .children(is_selected.then(|| {
                            Icon::new(IconName::Check)
                                .size(px(13.0))
                                .text_color(palette.accent_ink)
                        })),
                );
            }

            div()
                .absolute()
                .top(px(36.0))
                .right_0()
                .child(motion::animate_popover_enter(
                    "pref-filter-pop-anim",
                    div()
                        .id("pref-filter-popover")
                        .test_support()
                        .a11y_dialog("pref-filter-popover", "按提供商筛选")
                        .w(px(220.0))
                        .p(space::XS)
                        .rounded(radius::ROW)
                        .bg(palette.bg_elevated)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .occlude()
                        .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
                                            cx.notify();
                                            cx.stop_propagation();
                                        } else if super::activates_button(event) {
                                            this.provider_filter = this
                                                .picker_index
                                                .checked_sub(1)
                                                .and_then(|ix| choices.get(ix))
                                                .cloned();
                                            this.dismiss_dropdowns(window, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                                .child(items),
                        ),
                    palette.flat_popover_shadows(),
                    -6.0,
                ))
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

        self.visible_enabled_indices = visible_enabled_indices;
        let vis_for_all = visible_full_ids.clone();
        let vis_for_clear = visible_full_ids;

        let search_focused = self
            .search_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let search_has_text = !search_q.is_empty();
        let search_focus_ent = self.search_input.clone();

        let filter_row = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                div()
                    .id("preferences-search-box")
                    .w(px(340.0))
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(if search_focused {
                        palette.border_focus
                    } else {
                        palette.border_strong
                    })
                    .bg(palette.bg_workspace)
                    .flex()
                    .items_center()
                    .gap(space::SM)
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
                    .tab_index(0)
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        if super::activates_button(event) {
                            this.toggle_dropdown(false, filter_index, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .min_w(px(84.0))
                    .max_w(px(180.0))
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(if self.filter_dropdown_open {
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
                    .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_dropdown(false, filter_index, window, cx);
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(filter_label)),
                    )
                    .child(motion::animated_chevron(
                        "pref-filter",
                        self.filter_dropdown_open,
                        true,
                        px(14.0),
                        palette.text_secondary,
                        window,
                        cx,
                    )),
            );

        let batch_toolbar = div()
            .w_full()
            .flex()
            .items_center()
            .gap(space::SM)
            .child(
                apply_scale(div(), scale::UI)
                    .id("pref-select-all-btn")
                    .test_support()
                    .a11y_button("pref-select-all-btn", "全选可见模型")
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.text_secondary)
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_all_visible(vis_for_all.clone(), cx);
                    }))
                    .child(cjk_inline_label("全选")),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("pref-clear-all-btn")
                    .test_support()
                    .a11y_button("pref-clear-all-btn", "清空可见模型")
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.text_secondary)
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.clear_visible(vis_for_clear.clone(), cx);
                    }))
                    .child(cjk_inline_label("清空")),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("pref-refresh-btn")
                    .test_support()
                    .a11y_button("pref-refresh-btn", "刷新模型列表")
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .text_color(palette.text_secondary)
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                    .opacity(if self.refreshing_catalog { 0.6 } else { 1.0 })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.refresh_catalog(cx);
                    }))
                    .child(cjk_inline_label(if self.refreshing_catalog {
                        "刷新中…"
                    } else {
                        "刷新"
                    })),
            );

        // 3. Enabled & unchecked logged-in models (`56px` uniform rows, collapsed to 5 rows by default per `lTWwV`)
        let total_matching_rows = visible_rows.len();
        let filtering_active = active_provider_filter.is_some() || !search_q.is_empty();
        let can_collapse_models =
            !filtering_active && total_matching_rows > PREF_COLLAPSED_MODEL_LIMIT;
        let display_row_count = if can_collapse_models && !self.models_expanded {
            PREF_COLLAPSED_MODEL_LIMIT
        } else {
            total_matching_rows
        };
        let mut list_card = div()
            .w_full()
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_workspace)
            .overflow_hidden()
            .flex()
            .flex_col();

        if display_row_count == 0 {
            list_card = list_card.child(
                apply_scale(div(), scale::UI)
                    .p(space::LG)
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
            list_card = list_card.child(
                uniform_list(
                    "pref-models-virtual-list",
                    display_row_count,
                    cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                        range
                            .map(|vis_idx| match &rows_arc[vis_idx] {
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
                                    let selected = this.selected_row_id.as_ref() == Some(full_id);
                                    let row_id =
                                        SharedString::from(format!("pref-enabled-row-{idx}"));
                                    let chk_id =
                                        SharedString::from(format!("pref-toggle-enabled-{idx}"));
                                    let up_id = SharedString::from(format!("pref-move-up-{idx}"));
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
                                    let mut row = div()
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
                                        .px(space::LG)
                                        .bg(if selected {
                                            palette.bg_item_active
                                        } else {
                                            gpui_kit::transparent_black()
                                        })
                                        .flex()
                                        .items_center()
                                        .gap(space::MD)
                                        .hover(|s| s.bg(palette.bg_item_hover))
                                        .on_click(cx.listener(move |this, _, w, cx| {
                                            this.selected_row_id = Some(fid_select.clone());
                                            this.focus_handle.focus(w, cx);
                                            cx.notify();
                                        }))
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
                                                    this.toggle_model_enabled(fid_chk.clone(), cx);
                                                    cx.stop_propagation();
                                                }))
                                                .children(chk_mark)
                                        })
                                        .child(
                                            apply_scale(div(), scale::BODY)
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(palette.text_primary)
                                                .child(SharedString::from(name.clone())),
                                        )
                                        .child(
                                            apply_scale(div(), scale::UI)
                                                .w(px(140.0))
                                                .flex_shrink_0()
                                                .flex()
                                                .justify_end()
                                                .truncate()
                                                .text_color(palette.text_secondary)
                                                .child(SharedString::from(provider.clone())),
                                        )
                                        .child(
                                            div()
                                                .w(px(60.0))
                                                .flex_shrink_0()
                                                .flex()
                                                .items_center()
                                                .justify_end()
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
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .hover(|s| s.bg(palette.bg_item_hover))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
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
                                                                .text_color(palette.text_secondary),
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
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .hover(|s| s.bg(palette.bg_item_hover))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
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
                                                                .text_color(palette.text_secondary),
                                                        ),
                                                ),
                                        );
                                    if vis_idx > 0 {
                                        row = row.border_t_1().border_color(palette.border_subtle);
                                    }
                                    // GPUI positions the uniform-list item root itself. Move
                                    // the child so the FLIP offset also moves its hitboxes.
                                    div()
                                        .w_full()
                                        .h(px(PREF_LIST_ROW_HEIGHT_PX))
                                        .child(row)
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
                                    let mut row = div()
                                        .id(unchk_id.clone())
                                        .test_support()
                                        .a11y_checkbox(
                                            unchk_id,
                                            format!("启用模型：{name}（{provider}）"),
                                            false,
                                        )
                                        .w_full()
                                        .h(px(PREF_LIST_ROW_HEIGHT_PX))
                                        .px(space::LG)
                                        .flex()
                                        .items_center()
                                        .gap(space::MD)
                                        .hover(|s| s.bg(palette.bg_item_hover))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_model_enabled(fid_add.clone(), cx);
                                        }))
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
                                            apply_scale(div(), scale::BODY)
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(palette.text_secondary)
                                                .child(SharedString::from(name.clone())),
                                        )
                                        .child(
                                            apply_scale(div(), scale::UI)
                                                .w(px(140.0))
                                                .flex_shrink_0()
                                                .flex()
                                                .justify_end()
                                                .truncate()
                                                .text_color(palette.text_secondary)
                                                .child(SharedString::from(provider.clone())),
                                        )
                                        .child(div().w(px(60.0)).flex_shrink_0());
                                    if vis_idx > 0 {
                                        row = row.border_t_1().border_color(palette.border_subtle);
                                    }
                                    row.into_any_element()
                                }
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.list_scroll)
                .w_full()
                .flex_shrink_0()
                .h(viewport_h),
            );
            if can_collapse_models {
                let expanded = self.models_expanded;
                let toggle_label = if expanded {
                    SharedString::from("收起模型列表")
                } else {
                    SharedString::from(format!("查看全部模型（{total_matching_rows}）"))
                };
                list_card = list_card.child(
                    apply_scale(div(), scale::UI)
                        .id("pref-toggle-expand-models-btn")
                        .test_support()
                        .a11y_button("pref-toggle-expand-models-btn", toggle_label.clone())
                        .w_full()
                        .h(height::HEADER)
                        .px(space::LG)
                        .border_t_1()
                        .border_color(palette.border_subtle)
                        .text_color(palette.text_secondary)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.models_expanded = !this.models_expanded;
                            this.list_scroll = UniformListScrollHandle::new();
                            cx.notify();
                        }))
                        .child(cjk_inline_label(toggle_label))
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(14.0))
                            .text_color(palette.text_secondary),
                        ),
                );
            }
        }

        let enabled_count = enabled_ready_count;

        div()
            .id("settings-preferences-tab")
            .test_support()
            .a11y_region("settings-preferences-tab", "模型偏好设置")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .w_full()
            .flex()
            .flex_col()
            .gap(space::GROUP)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    .child(
                        apply_scale(div(), scale::TITLE)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("模型偏好")),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("设置新会话默认模型与输入框中的模型范围。")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("新会话默认模型")),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(space::GROUP)
                            .child(default_card)
                            .child(
                                div()
                                    .relative()
                                    .w_full()
                                    .flex()
                                    .flex_col()
                                    .gap(space::GROUP)
                                    .child(filter_row)
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(space::SM)
                                            .child(
                                                div()
                                                    .w_full()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .child(
                                                        apply_scale(div(), scale::BODY_SEMIBOLD)
                                                            .text_color(palette.text_primary)
                                                            .child(cjk_inline_label("启用模型")),
                                                    )
                                                    .child(
                                                        apply_scale(div(), scale::UI)
                                                            .text_color(palette.text_secondary)
                                                            .child(cjk_inline_label(format!(
                                                                "{enabled_count} 个已启用"
                                                            ))),
                                                    ),
                                            )
                                            .child(batch_toolbar)
                                            .child(list_card),
                                    )
                                    .children(filter_menu_opt),
                            )
                            .children(dropdown_menu_opt),
                    ),
            )
    }
}

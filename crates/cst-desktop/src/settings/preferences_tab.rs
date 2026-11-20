use std::sync::Arc;

use cst_client::{ClientHandle, ModelChoiceDto, ModelsSnapshot};
use gpui::KeyDownEvent;
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
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius, scale, space,
};

#[derive(Debug, Clone)]
pub enum PreferencesTabEvent {
    Notice(String),
    ErrorMessage(String),
}

pub struct PreferencesTabView {
    client: ClientHandle,
    search_input: Entity<InputState>,
    provider_filter: Option<String>,
    default_dropdown_open: bool,
    filter_dropdown_open: bool,
    ordered_enabled_ids: Vec<String>,
    selected_row_idx: usize,
    observed_models: Option<Arc<ModelsSnapshot>>,
    saving_scope: bool,
    saving_default: bool,
    refreshing_catalog: bool,
    default_trigger_focus: FocusHandle,
    filter_trigger_focus: FocusHandle,
    picker_focus: FocusHandle,
    picker_index: usize,
    picker_scroll: gpui::ScrollHandle,
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
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索模型或服务…"));
        let search_subscription = cx.subscribe(&search_input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            client,
            search_input,
            provider_filter: None,
            default_dropdown_open: false,
            filter_dropdown_open: false,
            ordered_enabled_ids: Vec::new(),
            selected_row_idx: 0,
            observed_models: None,
            saving_scope: false,
            saving_default: false,
            refreshing_catalog: false,
            default_trigger_focus: cx.focus_handle().tab_stop(true),
            filter_trigger_focus: cx.focus_handle().tab_stop(true),
            picker_focus: cx.focus_handle(),
            picker_index: 0,
            picker_scroll: gpui::ScrollHandle::new(),
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
        self.picker_scroll = gpui::ScrollHandle::new();
        self.picker_scroll.scroll_to_item(index);
        self.picker_focus.focus(window, cx);
        cx.notify();
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
        self.observed_models = Some(models.clone());
        self.ordered_enabled_ids = models.enabled.clone().unwrap_or_else(|| {
            models
                .catalog
                .iter()
                .filter(|m| m.credential_ready)
                .map(|m| format!("{}/{}", m.provider, m.id))
                .collect()
        });
        self.selected_row_idx = self
            .selected_row_idx
            .min(self.ordered_enabled_ids.len().saturating_sub(1));
    }

    fn save_scoped_with_rollback(&mut self, previous: Vec<String>, cx: &mut Context<Self>) {
        self.saving_scope = true;
        let next = self.ordered_enabled_ids.clone();
        let empty = next.is_empty();
        let client = self.client.clone();

        cx.spawn(async move |weak, cx| {
            let res = client.set_scoped_models(next, empty).await;
            let refreshed = client.refresh_models(None).await.is_ok();
            let _ = weak.update(cx, |this, cx| {
                this.saving_scope = false;
                if let Err(err) = res {
                    this.ordered_enabled_ids = previous;
                    cx.emit(PreferencesTabEvent::ErrorMessage(format!(
                        "保存启用模型范围失败，已自动回滚：{err}"
                    )));
                }
                if refreshed {
                    this.sync_from_snapshot();
                }
                this.selected_row_idx = this
                    .selected_row_idx
                    .min(this.ordered_enabled_ids.len().saturating_sub(1));
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_model_enabled(&mut self, full_id: String, cx: &mut Context<Self>) {
        if self.saving_scope {
            return;
        }
        let prev = self.ordered_enabled_ids.clone();
        if let Some(pos) = self.ordered_enabled_ids.iter().position(|x| x == &full_id) {
            self.ordered_enabled_ids.remove(pos);
        } else {
            self.ordered_enabled_ids.push(full_id);
        }
        cx.notify();
        self.save_scoped_with_rollback(prev, cx);
    }

    fn select_all_visible(&mut self, visible_full_ids: Vec<String>, cx: &mut Context<Self>) {
        if self.saving_scope {
            return;
        }
        let prev = self.ordered_enabled_ids.clone();
        for fid in visible_full_ids {
            if !self.ordered_enabled_ids.contains(&fid) {
                self.ordered_enabled_ids.push(fid);
            }
        }
        if self.ordered_enabled_ids != prev {
            cx.notify();
            self.save_scoped_with_rollback(prev, cx);
        }
    }

    fn clear_visible(&mut self, visible_full_ids: Vec<String>, cx: &mut Context<Self>) {
        if self.saving_scope {
            return;
        }
        let prev = self.ordered_enabled_ids.clone();
        self.ordered_enabled_ids
            .retain(|id| !visible_full_ids.contains(id));
        if self.ordered_enabled_ids != prev {
            cx.notify();
            self.save_scoped_with_rollback(prev, cx);
        }
    }

    fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        if self.refreshing_catalog {
            return;
        }
        self.refreshing_catalog = true;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.refresh_models_catalog().await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |this, cx| {
                this.refreshing_catalog = false;
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
        let len = self.ordered_enabled_ids.len();
        if idx >= len {
            return;
        }
        let target = idx as isize + delta;
        if target < 0 || (target as usize) >= len {
            return;
        }
        let prev = self.ordered_enabled_ids.clone();
        self.ordered_enabled_ids.swap(idx, target as usize);
        self.selected_row_idx = target as usize;
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
        cx.notify();
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.set_default_model(provider, model_id).await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |this, cx| {
                this.saving_default = false;
                if let Err(err) = res {
                    cx.emit(PreferencesTabEvent::ErrorMessage(err.to_string()));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if self.has_dropdown() {
            return;
        }
        if ks.modifiers.alt {
            match ks.key.as_str() {
                "up" => {
                    self.move_enabled_row(self.selected_row_idx, -1, cx);
                    cx.stop_propagation();
                }
                "down" => {
                    self.move_enabled_row(self.selected_row_idx, 1, cx);
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
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();
        let snap = self.client.snapshot();
        let models_snap = snap.models.as_ref();

        let catalog: &[ModelChoiceDto] = models_snap.map(|m| m.catalog.as_slice()).unwrap_or(&[]);
        let current_default = models_snap.and_then(|m| m.default_model.as_ref());

        let default_label = current_default
            .map(|d| {
                catalog
                    .iter()
                    .find(|m| m.provider == d.provider && m.id == d.id)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| d.id.clone())
            })
            .unwrap_or_else(|| "选择默认模型".to_owned());

        // 1. Default model `72px` card with `208×32px` custom dropdown trigger + frosted popover (`H2p2z`)
        let ready_models: Vec<_> = catalog.iter().filter(|m| m.credential_ready).collect();
        let choices: Vec<_> = ready_models
            .iter()
            .map(|m| (m.provider.clone(), m.id.clone()))
            .collect();
        let default_index = current_default
            .and_then(|d| {
                choices
                    .iter()
                    .position(|(p, m)| p == &d.provider && m == &d.id)
            })
            .unwrap_or(0);
        let mut dropdown_menu_opt = None;
        if self.default_dropdown_open {
            let mut items = div()
                .id("pref-default-model-scroll")
                .track_scroll(&self.picker_scroll)
                .w_full()
                .max_h(px(280.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(space::MICRO);

            if ready_models.is_empty() {
                items = items.child(
                    apply_scale(div(), scale::CAPTION)
                        .p(space::SM)
                        .text_color(palette.text_muted)
                        .child("暂无已配置凭据的可用模型"),
                );
            } else {
                for (idx, m) in ready_models.iter().enumerate() {
                    let is_def = idx == self.picker_index;
                    let prov = m.provider.clone();
                    let mid = m.id.clone();
                    let opt_id = SharedString::from(format!("pref-default-model-{prov}-{mid}"));
                    let mname_label = SharedString::from(m.name.clone());
                    let prov_label = SharedString::from(m.provider.clone());
                    let opt_aria = SharedString::from(format!("{} · {}", m.provider, m.name));
                    items = items.child(
                        apply_scale(div(), scale::UI)
                            .id(opt_id.clone())
                            .test_support()
                            .a11y_listitem(opt_id, opt_aria, is_def)
                            .flex_shrink_0()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .bg(if is_def {
                                palette.bg_item_active
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
                                this.set_default_model(prov.clone(), mid.clone(), window, cx);
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
                                    .children(is_def.then(|| {
                                        Icon::new(IconName::Check)
                                            .size(px(13.0))
                                            .text_color(palette.accent_ink)
                                    })),
                            ),
                    );
                }
            }
            let menu = motion::animate_popover_enter(
                "pref-default-model-pop-anim",
                render_frosted_surface(
                    "pref-default-model-popover",
                    FrostedGlassSpec::popover_h2p2z(&palette),
                    palette.bg_elevated,
                    transparency_enabled,
                    scale_factor,
                )
                .test_support()
                .a11y_dialog("pref-default-model-popover", "选择默认模型")
                .w(px(300.0))
                .p(space::XS)
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette.border_hairline)
                .occlude()
                .child(
                    div()
                        .w_full()
                        .focus_trap("pref-default-model-focus", &self.picker_focus)
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if super::move_picker_cursor(
                                event,
                                &mut this.picker_index,
                                choices.len(),
                                &this.picker_scroll,
                            ) {
                                cx.notify();
                                cx.stop_propagation();
                            } else if super::activates_button(event) {
                                if let Some((provider, model)) = choices.get(this.picker_index) {
                                    this.set_default_model(
                                        provider.clone(),
                                        model.clone(),
                                        window,
                                        cx,
                                    );
                                }
                                cx.stop_propagation();
                            }
                        }))
                        .child(items),
                ),
                palette.craft_stack_shadows(),
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
        for m in catalog {
            if !providers_seen.contains(&m.provider) {
                providers_seen.push(m.provider.clone());
            }
        }
        let filter_label = self
            .provider_filter
            .as_deref()
            .unwrap_or("全部服务")
            .to_owned();

        let filter_index = providers_seen
            .iter()
            .position(|p| Some(p) == self.provider_filter.as_ref())
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
                        .a11y_listitem("pref-filter-all", "全部服务", self.picker_index == 0)
                        .flex_shrink_0()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .bg(if self.picker_index == 0 {
                            palette.bg_item_active
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
                        .child(cjk_inline_label("全部服务"))
                        .children((self.picker_index == 0).then(|| {
                            Icon::new(IconName::Check)
                                .size(px(13.0))
                                .text_color(palette.accent_ink)
                        })),
                );

            for (index, prov) in providers_seen.iter().enumerate() {
                let active = self.picker_index == index + 1;
                let p_click = prov.clone();
                let filter_id = SharedString::from(format!("pref-filter-{prov}"));
                let prov_label = SharedString::from(prov.clone());
                items = items.child(
                    apply_scale(div(), scale::UI)
                        .id(filter_id.clone())
                        .test_support()
                        .a11y_listitem(filter_id, prov_label.clone(), active)
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
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.provider_filter = Some(p_click.clone());
                            this.dismiss_dropdowns(window, cx);
                        }))
                        .child(div().flex_1().min_w_0().truncate().child(prov_label))
                        .children(active.then(|| {
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
                    render_frosted_surface(
                        "pref-filter-popover",
                        FrostedGlassSpec::popover_h2p2z(&palette),
                        palette.bg_elevated,
                        transparency_enabled,
                        scale_factor,
                    )
                    .test_support()
                    .a11y_dialog("pref-filter-popover", "按模型服务筛选")
                    .w(px(220.0))
                    .p(space::XS)
                    .rounded(radius::ROW)
                    .border_1()
                    .border_color(palette.border_hairline)
                    .occlude()
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
                    palette.craft_stack_shadows(),
                    -6.0,
                ))
        });

        let search_q = self.search_input.read(cx).value().trim().to_lowercase();
        let matches_query = |prov: &str, mid: &str, mname: &str| -> bool {
            if let Some(ref pf) = self.provider_filter
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

        let mut visible_full_ids: Vec<String> = Vec::new();
        for full_id in &self.ordered_enabled_ids {
            let (prov, mid) = full_id.split_once('/').unwrap_or(("", full_id.as_str()));
            let mname = catalog
                .iter()
                .find(|m| m.provider == prov && m.id == mid)
                .map(|m| m.name.as_str())
                .unwrap_or(mid);
            if matches_query(prov, mid, mname) {
                visible_full_ids.push(full_id.clone());
            }
        }
        for m in catalog {
            let full_id = format!("{}/{}", m.provider, m.id);
            if !self.ordered_enabled_ids.contains(&full_id)
                && matches_query(&m.provider, &m.id, &m.name)
            {
                visible_full_ids.push(full_id);
            }
        }

        let vis_for_all = visible_full_ids.clone();
        let vis_for_clear = visible_full_ids.clone();

        let filter_row = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                div().w(px(340.0)).child(
                    Input::new(&self.search_input)
                        .accessibility_id("preferences-search-input")
                        .aria_label("搜索模型或服务")
                        .h(height::CONTROL)
                        .prefix(
                            Icon::new(IconName::Search)
                                .size(px(14.0))
                                .text_color(palette.text_muted),
                        )
                        .cleanable(true),
                ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id("pref-filter-dropdown-btn")
                    .test_support()
                    .a11y_toggle_button(
                        "pref-filter-dropdown-btn",
                        format!("筛选服务：{filter_label}"),
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
                    .min_w(px(108.0))
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
                    .gap(space::XS)
                    .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.refresh_catalog(cx);
                    }))
                    .child(
                        Icon::new(IconName::RefreshCw)
                            .size(px(13.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label("刷新")),
            );

        // 3. Enabled models `56px` rows (`16×16` checkbox + `16/24` model name + `140px` `14/20` provider + reorder)
        let mut list_card = div()
            .w_full()
            .rounded(radius::ROW)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_workspace)
            .flex()
            .flex_col();

        let mut rendered_count = 0usize;
        for (idx, full_id) in self.ordered_enabled_ids.iter().enumerate() {
            let (prov, mid) = full_id.split_once('/').unwrap_or(("", full_id.as_str()));
            let mname = catalog
                .iter()
                .find(|m| m.provider == prov && m.id == mid)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| mid.to_owned());
            if !matches_query(prov, mid, &mname) {
                continue;
            }
            let fid_chk = full_id.clone();
            let selected = idx == self.selected_row_idx;
            let row_id = SharedString::from(format!("pref-enabled-row-{idx}"));
            let chk_id = SharedString::from(format!("pref-toggle-enabled-{idx}"));
            let up_id = SharedString::from(format!("pref-move-up-{idx}"));
            let down_id = SharedString::from(format!("pref-move-down-{idx}"));
            let mut row = div()
                .id(row_id.clone())
                .test_support()
                .a11y_listitem(row_id, format!("{mname}（{prov}）"), selected)
                .w_full()
                .h(px(56.0))
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
                    this.selected_row_idx = idx;
                    this.focus_handle.focus(w, cx);
                    cx.notify();
                }))
                .child({
                    let chk_mark = motion::animated_checkbox_mark(
                        chk_id.clone(),
                        true,
                        px(11.0),
                        palette.text_on_accent,
                        window,
                        cx,
                    );
                    div()
                        .id(chk_id.clone())
                        .test_support()
                        .a11y_checkbox(chk_id, format!("启用模型：{mname}"), true)
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
                        .child(SharedString::from(mname.clone())),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .w(px(140.0))
                        .flex_shrink_0()
                        .truncate()
                        .text_color(palette.text_secondary)
                        .child(SharedString::from(prov.to_owned())),
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
                                .a11y_button(up_id, format!("上移模型：{mname}"))
                                .w(height::COMPACT)
                                .h(height::COMPACT)
                                .rounded(radius::CONTROL)
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_enabled_row(idx, -1, cx);
                                    cx.stop_propagation();
                                }))
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
                                .a11y_button(down_id, format!("下移模型：{mname}"))
                                .w(height::COMPACT)
                                .h(height::COMPACT)
                                .rounded(radius::CONTROL)
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(palette.bg_item_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_enabled_row(idx, 1, cx);
                                    cx.stop_propagation();
                                }))
                                .child(
                                    Icon::new(IconName::ArrowDown)
                                        .size(px(13.0))
                                        .text_color(palette.text_secondary),
                                ),
                        ),
                );
            if rendered_count > 0 {
                row = row.border_t_1().border_color(palette.border_subtle);
            }
            rendered_count += 1;
            list_card = list_card.child(row);
        }

        // Unchecked catalog models (`56px` rows)
        for m in catalog {
            let full_id = format!("{}/{}", m.provider, m.id);
            if self.ordered_enabled_ids.contains(&full_id)
                || !matches_query(&m.provider, &m.id, &m.name)
            {
                continue;
            }
            let fid_add = full_id.clone();
            let unchk_id = SharedString::from(format!("pref-unchecked-{full_id}"));
            let mut row = div()
                .id(unchk_id.clone())
                .test_support()
                .a11y_checkbox(
                    unchk_id,
                    format!("启用模型：{}（{}）", m.name, m.provider),
                    false,
                )
                .w_full()
                .h(px(56.0))
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
                        .child(SharedString::from(m.name.clone())),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .w(px(140.0))
                        .flex_shrink_0()
                        .truncate()
                        .text_color(palette.text_secondary)
                        .child(SharedString::from(m.provider.clone())),
                )
                .child(div().w(px(60.0)).flex_shrink_0());
            if rendered_count > 0 {
                row = row.border_t_1().border_color(palette.border_subtle);
            }
            rendered_count += 1;
            list_card = list_card.child(row);
        }

        if rendered_count == 0 {
            list_card = list_card.child(
                apply_scale(div(), scale::UI)
                    .p(space::LG)
                    .text_color(palette.text_muted)
                    .child("暂无匹配的模型项"),
            );
        }

        let enabled_count = self.ordered_enabled_ids.len();

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

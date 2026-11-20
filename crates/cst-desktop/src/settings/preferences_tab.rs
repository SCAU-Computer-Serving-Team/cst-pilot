use cst_client::{ClientHandle, ModelChoiceDto};
use gpui::KeyDownEvent;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt, Window, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopThemeGlobal, apply_scale, cjk_inline_label, height, radius, scale, space,
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
    ordered_enabled_ids: Vec<String>,
    selected_row_idx: usize,
    initialized_from_snapshot: bool,
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
        Self {
            client,
            search_input,
            provider_filter: None,
            default_dropdown_open: false,
            ordered_enabled_ids: Vec::new(),
            selected_row_idx: 0,
            initialized_from_snapshot: false,
            focus_handle: cx.focus_handle(),
        }
    }

    fn sync_from_snapshot_if_needed(&mut self) {
        if self.initialized_from_snapshot {
            return;
        }
        if let Some(models_snap) = self.client.snapshot().models.as_ref() {
            if let Some(ref enabled) = models_snap.enabled {
                self.ordered_enabled_ids = enabled.clone();
            } else {
                self.ordered_enabled_ids = models_snap
                    .catalog
                    .iter()
                    .filter(|m| m.credential_ready)
                    .map(|m| format!("{}/{}", m.provider, m.id))
                    .collect();
            }
            self.initialized_from_snapshot = true;
        }
    }

    fn save_scoped_with_rollback(&mut self, previous: Vec<String>, cx: &mut Context<Self>) {
        let next = self.ordered_enabled_ids.clone();
        let empty = next.is_empty();
        let client = self.client.clone();

        cx.spawn(async move |weak, cx| {
            let res = client.set_scoped_models(next, empty).await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |this, cx| {
                if let Err(err) = res {
                    this.ordered_enabled_ids = previous;
                    cx.emit(PreferencesTabEvent::ErrorMessage(format!(
                        "保存启用模型范围失败，已自动回滚：{err}"
                    )));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn toggle_model_enabled(&mut self, full_id: String, cx: &mut Context<Self>) {
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
        let prev = self.ordered_enabled_ids.clone();
        self.ordered_enabled_ids
            .retain(|id| !visible_full_ids.contains(id));
        if self.ordered_enabled_ids != prev {
            cx.notify();
            self.save_scoped_with_rollback(prev, cx);
        }
    }

    fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.refresh_models_catalog().await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |_, cx| {
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

    fn set_default_model(&mut self, provider: String, model_id: String, cx: &mut Context<Self>) {
        self.default_dropdown_open = false;
        cx.notify();
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.set_default_model(provider, model_id).await;
            let _ = client.refresh_models(None).await;
            let _ = weak.update(cx, |_, cx| {
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
        if ks.key == "escape" && self.default_dropdown_open {
            self.default_dropdown_open = false;
            cx.notify();
            cx.stop_propagation();
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_from_snapshot_if_needed();
        let palette = DesktopThemeGlobal::current(cx).palette;
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

        // 1. Default model `72px` card with `208×32px` custom dropdown trigger + floating popover
        let ready_models: Vec<_> = catalog.iter().filter(|m| m.credential_ready).collect();
        let mut dropdown_menu_opt = None;
        if self.default_dropdown_open {
            let mut menu = div()
                .id("pref-default-model-popover")
                .test_support()
                .a11y_dialog("pref-default-model-popover", "选择默认模型")
                .absolute()
                .top(px(60.0))
                .right(space::LG)
                .w(px(256.0))
                .max_h(px(280.0))
                .overflow_y_scroll()
                .p(space::XS)
                .rounded(radius::ROW)
                .border_1()
                .border_color(palette.border_subtle)
                .bg(palette.bg_elevated)
                .shadow(palette.craft_stack_shadows())
                .occlude()
                .flex()
                .flex_col();

            if ready_models.is_empty() {
                menu = menu.child(
                    apply_scale(div(), scale::CAPTION)
                        .p(space::SM)
                        .text_color(palette.text_muted)
                        .child("暂无已配置凭据的可用模型"),
                );
            } else {
                for m in &ready_models {
                    let is_def =
                        current_default.is_some_and(|d| d.provider == m.provider && d.id == m.id);
                    let prov = m.provider.clone();
                    let mid = m.id.clone();
                    let opt_id = SharedString::from(format!("pref-default-model-{prov}-{mid}"));
                    let opt_label = SharedString::from(format!("{} · {}", m.provider, m.name));
                    menu = menu.child(
                        apply_scale(div(), scale::UI)
                            .id(opt_id.clone())
                            .test_support()
                            .a11y_listitem(opt_id, opt_label.clone(), is_def)
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_default_model(prov.clone(), mid.clone(), cx);
                                cx.stop_propagation();
                            }))
                            .child(div().min_w_0().truncate().child(opt_label))
                            .children(is_def.then(|| {
                                Icon::new(IconName::Check)
                                    .size(px(13.0))
                                    .text_color(palette.accent_ink)
                            })),
                    );
                }
            }
            dropdown_menu_opt = Some(menu);
        }

        let default_card = div()
            .relative()
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
                    .w(px(208.0))
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(palette.bg_workspace)
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::XS)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.default_dropdown_open = !this.default_dropdown_open;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(default_label)),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(14.0))
                            .text_color(palette.text_secondary),
                    ),
            )
            .children(dropdown_menu_opt);

        // 2. Search + Provider filter bar + `"全选 / 清空 / 刷新"` ghost buttons
        let mut providers_seen = Vec::new();
        for m in catalog {
            if !providers_seen.contains(&m.provider) {
                providers_seen.push(m.provider.clone());
            }
        }
        let mut filter_pills = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space::XS)
            .child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id("pref-filter-all")
                    .test_support()
                    .a11y_tab("pref-filter-all", "全部", self.provider_filter.is_none())
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(if self.provider_filter.is_none() {
                        palette.border_subtle
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .bg(if self.provider_filter.is_none() {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .text_color(if self.provider_filter.is_none() {
                        palette.text_primary
                    } else {
                        palette.text_secondary
                    })
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.provider_filter = None;
                        cx.notify();
                    }))
                    .child(cjk_inline_label("全部")),
            );
        for prov in providers_seen {
            let active = self.provider_filter.as_deref() == Some(prov.as_str());
            let p_click = prov.clone();
            let filter_id = SharedString::from(format!("pref-filter-{prov}"));
            let prov_label = SharedString::from(prov);
            filter_pills = filter_pills.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(filter_id.clone())
                    .test_support()
                    .a11y_tab(filter_id, prov_label.clone(), active)
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(if active {
                        palette.border_subtle
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .bg(if active {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .text_color(if active {
                        palette.text_primary
                    } else {
                        palette.text_secondary
                    })
                    .flex()
                    .items_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.provider_filter = Some(p_click.clone());
                        cx.notify();
                    }))
                    .child(prov_label),
            );
        }

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

        let filter_toolbar = div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        div().w(px(208.0)).child(
                            Input::new(&self.search_input)
                                .accessibility_id("preferences-search-input")
                                .aria_label("搜索模型或服务")
                                .prefix(
                                    Icon::new(IconName::Search)
                                        .size(px(14.0))
                                        .text_color(palette.text_muted),
                                )
                                .cleanable(true),
                        ),
                    )
                    .child(filter_pills),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::XS)
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
                    ),
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
                .child(
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
                        .child(
                            Icon::new(IconName::Check)
                                .size(px(11.0))
                                .text_color(palette.text_on_accent),
                        ),
                )
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
                .child(
                    div()
                        .w(px(16.0))
                        .h(px(16.0))
                        .rounded(radius::CONTROL)
                        .border_1()
                        .border_color(palette.border_strong)
                        .bg(palette.bg_workspace)
                        .flex_shrink_0(),
                )
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
                apply_scale(div(), scale::TITLE)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label("模型偏好")),
            )
            .child(default_card)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("启用模型与顺序")),
                    )
                    .child(filter_toolbar)
                    .child(list_card),
            )
    }
}

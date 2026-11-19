use cst_client::{ClientHandle, ModelChoiceDto};
use gpui::KeyDownEvent;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::{
    Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

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
    provider_filter: Option<String>,
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
    pub fn new(client: ClientHandle, cx: &mut Context<Self>) -> Self {
        Self {
            client,
            provider_filter: None,
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
                    // Automatic rollback on failure!
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

        // 1. Default model selector for new sessions
        let mut default_pills = div().flex().flex_wrap().gap(space::XS);
        for m in catalog.iter().filter(|m| m.credential_ready) {
            let is_def = current_default.is_some_and(|d| d.provider == m.provider && d.id == m.id);
            let prov = m.provider.clone();
            let mid = m.id.clone();
            default_pills = default_pills.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(SharedString::from(format!(
                        "pref-default-model-{prov}-{mid}"
                    )))
                    .test_support()
                    .h(height::CONTROL)
                    .px(space::MD)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(if is_def {
                        palette.accent_solid
                    } else {
                        palette.border_subtle
                    })
                    .bg(if is_def {
                        palette.bg_item_active
                    } else {
                        palette.bg_elevated
                    })
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_default_model(prov.clone(), mid.clone(), cx);
                    }))
                    .child(SharedString::from(format!("{} · {}", m.provider, m.name))),
            );
        }

        // 2. Provider filter pills
        let mut providers_seen = Vec::new();
        for m in catalog {
            if !providers_seen.contains(&m.provider) {
                providers_seen.push(m.provider.clone());
            }
        }
        let mut filter_bar = div().flex().flex_wrap().gap(space::XS).child(
            apply_scale(div(), scale::CAPTION_MEDIUM)
                .id("pref-filter-all")
                .test_support()
                .h(height::COMPACT)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .bg(if self.provider_filter.is_none() {
                    palette.bg_item_active
                } else {
                    gpui_kit::transparent_black()
                })
                .text_color(palette.text_primary)
                .flex()
                .items_center()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.provider_filter = None;
                    cx.notify();
                }))
                .child(cjk_inline_label("全部服务")),
        );
        for prov in providers_seen {
            let active = self.provider_filter.as_deref() == Some(prov.as_str());
            let p_click = prov.clone();
            filter_bar = filter_bar.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(SharedString::from(format!("pref-filter-{prov}")))
                    .test_support()
                    .h(height::COMPACT)
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.provider_filter = Some(p_click.clone());
                        cx.notify();
                    }))
                    .child(SharedString::from(prov)),
            );
        }

        // 3. Enabled models ordered list + remaining catalog items
        let mut list_card = div()
            .w_full()
            .rounded(radius::SPECIAL)
            .border_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_surface)
            .flex()
            .flex_col();

        for (idx, full_id) in self.ordered_enabled_ids.iter().enumerate() {
            if let Some(ref pf) = self.provider_filter
                && !full_id.starts_with(&format!("{pf}/"))
            {
                continue;
            }
            let fid = full_id.clone();
            let fid_chk = fid.clone();
            let selected = idx == self.selected_row_idx;
            list_card = list_card.child(
                div()
                    .id(SharedString::from(format!("pref-enabled-row-{idx}")))
                    .test_support()
                    .px(space::LG)
                    .py(space::SM)
                    .border_b_1()
                    .border_color(palette.border_subtle)
                    .bg(if selected {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .flex()
                    .items_center()
                    .justify_between()
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.selected_row_idx = idx;
                        this.focus_handle.focus(w, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                div()
                                    .id(SharedString::from(format!("pref-toggle-enabled-{idx}")))
                                    .test_support()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .rounded(radius::CONTROL)
                                    .bg(palette.accent_solid)
                                    .flex()
                                    .items_center()
                                    .justify_center()
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
                                apply_scale(div(), scale::UI)
                                    .text_color(palette.text_primary)
                                    .child(SharedString::from(fid)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::MICRO)
                            .child(
                                div()
                                    .id(SharedString::from(format!("pref-move-up-{idx}")))
                                    .test_support()
                                    .w(height::INLINE)
                                    .h(height::INLINE)
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
                                            .size(px(12.0))
                                            .text_color(palette.text_secondary),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("pref-move-down-{idx}")))
                                    .test_support()
                                    .w(height::INLINE)
                                    .h(height::INLINE)
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
                                            .size(px(12.0))
                                            .text_color(palette.text_secondary),
                                    ),
                            ),
                    ),
            );
        }

        // Unchecked catalog models
        for m in catalog {
            let full_id = format!("{}/{}", m.provider, m.id);
            if self.ordered_enabled_ids.contains(&full_id) {
                continue;
            }
            if let Some(ref pf) = self.provider_filter
                && &m.provider != pf
            {
                continue;
            }
            let fid_add = full_id.clone();
            list_card = list_card.child(
                div()
                    .id(SharedString::from(format!("pref-unchecked-{full_id}")))
                    .test_support()
                    .px(space::LG)
                    .py(space::SM)
                    .border_b_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .gap(space::SM)
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
                            .bg(palette.bg_elevated),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .text_color(palette.text_secondary)
                            .child(SharedString::from(full_id)),
                    ),
            );
        }

        div()
            .id("settings-preferences-tab")
            .test_support()
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
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .text_color(palette.text_secondary)
                            .child("新会话默认模型（桌面／网页／终端共用）"),
                    )
                    .child(
                        div()
                            .p(space::LG)
                            .rounded(radius::SPECIAL)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_surface)
                            .child(default_pills),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.text_secondary)
                                    .child("启用模型范围与排序（支持 Alt + ↑/↓ 调整顺序）"),
                            )
                            .child(filter_bar),
                    )
                    .child(list_card),
            )
    }
}

pub mod general_tab;
pub mod preferences_tab;
pub mod providers_tab;
pub mod shortcuts_tab;
pub mod update_dialog;

use std::rc::Rc;

use cst_client::{
    ClientHandle, DefaultViewDto, DesktopSettingsPatchDto, ThemeModeDto, UpdatePhaseDto,
};
use gpui_kit::assets::IconName;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, TestSupportExt, Window,
    div, px,
};

use crate::settings::general_tab::render_general_tab;
use crate::settings::preferences_tab::{PreferencesTabEvent, PreferencesTabView};
use crate::settings::providers_tab::{ProvidersTabEvent, ProvidersTabView};
use crate::settings::shortcuts_tab::render_shortcuts_tab;
use crate::theme::{
    A11yElementExt as _, DesktopThemeGlobal, apply_desktop_theme, apply_scale, cjk_inline_label,
    height, layout, render_subpage_header_xwjof, scale, space,
};

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

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "常规",
            Self::Providers => "模型服务",
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
    providers_tab: Entity<ProvidersTabView>,
    preferences_tab: Entity<PreferencesTabView>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsViewEvent> for SettingsView {}

impl SettingsView {
    pub fn new(client: ClientHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let providers_tab = cx.new(|cx| ProvidersTabView::new(client.clone(), window, cx));
        let preferences_tab = cx.new(|cx| PreferencesTabView::new(client.clone(), window, cx));

        let mut subs = Vec::new();
        subs.push(cx.subscribe(&providers_tab, |_, _, ev, cx| match ev {
            ProvidersTabEvent::OpenLoginOrAccount { signed_in } => {
                cx.emit(SettingsViewEvent::OpenLoginOrAccount {
                    signed_in: *signed_in,
                });
            }
            ProvidersTabEvent::CredentialsSaved => {
                cx.emit(SettingsViewEvent::CredentialsSaved);
            }
            ProvidersTabEvent::Notice(msg) => {
                cx.emit(SettingsViewEvent::Notice(msg.clone()));
            }
            ProvidersTabEvent::ErrorMessage(msg) => {
                cx.emit(SettingsViewEvent::ErrorMessage(msg.clone()));
            }
        }));
        subs.push(cx.subscribe(&preferences_tab, |_, _, ev, cx| match ev {
            PreferencesTabEvent::Notice(msg) => {
                cx.emit(SettingsViewEvent::Notice(msg.clone()));
            }
            PreferencesTabEvent::ErrorMessage(msg) => {
                cx.emit(SettingsViewEvent::ErrorMessage(msg.clone()));
            }
        }));

        Self {
            client,
            category: SettingsCategory::General,
            providers_tab,
            preferences_tab,
            _subscriptions: subs,
        }
    }

    pub fn set_category(&mut self, category: SettingsCategory, cx: &mut Context<Self>) {
        self.category = category;
        cx.notify();
    }

    pub fn open_custom_gateway_creator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.category = SettingsCategory::Providers;
        self.providers_tab
            .update(cx, |pt, cx| pt.open_new_custom_provider(window, cx));
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let snap = self.client.snapshot();

        let mut tab_bar = div()
            .id("settings-tabbar-vpwaf")
            .test_support()
            .role(gpui_kit::Role::TabList)
            .accessibility_id("settings-tabbar-vpwaf")
            .aria_label("设置分类标签栏")
            .w_full()
            .h(height::FIELD)
            .border_b_1()
            .border_color(palette.border_subtle)
            .flex()
            .items_center()
            .gap(space::GROUP);

        for cat in SettingsCategory::ALL {
            let active = self.category == cat;
            let text_el = if active {
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label(cat.label()))
            } else {
                apply_scale(div(), scale::BODY)
                    .text_color(palette.text_secondary)
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
                        this.category = cat;
                        cx.notify();
                    }))
                    .child(text_el)
                    .children(active.then(|| {
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

        let entity = cx.entity();
        let e_theme = entity.clone();
        let e_view = entity.clone();
        let e_check = entity.clone();
        let e_cancel = entity.clone();

        let content_el = match self.category {
            SettingsCategory::General => render_general_tab(
                &snap.desktop_settings,
                &snap.version,
                snap.update_state.as_deref(),
                mono_family,
                &palette,
                Rc::new(move |theme: ThemeModeDto, w, cx| {
                    let _ = apply_desktop_theme(theme, Some(w), None, cx);
                    e_theme.update(cx, |this, cx| {
                        let c = this.client.clone();
                        cx.spawn(async move |weak, cx| {
                            let res = c
                                .patch_desktop_settings(DesktopSettingsPatchDto {
                                    theme: Some(theme),
                                    ..DesktopSettingsPatchDto::default()
                                })
                                .await;
                            if let Err(err) = res {
                                let _ = weak.update(cx, |_, cx| {
                                    cx.emit(SettingsViewEvent::ErrorMessage(err.to_string()));
                                });
                            }
                        })
                        .detach();
                        cx.notify();
                    });
                }),
                Rc::new(move |default_view: DefaultViewDto, _, cx| {
                    e_view.update(cx, |this, cx| {
                        let c = this.client.clone();
                        cx.spawn(async move |weak, cx| {
                            let res = c
                                .patch_desktop_settings(DesktopSettingsPatchDto {
                                    default_view: Some(default_view),
                                    ..DesktopSettingsPatchDto::default()
                                })
                                .await;
                            if let Err(err) = res {
                                let _ = weak.update(cx, |_, cx| {
                                    cx.emit(SettingsViewEvent::ErrorMessage(err.to_string()));
                                });
                            }
                        })
                        .detach();
                    });
                }),
                Rc::new(move |_, cx| {
                    e_check.update(cx, |this, cx| {
                        let cur_phase = this
                            .client
                            .snapshot()
                            .update_state
                            .as_ref()
                            .map(|u| u.status.phase);
                        if matches!(
                            cur_phase,
                            Some(UpdatePhaseDto::Available | UpdatePhaseDto::Ready)
                        ) {
                            cx.emit(SettingsViewEvent::OpenUpdateDialog);
                            return;
                        }
                        let c = this.client.clone();
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
                                    Ok(_) => {
                                        cx.emit(SettingsViewEvent::Notice(
                                            "当前已是最新版本。".into(),
                                        ));
                                    }
                                    Err(err) => {
                                        cx.emit(SettingsViewEvent::ErrorMessage(err.to_string()));
                                    }
                                }
                                cx.notify();
                            });
                        })
                        .detach();
                    });
                }),
                Rc::new(move |_, cx| {
                    e_cancel.update(cx, |_, cx| {
                        cx.emit(SettingsViewEvent::CancelUpdate);
                    });
                }),
            )
            .into_any_element(),
            SettingsCategory::Providers => self.providers_tab.clone().into_any_element(),
            SettingsCategory::Preferences => self.preferences_tab.clone().into_any_element(),
            SettingsCategory::Shortcuts => {
                render_shortcuts_tab(mono_family, &palette).into_any_element()
            }
        };

        div()
            .id("settings-view-vpwaf")
            .test_support()
            .a11y_region("settings-view", "设置")
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.bg_workspace)
            .child(render_subpage_header_xwjof(
                "settings-subpage-header",
                IconName::Settings,
                "设置",
                None,
                &palette,
            ))
            .child(
                div()
                    .id("settings-scroll-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .px(space::GROUP)
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
                            .gap(space::GROUP)
                            .child(tab_bar)
                            .child(content_el),
                    ),
            )
    }
}

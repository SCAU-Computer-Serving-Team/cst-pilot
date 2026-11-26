use cst_client::ThemeModeDto;

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, input::InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Entity, InteractiveElement, IntoElement, ParentElement, Pixels, Point,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt, div, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::theme::{
    DesktopPalette, apply_scale, cjk_inline_label, height, layout, motion, radius, scale, space,
};

use super::sidebar::{MAX_RECENT_SIDEBAR_SESSIONS, ShellRoute};
use super::{AppShell, RenameSessionModalState};

impl AppShell {
    /// Context menu `UWt8w` (`200px` wide, `4px` padding, `8px` radius, `H2p2z` frosted glass).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_sidebar_context_menu(
        &self,
        interactive: bool,
        progress: f32,
        sid: &str,
        anchor_pos: Point<Pixels>,
        viewport_size: gpui_kit::Size<Pixels>,
        palette: &DesktopPalette,
        _transparency_enabled: bool,
        _scale_factor: f32,
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

        let mut menu_box = div()
            .id(if interactive {
                "sidebar-session-context-menu"
            } else {
                "sidebar-session-context-menu-exit"
            })
            .opacity(0.01 + 0.99 * progress)
            .relative()
            .top(px((-4.0 * (1.0 - progress)).round()))
            .shadow(motion::scale_shadows(
                palette.flat_popover_shadows(),
                progress * progress,
            ))
            .w(px(200.0))
            .p(space::XS)
            .rounded(radius::ROW)
            .bg(palette.bg_glass)
            .border_1()
            .border_color(palette.border_subtle)
            .flex()
            .flex_col()
            .gap(space::MICRO);

        // In batch mode, right-click menu converges to single `"删除"`!
        if !self.sidebar_batch_mode {
            menu_box = menu_box
                .child(
                    apply_scale(div(), scale::UI)
                        .id(if interactive {
                            "sidebar-ctx-rename"
                        } else {
                            "sidebar-ctx-rename-exit"
                        })
                        .relative()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            Icon::new(IconName::Pencil)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("重命名"))
                        .map(|el| {
                            if interactive {
                                el.test_support()
                                    .a11y_menuitem("sidebar-ctx-rename", "重命名")
                                    .hover(|s| s.bg(palette.bg_item_active))
                                    .on_click(move |_, window, cx| {
                                        let target = sid_rename.clone();
                                        ent_rename.update(cx, |this, cx| {
                                            let prior =
                                                this.sidebar_menu_prior_focus.take().or_else(
                                                    || Some(this.sidebar_collapse_focus.clone()),
                                                );
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
                                                let mut st = InputState::new(window, cx)
                                                    .placeholder("输入新会话标题…");
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
                                    .into_any_element()
                            } else {
                                el.into_any_element()
                            }
                        }),
                )
                .child(
                    apply_scale(div(), scale::UI)
                        .id(if interactive {
                            "sidebar-ctx-manage"
                        } else {
                            "sidebar-ctx-manage-exit"
                        })
                        .relative()
                        .h(height::CONTROL)
                        .px(space::SM)
                        .rounded(radius::CONTROL)
                        .text_color(palette.text_primary)
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            Icon::new(IconName::ListChecks)
                                .size(px(16.0))
                                .text_color(palette.text_secondary),
                        )
                        .child(cjk_inline_label("管理"))
                        .map(|el| {
                            if interactive {
                                el.test_support()
                                    .a11y_menuitem("sidebar-ctx-manage", "管理")
                                    .hover(|s| s.bg(palette.bg_item_active))
                                    .on_click(move |_, window, cx| {
                                        ent_manage.update(cx, |this, cx| {
                                            this.dismiss_sidebar_menu(window, cx);
                                            this.sidebar_batch_mode = true;
                                            this.sidebar_selected_ids.clear();
                                            cx.notify();
                                        });
                                    })
                                    .into_any_element()
                            } else {
                                el.into_any_element()
                            }
                        }),
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
                .id(if interactive {
                    "sidebar-ctx-delete"
                } else {
                    "sidebar-ctx-delete-exit"
                })
                .relative()
                .h(height::CONTROL)
                .px(space::SM)
                .rounded(radius::CONTROL)
                .text_color(palette.status_danger)
                .flex()
                .items_center()
                .gap(space::SM)
                .child(
                    Icon::new(IconName::Trash)
                        .size(px(16.0))
                        .text_color(palette.status_danger),
                )
                .child(cjk_inline_label("删除"))
                .map(|el| {
                    if interactive {
                        el.test_support()
                            .a11y_menuitem("sidebar-ctx-delete", "删除")
                            .hover(|s| s.bg(palette.status_danger_subtle))
                            .on_click(move |_, window, cx| {
                                ent_del.update(cx, |this, cx| {
                                    this.dismiss_sidebar_menu(window, cx);
                                    this.all_sessions_view.update(cx, |view, cx| {
                                        view.delete_sessions_optimistic(vec![sid_del.clone()], cx)
                                    });
                                });
                            })
                            .into_any_element()
                    } else {
                        el.into_any_element()
                    }
                }),
        );

        let menu_box = if interactive {
            menu_box
                .test_support()
                .role(gpui::Role::Menu)
                .accessibility_id("sidebar-session-context-menu")
                .aria_label("会话右键菜单")
                .track_focus(&self.sidebar_menu_focus)
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .into_any_element()
        } else {
            menu_box.into_any_element()
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .when(interactive, |el| {
                el.on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                    ent_backdrop.update(cx, |this, cx| {
                        this.dismiss_sidebar_menu(window, cx);
                    });
                })
            })
            .child(
                div()
                    .absolute()
                    .top(px(menu_y))
                    .left(px(menu_x))
                    .child(menu_box),
            )
    }

    pub(super) fn render_recent_drawer_popover(
        &self,
        progress: f32,
        active_session_id: Option<&str>,
        palette: &DesktopPalette,
        _transparency_enabled: bool,
        _scale_factor: f32,
        entity: &Entity<Self>,
    ) -> impl IntoElement {
        let mut sorted: Vec<_> = self.snapshot.sessions.as_ref().clone();
        sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        if sorted.len() > MAX_RECENT_SIDEBAR_SESSIONS {
            sorted.truncate(MAX_RECENT_SIDEBAR_SESSIONS);
        }

        let interactive = self.recent_drawer_open;
        let mut rows = div()
            .id("recent-drawer-rows")
            .relative()
            .max_h(px(320.0))
            .when(interactive, |el| el.overflow_y_scroll())
            .when(!interactive, |el| el.overflow_hidden())
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
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .child(cjk_inline_label(title.clone())),
                    )
                    .map(|el| {
                        if interactive {
                            el.test_support()
                                .a11y_listitem(drawer_row_id, title, is_active)
                                .hover(|s| {
                                    s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                                })
                                .on_click(move |_, _, cx| {
                                    ent.update(cx, |this, cx| {
                                        this.activate_session_by_id(sid.clone(), cx)
                                    });
                                })
                                .into_any_element()
                        } else {
                            el.into_any_element()
                        }
                    }),
                );
            }
        }

        let ent_all = entity.clone();
        let ent_backdrop = entity.clone();

        let width = px(264.0);
        let anchor_x = self.recent_btn_anchor_x.get().max(16.0);
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .when(interactive, |el| {
                el.occlude()
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        ent_backdrop.update(cx, |this, cx| this.close_recent_drawer(window, cx));
                    })
            })
            .child(
                div().absolute().top(px(38.0)).left(px(anchor_x)).child(
                    div()
                        .id(if interactive {
                            "recent-sessions-drawer"
                        } else {
                            "recent-sessions-drawer-exit"
                        })
                        .opacity(0.01 + 0.99 * progress)
                        .relative()
                        .top(px((-8.0 * (1.0 - progress)).round()))
                        .shadow(motion::scale_shadows(
                            palette.flat_popover_shadows(),
                            progress.powi(3),
                        ))
                        .w(width)
                        .p(space::XS)
                        .rounded(radius::ROW)
                        .bg(palette.bg_glass)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .flex()
                        .flex_col()
                        .gap(space::MICRO)
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
                                .relative()
                                .h(height::CONTROL)
                                .px(space::SM)
                                .rounded(radius::CONTROL)
                                .text_color(palette.text_primary)
                                .flex()
                                .items_center()
                                .gap(space::SM)
                                .child(
                                    Icon::new(IconName::RotateCcwClock)
                                        .size(px(16.0))
                                        .text_color(palette.text_secondary),
                                )
                                .child(cjk_inline_label("查看全部会话"))
                                .map(|el| {
                                    if interactive {
                                        el.test_support()
                                            .a11y_button(
                                                "drawer-open-all-sessions-btn",
                                                "查看全部会话",
                                            )
                                            .hover(|s| s.bg(palette.bg_item_active))
                                            .on_click(move |_, window, cx| {
                                                ent_all.update(cx, |this, cx| {
                                                    this.close_recent_drawer(window, cx);
                                                    this.set_route(ShellRoute::AllSessions, cx);
                                                    this.all_sessions_view
                                                        .update(cx, |v, cx| v.focus(window, cx));
                                                });
                                            })
                                            .into_any_element()
                                    } else {
                                        el.into_any_element()
                                    }
                                }),
                        )
                        .map(|el| {
                            if interactive {
                                el.test_support()
                                    .a11y_dialog("recent-sessions-drawer", "最近会话抽屉")
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .into_any_element()
                            } else {
                                el.into_any_element()
                            }
                        }),
                ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_account_popover(
        &self,
        interactive: bool,
        progress: f32,
        palette: &DesktopPalette,
        _transparency_enabled: bool,
        _scale_factor: f32,
        entity: &Entity<Self>,
    ) -> impl IntoElement {
        let logged_in = self.snapshot.account.as_ref().is_some_and(|a| a.signed_in);
        let ent_auth = entity.clone();
        let ent_set = entity.clone();
        let ent_theme = entity.clone();
        let ent_feedback = entity.clone();
        let ent_exit = entity.clone();
        let ent_backdrop = entity.clone();

        let (theme_icon, theme_label) = match self.theme_preference {
            ThemeModeDto::System => (IconName::SunMoon, "跟随系统"),
            ThemeModeDto::Light => (IconName::Sun, "浅色"),
            ThemeModeDto::Dark => (IconName::Moon, "深色"),
        };

        let pos_div = if self.sidebar_collapsed {
            div().absolute().top(px(44.0)).right(px(124.0))
        } else {
            div()
                .absolute()
                .bottom(layout::SHELL_FRAME_INSET)
                .left(layout::SIDEBAR_WIDTH)
        };

        let account_identity = self
            .snapshot
            .account
            .as_ref()
            .filter(|a| a.signed_in)
            .map(|a| {
                let name = a.profile.name.as_deref().unwrap_or("已登录");
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
            .when(interactive, |el| {
                el.occlude()
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        ent_backdrop.update(cx, |this, cx| {
                            this.close_account_menu(window, cx);
                        });
                    })
            })
            .child(
                pos_div.child(
                    div()
                        .id(if interactive {
                            "account-popover-menu"
                        } else {
                            "account-popover-menu-exit"
                        })
                        .opacity(0.01 + 0.99 * progress)
                        .relative()
                        .top(px((if self.sidebar_collapsed { -6.0 } else { 6.0 }
                            * (1.0 - progress))
                            .round()))
                        .shadow(motion::scale_shadows(
                            palette.flat_popover_shadows(),
                            progress * progress,
                        ))
                        .w(px(224.0))
                        .p(space::XS)
                        .rounded(radius::ROW)
                        .bg(palette.bg_glass)
                        .border_1()
                        .border_color(palette.border_subtle)
                        .flex()
                        .flex_col()
                        .gap(space::MICRO)
                    .children(account_identity.map(|identity_label| {
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .child(
                                apply_scale(div(), scale::UI_MEDIUM)
                                    .id(if interactive {
                                        "account-menu-identity-header"
                                    } else {
                                        "account-menu-identity-header-exit"
                                    })
                                    .w_full()
                                    .px(space::SM)
                                    .py(space::SM)
                                    .text_color(palette.text_primary)
                                    .truncate()
                                    .child(cjk_inline_label(identity_label))
                                    .map(|el| {
                                        if interactive {
                                            el.test_support().into_any_element()
                                        } else {
                                            el.into_any_element()
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .relative()
                                    .px(space::XS)
                                    .py(space::MICRO)
                                    .child(div().w_full().h(px(1.0)).bg(palette.border_subtle)),
                            )
                    }))
                    .child(
                        apply_scale(div(), scale::UI)
                            .id(if interactive {
                                "account-menu-profile-btn"
                            } else {
                                "account-menu-profile-btn-exit"
                            })
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                Icon::new(IconName::User)
                                    .size(px(15.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label(if logged_in {
                                "账号信息"
                            } else {
                                "登录 CSTOA"
                            }))
                            .map(|el| {
                                if interactive {
                                    el.test_support()
                                        .a11y_menuitem(
                                            "account-menu-profile-btn",
                                            if logged_in {
                                                "账号信息"
                                            } else {
                                                "登录 CSTOA"
                                            },
                                        )
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
                                        .into_any_element()
                                } else {
                                    el.into_any_element()
                                }
                            }),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id(if interactive {
                                "account-menu-settings-btn"
                            } else {
                                "account-menu-settings-btn-exit"
                            })
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                Icon::new(IconName::Settings)
                                    .size(px(15.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("设置"))
                            .map(|el| {
                                if interactive {
                                    el.test_support()
                                        .a11y_menuitem("account-menu-settings-btn", "设置")
                                        .hover(|s| s.bg(palette.bg_item_active))
                                        .on_click(move |_, _, cx| {
                                            ent_set.update(cx, |this, cx| {
                                                this.account_menu_open = false;
                                                this.set_route(ShellRoute::Settings, cx);
                                                cx.notify();
                                            });
                                        })
                                        .into_any_element()
                                } else {
                                    el.into_any_element()
                                }
                            }),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id(if interactive {
                                "account-menu-theme-btn"
                            } else {
                                "account-menu-theme-btn-exit"
                            })
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(space::SM)
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
                            )
                            .map(|el| {
                                if interactive {
                                    el.test_support()
                                        .a11y_menuitem(
                                            "account-menu-theme-btn",
                                            format!("主题：{theme_label}"),
                                        )
                                        .hover(|s| s.bg(palette.bg_item_active))
                                        .on_click(move |_, window, cx| {
                                            ent_theme.update(cx, |this, cx| {
                                                this.cycle_theme_mode(window, cx);
                                            });
                                        })
                                        .into_any_element()
                                } else {
                                    el.into_any_element()
                                }
                            }),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id(if interactive {
                                "account-menu-feedback-btn"
                            } else {
                                "account-menu-feedback-btn-exit"
                            })
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                Icon::new(IconName::MessageSquare)
                                    .size(px(15.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("反馈问题"))
                            .map(|el| {
                                if interactive {
                                    el.test_support()
                                        .a11y_menuitem("account-menu-feedback-btn", "反馈问题")
                                        .hover(|s| s.bg(palette.bg_item_active))
                                        .on_click(move |_, window, cx| {
                                            ent_feedback.update(cx, |this, cx| {
                                                this.close_account_menu(window, cx);
                                            });
                                            super::open_url_in_trusted_browser(
                                                "https://github.com/SCAU-Computer-Serving-Team/cst-pilot/issues/new",
                                            );
                                        })
                                        .into_any_element()
                                } else {
                                    el.into_any_element()
                                }
                            }),
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
                            .id(if interactive {
                                "account-menu-exit-btn"
                            } else {
                                "account-menu-exit-btn-exit"
                            })
                            .relative()
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.status_danger)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                Icon::new(IconName::LogOut)
                                    .size(px(15.0))
                                    .text_color(palette.status_danger),
                            )
                            .child(cjk_inline_label("退出 CST Pilot"))
                            .map(|el| {
                                if interactive {
                                    el.test_support()
                                        .a11y_menuitem("account-menu-exit-btn", "退出 CST Pilot")
                                        .hover(|s| s.bg(palette.status_danger_subtle))
                                        .on_click(move |_, window, cx| {
                                            ent_exit.update(cx, |this, cx| {
                                                this.close_account_menu(window, cx);
                                                this.request_close_flow(window, cx);
                                            });
                                        })
                                        .into_any_element()
                                } else {
                                    el.into_any_element()
                                }
                            }),
                    )
                    .map(|el| {
                        if interactive {
                            el.test_support()
                                .role(gpui::Role::Menu)
                                .accessibility_id("account-popover-menu")
                                .aria_label("账号与偏好菜单")
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .into_any_element()
                        } else {
                            el.into_any_element()
                        }
                    }),
                ),
            )
    }

    /// Collapsed-sidebar session more menu (`分支树`, `导出会话`, `分享`) anchored beneath
    /// `topbar-session-more-btn` (`MKlBE` top-left).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_topbar_session_more_popover(
        &self,
        interactive: bool,
        progress: f32,
        session_id: &str,
        palette: &DesktopPalette,
        _transparency_enabled: bool,
        _scale_factor: f32,
        entity: &Entity<Self>,
    ) -> impl IntoElement {
        let sid_tree = session_id.to_owned();
        let sid_export = session_id.to_owned();
        let ent_tree = entity.clone();
        let ent_export = entity.clone();
        let ent_backdrop = entity.clone();
        let anchor_x = self.topbar_session_more_anchor_x.get().max(16.0);

        let menu_card = div()
            .id(if interactive {
                "topbar-session-more-menu"
            } else {
                "topbar-session-more-menu-exit"
            })
            .opacity(0.01 + 0.99 * progress)
            .relative()
            .top(px((-6.0 * (1.0 - progress)).round()))
            .shadow(motion::scale_shadows(
                palette.flat_popover_shadows(),
                progress * progress,
            ))
            .w(px(200.0))
            .p(space::XS)
            .rounded(radius::ROW)
            .bg(palette.bg_glass)
            .border_1()
            .border_color(palette.border_subtle)
            .flex()
            .flex_col()
            .gap(space::MICRO)
            .child(
                apply_scale(div(), scale::UI)
                    .id(if interactive {
                        "topbar-session-more-tree-btn"
                    } else {
                        "topbar-session-more-tree-btn-exit"
                    })
                    .relative()
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .text_color(palette.text_primary)
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size(px(15.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label("分支树"))
                    .map(|el| {
                        if interactive {
                            el.test_support()
                                .a11y_menuitem("topbar-session-more-tree-btn", "分支树")
                                .hover(|s| s.bg(palette.bg_item_active))
                                .on_click(move |_, window, cx| {
                                    let target = sid_tree.clone();
                                    ent_tree.update(cx, |this, cx| {
                                        this.topbar_session_more_open = false;
                                        this.topbar_session_more_prior_focus = None;
                                        this.open_session_tree(target, false, window, cx);
                                    });
                                })
                                .into_any_element()
                        } else {
                            el.into_any_element()
                        }
                    }),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id(if interactive {
                        "topbar-session-more-export-btn"
                    } else {
                        "topbar-session-more-export-btn-exit"
                    })
                    .relative()
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .text_color(palette.text_primary)
                    .child(
                        Icon::new(IconName::FileText)
                            .size(px(15.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label("导出会话"))
                    .map(|el| {
                        if interactive {
                            el.test_support()
                                .a11y_menuitem("topbar-session-more-export-btn", "导出会话")
                                .hover(|s| s.bg(palette.bg_item_active))
                                .on_click(move |_, window, cx| {
                                    let target = sid_export.clone();
                                    ent_export.update(cx, |this, cx| {
                                        this.topbar_session_more_open = false;
                                        this.topbar_session_more_prior_focus = None;
                                        this.open_session_export(target, false, window, cx);
                                    });
                                })
                                .into_any_element()
                        } else {
                            el.into_any_element()
                        }
                    }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .my(space::MICRO)
                    .h(px(1.0))
                    .bg(palette.border_subtle),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .id(if interactive {
                        "topbar-session-more-share-disabled"
                    } else {
                        "topbar-session-more-share-disabled-exit"
                    })
                    .relative()
                    .h(height::CONTROL)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .opacity(0.45)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .text_color(palette.text_secondary)
                    .child(
                        Icon::new(IconName::ExternalLink)
                            .size(px(15.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(cjk_inline_label("分享")),
            )
            .map(|el| {
                if interactive {
                    el.test_support()
                        .role(gpui::Role::Menu)
                        .accessibility_id("topbar-session-more-menu")
                        .aria_label("会话更多菜单")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .into_any_element()
                } else {
                    el.into_any_element()
                }
            });

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .when(interactive, |el| {
                el.occlude()
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        ent_backdrop.update(cx, |this, cx| {
                            this.close_topbar_session_more(window, cx);
                        });
                    })
            })
            .child(
                div()
                    .absolute()
                    .top(px(38.0))
                    .left(px(anchor_x))
                    .child(menu_card),
            )
    }
}

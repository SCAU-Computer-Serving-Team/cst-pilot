use super::*;

impl ProvidersTabView {
    pub(super) fn render_model_list_header(
        &self,
        title: &'static str,
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Strictly 3 icon-only action buttons (`search`, `refresh-cw`, `plus`) on right — ZERO text buttons!
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .child(
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(title),
            )
            .child(
                div()
                    .mr(px(-4.0))
                    .flex()
                    .items_center()
                    .gap(space::XS)
                    .child(
                        div()
                            .id("providers-hdr-search-btn")
                            .test_support()
                            .a11y_toggle_button(
                                "providers-hdr-search-btn",
                                "搜索模型",
                                self.search_visible,
                            )
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .active(|s| s.bg(palette.bg_active_press))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.search_visible = !this.search_visible;
                                if this.search_visible {
                                    this.search_input.update(cx, |i, cx| i.focus(w, cx));
                                }
                                cx.notify();
                            }))
                            .child(
                                Icon::new(IconName::Search)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    )
                    .child(
                        div()
                            .id("providers-hdr-refresh-btn")
                            .test_support()
                            .a11y_button("providers-hdr-refresh-btn", "刷新模型目录")
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .active(|s| s.bg(palette.bg_active_press))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.trigger_refresh_models(cx);
                            }))
                            .child(
                                Icon::new(IconName::RefreshCw)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    )
                    .child(
                        div()
                            .id("providers-hdr-add-model-btn")
                            .test_support()
                            .a11y_button("providers-hdr-add-model-btn", "添加模型行")
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .active(|s| s.bg(palette.bg_active_press))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.append_blank_editable_row(w, cx);
                            }))
                            .child(
                                Icon::new(IconName::Plus)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_token_cell(
        &self,
        row_idx: usize,
        kind: TokenFieldKind,
        value: u64,
        col_width: gpui_kit::Pixels,
        _mono_family: &SharedString,
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_editing = self.inline_token_edit == Some((row_idx, kind));
        let formatted = format_token_window(value);
        let cell_id = SharedString::from(match kind {
            TokenFieldKind::ContextWindow => format!("edit-row-cw-{row_idx}"),
            TokenFieldKind::MaxTokens => format!("edit-row-mt-{row_idx}"),
        });
        let aria_label = match kind {
            TokenFieldKind::ContextWindow => {
                format!("上下文窗口：{formatted}（单击切换，双击输入）")
            }
            TokenFieldKind::MaxTokens => format!("最大输出：{formatted}（单击切换，双击输入）"),
        };
        let presets = match kind {
            TokenFieldKind::ContextWindow => CONTEXT_WINDOW_PRESETS,
            TokenFieldKind::MaxTokens => MAX_OUTPUT_PRESETS,
        };

        let mut cell = div()
            .relative()
            .w(col_width)
            .h(height::FIELD)
            .flex()
            .items_center()
            .border_b_1()
            .border_color(if is_editing {
                palette.accent_solid
            } else {
                gpui_kit::transparent_black()
            });

        if is_editing {
            cell = cell.child(
                div()
                    .w_full()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.modifiers != gpui::Modifiers::default() {
                            return;
                        }
                        match event.keystroke.key.as_str() {
                            "enter" if !event.is_held => {
                                this.commit_inline_token_edit(cx);
                                cx.stop_propagation();
                            }
                            "escape" if !event.is_held => {
                                this.inline_token_edit = None;
                                cx.notify();
                                cx.stop_propagation();
                            }
                            _ => {}
                        }
                    }))
                    .child(
                        Input::new(&self.inline_token_input)
                            .accessibility_id(SharedString::from(format!("{cell_id}-input")))
                            .aria_label(aria_label)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .p_0()
                            .text_size(scale::UI.font_size)
                            .line_height(scale::UI.line_height)
                            .w_full()
                            .h(height::CONTROL),
                    ),
            );
        } else {
            cell = cell.child(
                apply_scale(div(), scale::UI)
                    .id(cell_id.clone())
                    .test_support()
                    .a11y_button(cell_id, aria_label)
                    .w_full()
                    .h_full()
                    .text_color(palette.text_primary)
                    .flex()
                    .items_center()
                    .hover(|s| s.text_color(palette.accent_ink))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                this.start_inline_token_edit(row_idx, kind, window, cx);
                            } else {
                                this.protocol_dropdown_open = false;
                                this.commit_inline_token_edit(cx);
                                if let Some(row) = this.editable_rows.get_mut(row_idx) {
                                    match kind {
                                        TokenFieldKind::ContextWindow => {
                                            row.context_window =
                                                cycle_token_preset(row.context_window, presets);
                                        }
                                        TokenFieldKind::MaxTokens => {
                                            row.max_tokens =
                                                cycle_token_preset(row.max_tokens, presets);
                                        }
                                    }
                                }
                                cx.notify();
                            }
                        }),
                    )
                    .child(SharedString::from(formatted)),
            );
        }

        cell
    }

    pub(super) fn render_custom_provider_editor(
        &self,
        mono_family: SharedString,
        palette: &DesktopPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name_focused = self
            .provider_name_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let pid_focused = self
            .provider_id_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let url_focused = self
            .base_url_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let key_focused = self
            .api_key_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);

        let pid_empty =
            self.form_error.is_some() && self.provider_id_input.read(cx).value().trim().is_empty();
        let name_empty = self.form_error.is_some()
            && self.provider_name_input.read(cx).value().trim().is_empty();
        let url_empty =
            self.form_error.is_some() && self.base_url_input.read(cx).value().trim().is_empty();

        // Flat deferred protocol dropdown popover aligned to trigger text (`left: -12px` offsets `p: 4px + item px: 8px`)
        let kbd_style = self.client.snapshot().desktop_settings.keyboard_focus_style;
        let protocol_popover = self.protocol_dropdown_open.then(|| {
            self.protocol_tracker.clear();
            let kbd_nav = self.protocol_kbd_active;
            let active_proto_spec = kbd_nav.then(|| {
                KbdDynamicSpec::row(format!(
                    "proto-{}",
                    self.protocol_dropdown_index
                        .min(CustomApiProtocol::ALL.len() - 1)
                ))
            });
            let mut items = div().w_full().flex().flex_col().gap(space::MICRO);
            for (idx, proto) in CustomApiProtocol::ALL.iter().copied().enumerate() {
                let is_selected = self.protocol == proto;
                let tr = self.protocol_tracker.clone();
                let key = SharedString::from(format!("proto-{idx}"));
                let proto_id = SharedString::from(format!("custom-proto-{}", proto.wire_value()));
                items = items.child(
                    apply_scale(div(), scale::UI)
                        .id(proto_id.clone())
                        .test_support()
                        .a11y_listitem(proto_id, proto.label(), is_selected)
                        .h(px(34.0))
                        .px(px(10.0))
                        .rounded(radius::CONTROL)
                        .text_color(if is_selected {
                            palette.text_primary
                        } else {
                            palette.text_secondary
                        })
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::SM)
                        .hover(move |s| {
                            if kbd_nav {
                                s
                            } else {
                                s.bg(palette.bg_item_hover).text_color(palette.text_primary)
                            }
                        })
                        .active(|s| s.bg(palette.bg_active_press))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.protocol = proto;
                            this.protocol_dropdown_open = false;
                            this.protocol_kbd_active = false;
                            this.protocol_trigger_focus.focus(window, cx);
                            cx.notify();
                            cx.stop_propagation();
                        }))
                        .on_prepaint(move |bounds, _, _| tr.set(key.clone(), bounds))
                        .child(SharedString::from(proto.label()))
                        .children(is_selected.then(|| {
                            Icon::new(IconName::Check)
                                .size(px(13.0))
                                .text_color(palette.accent_ink)
                        })),
                );
            }

            gpui::deferred(
                div().absolute().top(px(46.0)).left(px(-16.0)).child(
                    motion::animate_popover_enter(
                        "custom-protocol-popover-anim",
                        div()
                            .id("custom-protocol-popover")
                            .test_support()
                            .a11y_dialog("custom-protocol-popover", "选择接口协议")
                            .relative()
                            .w(px(276.0))
                            .p(px(6.0))
                            .rounded(radius::ROW)
                            .bg(palette.bg_glass)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .occlude()
                            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                                if this.protocol_kbd_active
                                    && pointer_moved_past_threshold(
                                        &mut this.protocol_last_pointer,
                                        ev.position,
                                    )
                                {
                                    this.protocol_kbd_active = false;
                                    cx.notify();
                                }
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .focus_trap("custom-protocol-focus", &self.protocol_focus)
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if event.keystroke.modifiers
                                                != gpui::Modifiers::default()
                                            {
                                                return;
                                            }
                                            let len = CustomApiProtocol::ALL.len();
                                            match event.keystroke.key.as_str() {
                                                "up" => {
                                                    this.protocol_dropdown_index = this
                                                        .protocol_dropdown_index
                                                        .checked_sub(1)
                                                        .unwrap_or(len - 1);
                                                    this.protocol_kbd_active = true;
                                                    this.protocol_last_pointer =
                                                        Some(window.mouse_position());
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                                "down" => {
                                                    this.protocol_dropdown_index =
                                                        (this.protocol_dropdown_index + 1) % len;
                                                    this.protocol_kbd_active = true;
                                                    this.protocol_last_pointer =
                                                        Some(window.mouse_position());
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                                "enter" | "space" if !event.is_held => {
                                                    if let Some(&proto) = CustomApiProtocol::ALL
                                                        .get(this.protocol_dropdown_index)
                                                    {
                                                        this.protocol = proto;
                                                    }
                                                    this.protocol_dropdown_open = false;
                                                    this.protocol_kbd_active = false;
                                                    this.protocol_trigger_focus.focus(window, cx);
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                                "escape" if !event.is_held => {
                                                    this.protocol_dropdown_open = false;
                                                    this.protocol_kbd_active = false;
                                                    this.protocol_trigger_focus.focus(window, cx);
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                                _ => {}
                                            }
                                        },
                                    ))
                                    .child(items),
                            )
                            .child(render_gliding_dynamic_indicator(
                                "custom-proto-pop",
                                kbd_style,
                                active_proto_spec,
                                self.protocol_tracker.clone(),
                                palette,
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

        // Borderless hairline connection properties group (`Geto8` -> `bB67m` -> `Yop98`)
        let connection_section = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(
                apply_scale(div(), scale::BODY_SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(cjk_inline_label("服务连接")),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    // 1. 服务名称 (`MfnjS`)
                    .child(
                        div()
                            .w_full()
                            .h(height::FIELD)
                            .border_b_1()
                            .border_color(if name_empty {
                                palette.status_danger
                            } else if name_focused {
                                palette.accent_solid
                            } else {
                                palette.border_subtle
                            })
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(184.0))
                                    .flex_shrink_0()
                                    .text_color(if name_focused {
                                        palette.accent_ink
                                    } else {
                                        palette.text_secondary
                                    })
                                    .child(cjk_inline_label("服务名称")),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.provider_name_input)
                                        .accessibility_id("providers-name-input")
                                        .disabled(self.loading || self.saving)
                                        .aria_label("服务名称")
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .p_0()
                                        .text_size(scale::UI.font_size)
                                        .line_height(scale::UI.line_height)
                                        .w_full()
                                        .h(height::CONTROL),
                                ),
                            ),
                    )
                    // 2. 服务 ID (`jwCXL`)
                    .child(
                        div()
                            .w_full()
                            .h(height::FIELD)
                            .border_b_1()
                            .border_color(if pid_empty {
                                palette.status_danger
                            } else if pid_focused {
                                palette.accent_solid
                            } else {
                                palette.border_subtle
                            })
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(184.0))
                                    .flex_shrink_0()
                                    .text_color(if pid_focused {
                                        palette.accent_ink
                                    } else {
                                        palette.text_secondary
                                    })
                                    .child(cjk_inline_label("服务 ID")),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.provider_id_input)
                                        .accessibility_id("providers-id-input")
                                        .disabled(
                                            self.loading
                                                || self.saving
                                                || !self.creating_new_custom,
                                        )
                                        .aria_label("服务 ID")
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .p_0()
                                        .text_size(scale::UI.font_size)
                                        .line_height(scale::UI.line_height)
                                        .w_full()
                                        .h(height::CONTROL),
                                ),
                            ),
                    )
                    // 3. 接口协议 (`u0VG75`, borderless dropdown row)
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(height::FIELD)
                            .border_b_1()
                            .border_color(if self.protocol_dropdown_open {
                                palette.accent_solid
                            } else {
                                palette.border_subtle
                            })
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(184.0))
                                    .flex_shrink_0()
                                    .text_color(if self.protocol_dropdown_open {
                                        palette.accent_ink
                                    } else {
                                        palette.text_secondary
                                    })
                                    .child(cjk_inline_label("接口协议")),
                            )
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .h_full()
                                    .child(
                                        apply_scale(div(), scale::UI)
                                            .id("custom-protocol-dropdown-trigger")
                                            .test_support()
                                            .a11y_toggle_button(
                                                "custom-protocol-dropdown-trigger",
                                                format!("接口协议：{}", self.protocol.label()),
                                                self.protocol_dropdown_open,
                                            )
                                            .track_focus(&self.protocol_trigger_focus)
                                            .tab_index(0)
                                            .mx(px(-10.0))
                                            .my(px(6.0))
                                            .h(height::CONTROL)
                                            .px(px(10.0))
                                            .rounded(radius::CONTROL)
                                            .bg(if self.protocol_dropdown_open {
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
                                            .active(|s| s.bg(palette.bg_active_press))
                                            .capture_any_mouse_down(
                                                crate::shell::keep_popover_open_on_mouse_down,
                                            )
                                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation();
                                            })
                                            .on_key_down(cx.listener(
                                                |this, ev: &KeyDownEvent, window, cx| {
                                                    if !ev.is_held
                                                        && ev.keystroke.modifiers
                                                            == gpui::Modifiers::default()
                                                        && matches!(
                                                            ev.keystroke.key.as_str(),
                                                            "enter" | "space" | "down"
                                                        )
                                                    {
                                                        this.commit_inline_token_edit(cx);
                                                        this.protocol_dropdown_open = true;
                                                        this.protocol_kbd_active = true;
                                                        this.protocol_last_pointer =
                                                            Some(window.mouse_position());
                                                        this.protocol_dropdown_index =
                                                            CustomApiProtocol::ALL
                                                                .iter()
                                                                .position(|&p| p == this.protocol)
                                                                .unwrap_or(0);
                                                        this.protocol_focus.focus(window, cx);
                                                        cx.notify();
                                                        cx.stop_propagation();
                                                    }
                                                },
                                            ))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.commit_inline_token_edit(cx);
                                                this.protocol_dropdown_open =
                                                    !this.protocol_dropdown_open;
                                                this.protocol_kbd_active = false;
                                                if this.protocol_dropdown_open {
                                                    this.protocol_dropdown_index =
                                                        CustomApiProtocol::ALL
                                                            .iter()
                                                            .position(|&p| p == this.protocol)
                                                            .unwrap_or(0);
                                                    this.protocol_focus.focus(window, cx);
                                                } else {
                                                    this.protocol_trigger_focus.focus(window, cx);
                                                }
                                                cx.notify();
                                                cx.stop_propagation();
                                            }))
                                            .child(SharedString::from(self.protocol.label()))
                                            .child(motion::animated_chevron(
                                                "custom-protocol-chevron",
                                                self.protocol_dropdown_open,
                                                true,
                                                px(14.0),
                                                palette.text_secondary,
                                                window,
                                                cx,
                                            )),
                                    )
                                    .children(protocol_popover),
                            ),
                    )
                    // 4. Base URL (`W2aZbT`)
                    .child(
                        div()
                            .w_full()
                            .h(height::FIELD)
                            .border_b_1()
                            .border_color(if url_empty {
                                palette.status_danger
                            } else if url_focused {
                                palette.accent_solid
                            } else {
                                palette.border_subtle
                            })
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(184.0))
                                    .flex_shrink_0()
                                    .text_color(if url_focused {
                                        palette.accent_ink
                                    } else {
                                        palette.text_secondary
                                    })
                                    .child("Base URL"),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.base_url_input)
                                        .accessibility_id("providers-baseurl-input")
                                        .disabled(self.loading || self.saving)
                                        .aria_label("服务 Base URL")
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .p_0()
                                        .text_size(scale::UI.font_size)
                                        .line_height(scale::UI.line_height)
                                        .w_full()
                                        .h(height::CONTROL),
                                ),
                            ),
                    )
                    // 5. API Key (`l0m9s`)
                    .child(
                        div()
                            .w_full()
                            .h(height::FIELD)
                            .border_b_1()
                            .border_color(if key_focused {
                                palette.accent_solid
                            } else {
                                palette.border_subtle
                            })
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(184.0))
                                    .flex_shrink_0()
                                    .text_color(if key_focused {
                                        palette.accent_ink
                                    } else {
                                        palette.text_secondary
                                    })
                                    .child("API Key"),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.api_key_input)
                                        .accessibility_id("providers-apikey-input")
                                        .disabled(self.loading || self.saving)
                                        .content_type(InputContentType::Password)
                                        .aria_label("服务 API Key")
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .p_0()
                                        .text_size(scale::UI.font_size)
                                        .line_height(scale::UI.line_height)
                                        .w_full()
                                        .h(height::CONTROL)
                                        .mask_toggle(),
                                ),
                            ),
                    ),
            );

        // Borderless hairline editable model table (`Geto8` -> `haI3x`, columns `184 / 204 / 112 / 104 / 48 / 48 / 20`)
        let mut hairline_table = div()
            .id("providers-hairline-edit-table")
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .h(height::CONTROL)
                    .border_b_1()
                    .border_color(palette.border_strong)
                    .flex()
                    .items_center()
                    .gap(space::LG)
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .w(px(184.0))
                            .flex_shrink_0()
                            .text_color(palette.text_muted)
                            .child(cjk_inline_label("模型 ID")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .flex_1()
                            .min_w_0()
                            .text_color(palette.text_muted)
                            .child(cjk_inline_label("显示名称")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .w(px(108.0))
                            .flex_shrink_0()
                            .text_color(palette.text_muted)
                            .child(cjk_inline_label("上下文窗口")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .w(px(96.0))
                            .flex_shrink_0()
                            .text_color(palette.text_muted)
                            .child(cjk_inline_label("最大输出")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .w(px(44.0))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .text_color(palette.text_muted)
                            .child(cjk_inline_label("思考")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .w(px(44.0))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .text_color(palette.text_muted)
                            .child(cjk_inline_label("图片")),
                    )
                    .child(div().w(px(28.0)).flex_shrink_0()),
            );

        let filter_q = self.search_input.read(cx).value().to_lowercase();
        for (idx, row) in self.editable_rows.iter().enumerate() {
            if self.search_visible && !filter_q.is_empty() {
                let id_v = row.id_input.read(cx).value().to_lowercase();
                let nm_v = row.name_input.read(cx).value().to_lowercase();
                if !id_v.contains(&filter_q) && !nm_v.contains(&filter_q) {
                    continue;
                }
            }
            let reasoning = row.reasoning;
            let image = row.image;
            let id_focused = row.id_input.read(cx).focus_handle(cx).is_focused(window);
            let name_focused = row.name_input.read(cx).focus_handle(cx).is_focused(window);
            let id_filled = !row.id_input.read(cx).value().trim().is_empty();
            let name_filled = !row.name_input.read(cx).value().trim().is_empty();
            let show_delete = !row.is_new || (id_filled && name_filled);
            let rm_id = SharedString::from(format!("edit-row-remove-{idx}"));

            hairline_table = hairline_table.child(
                div()
                    .w_full()
                    .h(height::FIELD)
                    .border_b_1()
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .gap(space::LG)
                    .child(
                        div()
                            .w(px(184.0))
                            .h_full()
                            .flex_shrink_0()
                            .border_b_1()
                            .border_color(if id_focused {
                                palette.accent_solid
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .flex()
                            .items_center()
                            .child(
                                Input::new(&row.id_input)
                                    .disabled(self.saving)
                                    .accessibility_id(SharedString::from(format!(
                                        "providers-row-id-input-{idx}"
                                    )))
                                    .aria_label(format!("第 {} 行模型 ID", idx + 1))
                                    .appearance(false)
                                    .bordered(false)
                                    .focus_bordered(false)
                                    .p_0()
                                    .text_size(scale::UI.font_size)
                                    .line_height(scale::UI.line_height)
                                    .w_full()
                                    .h(height::CONTROL),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .border_b_1()
                            .border_color(if name_focused {
                                palette.accent_solid
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .flex()
                            .items_center()
                            .child(
                                Input::new(&row.name_input)
                                    .disabled(self.saving)
                                    .accessibility_id(SharedString::from(format!(
                                        "providers-row-name-input-{idx}"
                                    )))
                                    .aria_label(format!("第 {} 行模型显示名称", idx + 1))
                                    .appearance(false)
                                    .bordered(false)
                                    .focus_bordered(false)
                                    .p_0()
                                    .text_size(scale::UI.font_size)
                                    .line_height(scale::UI.line_height)
                                    .w_full()
                                    .h(height::CONTROL),
                            ),
                    )
                    .child(self.render_token_cell(
                        idx,
                        TokenFieldKind::ContextWindow,
                        row.context_window,
                        px(108.0),
                        &mono_family,
                        palette,
                        cx,
                    ))
                    .child(self.render_token_cell(
                        idx,
                        TokenFieldKind::MaxTokens,
                        row.max_tokens,
                        px(96.0),
                        &mono_family,
                        palette,
                        cx,
                    ))
                    .child(
                        div()
                            .w(px(44.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(capability_checkbox(
                                SharedString::from(format!("edit-row-reasoning-{idx}")),
                                SharedString::from(format!("第 {} 行支持思考", idx + 1)),
                                reasoning,
                                palette,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(r) = this.editable_rows.get_mut(idx) {
                                        r.reasoning = !r.reasoning;
                                        cx.notify();
                                    }
                                }),
                            )),
                    )
                    .child(
                        div()
                            .w(px(44.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(capability_checkbox(
                                SharedString::from(format!("edit-row-image-{idx}")),
                                SharedString::from(format!("第 {} 行支持图片", idx + 1)),
                                image,
                                palette,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(r) = this.editable_rows.get_mut(idx) {
                                        r.image = !r.image;
                                        cx.notify();
                                    }
                                }),
                            )),
                    )
                    .child(
                        div()
                            .w(px(28.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .children(show_delete.then(|| {
                                div()
                                    .id(rm_id.clone())
                                    .test_support()
                                    .a11y_button(rm_id, format!("移除第 {} 行模型", idx + 1))
                                    .w(height::COMPACT)
                                    .h(height::COMPACT)
                                    .rounded(radius::CONTROL)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|s| s.bg(palette.bg_item_hover))
                                    .active(|s| s.bg(palette.bg_active_press))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if idx < this.editable_rows.len() {
                                            this.editable_rows.remove(idx);
                                            if this.inline_token_edit.is_some_and(|(r, _)| r >= idx)
                                            {
                                                this.inline_token_edit = None;
                                            }
                                            cx.notify();
                                        }
                                    }))
                                    .child(
                                        Icon::new(IconName::Trash)
                                            .size(px(16.0))
                                            .text_color(palette.text_muted),
                                    )
                            })),
                    ),
            );
        }

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::GROUP)
            .child(connection_section)
            .children(self.form_error.clone().map(|err| {
                apply_scale(div(), scale::CAPTION)
                    .id("providers-form-error-text")
                    .test_support()
                    .a11y_status("providers-form-error-text", err.clone())
                    .text_color(palette.status_danger)
                    .child(err)
            }))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(space::SM)
                    .child(self.render_model_list_header("模型", palette, cx))
                    .children(self.search_visible.then(|| {
                        let search_focused = self
                            .search_input
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window);
                        let search_focus_ent = self.search_input.clone();
                        div()
                            .id("providers-custom-search-box")
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
                                        .accessibility_id("providers-search-input")
                                        .aria_label("筛选模型")
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .p_0(),
                                ),
                            )
                    }))
                    .child(hairline_table),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-inline-cancel-btn")
                            .test_support()
                            .a11y_button("providers-inline-cancel-btn", "取消")
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_workspace)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .active(|s| s.bg(palette.bg_active_press))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.return_to_provider_list(window, cx);
                            }))
                            .child(cjk_inline_label("取消")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("providers-inline-save-btn")
                            .test_support()
                            .a11y_button(
                                "providers-inline-save-btn",
                                if self.saving {
                                    "正在保存"
                                } else if self.creating_new_custom {
                                    "添加提供商"
                                } else {
                                    "保存"
                                },
                            )
                            .h(height::CONTROL)
                            .px(space::LG)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .active(|s| s.opacity(0.88))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.save_inline_provider_form(w, cx);
                            }))
                            .child(cjk_inline_label(if self.saving {
                                "正在保存…"
                            } else if self.creating_new_custom {
                                "添加提供商"
                            } else {
                                "保存"
                            })),
                    ),
            )
            .children(
                (!self.creating_new_custom && self.active_provider_id.is_some()).then(|| {
                    self.render_remove_credentials_section(palette, kbd_style, window, cx)
                }),
            )
    }
}

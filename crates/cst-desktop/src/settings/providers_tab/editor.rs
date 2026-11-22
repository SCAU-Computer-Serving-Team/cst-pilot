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
                    .child(cjk_inline_label(title)),
            )
            .child(
                div()
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

    pub(super) fn render_custom_provider_editor(
        &self,
        mono_family: SharedString,
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut protocol_row = div().flex().flex_wrap().gap(space::XS);
        for proto in CustomApiProtocol::ALL {
            let active = self.protocol == proto;
            let proto_id = SharedString::from(format!("custom-proto-{}", proto.wire_value()));
            protocol_row = protocol_row.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(proto_id.clone())
                    .test_support()
                    .a11y_tab(proto_id, proto.label(), active)
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(if active {
                        palette.border_strong
                    } else {
                        palette.border_subtle
                    })
                    .bg(if active {
                        palette.bg_item_active
                    } else {
                        palette.bg_workspace
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
                        this.protocol = proto;
                        cx.notify();
                    }))
                    .child(SharedString::from(proto.label())),
            );
        }

        // Borderless hairline editable table with `32px` column header row (`思考` / `图片` 16×16 checkboxes)
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
                    .border_color(palette.border_subtle)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .flex_1()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("模型 ID")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .flex_1()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("显示名称")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(76.0))
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("上下文窗口")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(68.0))
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("最大输出")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(40.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("思考")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(40.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("图片")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .w(px(36.0))
                            .flex()
                            .justify_center()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("移除")),
                    ),
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
            let cw_text = format_token_window(row.context_window);
            let mt_text = format_token_window(row.max_tokens);
            let cw_id = SharedString::from(format!("edit-row-cw-{idx}"));
            let mt_id = SharedString::from(format!("edit-row-mt-{idx}"));
            let rm_id = SharedString::from(format!("edit-row-remove-{idx}"));

            hairline_table = hairline_table.child(
                div()
                    .py(space::SM)
                    .border_b_1()
                    .border_color(palette.border_hairline)
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        div().flex_1().child(
                            Input::new(&row.id_input)
                                .disabled(self.saving)
                                .accessibility_id(SharedString::from(format!(
                                    "providers-row-id-input-{idx}"
                                )))
                                .aria_label(format!("第 {} 行模型 ID", idx + 1)),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            Input::new(&row.name_input)
                                .disabled(self.saving)
                                .accessibility_id(SharedString::from(format!(
                                    "providers-row-name-input-{idx}"
                                )))
                                .aria_label(format!("第 {} 行模型显示名称", idx + 1)),
                        ),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .id(cw_id.clone())
                            .test_support()
                            .a11y_button(cw_id, format!("切换上下文窗口：{cw_text}"))
                            .w(px(76.0))
                            .h(height::COMPACT)
                            .px(space::XS)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_workspace)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.editable_rows.get_mut(idx) {
                                    r.context_window = next_context_window(r.context_window);
                                    cx.notify();
                                }
                            }))
                            .child(SharedString::from(cw_text)),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .id(mt_id.clone())
                            .test_support()
                            .a11y_button(mt_id, format!("切换最大输出：{mt_text}"))
                            .w(px(68.0))
                            .h(height::COMPACT)
                            .px(space::XS)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_subtle)
                            .bg(palette.bg_workspace)
                            .font_family(mono_family.clone())
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = this.editable_rows.get_mut(idx) {
                                    r.max_tokens = next_max_tokens(r.max_tokens);
                                    cx.notify();
                                }
                            }))
                            .child(SharedString::from(mt_text)),
                    )
                    .child(
                        div()
                            .w(px(40.0))
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
                            .w(px(40.0))
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
                            .w(px(36.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
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
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if idx < this.editable_rows.len() {
                                            this.editable_rows.remove(idx);
                                            cx.notify();
                                        }
                                    }))
                                    .child(
                                        Icon::new(IconName::Trash)
                                            .size(px(13.0))
                                            .text_color(palette.text_muted),
                                    ),
                            ),
                    ),
            );
        }

        let pid_empty =
            self.form_error.is_some() && self.provider_id_input.read(cx).value().trim().is_empty();
        let name_empty = self.form_error.is_some()
            && self.provider_name_input.read(cx).value().trim().is_empty();
        let url_empty =
            self.form_error.is_some() && self.base_url_input.read(cx).value().trim().is_empty();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space::MD)
            .child(self.render_back_bar(palette, cx))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("服务标识")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(if pid_empty {
                                        palette.status_danger
                                    } else {
                                        palette.border_subtle
                                    })
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.provider_id_input)
                                            .accessibility_id("providers-id-input")
                                            .disabled(
                                                self.loading
                                                    || self.saving
                                                    || !self.creating_new_custom,
                                            )
                                            .aria_label("服务标识")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full(),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("显示名称")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(if name_empty {
                                        palette.status_danger
                                    } else {
                                        palette.border_subtle
                                    })
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.provider_name_input)
                                            .accessibility_id("providers-name-input")
                                            .disabled(self.loading || self.saving)
                                            .aria_label("服务显示名称")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full(),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_h(px(44.0))
                            .py(space::XS)
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label("接口协议")),
                            )
                            .child(div().flex_1().child(protocol_row)),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child("Base URL"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(if url_empty {
                                        palette.status_danger
                                    } else {
                                        palette.border_subtle
                                    })
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.base_url_input)
                                            .accessibility_id("providers-baseurl-input")
                                            .disabled(self.loading || self.saving)
                                            .aria_label("服务 Base URL")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full(),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(44.0))
                            .flex()
                            .items_center()
                            .gap(space::LG)
                            .child(
                                apply_scale(div(), scale::UI)
                                    .w(px(140.0))
                                    .flex_shrink_0()
                                    .text_color(palette.text_secondary)
                                    .child("API Key"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(height::CONTROL)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .border_1()
                                    .border_color(palette.border_subtle)
                                    .bg(palette.bg_workspace)
                                    .flex()
                                    .items_center()
                                    .child(
                                        Input::new(&self.api_key_input)
                                            .accessibility_id("providers-apikey-input")
                                            .disabled(self.loading || self.saving)
                                            .content_type(InputContentType::Password)
                                            .aria_label("服务 API Key")
                                            .appearance(false)
                                            .bordered(false)
                                            .w_full()
                                            .h_full()
                                            .mask_toggle(),
                                    ),
                            ),
                    ),
            )
            .children(self.form_error.clone().map(|err| {
                apply_scale(div(), scale::CAPTION)
                    .id("providers-form-error-text")
                    .test_support()
                    .a11y_status("providers-form-error-text", err.clone())
                    .text_color(palette.status_danger)
                    .child(err)
            }))
            .child(self.render_model_list_header("模型列表", palette, cx))
            .children(self.search_visible.then(|| {
                Input::new(&self.search_input)
                    .accessibility_id("providers-search-input")
                    .aria_label("筛选模型")
                    .h(height::FIELD)
                    .cleanable(true)
            }))
            .child(hairline_table)
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
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.save_inline_provider_form(w, cx);
                            }))
                            .child(cjk_inline_label(if self.saving {
                                "正在保存…"
                            } else {
                                "保存"
                            })),
                    ),
            )
            .children(
                (!self.creating_new_custom && self.active_provider_id.is_some()).then(|| {
                    render_remove_credentials_card(
                        palette,
                        cx.listener(|this, _, window, cx| {
                            this.remove_active_provider_credentials(window, cx);
                        }),
                    )
                }),
            )
    }
}

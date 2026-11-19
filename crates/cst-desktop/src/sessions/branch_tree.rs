use std::collections::{BTreeMap, BTreeSet};

use cst_client::{ClientHandle, DraftKey};
use gpui::KeyDownEvent;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt, Window, div, px,
};
use serde_json::Value;

use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, layout, radius,
    scale, space,
};
use crate::workbench::turn_segments::extract_message_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TreeFilterMode {
    #[default]
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl TreeFilterMode {
    pub const ALL_MODES: [Self; 5] = [
        Self::Default,
        Self::NoTools,
        Self::UserOnly,
        Self::LabeledOnly,
        Self::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "默认",
            Self::NoTools => "无工具",
            Self::UserOnly => "仅用户",
            Self::LabeledOnly => "仅标记",
            Self::All => "全部",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Default => Self::NoTools,
            Self::NoTools => Self::UserOnly,
            Self::UserOnly => Self::LabeledOnly,
            Self::LabeledOnly => Self::All,
            Self::All => Self::Default,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Default => Self::All,
            Self::NoTools => Self::Default,
            Self::UserOnly => Self::NoTools,
            Self::LabeledOnly => Self::UserOnly,
            Self::All => Self::LabeledOnly,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TreeNodeDisplay {
    pub id: String,
    pub parent_id: Option<String>,
    pub role: String,
    pub label: Option<String>,
    pub summary_text: String,
    pub depth: usize,
    pub is_segment_head: bool,
    pub has_children_in_segment: bool,
    pub is_active_leaf: bool,
    pub on_active_branch: bool,
}

/// Projects raw session `entries` into a depth-indented geometric branch tree matching Pi TUI / Web.
pub fn build_visible_tree_nodes(
    raw_entries: &[Value],
    leaf_id: Option<&str>,
    filter: TreeFilterMode,
    collapsed_heads: &BTreeSet<String>,
) -> Vec<TreeNodeDisplay> {
    if raw_entries.is_empty() {
        return Vec::new();
    }

    let mut by_id: BTreeMap<String, &Value> = BTreeMap::new();
    let mut children_map: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();

    for entry in raw_entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let parent = entry
            .get("parentId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        by_id.insert(id.to_owned(), entry);
        children_map.entry(parent).or_default().push(id.to_owned());
    }

    let resolved_leaf = leaf_id.map(str::to_owned).or_else(|| {
        raw_entries
            .last()
            .and_then(|e| e.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });

    let mut active_chain = BTreeSet::new();
    let mut cur = resolved_leaf.clone();
    while let Some(cid) = cur {
        if !active_chain.insert(cid.clone()) {
            break;
        }
        cur = by_id
            .get(&cid)
            .and_then(|e| e.get("parentId"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
    }

    let mut out = Vec::new();
    #[allow(clippy::too_many_arguments)]
    fn dfs(
        node_id: &str,
        depth: usize,
        is_seg_head: bool,
        by_id: &BTreeMap<String, &Value>,
        children_map: &BTreeMap<Option<String>, Vec<String>>,
        active_chain: &BTreeSet<String>,
        leaf_id: Option<&str>,
        filter: TreeFilterMode,
        collapsed_heads: &BTreeSet<String>,
        out: &mut Vec<TreeNodeDisplay>,
    ) {
        let Some(entry) = by_id.get(node_id) else {
            return;
        };
        let msg = entry.get("message").unwrap_or(entry);
        let entry_type = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or(entry_type)
            .to_owned();
        let label = entry
            .get("label")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);

        let keep = match filter {
            TreeFilterMode::All => true,
            TreeFilterMode::Default => {
                matches!(
                    role.as_str(),
                    "user" | "assistant" | "branchSummary" | "compaction"
                ) || label.is_some()
            }
            TreeFilterMode::NoTools => role != "toolResult",
            TreeFilterMode::UserOnly => role == "user" || label.is_some(),
            TreeFilterMode::LabeledOnly => label.is_some() || Some(node_id) == leaf_id,
        };

        let kids = children_map
            .get(&Some(node_id.to_owned()))
            .cloned()
            .unwrap_or_default();
        let branches = kids.len() > 1;

        if keep {
            let text = extract_message_text(msg);
            let clean_line = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or(match role.as_str() {
                    "user" => "用户消息",
                    "assistant" => "助手回复",
                    "toolResult" => "工具结果",
                    "branchSummary" => "分支总结",
                    _ => entry_type,
                });
            let truncated: String = clean_line.chars().take(72).collect();

            out.push(TreeNodeDisplay {
                id: node_id.to_owned(),
                parent_id: entry
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                role,
                label,
                summary_text: truncated,
                depth,
                is_segment_head: is_seg_head,
                has_children_in_segment: kids.len() == 1,
                is_active_leaf: Some(node_id) == leaf_id,
                on_active_branch: active_chain.contains(node_id),
            });
        }

        if is_seg_head && collapsed_heads.contains(node_id) && kids.len() == 1 {
            return;
        }

        for (idx, kid) in kids.iter().enumerate() {
            let next_depth = if branches && idx > 0 {
                depth + 1
            } else {
                depth
            };
            let next_head = branches;
            dfs(
                kid,
                next_depth,
                next_head,
                by_id,
                children_map,
                active_chain,
                leaf_id,
                filter,
                collapsed_heads,
                out,
            );
        }
    }

    let roots = children_map.get(&None).cloned().unwrap_or_default();
    for root_id in roots {
        dfs(
            &root_id,
            0,
            true,
            &by_id,
            &children_map,
            &active_chain,
            resolved_leaf.as_deref(),
            filter,
            collapsed_heads,
            &mut out,
        );
    }
    out
}

#[derive(Debug, Clone)]
pub enum BranchTreeEvent {
    BackToChat,
    SwitchToForkedSession(String),
    PrefillComposer(String),
    SummarizingStarted { session_id: String },
    SummarizingFinished,
    ErrorMessage(String),
}

pub struct BranchTreeView {
    client: ClientHandle,
    session_id: String,
    filter_mode: TreeFilterMode,
    collapsed_heads: BTreeSet<String>,
    selected_index: usize,
    pending_target_id: Option<String>,
    custom_prompt_mode: bool,
    custom_prompt_input: Entity<InputState>,
    focus_handle: FocusHandle,
}

impl EventEmitter<BranchTreeEvent> for BranchTreeView {}

impl Focusable for BranchTreeView {
    fn focus_handle(&self, _cx: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BranchTreeView {
    pub fn new(
        client: ClientHandle,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let custom_prompt_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("输入分支总结附加提示词…"));
        Self {
            client,
            session_id,
            filter_mode: TreeFilterMode::Default,
            collapsed_heads: BTreeSet::new(),
            selected_index: 0,
            pending_target_id: None,
            custom_prompt_mode: false,
            custom_prompt_input,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_session_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.session_id != session_id {
            self.session_id = session_id;
            self.selected_index = 0;
            self.pending_target_id = None;
            self.custom_prompt_mode = false;
            cx.notify();
        }
    }

    pub fn filter_mode(&self) -> TreeFilterMode {
        self.filter_mode
    }

    pub fn cycle_filter(&mut self, reverse: bool, cx: &mut Context<Self>) {
        self.filter_mode = if reverse {
            self.filter_mode.prev()
        } else {
            self.filter_mode.next()
        };
        self.selected_index = 0;
        cx.notify();
    }

    fn current_nodes(&self) -> Vec<TreeNodeDisplay> {
        let snap = self.client.snapshot();
        let Some(sess) = snap.loaded_sessions.get(&self.session_id) else {
            return Vec::new();
        };
        let entries = if !sess.entries.is_empty() {
            &sess.entries
        } else {
            &sess.messages
        };
        build_visible_tree_nodes(
            entries,
            sess.leaf_id.as_deref(),
            self.filter_mode,
            &self.collapsed_heads,
        )
    }

    fn execute_navigate(
        &mut self,
        target_id: String,
        summarize: bool,
        custom_instructions: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.pending_target_id = None;
        self.custom_prompt_mode = false;
        let sid = self.session_id.clone();
        let client = self.client.clone();

        if summarize {
            cx.emit(BranchTreeEvent::SummarizingStarted {
                session_id: sid.clone(),
            });
            cx.emit(BranchTreeEvent::BackToChat);
        }

        cx.spawn(async move |weak, cx| {
            let res = client
                .navigate_session_tree(sid.clone(), target_id, summarize, custom_instructions)
                .await;
            let _ = client.activate_session(Some(sid.clone())).await;
            let _ = weak.update(cx, |_, cx| {
                if summarize {
                    cx.emit(BranchTreeEvent::SummarizingFinished);
                }
                match res {
                    Ok(val) => {
                        let cancelled = val
                            .get("cancelled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        if !cancelled {
                            if let Some(editor_text) = val
                                .get("editorText")
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                            {
                                client.with_workspace_mut(|ws| {
                                    ws.draft_mut(DraftKey::Session(sid.clone())).text =
                                        editor_text.to_owned();
                                });
                                cx.emit(BranchTreeEvent::PrefillComposer(editor_text.to_owned()));
                            }
                            if !summarize {
                                cx.emit(BranchTreeEvent::BackToChat);
                            }
                        }
                    }
                    Err(err) => {
                        cx.emit(BranchTreeEvent::ErrorMessage(err.to_string()));
                    }
                }
            });
        })
        .detach();
    }

    fn execute_fork(&mut self, entry_id: String, cx: &mut Context<Self>) {
        let sid = self.session_id.clone();
        let client = self.client.clone();
        cx.spawn(
            async move |weak, cx| match client.fork_session(sid, entry_id).await {
                Ok(new_sid) => {
                    let _ = client.refresh_globals().await;
                    let _ = weak.update(cx, |_, cx| {
                        cx.emit(BranchTreeEvent::SwitchToForkedSession(new_sid));
                    });
                }
                Err(err) => {
                    let _ = weak.update(cx, |_, cx| {
                        cx.emit(BranchTreeEvent::ErrorMessage(err.to_string()));
                    });
                }
            },
        )
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let key = ks.key.as_str();
        if ks.modifiers.control && key.eq_ignore_ascii_case("o") {
            self.cycle_filter(ks.modifiers.shift, cx);
            cx.stop_propagation();
            return;
        }
        if self.pending_target_id.is_some() {
            if key == "escape" {
                self.pending_target_id = None;
                self.custom_prompt_mode = false;
                cx.notify();
                cx.stop_propagation();
            }
            return;
        }

        let nodes = self.current_nodes();
        if nodes.is_empty() {
            return;
        }
        match key {
            "up" => {
                self.selected_index = self.selected_index.saturating_sub(1);
                cx.notify();
                cx.stop_propagation();
            }
            "down" => {
                if self.selected_index + 1 < nodes.len() {
                    self.selected_index += 1;
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "enter" => {
                if let Some(node) = nodes.get(self.selected_index)
                    && !node.is_active_leaf
                {
                    self.pending_target_id = Some(node.id.clone());
                    self.custom_prompt_mode = false;
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "escape" => {
                cx.emit(BranchTreeEvent::BackToChat);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn render_summary_mode_card(
        &self,
        target_id: &str,
        palette: &DesktopPalette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t1 = target_id.to_owned();
        let t2 = target_id.to_owned();
        let t3 = target_id.to_owned();

        let mut card = div()
            .id("branch-summary-mode-card")
            .test_support()
            .w_full()
            .p(space::LG)
            .rounded(radius::SPECIAL)
            .border_1()
            .border_color(palette.border_strong)
            .bg(palette.bg_elevated)
            .flex()
            .flex_col()
            .gap(space::MD)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child("切换至所选分支节点"),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id("branch-summary-cancel-btn")
                            .test_support()
                            .h(height::INLINE)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pending_target_id = None;
                                this.custom_prompt_mode = false;
                                cx.notify();
                            }))
                            .child(cjk_inline_label("取消")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("branch-mode-no-summary-btn")
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_surface)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.execute_navigate(t1.clone(), false, None, cx);
                            }))
                            .child(cjk_inline_label("不总结")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("branch-mode-summary-btn")
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.execute_navigate(t2.clone(), true, None, cx);
                            }))
                            .child(cjk_inline_label("总结")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("branch-mode-custom-summary-btn")
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_surface)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.custom_prompt_mode = true;
                                this.custom_prompt_input.update(cx, |inp, cx| {
                                    inp.focus(window, cx);
                                });
                                cx.notify();
                            }))
                            .child(cjk_inline_label("用自定义提示词总结")),
                    ),
            );

        if self.custom_prompt_mode {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(div().flex_1().child(Input::new(&self.custom_prompt_input)))
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("branch-custom-summary-submit-btn")
                            .test_support()
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let prompt = this.custom_prompt_input.read(cx).value().to_string();
                                this.execute_navigate(t3.clone(), true, Some(prompt), cx);
                            }))
                            .child(cjk_inline_label("执行总结")),
                    ),
            );
        }

        card.into_any_element()
    }
}

impl Render for BranchTreeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = DesktopThemeGlobal::current(cx).palette;
        let nodes = self.current_nodes();
        if self.selected_index >= nodes.len() && !nodes.is_empty() {
            self.selected_index = nodes.len() - 1;
        }

        let mut filter_bar = div().flex().items_center().gap(space::XS);
        for mode in TreeFilterMode::ALL_MODES {
            let active = self.filter_mode == mode;
            filter_bar = filter_bar.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(SharedString::from(format!("tree-filter-{:?}", mode)))
                    .test_support()
                    .h(height::COMPACT)
                    .px(space::SM)
                    .rounded(radius::CONTROL)
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
                        this.filter_mode = mode;
                        this.selected_index = 0;
                        cx.notify();
                    }))
                    .child(cjk_inline_label(mode.label())),
            );
        }

        // Render continuous geometric branch guide lines meeting `10×10px` node dots without gaps:
        // Each row has fixed `36px` height; upper half (`13px`) connects from previous row to top of `10×10px` dot,
        // lower half (`13px`) connects from bottom of `10×10px` dot to next row!
        let total = nodes.len();
        let mut tree_list = div()
            .id("branch-tree-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();

        for (idx, node) in nodes.iter().enumerate() {
            let is_selected = idx == self.selected_index;
            let nid = node.id.clone();
            let nid_click = nid.clone();
            let nid_fork = nid.clone();
            let nid_collapse = nid.clone();
            let is_collapsed = self.collapsed_heads.contains(&nid);
            let is_active_leaf = node.is_active_leaf;
            let has_upper = idx > 0;
            let has_lower = idx + 1 < total;

            let dot_color = if is_active_leaf {
                palette.accent_solid
            } else if node.on_active_branch {
                palette.text_secondary
            } else {
                palette.border_strong
            };

            let indent_px = px((node.depth as f32) * 20.0);

            // Continuous `1px` vertical guide + `10×10px` dot with ZERO vertical gap (`13px + 10px + 13px = 36px`)
            let guide_cell = div()
                .w(px(20.0))
                .h(px(36.0))
                .flex()
                .flex_col()
                .items_center()
                .flex_shrink_0()
                .child(div().w(px(1.0)).h(px(13.0)).bg(if has_upper {
                    palette.border_strong
                } else {
                    gpui_kit::transparent_black()
                }))
                .child(
                    div()
                        .w(px(10.0))
                        .h(px(10.0))
                        .rounded(radius::FULL)
                        .border_1()
                        .border_color(dot_color)
                        .bg(if is_active_leaf {
                            palette.accent_solid
                        } else {
                            palette.bg_workspace
                        }),
                )
                .child(div().w(px(1.0)).h(px(13.0)).bg(if has_lower {
                    palette.border_strong
                } else {
                    gpui_kit::transparent_black()
                }));

            let role_badge = match node.role.as_str() {
                "user" => ("用户", palette.accent_solid),
                "assistant" => ("助手", palette.text_secondary),
                "branchSummary" => ("总结", palette.status_warning),
                _ => ("节点", palette.text_muted),
            };

            tree_list = tree_list.child(
                div()
                    .id(SharedString::from(format!("tree-node-row-{nid}")))
                    .test_support()
                    .w_full()
                    .h(px(36.0))
                    .pl(indent_px)
                    .pr(space::SM)
                    .rounded(radius::CONTROL)
                    .bg(if is_selected {
                        palette.bg_item_active
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::SM)
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_index = idx;
                        if !is_active_leaf {
                            this.pending_target_id = Some(nid_click.clone());
                            this.custom_prompt_mode = false;
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .min_w_0()
                            .flex_1()
                            .child(guide_cell)
                            .children((node.is_segment_head && node.has_children_in_segment).then(
                                || {
                                    div()
                                        .id(SharedString::from(format!(
                                            "tree-collapse-btn-{nid_collapse}"
                                        )))
                                        .test_support()
                                        .w(px(18.0))
                                        .h(px(18.0))
                                        .rounded(radius::CONTROL)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .on_click(cx.listener({
                                            let hc = node.id.clone();
                                            move |this, _, _, cx| {
                                                if !this.collapsed_heads.remove(&hc) {
                                                    this.collapsed_heads.insert(hc.clone());
                                                }
                                                cx.stop_propagation();
                                                cx.notify();
                                            }
                                        }))
                                        .child(
                                            Icon::new(if is_collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .size(px(11.0))
                                            .text_color(palette.text_muted),
                                        )
                                },
                            ))
                            .child(
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(role_badge.1)
                                    .flex_shrink_0()
                                    .child(cjk_inline_label(role_badge.0)),
                            )
                            .children(node.label.as_ref().map(|lbl| {
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .px(space::TIGHT)
                                    .rounded(radius::CONTROL)
                                    .bg(palette.bg_surface)
                                    .text_color(palette.accent_solid)
                                    .flex_shrink_0()
                                    .child(SharedString::from(lbl.clone()))
                            }))
                            .child(
                                apply_scale(div(), scale::UI)
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(palette.text_primary)
                                    .child(SharedString::from(node.summary_text.clone())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .flex_shrink_0()
                            .children(is_active_leaf.then(|| {
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .text_color(palette.accent_solid)
                                    .child(cjk_inline_label("当前叶节点"))
                            }))
                            .children((node.role == "user").then(|| {
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .id(SharedString::from(format!("tree-fork-btn-{nid}")))
                                    .test_support()
                                    .h(height::INLINE)
                                    .px(space::XS)
                                    .rounded(radius::CONTROL)
                                    .text_color(palette.text_secondary)
                                    .flex()
                                    .items_center()
                                    .hover(|s| s.bg(palette.bg_surface))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.execute_fork(nid_fork.clone(), cx);
                                        cx.stop_propagation();
                                    }))
                                    .child(cjk_inline_label("派生会话"))
                            })),
                    ),
            );
        }

        let mut summary_card_opt = None;
        if let Some(tid) = self.pending_target_id.clone() {
            summary_card_opt = Some(self.render_summary_mode_card(&tid, &palette, cx));
        }

        div()
            .id("branch-tree-view")
            .test_support()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .px(space::GROUP)
            .py(space::LG)
            .child(
                div()
                    .w_full()
                    .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap(space::MD)
                    .child(
                        div()
                            .w_full()
                            .h(height::HEADER)
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(palette.border_subtle)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(space::SM)
                                    .child(
                                        apply_scale(div(), scale::UI_MEDIUM)
                                            .id("branch-tree-back-btn")
                                            .test_support()
                                            .h(height::CONTROL)
                                            .px(space::SM)
                                            .rounded(radius::CONTROL)
                                            .text_color(palette.text_secondary)
                                            .flex()
                                            .items_center()
                                            .gap(space::XS)
                                            .hover(|s| s.bg(palette.bg_item_hover))
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(BranchTreeEvent::BackToChat);
                                            }))
                                            .child(
                                                Icon::new(IconName::ArrowLeft)
                                                    .size(px(14.0))
                                                    .text_color(palette.text_secondary),
                                            )
                                            .child(cjk_inline_label("返回对话")),
                                    )
                                    .child(
                                        apply_scale(div(), scale::TITLE)
                                            .text_color(palette.text_primary)
                                            .child(cjk_inline_label("会话分支树")),
                                    ),
                            )
                            .child(filter_bar),
                    )
                    .children(summary_card_opt)
                    .child(tree_list),
            )
    }
}

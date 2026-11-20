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

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    A11yElementExt as _, DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height,
    layout, radius, render_subpage_header_xwjof, scale, space,
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

    pub fn a11y_id(self) -> &'static str {
        match self {
            Self::Default => "tree-filter-default",
            Self::NoTools => "tree-filter-no-tools",
            Self::UserOnly => "tree-filter-user-only",
            Self::LabeledOnly => "tree-filter-labeled-only",
            Self::All => "tree-filter-all",
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

/// Checks if a branch line at `col_depth` continues after `row_idx` (`Some(is_on_active_branch)` if yes, `None` if terminated).
fn ancestor_continuation(
    nodes: &[TreeNodeDisplay],
    row_idx: usize,
    col_depth: usize,
) -> Option<bool> {
    let mut any_continues = false;
    let mut active_continues = false;
    for next in nodes.iter().skip(row_idx + 1) {
        if next.depth < col_depth {
            break;
        }
        if next.depth == col_depth {
            any_continues = true;
            if next.on_active_branch {
                active_continues = true;
            }
        } else if next.on_active_branch {
            any_continues = true;
            active_continues = true;
        }
    }
    any_continues.then_some(active_continues)
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
    search_input: Entity<InputState>,
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
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索分支节点内容…"));
        let custom_prompt_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("输入分支总结附加提示词…"));
        Self {
            client,
            session_id,
            filter_mode: TreeFilterMode::Default,
            search_input,
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

    fn raw_entry_count(&self) -> usize {
        let snap = self.client.snapshot();
        let Some(sess) = snap.loaded_sessions.get(&self.session_id) else {
            return 0;
        };
        if !sess.entries.is_empty() {
            sess.entries.len()
        } else {
            sess.messages.len()
        }
    }

    fn current_nodes(&self, cx: &gpui_kit::App) -> Vec<TreeNodeDisplay> {
        let snap = self.client.snapshot();
        let Some(sess) = snap.loaded_sessions.get(&self.session_id) else {
            return Vec::new();
        };
        let entries = if !sess.entries.is_empty() {
            &sess.entries
        } else {
            &sess.messages
        };
        let base = build_visible_tree_nodes(
            entries,
            sess.leaf_id.as_deref(),
            self.filter_mode,
            &self.collapsed_heads,
        );
        let q = self.search_input.read(cx).value().trim().to_lowercase();
        if q.is_empty() {
            return base;
        }
        base.into_iter()
            .filter(|n| {
                n.summary_text.to_lowercase().contains(&q)
                    || n.role.to_lowercase().contains(&q)
                    || n.label
                        .as_ref()
                        .is_some_and(|l| l.to_lowercase().contains(&q))
            })
            .collect()
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        let snap = self.client.snapshot();
        if let Some(sess) = snap.loaded_sessions.get(&self.session_id) {
            let entries = if !sess.entries.is_empty() {
                &sess.entries
            } else {
                &sess.messages
            };
            let empty = BTreeSet::new();
            let all_nodes = build_visible_tree_nodes(
                entries,
                sess.leaf_id.as_deref(),
                self.filter_mode,
                &empty,
            );
            for n in all_nodes {
                if n.is_segment_head && n.has_children_in_segment {
                    self.collapsed_heads.insert(n.id);
                }
            }
        }
        cx.notify();
    }

    fn expand_all(&mut self, cx: &mut Context<Self>) {
        self.collapsed_heads.clear();
        cx.notify();
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

        let nodes = self.current_nodes(cx);
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

    /// Renders `424px` wide, `8px` radius `K3x72` summary mode modal overlaid on `DZ0Fp` frosted scrim
    /// (`p: 20px`, zero dual `accent_solid` conflict).
    fn render_summary_mode_modal(
        &self,
        target_id: &str,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t1 = target_id.to_owned();
        let t2 = target_id.to_owned();
        let t3 = target_id.to_owned();
        let in_custom = self.custom_prompt_mode;

        let summary_btn = if in_custom {
            apply_scale(div(), scale::UI_MEDIUM)
                .id("branch-mode-summary-btn")
                .test_support()
                .a11y_button("branch-mode-summary-btn", "直接总结")
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.execute_navigate(t2.clone(), true, None, cx);
                }))
                .child(cjk_inline_label("直接总结"))
        } else {
            apply_scale(div(), scale::UI_MEDIUM)
                .id("branch-mode-summary-btn")
                .test_support()
                .a11y_button("branch-mode-summary-btn", "总结")
                .h(height::CONTROL)
                .px(space::LG)
                .rounded(radius::CONTROL)
                .bg(palette.accent_solid)
                .text_color(palette.text_on_accent)
                .flex()
                .items_center()
                .hover(|s| s.bg(palette.accent_hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.execute_navigate(t2.clone(), true, None, cx);
                }))
                .child(cjk_inline_label("总结"))
        };

        let mut card = render_frosted_surface(
            "branch-summary-mode-card",
            FrostedGlassSpec::modal_k3x72(palette, 8.0),
            palette.bg_modal,
            transparency_enabled,
            scale_factor,
        )
        .test_support()
        .a11y_dialog("branch-summary-mode-card", "切换至所选分支节点")
        .w(px(424.0))
        .p(px(20.0))
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .flex()
        .flex_col()
        .gap(space::MD)
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::SM)
                        .child(
                            Icon::new(IconName::GitBranch)
                                .size(px(16.0))
                                .text_color(palette.accent_ink),
                        )
                        .child(
                            apply_scale(div(), scale::BODY_SEMIBOLD)
                                .text_color(palette.text_primary)
                                .child(cjk_inline_label("切换至所选分支节点")),
                        ),
                )
                .child(
                    div()
                        .id("branch-summary-cancel-btn")
                        .test_support()
                        .a11y_button("branch-summary-cancel-btn", "取消切换分支")
                        .w(height::COMPACT)
                        .h(height::COMPACT)
                        .rounded(radius::CONTROL)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(palette.bg_item_hover))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pending_target_id = None;
                            this.custom_prompt_mode = false;
                            cx.notify();
                        }))
                        .child(
                            Icon::new(IconName::X)
                                .size(px(14.0))
                                .text_color(palette.text_secondary),
                        ),
                ),
        )
        .child(
            apply_scale(div(), scale::UI)
                .relative()
                .text_color(palette.text_secondary)
                .child("选择跳转方式，决定是否将离开分支的上下文压缩为分支总结。"),
        )
        .child(
            div()
                .relative()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(space::SM)
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("branch-mode-no-summary-btn")
                        .test_support()
                        .a11y_button("branch-mode-no-summary-btn", "不总结")
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.execute_navigate(t1.clone(), false, None, cx);
                        }))
                        .child(cjk_inline_label("不总结")),
                )
                .child(
                    apply_scale(div(), scale::UI_MEDIUM)
                        .id("branch-mode-custom-summary-btn")
                        .test_support()
                        .a11y_toggle_button(
                            "branch-mode-custom-summary-btn",
                            "自定义提示词总结",
                            in_custom,
                        )
                        .h(height::CONTROL)
                        .px(space::MD)
                        .rounded(radius::CONTROL)
                        .border_1()
                        .border_color(if in_custom {
                            palette.border_focus
                        } else {
                            palette.border_strong
                        })
                        .bg(if in_custom {
                            palette.bg_item_active
                        } else {
                            palette.bg_workspace
                        })
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
                        .child(cjk_inline_label("自定义提示词总结")),
                )
                .child(summary_btn),
        );

        if in_custom {
            card = card.child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.custom_prompt_input)
                                .accessibility_id("branch-tree-custom-prompt-input")
                                .aria_label("分支总结附加提示词")
                                .h(height::FIELD)
                                .cleanable(true),
                        ),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("branch-custom-summary-submit-btn")
                            .test_support()
                            .a11y_button("branch-custom-summary-submit-btn", "执行总结")
                            .h(height::FIELD)
                            .px(space::LG)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let prompt = this.custom_prompt_input.read(cx).value().to_string();
                                this.execute_navigate(t3.clone(), true, Some(prompt), cx);
                            }))
                            .child(cjk_inline_label("执行总结")),
                    ),
            );
        }

        render_frosted_surface(
            "branch-summary-scrim-dz0fp",
            FrostedGlassSpec::scrim_dz0fp(palette),
            palette.bg_scrim,
            transparency_enabled,
            scale_factor,
        )
        .test_support()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .child(card)
        .into_any_element()
    }
}

impl Render for BranchTreeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let mono_family = theme_global.mono_font_family;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();

        let total_raw = self.raw_entry_count();
        let nodes = self.current_nodes(cx);
        if self.selected_index >= nodes.len() && !nodes.is_empty() {
            self.selected_index = nodes.len() - 1;
        }

        // Neutral 5-mode filter switcher (`bg_item_active` + `border_subtle`, zero `accent_solid` conflict)
        let mut filter_bar = div().flex().items_center().gap(space::XS);
        for mode in TreeFilterMode::ALL_MODES {
            let active = self.filter_mode == mode;
            filter_bar = filter_bar.child(
                apply_scale(div(), scale::CAPTION_MEDIUM)
                    .id(SharedString::from(format!("tree-filter-{:?}", mode)))
                    .test_support()
                    .a11y_tab(mode.a11y_id(), mode.label(), active)
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
                        this.filter_mode = mode;
                        this.selected_index = 0;
                        cx.notify();
                    }))
                    .child(cjk_inline_label(mode.label())),
            );
        }

        let visible_count = nodes.len();
        let toolbar = div()
            .id("branch-tree-toolbar")
            .test_support()
            .w_full()
            .h(px(56.0))
            .px(space::GROUP)
            .border_b_1()
            .border_color(palette.border_subtle)
            .bg(palette.bg_workspace)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(space::LG)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .child(
                        div().w(px(208.0)).child(
                            Input::new(&self.search_input)
                                .accessibility_id("branch-tree-search-input")
                                .aria_label("搜索分支节点内容")
                                .prefix(
                                    Icon::new(IconName::Search)
                                        .size(px(14.0))
                                        .text_color(palette.text_muted),
                                )
                                .cleanable(true),
                        ),
                    )
                    .child(filter_bar),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("branch-tree-expand-all-btn")
                            .test_support()
                            .a11y_button("branch-tree-expand-all-btn", "全部展开")
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                            .on_click(cx.listener(|this, _, _, cx| this.expand_all(cx)))
                            .child(cjk_inline_label("全部展开")),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("branch-tree-collapse-all-btn")
                            .test_support()
                            .a11y_button("branch-tree-collapse-all-btn", "全部折叠")
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_secondary)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.bg_item_hover).text_color(palette.text_primary))
                            .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx)))
                            .child(cjk_inline_label("全部折叠")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .font_family(mono_family)
                            .text_color(palette.text_muted)
                            .child(SharedString::from(format!(
                                "{visible_count} / {total_raw} 条"
                            ))),
                    ),
            );

        // Render `40px` rows with continuous ancestor active-branch vertical guide lines + `15px + 10px + 15px` dot cell
        let mut tree_col = div()
            .w_full()
            .max_w(layout::CENTER_COLUMN_MAX_WIDTH)
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
            let lower_cont = ancestor_continuation(&nodes, idx, node.depth);

            let dot_color = if is_active_leaf {
                palette.accent_solid
            } else if node.on_active_branch {
                palette.accent_ink
            } else {
                palette.border_strong
            };

            let mut guides_row = div().flex().items_center().flex_shrink_0();
            for col_d in 0..node.depth {
                let cont = ancestor_continuation(&nodes, idx, col_d);
                let line_col = match cont {
                    Some(true) => palette.accent_ink,
                    Some(false) => palette.border_strong,
                    None => gpui_kit::transparent_black(),
                };
                guides_row = guides_row.child(
                    div()
                        .w(px(20.0))
                        .h(height::FIELD)
                        .flex()
                        .justify_center()
                        .flex_shrink_0()
                        .child(div().w(px(1.0)).h_full().bg(line_col)),
                );
            }

            let upper_col = if !has_upper {
                gpui_kit::transparent_black()
            } else if node.on_active_branch {
                palette.accent_ink
            } else {
                palette.border_strong
            };
            let lower_col = match lower_cont {
                Some(true) => palette.accent_ink,
                Some(false) => palette.border_strong,
                None => gpui_kit::transparent_black(),
            };

            let self_guide_cell = div()
                .w(px(20.0))
                .h(height::FIELD)
                .flex()
                .flex_col()
                .items_center()
                .flex_shrink_0()
                .child(div().w(px(1.0)).h(px(15.0)).bg(upper_col))
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
                .child(div().w(px(1.0)).h(px(15.0)).bg(lower_col));
            guides_row = guides_row.child(self_guide_cell);

            let role_badge = match node.role.as_str() {
                "user" => ("用户", palette.text_secondary),
                "assistant" => ("助手", palette.text_muted),
                "branchSummary" => ("总结", palette.status_warning),
                _ => ("节点", palette.text_muted),
            };

            let row_id = SharedString::from(format!("tree-node-row-{nid}"));
            let row_label = SharedString::from(format!("{}：{}", role_badge.0, node.summary_text));
            let fork_id = SharedString::from(format!("tree-fork-btn-{nid}"));
            tree_col = tree_col.child(
                div()
                    .id(row_id.clone())
                    .test_support()
                    .a11y_treeitem(row_id, row_label, is_active_leaf)
                    .w_full()
                    .h(height::FIELD)
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
                            .child(guides_row)
                            .children((node.is_segment_head && node.has_children_in_segment).then(
                                || {
                                    let col_btn_id = SharedString::from(format!(
                                        "tree-collapse-btn-{nid_collapse}"
                                    ));
                                    div()
                                        .id(col_btn_id.clone())
                                        .test_support()
                                        .a11y_toggle_button(
                                            col_btn_id,
                                            "折叠或展开分支段",
                                            !is_collapsed,
                                        )
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
                                            .size(px(12.0))
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
                                    .text_color(palette.text_primary)
                                    .flex_shrink_0()
                                    .child(SharedString::from(lbl.clone()))
                            }))
                            .child(
                                apply_scale(div(), scale::BODY)
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
                                    .text_color(palette.accent_ink)
                                    .child(cjk_inline_label("当前叶节点"))
                            }))
                            .children((node.role == "user").then(|| {
                                apply_scale(div(), scale::CAPTION_MEDIUM)
                                    .id(fork_id.clone())
                                    .test_support()
                                    .a11y_button(fork_id, "从此节点派生会话")
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

        let close_btn = div()
            .id("branch-tree-back-btn")
            .test_support()
            .a11y_button("branch-tree-back-btn", "关闭分支树返回对话")
            .w(height::CONTROL)
            .h(height::CONTROL)
            .rounded(radius::CONTROL)
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(palette.bg_item_hover))
            .on_click(cx.listener(|_, _, _, cx| {
                cx.emit(BranchTreeEvent::BackToChat);
            }))
            .child(
                Icon::new(IconName::X)
                    .size(px(16.0))
                    .text_color(palette.text_secondary),
            )
            .into_any_element();

        let summary_modal_opt = self.pending_target_id.clone().map(|tid| {
            self.render_summary_mode_modal(&tid, &palette, transparency_enabled, scale_factor, cx)
        });

        div()
            .id("branch-tree-view")
            .test_support()
            .a11y_region("branch-tree-view", "分支树视图")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.bg_workspace)
            .child(render_subpage_header_xwjof(
                "branch-tree-subpage-header",
                IconName::GitBranch,
                "分支树",
                Some(close_btn),
                &palette,
            ))
            .child(toolbar)
            .child(
                div()
                    .id("branch-tree-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .px(space::GROUP)
                    .py(space::GROUP)
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(tree_col),
            )
            .children(summary_modal_opt)
    }
}

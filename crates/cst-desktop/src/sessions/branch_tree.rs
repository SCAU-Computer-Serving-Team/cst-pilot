use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use cst_client::{ClientHandle, DraftKey};
use gpui::{KeyDownEvent, MouseMoveEvent, Pixels, Point, ScrollHandle};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Escape, Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt, Window, div, px,
};
use serde_json::{Map, Value, json};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_scrim, render_frosted_surface};
use crate::theme::kbd_focus::{
    KbdRowTarget, pointer_moved_past_threshold, render_gliding_row_indicator,
};
use crate::theme::{
    DesktopPalette, DesktopThemeGlobal, apply_scale, cjk_inline_label, height, motion, radius,
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
            Self::Default => "默认视图",
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

/// Projects flat session `entries` into a depth-indented branch tree (compatibility helper + orphan root handling).
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
    let mut order: Vec<String> = Vec::new();

    for entry in raw_entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if by_id.insert(id.to_owned(), entry).is_none() {
            order.push(id.to_owned());
        }
    }

    let mut children_map: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
    for id in &order {
        let entry = by_id[id];
        let raw_parent = entry
            .get("parentId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        // If `parentId` references an entry filtered out of the flat snapshot (e.g., initial
        // `model_change` / `session_info`), treat this entry as a root instead of dropping the whole tree.
        let parent = raw_parent.filter(|pid| by_id.contains_key(pid));
        children_map.entry(parent).or_default().push(id.clone());
    }

    let resolved_leaf = leaf_id.map(str::to_owned).or_else(|| {
        raw_entries
            .last()
            .and_then(|e| e.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });

    let leaf_id = resolved_leaf.as_deref();
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
            .filter(|s| !s.is_empty() && by_id.contains_key(*s))
            .map(str::to_owned);
    }

    let mut out = Vec::new();
    let mut pending: Vec<_> = children_map
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|id| (id.as_str(), 0, true))
        .collect();
    let mut visited = BTreeSet::new();
    while let Some((node_id, depth, is_seg_head)) = pending.pop() {
        if !visited.insert(node_id) {
            continue;
        }
        let Some(entry) = by_id.get(node_id) else {
            continue;
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
                    "user"
                        | "assistant"
                        | "branchSummary"
                        | "branch_summary"
                        | "compaction"
                        | "compactionSummary"
                ) || label.is_some()
            }
            TreeFilterMode::NoTools => role != "toolResult",
            TreeFilterMode::UserOnly => role == "user" || label.is_some(),
            TreeFilterMode::LabeledOnly => label.is_some() || Some(node_id) == leaf_id,
        };

        let kids = children_map
            .get(&Some(node_id.to_owned()))
            .map_or(&[][..], Vec::as_slice);
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
                    "branchSummary" | "branch_summary" => "分支总结",
                    "compaction" | "compactionSummary" => "上下文压缩",
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
            continue;
        }
        pending.extend(kids.iter().enumerate().rev().map(|(idx, kid)| {
            (
                kid.as_str(),
                depth + usize::from(branches && idx > 0),
                branches,
            )
        }));
    }
    out
}

/// Nested tree node returned by `GET /api/sessions/:id/tree` (aligned with `pi_session::SessionTreeNode` & web `branch-tree.ts`).
#[derive(Debug)]
struct RawTreeNode {
    id: String,
    entry_type: String,
    timestamp: String,
    entry: Value,
    label: Option<String>,
}

type TreeNodes = Vec<Rc<RawTreeNode>>;

impl RawTreeNode {
    fn new(entry: Value, label: Option<&str>) -> Option<Rc<Self>> {
        let id = entry.get("id").and_then(Value::as_str)?;
        if id.is_empty() {
            return None;
        }
        let label = label
            .or_else(|| entry.get("label").and_then(Value::as_str))
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
        let timestamp = entry
            .get("timestamp")
            .and_then(|ts| {
                ts.as_str()
                    .map(str::to_owned)
                    .or_else(|| ts.as_u64().map(|n| n.to_string()))
            })
            .unwrap_or_default();
        Some(Rc::new(Self {
            id: id.to_owned(),
            entry_type: entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("message")
                .to_owned(),
            timestamp,
            entry,
            label,
        }))
    }

    /// Normalize the active branch from a session snapshot while a full tree is unavailable.
    fn from_flat_entries(raw_entries: &[Value]) -> TreeNodes {
        raw_entries.iter().enumerate().filter_map(|(idx, raw)| {
            let id = raw
                .get("id")
                .or_else(|| raw.get("message").and_then(|m| m.get("id")))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("entry-{idx}"));
            let normalized = if raw.get("type").is_some() {
                raw.clone()
            } else if let Some(msg) = raw.get("message") {
                let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
                match role {
                    "branchSummary" => json!({
                        "id": id,
                        "parentId": raw.get("parentId").cloned().unwrap_or(Value::Null),
                        "type": "branch_summary",
                        "summary": msg.get("summary").cloned().unwrap_or(Value::String(String::new())),
                        "timestamp": msg.get("timestamp").cloned().unwrap_or(Value::Null),
                    }),
                    "compactionSummary" => json!({
                        "id": id,
                        "parentId": raw.get("parentId").cloned().unwrap_or(Value::Null),
                        "type": "compaction",
                        "summary": msg.get("summary").cloned().unwrap_or(Value::String(String::new())),
                        "tokensBefore": msg.get("tokensBefore").cloned().unwrap_or(json!(0)),
                        "timestamp": msg.get("timestamp").cloned().unwrap_or(Value::Null),
                    }),
                    _ => json!({
                        "id": id,
                        "parentId": raw.get("parentId").cloned().unwrap_or(Value::Null),
                        "type": "message",
                        "label": raw.get("label").cloned().unwrap_or(Value::Null),
                        "message": msg,
                        "timestamp": msg.get("timestamp").cloned().unwrap_or(Value::Null),
                    }),
                }
            } else {
                json!({
                    "id": id,
                    "parentId": raw.get("parentId").cloned().unwrap_or(Value::Null),
                    "type": "message",
                    "message": raw,
                    "timestamp": raw.get("timestamp").cloned().unwrap_or(Value::Null),
                })
            };

            Self::new(normalized, None)
        }).collect()
    }
}

fn parse_flat_tree(mut payload: Value) -> Option<(TreeNodes, Option<String>)> {
    let leaf = match payload.get("leafId")? {
        Value::Null => None,
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        _ => return None,
    };
    let Value::Array(entries) = payload.get_mut("entries")?.take() else {
        return None;
    };
    let mut ids = BTreeSet::new();
    let mut nodes = Vec::with_capacity(entries.len());
    for mut row in entries {
        let entry = row.get_mut("entry")?.take();
        let label = match row.get("label") {
            None | Some(Value::Null) => None,
            Some(Value::String(label)) => Some(label.as_str()),
            _ => return None,
        };
        entry.get("type")?.as_str()?;
        let node = RawTreeNode::new(entry, label)?;
        if !ids.insert(node.id.clone()) {
            return None;
        }
        nodes.push(node);
    }
    if leaf.as_ref().is_some_and(|id| !ids.contains(id)) {
        return None;
    }
    Some((nodes, leaf))
}

fn is_settings_entry_type(t: &str) -> bool {
    matches!(
        t,
        "label" | "custom" | "model_change" | "thinking_level_change" | "session_info"
    )
}

fn entry_visible_by_default(
    entry: &Value,
    entry_type: &str,
    id: &str,
    leaf_id: Option<&str>,
) -> bool {
    if entry_type == "message"
        && let Some(msg) = entry.get("message")
        && msg.get("role").and_then(Value::as_str) == Some("assistant")
        && Some(id) != leaf_id
    {
        let has_text = !extract_message_text(msg).trim().is_empty();
        let stop = msg.get("stopReason").and_then(Value::as_str);
        let error_or_aborted = matches!(stop, Some(s) if s != "stop" && s != "toolUse");
        if !has_text && !error_or_aborted {
            return false;
        }
    }
    true
}

fn node_visible_by_filter(node: &RawTreeNode, filter: TreeFilterMode) -> bool {
    let role = node
        .entry
        .get("message")
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str);
    match filter {
        TreeFilterMode::UserOnly => node.entry_type == "message" && role == Some("user"),
        TreeFilterMode::NoTools => {
            !is_settings_entry_type(&node.entry_type)
                && !(node.entry_type == "message" && role == Some("toolResult"))
        }
        TreeFilterMode::LabeledOnly => node.label.is_some(),
        TreeFilterMode::All => true,
        TreeFilterMode::Default => !is_settings_entry_type(&node.entry_type),
    }
}

struct VisibleTree {
    roots: Vec<usize>,
    children: Vec<Vec<usize>>,
}

/// Filter in preorder and attach each visible node to its closest visible ancestor.
fn build_visible_raw_tree(
    nodes: &[Rc<RawTreeNode>],
    leaf_id: Option<&str>,
    filter: TreeFilterMode,
) -> VisibleTree {
    let by_id: BTreeMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| (node.id.as_str(), idx))
        .collect();
    let parents: Vec<_> = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            node.entry
                .get("parentId")
                .and_then(Value::as_str)
                .and_then(|id| by_id.get(id).copied())
                .filter(|&p| p != idx)
        })
        .collect();
    let mut roots = Vec::new();
    let mut children = vec![Vec::new(); nodes.len()];
    for (idx, parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            children[*parent].push(idx);
        } else {
            roots.push(idx);
        }
    }
    let mut active = vec![false; nodes.len()];
    let mut ancestor = leaf_id.and_then(|id| by_id.get(id).copied());
    while let Some(idx) = ancestor {
        if std::mem::replace(&mut active[idx], true) {
            break;
        }
        ancestor = parents[idx];
    }
    let order = |indices: &mut Vec<usize>| {
        indices.sort_by(|&a, &b| {
            active[b]
                .cmp(&active[a])
                .then_with(|| nodes[a].timestamp.cmp(&nodes[b].timestamp))
        });
    };
    order(&mut roots);
    for children in &mut children {
        order(children);
    }

    let mut visible = VisibleTree {
        roots: Vec::new(),
        children: vec![Vec::new(); nodes.len()],
    };
    let mut stack: Vec<(usize, Option<usize>)> =
        roots.into_iter().rev().map(|idx| (idx, None)).collect();
    while let Some((idx, mut parent)) = stack.pop() {
        let node = &nodes[idx];
        if entry_visible_by_default(&node.entry, &node.entry_type, &node.id, leaf_id)
            && node_visible_by_filter(node, filter)
        {
            if let Some(p) = parent {
                visible.children[p].push(idx);
            } else {
                visible.roots.push(idx);
            }
            parent = Some(idx);
        }
        stack.extend(children[idx].iter().rev().map(|&child| (child, parent)));
    }
    visible
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotMark {
    Empty,
    Square,
    Line,
    LineHidden,
}

#[derive(Debug, Clone)]
struct TreeGutter {
    column: usize,
    show: bool,
}

#[derive(Debug, Clone)]
struct LayoutTreeRow {
    node: Rc<RawTreeNode>,
    display_indent: usize,
    show_connector: bool,
    connector_column: usize,
    gutters: Vec<TreeGutter>,
    is_last: bool,
    child_count: usize,
    parent_index: isize,
    parent_child_count: usize,
    index: usize,
    subtree_end: usize,
    /// True when this connector row is not the first branch sibling (needs upper vertical line).
    has_prev_branch_sibling: bool,
}

impl LayoutTreeRow {
    fn slot_mark(&self, column: usize) -> SlotMark {
        if self.show_connector && column == self.connector_column {
            return SlotMark::Square;
        }
        if let Some(g) = self.gutters.iter().find(|g| g.column == column) {
            return if g.show {
                SlotMark::Line
            } else {
                SlotMark::LineHidden
            };
        }
        SlotMark::Empty
    }

    fn is_foldable(&self) -> bool {
        self.child_count > 0 && (self.parent_index < 0 || self.parent_child_count > 1)
    }
}

fn layout_visible_tree(nodes: &[Rc<RawTreeNode>], tree: &VisibleTree) -> Vec<LayoutTreeRow> {
    struct StackItem {
        node: usize,
        indent: usize,
        has_prev_sibling: bool,
        is_last: bool,
        gutters: Vec<TreeGutter>,
        parent_index: isize,
        parent_child_count: usize,
    }
    enum Step {
        Enter(StackItem),
        Leave(usize),
    }

    let multiple_roots = tree.roots.len() > 1;
    let mut stack: Vec<_> = tree
        .roots
        .iter()
        .enumerate()
        .rev()
        .map(|(i, &node)| {
            Step::Enter(StackItem {
                node,
                indent: usize::from(multiple_roots),
                is_last: i + 1 == tree.roots.len(),
                has_prev_sibling: i > 0,
                gutters: Vec::new(),
                parent_index: -1,
                parent_child_count: 0,
            })
        })
        .collect();
    let mut rows: Vec<LayoutTreeRow> = Vec::new();
    while let Some(step) = stack.pop() {
        let item = match step {
            Step::Leave(index) => {
                let end = rows.len() - 1;
                rows[index].subtree_end = end;
                continue;
            }
            Step::Enter(item) => item,
        };
        let index = rows.len();
        let display_indent = item.indent.saturating_sub(usize::from(multiple_roots));
        let connector_displayed = item.parent_child_count > 1;
        let connector_column = display_indent.saturating_sub(1);
        let children = &tree.children[item.node];
        rows.push(LayoutTreeRow {
            node: Rc::clone(&nodes[item.node]),
            display_indent,
            show_connector: connector_displayed,
            connector_column,
            gutters: item.gutters.clone(),
            is_last: item.is_last,
            child_count: children.len(),
            parent_index: item.parent_index,
            parent_child_count: item.parent_child_count,
            index,
            subtree_end: index,
            has_prev_branch_sibling: item.has_prev_sibling,
        });
        stack.push(Step::Leave(index));
        let just_branched = connector_displayed || (multiple_roots && item.parent_index < 0);
        let child_indent =
            item.indent + usize::from(children.len() > 1 || (just_branched && item.indent > 0));
        let mut gutters = item.gutters;
        if connector_displayed {
            gutters.push(TreeGutter {
                column: connector_column,
                show: !item.is_last,
            });
        }
        for (i, &child) in children.iter().enumerate().rev() {
            stack.push(Step::Enter(StackItem {
                node: child,
                indent: child_indent,
                has_prev_sibling: i > 0,
                is_last: i + 1 == children.len(),
                gutters: gutters.clone(),
                parent_index: index as isize,
                parent_child_count: children.len(),
            }));
        }
    }
    rows
}

fn visible_tree_cursor(
    all_rows: &[LayoutTreeRow],
    rows: &[LayoutTreeRow],
    id: Option<&str>,
) -> Option<usize> {
    let id = id?;
    let mut row = all_rows.iter().find(|row| row.node.id == id)?;
    loop {
        if let Some(index) = rows
            .iter()
            .position(|candidate| candidate.index == row.index)
        {
            return Some(index);
        }
        row = all_rows.get(usize::try_from(row.parent_index).ok()?)?;
    }
}

fn apply_fold_rows(
    rows: &[LayoutTreeRow],
    folded: &BTreeSet<String>,
) -> (Vec<LayoutTreeRow>, BTreeMap<String, usize>) {
    let mut hidden_counts = BTreeMap::new();
    let mut shown = Vec::new();
    let mut next = 0;
    for row in rows {
        if folded.contains(&row.node.id) && row.is_foldable() {
            hidden_counts.insert(row.node.id.clone(), row.subtree_end - row.index);
            if row.index >= next {
                next = row.subtree_end + 1;
                shown.push(row.clone());
            }
        } else if row.index >= next {
            shown.push(row.clone());
        }
    }
    (shown, hidden_counts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeRowKind {
    User,
    Assistant,
    Tool,
    Compaction,
    BranchSummary,
    Info,
}

#[derive(Debug, Clone)]
struct TreeRowPresentation {
    kind: TreeRowKind,
    role_prefix: Option<&'static str>,
    text: String,
}

fn shorten_line(text: &str, max_chars: usize) -> String {
    let cleaned = text.replace(['\r', '\n', '\t'], " ");
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_owned()
    } else {
        let mut s: String = trimmed.chars().take(max_chars).collect();
        s.push('…');
        s
    }
}

fn tool_args_summary(args: &Map<String, Value>) -> String {
    for key in [
        "path",
        "file_path",
        "pattern",
        "command",
        "query",
        "url",
        "directory",
        "target",
        "scope",
        "action",
    ] {
        if let Some(val) = args.get(key).and_then(Value::as_str)
            && !val.trim().is_empty()
        {
            return shorten_line(val, 80);
        }
    }
    let json_str = Value::Object(args.clone()).to_string();
    if json_str == "{}" {
        String::new()
    } else {
        shorten_line(&json_str, 48)
    }
}

fn collect_tool_calls(nodes: &[Rc<RawTreeNode>]) -> BTreeMap<String, (String, Map<String, Value>)> {
    let mut map = BTreeMap::new();
    for node in nodes {
        if node.entry_type == "message"
            && let Some(msg) = node.entry.get("message")
            && msg.get("role").and_then(Value::as_str) == Some("assistant")
            && let Some(arr) = msg.get("content").and_then(Value::as_array)
        {
            for block in arr {
                if block.get("type").and_then(Value::as_str) == Some("toolCall")
                    && let Some(id) = block.get("id").and_then(Value::as_str)
                {
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_owned();
                    let args = block
                        .get("arguments")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    map.insert(id.to_owned(), (name, args));
                }
            }
        }
    }
    map
}

fn present_tree_row(
    node: &RawTreeNode,
    tool_calls: &BTreeMap<String, (String, Map<String, Value>)>,
) -> TreeRowPresentation {
    let entry = &node.entry;
    if node.entry_type == "message"
        && let Some(msg) = entry.get("message")
    {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "user" => {
                let txt = shorten_line(&extract_message_text(msg), 140);
                return TreeRowPresentation {
                    kind: TreeRowKind::User,
                    role_prefix: Some("user:"),
                    text: if txt.is_empty() {
                        "（图片消息）".to_owned()
                    } else {
                        txt
                    },
                };
            }
            "assistant" => {
                let txt = shorten_line(&extract_message_text(msg), 140);
                let text = if !txt.is_empty() {
                    txt
                } else if msg.get("stopReason").and_then(Value::as_str) == Some("aborted") {
                    "（已停止）".to_owned()
                } else if let Some(err) = msg
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    shorten_line(err, 80)
                } else {
                    "（无文本回复）".to_owned()
                };
                return TreeRowPresentation {
                    kind: TreeRowKind::Assistant,
                    role_prefix: Some("assistant:"),
                    text,
                };
            }
            "toolResult" => {
                let call_id = msg.get("toolCallId").and_then(Value::as_str);
                let call = call_id.and_then(|id| tool_calls.get(id));
                let name = call
                    .map(|(n, _)| n.as_str())
                    .or_else(|| msg.get("toolName").and_then(Value::as_str))
                    .unwrap_or("tool");
                let first_line = extract_message_text(msg)
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .map(|l| shorten_line(l, 100))
                    .unwrap_or_default();
                let summary = if !first_line.is_empty() {
                    first_line
                } else if let Some((_, args)) = call {
                    tool_args_summary(args)
                } else {
                    String::new()
                };
                return TreeRowPresentation {
                    kind: TreeRowKind::Tool,
                    role_prefix: None,
                    text: if summary.is_empty() {
                        format!("[{name}]")
                    } else {
                        format!("[{name}] {summary}")
                    },
                };
            }
            "bashExecution" => {
                let cmd = msg.get("command").and_then(Value::as_str).unwrap_or("");
                return TreeRowPresentation {
                    kind: TreeRowKind::Tool,
                    role_prefix: None,
                    text: format!("[bash] {}", shorten_line(cmd, 80)),
                };
            }
            _ => {
                return TreeRowPresentation {
                    kind: TreeRowKind::Info,
                    role_prefix: None,
                    text: format!("[{role}]"),
                };
            }
        }
    }

    match node.entry_type.as_str() {
        "compaction" => {
            let tokens = entry
                .get("tokensBefore")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let k = (tokens as f64 / 1000.0).round() as u64;
            let text = if k > 0 {
                format!("压缩 · {k}k tokens")
            } else if tokens > 0 {
                format!("压缩 · {tokens} tokens")
            } else {
                "压缩 · 上下文已压缩".to_owned()
            };
            TreeRowPresentation {
                kind: TreeRowKind::Compaction,
                role_prefix: None,
                text,
            }
        }
        "branch_summary" => {
            let summary = entry
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default();
            TreeRowPresentation {
                kind: TreeRowKind::BranchSummary,
                role_prefix: None,
                text: format!("分支总结 · {}", shorten_line(summary, 120)),
            }
        }
        "custom_message" => {
            let ctype = entry
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or("custom");
            let txt = shorten_line(&extract_message_text(entry), 120);
            TreeRowPresentation {
                kind: TreeRowKind::Info,
                role_prefix: None,
                text: format!("[{ctype}] {txt}"),
            }
        }
        "session_info" => {
            let name = entry.get("name").and_then(Value::as_str).unwrap_or("空");
            TreeRowPresentation {
                kind: TreeRowKind::Info,
                role_prefix: None,
                text: format!("[标题: {name}]"),
            }
        }
        "model_change" => {
            let model_id = entry.get("modelId").and_then(Value::as_str).unwrap_or("");
            TreeRowPresentation {
                kind: TreeRowKind::Info,
                role_prefix: None,
                text: format!("[模型: {model_id}]"),
            }
        }
        "thinking_level_change" => {
            let level = entry
                .get("thinkingLevel")
                .and_then(Value::as_str)
                .unwrap_or("");
            TreeRowPresentation {
                kind: TreeRowKind::Info,
                role_prefix: None,
                text: format!("[思考强度: {level}]"),
            }
        }
        "label" => {
            let lbl = entry
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("(已清除)");
            TreeRowPresentation {
                kind: TreeRowKind::Info,
                role_prefix: None,
                text: format!("[标记: {lbl}]"),
            }
        }
        other => TreeRowPresentation {
            kind: TreeRowKind::Info,
            role_prefix: None,
            text: format!("[{other}]"),
        },
    }
}

fn matches_tree_search(node: &RawTreeNode, pres: &TreeRowPresentation, query: &str) -> bool {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {}",
        node.label.as_deref().unwrap_or(""),
        pres.role_prefix.unwrap_or(""),
        pres.text
    )
    .to_lowercase();
    tokens.iter().all(|tok| haystack.contains(tok))
}

struct TreeLayout {
    key: (TreeSource, TreeFilterMode),
    all_rows: Vec<LayoutTreeRow>,
    presentations: BTreeMap<String, TreeRowPresentation>,
    leaf_id: Option<String>,
}

struct ComputedTreeView {
    layout: Rc<TreeLayout>,
    query: String,
    folded: BTreeSet<String>,
    shown_rows: Vec<LayoutTreeRow>,
    hidden_counts: BTreeMap<String, usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeSource {
    Full(u64),
    Snapshot(u64),
}

#[derive(Default)]
struct TreeRequest {
    pending: Option<u64>,
    generation: u64,
    dirty: bool,
}

impl TreeRequest {
    fn begin(&mut self) -> Option<u64> {
        if self.pending.is_some() {
            self.dirty = true;
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some(self.generation);
        Some(self.generation)
    }

    fn finish(&mut self, generation: u64) -> Option<bool> {
        if self.pending != Some(generation) {
            return None;
        }
        self.pending = None;
        Some(std::mem::take(&mut self.dirty))
    }

    fn cancel(&mut self) {
        self.pending = None;
        self.dirty = false;
    }
}

#[derive(Debug, Clone)]
pub enum BranchTreeEvent {
    BackToChat,
    SwitchToForkedSession(String),
    PrefillComposer { session_id: String, text: String },
    SummarizingStarted { session_id: String },
    SummarizingFinished { session_id: String },
    Notice(String),
    ErrorMessage(String),
}

pub struct BranchTreeView {
    client: ClientHandle,
    session_id: String,
    filter_mode: TreeFilterMode,
    filter_dropdown_open: bool,
    search_input: Entity<InputState>,
    collapsed_heads: BTreeSet<String>,
    selected_id: Option<String>,
    pending_focus_entry_id: Option<String>,
    keyboard_active: bool,
    last_keyboard_row: Option<(String, usize)>,
    last_pointer_pos: Option<Point<Pixels>>,
    scroll_handle: ScrollHandle,
    pending_target_id: Option<String>,
    summary_selected_idx: usize,
    custom_prompt_mode: bool,
    last_summary_custom_mode: bool,
    custom_prompt_input: Entity<InputState>,
    enter_anim_start: Option<Instant>,
    focus_handle: FocusHandle,
    modal_trap_focus: FocusHandle,
    modal_prior_focus: Option<FocusHandle>,
    visit: u64,
    pending_operations: BTreeSet<String>,
    loaded_tree: Option<(TreeNodes, Option<String>, u64)>,
    request: TreeRequest,
    layout: Option<Rc<TreeLayout>>,
    computed: Option<Rc<ComputedTreeView>>,
    is_open: bool,
    last_observed_session_rev: u64,
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
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索条目"));
        let custom_prompt_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("附加提示词（可选）"));
        let is_open = !session_id.is_empty();
        let enter_anim_start = is_open.then(|| cx.background_executor().now());
        let mut view = Self {
            client,
            session_id: session_id.clone(),
            filter_mode: TreeFilterMode::Default,
            filter_dropdown_open: false,
            search_input,
            collapsed_heads: BTreeSet::new(),
            selected_id: None,
            pending_focus_entry_id: None,
            keyboard_active: false,
            last_keyboard_row: None,
            last_pointer_pos: None,
            scroll_handle: ScrollHandle::new(),
            pending_target_id: None,
            summary_selected_idx: 0,
            custom_prompt_mode: false,
            last_summary_custom_mode: false,
            custom_prompt_input,
            enter_anim_start,
            focus_handle: cx.focus_handle(),
            modal_trap_focus: cx.focus_handle(),
            modal_prior_focus: None,
            visit: 0,
            pending_operations: BTreeSet::new(),
            loaded_tree: None,
            request: TreeRequest::default(),
            layout: None,
            computed: None,
            is_open,
            last_observed_session_rev: 0,
        };
        if !session_id.is_empty() {
            view.trigger_tree_fetch(cx);
        }
        view
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn set_session_id(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_view();
        let changed = self.session_id != session_id;
        if changed {
            self.session_id = session_id;
            self.selected_id = None;
            self.last_keyboard_row = None;
            self.scroll_handle.set_offset(Default::default());
            self.collapsed_heads.clear();
            self.loaded_tree = None;
            self.layout = None;
            self.computed = None;
            self.last_observed_session_rev = 0;
            self.search_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.keyboard_active = false;
        self.last_pointer_pos = None;
        self.pending_focus_entry_id = None;
        self.is_open = !self.session_id.is_empty();
        self.enter_anim_start = self.is_open.then(|| cx.background_executor().now());
        self.trigger_tree_fetch(cx);
        cx.notify();
    }

    pub fn set_session_id_and_focus(
        &mut self,
        session_id: String,
        focus_entry_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_session_id(session_id, window, cx);
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.focus_handle.focus(window, cx);
        self.filter_mode = TreeFilterMode::All;
        self.collapsed_heads.clear();
        self.selected_id = Some(focus_entry_id.clone());
        self.pending_focus_entry_id = Some(focus_entry_id);
        self.keyboard_active = true;
        self.layout = None;
        self.computed = None;
        cx.notify();
    }

    pub fn is_keyboard_active(&self) -> bool {
        self.keyboard_active
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        self.keyboard_active = false;
        self.last_pointer_pos = None;
        cx.notify();
    }

    pub(crate) fn focus_from_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        self.keyboard_active = true;
        self.selected_id = None;
        self.pending_focus_entry_id = None;
        self.last_keyboard_row = None;
        self.last_pointer_pos = Some(window.mouse_position());
        cx.notify();
    }

    fn on_list_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.keyboard_active
            && pointer_moved_past_threshold(&mut self.last_pointer_pos, event.position)
        {
            self.keyboard_active = false;
            cx.notify();
        }
    }

    fn cursor_index(&self, computed: &ComputedTreeView) -> Option<usize> {
        let all = &computed.layout.all_rows;
        let shown = &computed.shown_rows;
        let leaf = computed.layout.leaf_id.as_deref();
        visible_tree_cursor(all, shown, self.selected_id.as_deref().or(leaf))
            .or_else(|| visible_tree_cursor(all, shown, leaf))
            .or_else(|| (!shown.is_empty()).then_some(0))
    }

    fn select_keyboard_row(&mut self, rows: &[LayoutTreeRow], index: usize, window: &Window) {
        self.keyboard_active = true;
        self.selected_id = Some(rows[index].node.id.clone());
        self.last_pointer_pos = Some(window.mouse_position());
        self.scroll_handle.scroll_to_item(index);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_summary_modal()
            || self.filter_dropdown_open
            || event.keystroke.modifiers != gpui::Modifiers::default()
        {
            return;
        }
        let key = event.keystroke.key.as_str();
        let search_focused = self
            .search_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if search_focused {
            if matches!(key, "down" | "enter")
                && !super::input_is_composing(&self.search_input, window, cx)
            {
                let computed = self.compute_rows(cx);
                if !computed.shown_rows.is_empty() {
                    self.focus_handle.focus(window, cx);
                    self.select_keyboard_row(&computed.shown_rows, 0, window);
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            return;
        }

        if !self.focus_handle.is_focused(window) {
            return;
        }
        let computed = self.compute_rows(cx);
        let rows = &computed.shown_rows;
        let Some(cur) = self.cursor_index(&computed) else {
            return;
        };
        let len = rows.len();
        let row = &rows[cur];
        let id = &row.node.id;
        let next = match key {
            "down" if self.keyboard_active => (cur + 1).min(len - 1),
            "up" if self.keyboard_active => cur.saturating_sub(1),
            "down" | "up" => cur,
            "home" => 0,
            "end" => len - 1,
            "left" if computed.query.is_empty() => {
                if row.is_foldable() && self.collapsed_heads.insert(id.clone()) {
                    cur
                } else {
                    let parent_id = usize::try_from(row.parent_index)
                        .ok()
                        .and_then(|index| computed.layout.all_rows.get(index))
                        .map(|row| row.node.id.as_str());
                    visible_tree_cursor(&computed.layout.all_rows, rows, parent_id).unwrap_or(cur)
                }
            }
            "right" if computed.query.is_empty() => {
                if row.is_foldable() && self.collapsed_heads.remove(id) {
                    cur
                } else if rows
                    .get(cur + 1)
                    .is_some_and(|child| child.parent_index == row.index as isize)
                {
                    cur + 1
                } else {
                    cur
                }
            }
            "enter" if !event.is_held => {
                self.select_keyboard_row(rows, cur, window);
                if Some(id.as_str()) == computed.layout.leaf_id.as_deref() {
                    cx.emit(BranchTreeEvent::Notice("你已经在本消息处".into()));
                } else {
                    self.open_summary(id.clone(), window, cx);
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            _ => return,
        };
        self.select_keyboard_row(rows, next, window);
        cx.stop_propagation();
        cx.notify();
    }

    fn trigger_tree_fetch(&mut self, cx: &mut Context<Self>) {
        if !self.is_open {
            return;
        }
        let Some(generation) = self.request.begin() else {
            return;
        };
        let sid = self.session_id.clone();
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let result = client
                .get_session_tree(sid)
                .await
                .ok()
                .and_then(parse_flat_tree);
            let _ = weak.update(cx, |this, cx| {
                let Some(reload) = this.request.finish(generation) else {
                    return;
                };
                if let Some((nodes, leaf)) = result {
                    this.loaded_tree = Some((nodes, leaf, generation));
                }
                if reload {
                    this.trigger_tree_fetch(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn close_view(&mut self) {
        self.visit = self.visit.wrapping_add(1);
        self.request.cancel();
        self.is_open = false;
        self.keyboard_active = false;
        self.selected_id = None;
        self.pending_focus_entry_id = None;
        self.last_keyboard_row = None;
        self.last_pointer_pos = None;
        self.enter_anim_start = None;
        self.pending_target_id = None;
        self.summary_selected_idx = 0;
        self.custom_prompt_mode = false;
        self.filter_dropdown_open = false;
        self.modal_prior_focus = None;
    }

    pub(crate) fn has_summary_modal(&self) -> bool {
        self.pending_target_id.is_some()
    }

    pub(crate) fn focus_primary_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.has_summary_modal() {
            if self.custom_prompt_mode {
                self.custom_prompt_input
                    .update(cx, |inp, cx| inp.focus(window, cx));
                cx.notify();
                return true;
            }
            return false;
        }
        if self.filter_dropdown_open {
            self.filter_dropdown_open = false;
        }
        self.search_input
            .update(cx, |inp, cx| inp.focus(window, cx));
        cx.notify();
        true
    }

    pub(crate) fn dismiss_summary(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.filter_dropdown_open {
            self.filter_dropdown_open = false;
            self.focus_handle.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.pending_target_id.take().is_none() {
            return false;
        }
        self.custom_prompt_mode = false;
        self.modal_prior_focus
            .take()
            .unwrap_or_else(|| self.focus_handle.clone())
            .focus(window, cx);
        cx.notify();
        true
    }

    pub(crate) fn dismiss_filter_dropdown(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.filter_dropdown_open {
            return false;
        }
        self.filter_dropdown_open = false;
        cx.notify();
        true
    }

    fn open_summary(&mut self, target: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_summary_modal() || self.pending_operations.contains(&self.session_id) {
            return;
        }
        self.filter_dropdown_open = false;
        self.modal_prior_focus = window
            .focused(cx)
            .filter(|f| self.focus_handle.contains(f, window));
        self.custom_prompt_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.pending_target_id = Some(target);
        self.summary_selected_idx = 0;
        self.custom_prompt_mode = false;
        self.modal_trap_focus.focus(window, cx);
        cx.notify();
    }

    pub fn filter_mode(&self) -> TreeFilterMode {
        self.filter_mode
    }

    pub fn search_input(&self) -> &Entity<InputState> {
        &self.search_input
    }

    pub fn custom_prompt_input(&self) -> &Entity<InputState> {
        &self.custom_prompt_input
    }

    pub fn cycle_filter(&mut self, reverse: bool, cx: &mut Context<Self>) {
        self.filter_mode = if reverse {
            self.filter_mode.prev()
        } else {
            self.filter_mode.next()
        };
        self.collapsed_heads.clear();
        cx.notify();
    }

    fn compute_rows(&mut self, cx: &gpui_kit::App) -> Rc<ComputedTreeView> {
        let snap = self.client.snapshot();
        let session = snap.loaded_sessions.get(&self.session_id);
        let source = match self.loaded_tree {
            Some((_, _, generation)) => TreeSource::Full(generation),
            None => TreeSource::Snapshot(session.map_or(0, |s| s.revision)),
        };
        let key = (source, self.filter_mode);
        let layout = match &self.layout {
            Some(layout) if layout.key == key => Rc::clone(layout),
            _ => {
                let (nodes, leaf_id) = if let Some((nodes, leaf, _)) = &self.loaded_tree {
                    (nodes.clone(), leaf.clone())
                } else if let Some(session) = session {
                    let entries = if session.entries.is_empty() {
                        &session.messages
                    } else {
                        &session.entries
                    };
                    let nodes = RawTreeNode::from_flat_entries(entries);
                    let leaf = session
                        .leaf_id
                        .clone()
                        .or_else(|| nodes.last().map(|node| node.id.clone()));
                    (nodes, leaf)
                } else {
                    (Vec::new(), None)
                };
                let tool_calls = collect_tool_calls(&nodes);
                let visible = build_visible_raw_tree(&nodes, leaf_id.as_deref(), self.filter_mode);
                let all_rows = layout_visible_tree(&nodes, &visible);
                let presentations = all_rows
                    .iter()
                    .map(|row| {
                        (
                            row.node.id.clone(),
                            present_tree_row(&row.node, &tool_calls),
                        )
                    })
                    .collect();
                let layout = Rc::new(TreeLayout {
                    key,
                    all_rows,
                    presentations,
                    leaf_id,
                });
                self.layout = Some(Rc::clone(&layout));
                layout
            }
        };
        let q = self.search_input.read(cx).value().trim().to_owned();
        if let Some(computed) = &self.computed
            && Rc::ptr_eq(&computed.layout, &layout)
            && computed.query == q
            && computed.folded == self.collapsed_heads
        {
            return Rc::clone(computed);
        }
        let (mut shown_rows, hidden_counts) =
            apply_fold_rows(&layout.all_rows, &self.collapsed_heads);
        if !q.is_empty() {
            shown_rows = layout
                .all_rows
                .iter()
                .filter(|row| {
                    matches_tree_search(&row.node, &layout.presentations[&row.node.id], &q)
                })
                .cloned()
                .collect();
        }
        let computed = Rc::new(ComputedTreeView {
            layout,
            query: q,
            folded: self.collapsed_heads.clone(),
            shown_rows,
            hidden_counts,
        });
        self.computed = Some(Rc::clone(&computed));
        computed
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        let computed = self.compute_rows(cx);
        for row in &computed.layout.all_rows {
            if row.is_foldable() {
                self.collapsed_heads.insert(row.node.id.clone());
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_target_id.as_deref() != Some(&target_id)
            || super::input_is_composing(&self.custom_prompt_input, window, cx)
        {
            return;
        }
        let sid = self.session_id.clone();
        if !self.pending_operations.insert(sid.clone()) {
            return;
        }
        self.dismiss_summary(window, cx);
        let visit = self.visit;
        let client = self.client.clone();
        let draft_key = DraftKey::Session(sid.clone());
        let draft_revision = client.with_workspace(|ws| ws.draft_edit_revision(&draft_key));
        if summarize {
            cx.emit(BranchTreeEvent::SummarizingStarted {
                session_id: sid.clone(),
            });
        }

        cx.spawn(async move |weak, cx| {
            let res = client
                .navigate_session_tree(sid.clone(), target_id, summarize, custom_instructions)
                .await;
            if res
                .as_ref()
                .is_ok_and(|val| val.get("cancelled").and_then(Value::as_bool) != Some(true))
                && client.with_workspace(|ws| ws.active_session_id() == Some(sid.as_str()))
            {
                let _ = client.activate_session(Some(sid.clone())).await;
            }
            let _ = weak.update(cx, |this, cx| {
                this.pending_operations.remove(&sid);
                if summarize {
                    cx.emit(BranchTreeEvent::SummarizingFinished {
                        session_id: sid.clone(),
                    });
                }
                let still_viewing = this.visit == visit;
                match res {
                    Ok(val) if val.get("cancelled").and_then(Value::as_bool) != Some(true) => {
                        this.trigger_tree_fetch(cx);
                        if !summarize && still_viewing {
                            cx.emit(BranchTreeEvent::BackToChat);
                        }
                        if let Some(text) = val
                            .get("editorText")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            let applied = client.with_workspace_mut(|ws| {
                                if ws.draft_edit_revision(&draft_key) != draft_revision {
                                    return false;
                                }
                                ws.draft_mut(draft_key).text = text.to_owned();
                                true
                            });
                            if applied && (summarize || still_viewing) {
                                cx.emit(BranchTreeEvent::PrefillComposer {
                                    session_id: sid,
                                    text: text.to_owned(),
                                });
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(err) => cx.emit(BranchTreeEvent::ErrorMessage(err.to_string())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn execute_fork(&mut self, entry_id: String, cx: &mut Context<Self>) {
        let sid = self.session_id.clone();
        if self.has_summary_modal() || !self.pending_operations.insert(sid.clone()) {
            return;
        }
        let visit = self.visit;
        let client = self.client.clone();
        cx.spawn(async move |weak, cx| {
            let res = client.fork_session(sid.clone(), entry_id).await;
            if res.is_ok() {
                let _ = client.refresh_globals().await;
            }
            let _ = weak.update(cx, |this, cx| {
                this.pending_operations.remove(&sid);
                match res {
                    Ok(new_sid) if this.visit == visit => {
                        cx.emit(BranchTreeEvent::SwitchToForkedSession(new_sid))
                    }
                    Ok(_) => {}
                    Err(err) => cx.emit(BranchTreeEvent::ErrorMessage(err.to_string())),
                }
            });
        })
        .detach();
    }

    /// Renders `424px` wide `K3x72` summary selection modal (`GyiV5` -> `c1wymw`) or independent
    /// `440px` custom prompt modal (`AtYcx` -> `l6fle`) overlaid on `DZ0Fp` frosted scrim.
    fn render_summary_mode_modal(
        &self,
        scrim_progress: f32,
        target_id: &str,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t_direct = target_id.to_owned();
        let t_summary = target_id.to_owned();
        let t_custom = target_id.to_owned();
        let t_key = target_id.to_owned();
        let t_custom_key = target_id.to_owned();
        let in_custom = self.custom_prompt_mode;

        let modal_body = if in_custom {
            // Independent custom-prompt dialog (`AtYcx` -> `l6fle`)
            render_frosted_surface(
                "branch-custom-prompt-card",
                FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
                palette.bg_modal,
                transparency_enabled,
                scale_factor,
            )
            .test_support()
            .a11y_dialog("branch-custom-prompt-card", "自定义提示词总结")
            .w(px(440.0))
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
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("自定义提示词总结")),
                    )
                    .child(
                        div()
                            .id("branch-summary-cancel-btn")
                            .test_support()
                            .tab_index(0)
                            .a11y_button("branch-summary-cancel-btn", "关闭自定义提示词窗口")
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_summary(window, cx);
                            }))
                            .child(
                                Icon::new(IconName::X)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(96.0))
                    .px(space::MD)
                    .py(px(10.0))
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(palette.bg_workspace)
                    .flex()
                    .items_start()
                    .child(
                        div().w_full().child(
                            Input::new(&self.custom_prompt_input)
                                .accessibility_id("branch-tree-custom-prompt-input")
                                .aria_label("分支总结附加提示词")
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .w_full()
                                .cleanable(true),
                        ),
                    ),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("branch-custom-prompt-cancel-btn")
                            .test_support()
                            .a11y_button("branch-custom-prompt-cancel-btn", "取消")
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
                            .tab_index(0)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_summary(window, cx);
                            }))
                            .child(cjk_inline_label("取消")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .id("branch-custom-summary-submit-btn")
                            .test_support()
                            .a11y_button("branch-custom-summary-submit-btn", "执行总结")
                            .h(height::CONTROL)
                            .px(space::LG)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(palette.accent_hover))
                            .tab_index(0)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let value = this.custom_prompt_input.read(cx).value();
                                let prompt = value.trim();
                                this.execute_navigate(
                                    t_custom.clone(),
                                    true,
                                    (!prompt.is_empty()).then(|| prompt.to_owned()),
                                    window,
                                    cx,
                                );
                            }))
                            .child(cjk_inline_label("执行总结")),
                    ),
            )
            .into_any_element()
        } else {
            let sel = self.summary_selected_idx.min(2);
            render_frosted_surface(
                "branch-summary-mode-card",
                FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
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
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .pl(space::MD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("选择")),
                    )
                    .child(
                        div()
                            .id("branch-summary-cancel-btn")
                            .test_support()
                            .tab_index(0)
                            .a11y_button("branch-summary-cancel-btn", "关闭选择弹窗")
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_summary(window, cx);
                            }))
                            .child(
                                Icon::new(IconName::X)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(space::MICRO)
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("branch-mode-direct-btn")
                            .test_support()
                            .tab_index(0)
                            .a11y_button("branch-mode-direct-btn", "直接回退")
                            .w_full()
                            .h(height::FIELD)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(if sel == 0 {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::MD)
                            .hover(move |s| {
                                s.bg(if sel == 0 {
                                    palette.bg_item_active
                                } else {
                                    palette.bg_item_hover
                                })
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.execute_navigate(t_direct.clone(), false, None, window, cx);
                            }))
                            .child(
                                Icon::new(IconName::RotateCcw)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("直接回退")),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("branch-mode-summary-btn")
                            .test_support()
                            .tab_index(0)
                            .a11y_button("branch-mode-summary-btn", "总结")
                            .w_full()
                            .h(height::FIELD)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(if sel == 1 {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::MD)
                            .hover(move |s| {
                                s.bg(if sel == 1 {
                                    palette.bg_item_active
                                } else {
                                    palette.bg_item_hover
                                })
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.execute_navigate(t_summary.clone(), true, None, window, cx);
                            }))
                            .child(
                                Icon::new(IconName::Sparkles)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("总结")),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("branch-mode-custom-summary-btn")
                            .test_support()
                            .tab_index(0)
                            .a11y_button("branch-mode-custom-summary-btn", "自定义提示词总结")
                            .w_full()
                            .h(height::FIELD)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(if sel == 2 {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::MD)
                            .hover(move |s| {
                                s.bg(if sel == 2 {
                                    palette.bg_item_active
                                } else {
                                    palette.bg_item_hover
                                })
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.custom_prompt_mode = true;
                                this.custom_prompt_input.update(cx, |inp, cx| {
                                    inp.focus(window, cx);
                                });
                                cx.notify();
                            }))
                            .child(
                                Icon::new(IconName::PencilLine)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label("自定义提示词总结")),
                    ),
            )
            .into_any_element()
        };

        render_frosted_scrim(
            "branch-summary-scrim-dz0fp",
            FrostedGlassSpec::workspace_scrim(palette).with_opacity(scrim_progress),
            palette.bg_scrim,
            transparency_enabled,
            scale_factor,
        )
        .rounded(radius::ROW)
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
        .child(
            div()
                .tab_stop(false)
                .focus_trap("branch-summary-focus-trap", &self.modal_trap_focus)
                .capture_action(cx.listener(
                    |this, _: &gpui_kit::component::input::MoveUp, window, cx| {
                        if this.custom_prompt_mode
                            && crate::shell::input_nav::handle_input_vertical_boundary(
                                &this.custom_prompt_input,
                                false,
                                window,
                                cx,
                            )
                        {
                            cx.stop_propagation();
                        }
                    },
                ))
                .capture_action(cx.listener(
                    |this, _: &gpui_kit::component::input::MoveDown, window, cx| {
                        if this.custom_prompt_mode
                            && crate::shell::input_nav::handle_input_vertical_boundary(
                                &this.custom_prompt_input,
                                true,
                                window,
                                cx,
                            )
                        {
                            cx.stop_propagation();
                        }
                    },
                ))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if this.custom_prompt_mode {
                        if event.keystroke.key == "enter"
                            && !event.is_held
                            && this
                                .custom_prompt_input
                                .read(cx)
                                .focus_handle(cx)
                                .is_focused(window)
                            && !super::input_is_composing(&this.custom_prompt_input, window, cx)
                            && (event.keystroke.modifiers == gpui::Modifiers::default()
                                || event.keystroke.modifiers.control)
                        {
                            let value = this.custom_prompt_input.read(cx).value();
                            let prompt = value.trim();
                            this.execute_navigate(
                                t_custom_key.clone(),
                                true,
                                (!prompt.is_empty()).then(|| prompt.to_owned()),
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                        return;
                    }
                    if event.keystroke.modifiers != gpui::Modifiers::default() {
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        "up" => {
                            this.summary_selected_idx =
                                this.summary_selected_idx.checked_sub(1).unwrap_or(2);
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "down" => {
                            this.summary_selected_idx = (this.summary_selected_idx + 1) % 3;
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "home" => {
                            this.summary_selected_idx = 0;
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "end" => {
                            this.summary_selected_idx = 2;
                            cx.notify();
                            cx.stop_propagation();
                        }
                        "enter" | "space"
                            if !event.is_held && this.modal_trap_focus.is_focused(window) =>
                        {
                            match this.summary_selected_idx {
                                0 => this.execute_navigate(t_key.clone(), false, None, window, cx),
                                1 => this.execute_navigate(t_key.clone(), true, None, window, cx),
                                _ => {
                                    this.custom_prompt_mode = true;
                                    this.custom_prompt_input.update(cx, |inp, cx| {
                                        inp.focus(window, cx);
                                    });
                                    cx.notify();
                                }
                            }
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }))
                .child(motion::apply_modal_motion(
                    div().child(modal_body),
                    scrim_progress,
                    scale_factor,
                    palette.craft_stack_shadows(),
                )),
        )
        .into_any_element()
    }

    fn render_summary_mode_modal_inert(
        scrim_progress: f32,
        in_custom: bool,
        custom_prompt_text: &str,
        sel: usize,
        palette: &DesktopPalette,
        transparency_enabled: bool,
        scale_factor: f32,
    ) -> gpui_kit::AnyElement {
        let modal_body = if in_custom {
            render_frosted_surface(
                "branch-custom-prompt-modal-exit",
                FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
                palette.bg_modal,
                transparency_enabled,
                scale_factor,
            )
            .w(px(440.0))
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
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("自定义提示词总结")),
                    )
                    .child(
                        div()
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::X)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
            .child(
                apply_scale(div(), scale::UI)
                    .w_full()
                    .h(px(96.0))
                    .px(space::MD)
                    .py(px(10.0))
                    .rounded(radius::CONTROL)
                    .border_1()
                    .border_color(palette.border_strong)
                    .bg(palette.bg_workspace)
                    .text_color(if custom_prompt_text.is_empty() {
                        palette.text_muted
                    } else {
                        palette.text_primary
                    })
                    .overflow_hidden()
                    .child(SharedString::from(custom_prompt_text.to_owned())),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(space::SM)
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .h(height::CONTROL)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .border_1()
                            .border_color(palette.border_strong)
                            .bg(palette.bg_workspace)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .child(cjk_inline_label("取消")),
                    )
                    .child(
                        apply_scale(div(), scale::UI_MEDIUM)
                            .h(height::CONTROL)
                            .px(space::LG)
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_solid)
                            .text_color(palette.text_on_accent)
                            .flex()
                            .items_center()
                            .child(cjk_inline_label("执行总结")),
                    ),
            )
            .into_any_element()
        } else {
            render_frosted_surface(
                "branch-summary-modal-c1wymw-exit",
                FrostedGlassSpec::modal(palette, 8.0).with_opacity(scrim_progress),
                palette.bg_modal,
                transparency_enabled,
                scale_factor,
            )
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
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .pl(space::MD)
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label("选择")),
                    )
                    .child(
                        div()
                            .w(height::COMPACT)
                            .h(height::COMPACT)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::X)
                                    .size(px(14.0))
                                    .text_color(palette.text_secondary),
                            ),
                    ),
            )
            .child(
                div().w_full().flex().flex_col().gap(space::MICRO).children(
                    [
                        (0usize, IconName::RotateCcw, "直接回退"),
                        (1usize, IconName::Sparkles, "总结"),
                        (2usize, IconName::PencilLine, "自定义提示词总结"),
                    ]
                    .into_iter()
                    .map(|(idx, icon, label)| {
                        apply_scale(div(), scale::UI)
                            .w_full()
                            .h(height::FIELD)
                            .px(space::MD)
                            .rounded(radius::CONTROL)
                            .bg(if sel == idx {
                                palette.bg_item_active
                            } else {
                                gpui_kit::transparent_black()
                            })
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::MD)
                            .child(
                                Icon::new(icon)
                                    .size(px(16.0))
                                    .text_color(palette.text_secondary),
                            )
                            .child(cjk_inline_label(label))
                    }),
                ),
            )
            .into_any_element()
        };

        render_frosted_scrim(
            "branch-summary-scrim-dz0fp-exit",
            FrostedGlassSpec::workspace_scrim(palette).with_opacity(scrim_progress),
            palette.bg_scrim,
            transparency_enabled,
            scale_factor,
        )
        .rounded(radius::ROW)
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .flex()
        .items_center()
        .justify_center()
        .child(motion::apply_modal_motion(
            div().child(modal_body),
            scrim_progress,
            scale_factor,
            palette.craft_stack_shadows(),
        ))
        .into_any_element()
    }
}

impl Render for BranchTreeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_global = DesktopThemeGlobal::current(cx);
        let palette = theme_global.palette;
        let transparency_enabled = theme_global.transparency_enabled;
        let scale_factor = window.scale_factor();

        let snap = self.client.snapshot();
        if let Some(sess) = snap.loaded_sessions.get(&self.session_id)
            && sess.revision != self.last_observed_session_rev
        {
            self.last_observed_session_rev = sess.revision;
            self.trigger_tree_fetch(cx);
        }

        let session_title = snap
            .sessions
            .iter()
            .find(|s| s.id == self.session_id)
            .map(|s| {
                let t = s.title.trim();
                if t.is_empty() {
                    "新对话".to_owned()
                } else {
                    t.replace(['\r', '\n'], " ")
                }
            })
            .unwrap_or_else(|| "当前对话".to_owned());

        let computed = self.compute_rows(cx);
        let rows = &computed.shown_rows;
        let hidden_counts = &computed.hidden_counts;
        let presentations = &computed.layout.presentations;
        let leaf_id = &computed.layout.leaf_id;
        let leaf_idx = leaf_id
            .as_deref()
            .and_then(|lid| rows.iter().position(|r| r.node.id == lid));
        let keyboard_row = (self.keyboard_active
            && self.focus_handle.is_focused(window)
            && !self.has_summary_modal()
            && !self.filter_dropdown_open)
            .then(|| self.cursor_index(&computed))
            .flatten();
        let next_row = keyboard_row.map(|idx| (rows[idx].node.id.clone(), idx));
        if let Some(target_eid) = self.pending_focus_entry_id.clone()
            && let Some(idx) = rows.iter().position(|r| r.node.id == target_eid)
        {
            self.selected_id = Some(target_eid);
            self.scroll_handle.scroll_to_item(idx);
            self.pending_focus_entry_id = None;
        } else if next_row != self.last_keyboard_row {
            self.last_keyboard_row = next_row;
            if let Some(idx) = keyboard_row {
                self.scroll_handle.scroll_to_item(idx);
            }
        }

        // 1. Header `OMpWf` (`44px`: MessageSquare per `V5XocM` + Session title + `· 分支树` on left, close `X` on right)
        let header = div()
            .id("branch-tree-header-ompwf")
            .test_support()
            .w_full()
            .h(height::HEADER)
            .pl(space::GROUP)
            .pr(space::LG)
            .border_b_1()
            .border_color(palette.border_subtle)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .min_w_0()
                    .child(
                        Icon::new(IconName::MessageSquare)
                            .size(px(16.0))
                            .text_color(palette.text_secondary),
                    )
                    .child(
                        apply_scale(div(), scale::BODY_SEMIBOLD)
                            .min_w_0()
                            .truncate()
                            .text_color(palette.text_primary)
                            .child(cjk_inline_label(session_title)),
                    )
                    .child(
                        apply_scale(div(), scale::UI)
                            .flex_shrink_0()
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label("· 分支树")),
                    ),
            )
            .child(
                div()
                    .id("branch-tree-back-btn")
                    .test_support()
                    .a11y_button("branch-tree-back-btn", "关闭分支树返回对话")
                    .w(height::CONTROL)
                    .h(height::CONTROL)
                    .flex_shrink_0()
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
                    ),
            );

        // 2. Toolbar `tXgIY` (`56px`: search `gVNCA` + `150px` filter dropdown `PHFud` on left;
        //    `PP59g` icon+text expand/collapse + `{leaf_pos} / {visible} 条` counter on right)
        let counter_text = match leaf_idx {
            Some(idx) => format!("{} / {} 条", idx + 1, rows.len()),
            None => format!("– / {} 条", rows.len()),
        };
        let filter_trigger = apply_scale(div(), scale::UI)
            .id("tree-filter-dropdown-btn")
            .test_support()
            .a11y_toggle_button(
                "tree-filter-dropdown-btn",
                format!("视图过滤：{}", self.filter_mode.label()),
                self.filter_dropdown_open,
            )
            .w(px(150.0))
            .h(height::CONTROL)
            .px(space::MD)
            .rounded(radius::CONTROL)
            .border_1()
            .border_color(if self.filter_dropdown_open {
                palette.border_strong
            } else {
                palette.border_subtle
            })
            .bg(palette.bg_elevated)
            .text_color(palette.text_primary)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::SM)
            .hover(|s| s.bg(palette.bg_item_hover))
            .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.filter_dropdown_open = !this.filter_dropdown_open;
                cx.notify();
            }))
            .child(cjk_inline_label(self.filter_mode.label()))
            .child(motion::animated_chevron(
                "tree-filter-dropdown",
                self.filter_dropdown_open,
                true,
                px(14.0),
                palette.text_secondary,
                window,
                cx,
            ));

        let toolbar = div()
            .id("branch-tree-toolbar")
            .test_support()
            .w_full()
            .h(px(56.0))
            .px(space::GROUP)
            .border_b_1()
            .border_color(palette.border_subtle)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .child({
                        let search_focused = self
                            .search_input
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window);
                        let search_focus_ent = self.search_input.clone();
                        let has_q = !self.search_input.read(cx).value().is_empty();
                        div()
                            .id("branch-tree-search-box")
                            .w(px(240.0))
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
                                        .accessibility_id("branch-tree-search-input")
                                        .aria_label("搜索条目")
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .p_0(),
                                ),
                            )
                            .children(has_q.then(|| {
                                div()
                                    .id("branch-tree-search-clear-btn")
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
                            }))
                    })
                    .child(filter_trigger),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .child(
                        apply_scale(div(), scale::UI)
                            .id("branch-tree-expand-all-btn")
                            .test_support()
                            .a11y_button("branch-tree-expand-all-btn", "全部展开")
                            .h(height::CONTROL)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::TIGHT)
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, _, cx| this.expand_all(cx)))
                            .child(
                                Icon::new(IconName::ChevronsUpDown)
                                    .size(px(15.0))
                                    .text_color(palette.accent_ink),
                            )
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
                            .text_color(palette.text_primary)
                            .flex()
                            .items_center()
                            .gap(space::TIGHT)
                            .hover(|s| s.bg(palette.bg_item_hover))
                            .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx)))
                            .child(
                                Icon::new(IconName::ChevronsDownUp)
                                    .size(px(15.0))
                                    .text_color(palette.accent_ink),
                            )
                            .child(cjk_inline_label("全部折叠")),
                    )
                    .child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.text_secondary)
                            .child(cjk_inline_label(counter_text)),
                    ),
            );

        // 3. Full-width `udjpo` tree list (`40px` rows, `px: 24px`, `32px` geometric tree slots + `28px` role icon slot)
        let mut tree_col = div()
            .id("branch-tree-scroll")
            .track_scroll(&self.scroll_handle)
            .size_full()
            .overflow_y_scroll()
            .py(space::SM)
            .pb(space::LG)
            .flex()
            .flex_col();
        let search_active = !self.search_input.read(cx).value().trim().is_empty();

        if rows.is_empty() {
            let empty_text = if self.request.pending.is_some() && self.loaded_tree.is_none() {
                "正在读取分支树…"
            } else if search_active {
                "没有匹配的条目"
            } else {
                "此会话还没有条目"
            };
            tree_col = tree_col.child(
                div().w_full().py(px(48.0)).flex().justify_center().child(
                    apply_scale(div(), scale::UI)
                        .text_color(palette.text_muted)
                        .child(empty_text),
                ),
            );
        } else {
            let reduce_motion = cx.reduce_motion();
            let enter_elapsed = if reduce_motion {
                self.enter_anim_start = None;
                None
            } else if let Some(started) = self.enter_anim_start {
                let elapsed = cx
                    .background_executor()
                    .now()
                    .saturating_duration_since(started);
                let animated_count = rows.len().min(16);
                let total_ms = ((animated_count.saturating_sub(1) as u64) * 24).min(280) + 220;
                if elapsed >= Duration::from_millis(total_ms) {
                    self.enter_anim_start = None;
                    None
                } else {
                    window.request_animation_frame();
                    Some(elapsed)
                }
            } else {
                None
            };
            for (idx, row) in rows.iter().enumerate() {
                let nid = row.node.id.clone();
                let is_active_leaf = Some(nid.as_str()) == leaf_id.as_deref();
                let pres = presentations
                    .get(&nid)
                    .cloned()
                    .unwrap_or(TreeRowPresentation {
                        kind: TreeRowKind::Info,
                        role_prefix: None,
                        text: nid.clone(),
                    });
                let is_foldable = row.is_foldable() && !search_active;
                let is_folded = is_foldable && self.collapsed_heads.contains(&nid);
                let hidden_count = hidden_counts.get(&nid).copied().unwrap_or(0);

                // Geometric `32px` slots matching `s5gdb` (`10x10` `$accent-ink` square + `2px` `$border-strong` lines)
                let mut slots = div().flex().items_center().flex_shrink_0();
                for col in 0..row.display_indent {
                    match row.slot_mark(col) {
                        SlotMark::Square => {
                            let upper_col = if row.has_prev_branch_sibling {
                                palette.border_strong
                            } else {
                                gpui_kit::transparent_black()
                            };
                            let lower_col = if !row.is_last {
                                palette.border_strong
                            } else {
                                gpui_kit::transparent_black()
                            };
                            slots = slots.child(
                                div()
                                    .w(px(32.0))
                                    .h(height::FIELD)
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .flex_shrink_0()
                                    .child(div().w(px(2.0)).h(px(15.0)).bg(upper_col))
                                    .child(div().w(px(10.0)).h(px(10.0)).bg(palette.accent_ink))
                                    .child(div().w(px(2.0)).h(px(15.0)).bg(lower_col)),
                            );
                        }
                        SlotMark::Line => {
                            slots = slots.child(
                                div()
                                    .w(px(32.0))
                                    .h(height::FIELD)
                                    .flex()
                                    .justify_center()
                                    .flex_shrink_0()
                                    .child(div().w(px(2.0)).h_full().bg(palette.border_strong)),
                            );
                        }
                        SlotMark::Empty | SlotMark::LineHidden => {
                            slots = slots.child(div().w(px(32.0)).h(height::FIELD).flex_shrink_0());
                        }
                    }
                }

                let (icon_name, icon_color, prefix_color, text_scale, text_color) = match pres.kind
                {
                    TreeRowKind::User => (
                        IconName::User,
                        palette.accent_ink,
                        palette.accent_ink,
                        scale::BODY,
                        palette.text_primary,
                    ),
                    TreeRowKind::Assistant => (
                        IconName::Bot,
                        palette.status_success,
                        palette.status_success,
                        scale::BODY,
                        palette.text_primary,
                    ),
                    TreeRowKind::Tool => (
                        IconName::Wrench,
                        palette.text_secondary,
                        palette.text_secondary,
                        scale::UI,
                        palette.text_secondary,
                    ),
                    TreeRowKind::Compaction => (
                        IconName::Minimize2,
                        palette.text_secondary,
                        palette.text_secondary,
                        scale::BODY,
                        palette.text_primary,
                    ),
                    TreeRowKind::BranchSummary => (
                        IconName::GitBranch,
                        palette.accent_ink,
                        palette.accent_ink,
                        scale::BODY,
                        palette.text_primary,
                    ),
                    TreeRowKind::Info => (
                        IconName::Minimize2,
                        palette.text_secondary,
                        palette.text_secondary,
                        scale::UI,
                        palette.text_secondary,
                    ),
                };

                let row_bg = if is_active_leaf {
                    palette.bg_item_active
                } else {
                    gpui_kit::transparent_black()
                };

                let row_id = SharedString::from(format!("tree-node-row-{nid}"));
                let row_label = SharedString::from(match pres.role_prefix {
                    Some(p) => format!("{p} {}", pres.text),
                    None => pres.text.clone(),
                });
                let row_group = SharedString::from(format!("tree-row-grp-{nid}"));
                let nid_click = nid.clone();
                let nid_fold = nid.clone();
                let nid_fork = nid.clone();
                let fork_id = SharedString::from(format!("tree-fork-btn-{nid}"));

                let mut left_content = div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .flex_1()
                    .child(slots)
                    .child(
                        div()
                            .w(px(28.0))
                            .h(height::FIELD)
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .child(Icon::new(icon_name).size(px(16.0)).text_color(icon_color)),
                    );

                if let Some(prefix) = pres.role_prefix {
                    left_content = left_content.child(
                        apply_scale(div(), scale::BODY)
                            .pr(space::TIGHT)
                            .text_color(prefix_color)
                            .flex_shrink_0()
                            .child(cjk_inline_label(prefix)),
                    );
                }

                left_content = left_content.child(
                    apply_scale(div(), text_scale)
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(text_color)
                        .child(cjk_inline_label(pres.text.clone())),
                );

                let mut right_meta = div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .pl(space::MD)
                    .flex_shrink_0();

                if let Some(ref lbl) = row.node.label {
                    right_meta = right_meta.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .px(space::TIGHT)
                            .h(px(22.0))
                            .rounded(radius::CONTROL)
                            .bg(palette.accent_subtle)
                            .text_color(palette.accent_ink)
                            .flex()
                            .items_center()
                            .child(cjk_inline_label(lbl.clone())),
                    );
                }

                if pres.kind == TreeRowKind::User {
                    right_meta = right_meta.child(
                        apply_scale(div(), scale::CAPTION_MEDIUM)
                            .id(fork_id.clone())
                            .test_support()
                            .a11y_button(fork_id, "从此节点派生会话")
                            .h(height::INLINE)
                            .px(space::SM)
                            .rounded(radius::CONTROL)
                            .text_color(palette.text_secondary)
                            .opacity(0.0)
                            .group_hover(row_group.clone(), |s| s.opacity(1.0))
                            .flex()
                            .items_center()
                            .hover(|s| {
                                s.opacity(1.0)
                                    .bg(palette.bg_surface)
                                    .text_color(palette.text_primary)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.execute_fork(nid_fork.clone(), cx);
                                cx.stop_propagation();
                            }))
                            .child(cjk_inline_label("派生会话")),
                    );
                }

                if is_active_leaf {
                    right_meta = right_meta.child(
                        apply_scale(div(), scale::CAPTION)
                            .text_color(palette.accent_ink)
                            .child(cjk_inline_label("当前位置")),
                    );
                }

                if is_foldable {
                    let fold_btn_id = SharedString::from(format!("tree-collapse-btn-{nid}"));
                    right_meta = right_meta.child(
                        div()
                            .id(fold_btn_id.clone())
                            .test_support()
                            .a11y_toggle_button(
                                fold_btn_id,
                                if is_folded {
                                    "展开分支"
                                } else {
                                    "折叠分支"
                                },
                                !is_folded,
                            )
                            .h(height::INLINE)
                            .px(space::XS)
                            .rounded(radius::CONTROL)
                            .flex()
                            .items_center()
                            .gap(space::XS)
                            .text_color(palette.text_secondary)
                            .hover(|s| s.bg(palette.bg_surface).text_color(palette.text_primary))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.collapsed_heads.remove(&nid_fold) {
                                    this.collapsed_heads.insert(nid_fold.clone());
                                }
                                cx.stop_propagation();
                                cx.notify();
                            }))
                            .child(
                                apply_scale(div(), scale::UI)
                                    .child(cjk_inline_label(if is_folded { "▸" } else { "▾" })),
                            )
                            .children(is_folded.then(|| {
                                apply_scale(div(), scale::CAPTION)
                                    .text_color(palette.text_secondary)
                                    .child(cjk_inline_label(format!("已隐藏 {hidden_count} 条")))
                            })),
                    );
                }

                let (row_opacity, row_slide_y) = if let Some(elapsed) = enter_elapsed {
                    if idx < 16 {
                        let delay_ms = ((idx as u64) * 24).min(280);
                        let elapsed_ms = elapsed.as_secs_f32() * 1000.0;
                        let local = ((elapsed_ms - delay_ms as f32) / 220.0).clamp(0.0, 1.0);
                        let eased = motion::ease_out()(local);
                        (0.01 + 0.99 * eased, 14.0 * (1.0 - eased))
                    } else {
                        (1.0, 0.0)
                    }
                } else {
                    (1.0, 0.0)
                };

                tree_col = tree_col.child(
                    div()
                        .id(row_id.clone())
                        .test_support()
                        .a11y_treeitem(row_id, row_label, is_active_leaf)
                        .group(row_group)
                        .relative()
                        .top(px(row_slide_y))
                        .opacity(row_opacity)
                        .w_full()
                        .h(height::FIELD)
                        .flex_shrink_0()
                        .px(space::GROUP)
                        .bg(row_bg)
                        .flex()
                        .items_center()
                        .justify_between()
                        .when(keyboard_row.is_none() && !is_active_leaf, |row| {
                            row.hover(|s| s.bg(palette.bg_item_hover))
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.selected_id = Some(nid_click.clone());
                            this.keyboard_active = false;
                            this.filter_dropdown_open = false;
                            if !is_active_leaf {
                                this.open_summary(nid_click.clone(), window, cx);
                            } else {
                                this.focus_handle.focus(window, cx);
                            }
                            cx.notify();
                        }))
                        .child(left_content)
                        .child(right_meta),
                );
            }
        }

        // Flat filter dropdown popover positioned below filter trigger
        let filter_open = self.filter_dropdown_open;
        let filter_p = motion::surface_presence(
            "branch-filter-pop-p",
            filter_open,
            motion::POPOVER_OPEN,
            window,
            cx,
        );
        let filter_popover = (filter_open || filter_p > 0.001).then(|| {
            let mut menu_rows: Vec<gpui_kit::AnyElement> = Vec::new();
            for mode in TreeFilterMode::ALL_MODES {
                let active = self.filter_mode == mode;
                let base_row = apply_scale(div(), scale::UI)
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
                    .child(cjk_inline_label(mode.label()))
                    .children(active.then(|| {
                        Icon::new(IconName::Check)
                            .size(px(14.0))
                            .text_color(palette.accent_ink)
                    }));
                let row = if filter_open {
                    let hover_bg = if active {
                        palette.bg_item_active
                    } else {
                        palette.bg_item_hover
                    };
                    base_row
                        .id(SharedString::from(format!("tree-filter-{:?}", mode)))
                        .test_support()
                        .a11y_menuitem(mode.a11y_id(), mode.label())
                        .hover(move |s| s.bg(hover_bg))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.filter_mode = mode;
                            this.filter_dropdown_open = false;
                            this.collapsed_heads.clear();
                            this.keyboard_active = false;
                            this.focus_handle.focus(window, cx);
                            cx.notify();
                        }))
                        .into_any_element()
                } else {
                    base_row.into_any_element()
                };
                menu_rows.push(row);
            }

            let menu_base = div()
                .id(if filter_open {
                    "tree-filter-dropdown-menu"
                } else {
                    "tree-filter-dropdown-menu-exit"
                })
                .w(px(164.0))
                .p(space::TIGHT)
                .rounded(radius::ROW)
                .bg(palette.bg_glass)
                .border_1()
                .border_color(palette.border_subtle)
                .flex()
                .flex_col()
                .gap(space::MICRO)
                .children(menu_rows);
            let menu = if filter_open {
                menu_base
                    .test_support()
                    .role(gpui::Role::Menu)
                    .accessibility_id("tree-filter-dropdown-menu")
                    .aria_label("视图过滤选项")
                    .capture_any_mouse_down(crate::shell::keep_popover_open_on_mouse_down)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .into_any_element()
            } else {
                menu_base.into_any_element()
            };

            let animated = motion::apply_popover_motion(
                div().rounded(radius::ROW).child(menu),
                filter_p,
                -6.0,
                scale_factor,
                palette.flat_popover_shadows(),
            );
            let wrap = div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .child(
                    div()
                        .absolute()
                        .top(px(92.0))
                        .left(px(276.0))
                        .child(animated),
                );
            if filter_open {
                wrap.id("tree-filter-dropdown-backdrop")
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.filter_dropdown_open = false;
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            } else {
                wrap.into_any_element()
            }
        });

        let summary_open = self.pending_target_id.is_some();
        if summary_open {
            self.last_summary_custom_mode = self.custom_prompt_mode;
        }
        let summary_scrim_p = motion::surface_presence(
            "branch-summary-scrim-p",
            summary_open,
            motion::MODAL_OPEN,
            window,
            cx,
        );
        let summary_modal_opt = if let Some(tid) = self.pending_target_id.clone() {
            Some(self.render_summary_mode_modal(
                summary_scrim_p,
                &tid,
                &palette,
                transparency_enabled,
                scale_factor,
                cx,
            ))
        } else if summary_scrim_p > 0.001 {
            let custom_text = self.custom_prompt_input.read(cx).value().to_string();
            Some(Self::render_summary_mode_modal_inert(
                summary_scrim_p,
                self.last_summary_custom_mode,
                &custom_text,
                self.summary_selected_idx,
                &palette,
                transparency_enabled,
                scale_factor,
            ))
        } else {
            None
        };

        let kbd_style = snap.desktop_settings.keyboard_focus_style;
        let kbd_target = keyboard_row.map(|idx| {
            let row_h = f32::from(height::FIELD);
            KbdRowTarget {
                top: f32::from(space::SM) + (idx as f32) * row_h + 1.0,
                height: row_h - 2.0,
                inset_x: 14.0,
                corner_radius: 6.5,
            }
        });
        let kbd_overlay = render_gliding_row_indicator(
            "branch-tree",
            kbd_style,
            kbd_target,
            &self.scroll_handle,
            &palette,
            window,
            cx,
        );

        div()
            .id("branch-tree-view")
            .test_support()
            .a11y_region("branch-tree-view", "分支树视图")
            .track_focus(&self.focus_handle)
            .on_mouse_move(cx.listener(Self::on_list_mouse_move))
            .on_key_down(cx.listener(Self::on_key_down))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                super::consume_composing_escape(&this.search_input, window, cx);
                super::consume_composing_escape(&this.custom_prompt_input, window, cx);
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(toolbar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(tree_col)
                    .children(kbd_overlay),
            )
            .children(filter_popover)
            .children(summary_modal_opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_parent_ids_in_flat_entries_do_not_produce_empty_tree() {
        // Real sessions start with non-message entries (e.g. `model_change` id="e0") that are
        // absent from `full_snapshot().entries`. The first user message still points to `parentId: "e0"`.
        let flat = vec![
            json!({
                "id": "u1",
                "parentId": "e0-model-change",
                "message": { "role": "user", "content": "你好" }
            }),
            json!({
                "id": "a1",
                "parentId": "u1",
                "message": { "role": "assistant", "content": [{"type": "text", "text": "你好，有什么可以帮你？"}] }
            }),
        ];
        let empty = BTreeSet::new();
        let display = build_visible_tree_nodes(&flat, Some("a1"), TreeFilterMode::Default, &empty);
        assert_eq!(display.len(), 2);
        assert_eq!(display[0].id, "u1");
        assert_eq!(display[1].id, "a1");

        let raw_roots = RawTreeNode::from_flat_entries(&flat);
        let visible = build_visible_raw_tree(&raw_roots, Some("a1"), TreeFilterMode::Default);
        let rows = layout_visible_tree(&raw_roots, &visible);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].node.id, "u1");
        assert_eq!(rows[1].node.id, "a1");
    }

    #[test]
    fn flat_tree_handles_deep_history_and_rejects_invalid_payloads() {
        let entries: Vec<_> = (0..4096)
            .map(|i: usize| {
                json!({"entry": {
                    "id": i.to_string(), "parentId": i.checked_sub(1).map(|p| p.to_string()),
                    "type": "message", "message": {"role":"user"}
                }})
            })
            .collect();
        let payload = json!({"entries": entries, "leafId":"4095"});
        let (nodes, leaf) = parse_flat_tree(
            serde_json::from_slice(&serde_json::to_vec(&payload).unwrap()).unwrap(),
        )
        .unwrap();
        let visible = build_visible_raw_tree(&nodes, leaf.as_deref(), TreeFilterMode::Default);
        let rows = layout_visible_tree(&nodes, &visible);
        assert_eq!(rows.len(), 4096);
        assert_eq!(rows[0].subtree_end, 4095);
        assert_eq!(rows[4095].parent_index, 4094);
        assert!(rows.iter().zip(&nodes).all(|(r, n)| Rc::ptr_eq(&r.node, n)));
        for invalid in [
            Value::Null,
            json!({"entries": [], "leafId":"4095"}),
            json!({"entries": [{"entry":{}}], "leafId":null}),
        ] {
            assert!(parse_flat_tree(invalid).is_none());
        }
    }

    #[test]
    fn tree_requests_coalesce_refresh_and_discard_closed_generation() {
        let mut request = TreeRequest::default();
        let old = request.begin().unwrap();
        assert_eq!(request.begin(), None);
        assert_eq!(request.finish(old), Some(true));
        let refresh = request.begin().unwrap();
        request.cancel();
        let reopened = request.begin().unwrap();
        assert_ne!(refresh, reopened);
        assert_eq!(request.finish(refresh), None);
        assert_eq!(request.finish(reopened), Some(false));
    }

    #[test]
    fn api_tree_supports_branches_compaction_filters_and_folding() {
        let (roots, leaf) = parse_flat_tree(json!({
            "leafId": "a1",
            "entries": [
                {"entry": {"id":"m0", "type":"model_change", "timestamp":"2026-11-22T10:00:00Z"}},
                {"entry": {"id":"u1", "parentId":"m0", "type":"message", "timestamp":"2026-11-22T10:00:01Z", "message":{"role":"user", "content":"查一下 C 盘"}}},
                {"entry": {"id":"c1", "parentId":"u1", "type":"compaction", "timestamp":"2026-11-22T10:00:02Z", "summary":"已整理 C 盘诊断记录", "tokensBefore":148000}},
                {"entry": {"id":"a1", "parentId":"c1", "type":"message", "timestamp":"2026-11-22T10:00:03Z", "message":{"role":"assistant", "content":"C 盘剩余 18.4 GB"}}},
                {"label":"备选方案","entry": {"id":"u2", "parentId":"u1", "type":"message", "timestamp":"2026-11-22T10:00:04Z", "message":{"role":"user", "content":"分支 B 提问"}}}
            ]
        })).unwrap();
        assert_eq!(leaf.as_deref(), Some("a1"));
        let tool_calls = collect_tool_calls(&roots);

        // Default view skips `model_change` (`m0`) and promotes `u1` as root with 2 branches.
        let vis_default = build_visible_raw_tree(&roots, Some("a1"), TreeFilterMode::Default);
        let rows = layout_visible_tree(&roots, &vis_default);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].node.id, "u1");
        assert_eq!(rows[1].node.id, "c1");
        assert_eq!(rows[2].node.id, "a1");
        assert_eq!(rows[3].node.id, "u2");

        let comp_pres = present_tree_row(&rows[1].node, &tool_calls);
        assert_eq!(comp_pres.kind, TreeRowKind::Compaction);
        assert_eq!(comp_pres.text, "压缩 · 148k tokens");

        // Folding root `u1` hides all 3 descendants.
        let mut folded = BTreeSet::new();
        folded.insert("u1".to_owned());
        let (folded_rows, hidden) = apply_fold_rows(&rows, &folded);
        assert_eq!(folded_rows.len(), 1);
        assert_eq!(hidden.get("u1").copied(), Some(3));
        assert_eq!(
            visible_tree_cursor(&rows, &folded_rows, Some("a1")),
            Some(0)
        );

        // LabeledOnly shows only `u2`.
        let vis_labeled = build_visible_raw_tree(&roots, Some("a1"), TreeFilterMode::LabeledOnly);
        let labeled_rows = layout_visible_tree(&roots, &vis_labeled);
        assert_eq!(labeled_rows.len(), 1);
        assert_eq!(labeled_rows[0].node.id, "u2");
    }
}

use std::collections::{BTreeMap, BTreeSet};

use pi_agent::AgentMessage;
use pi_ai::{ContentBlock, UserContent, UserContentBlock};
use serde::{Deserialize, Serialize};

use crate::session_manager::{SessionEntry, SessionTreeNode};

/// 分支树五档过滤模式（对照 `FilterMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TreeFilterMode {
    #[default]
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

/// 树节点左侧连接线竖列状态（对照 `GutterInfo`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GutterInfo {
    pub position: usize,
    pub show: bool,
}

/// 经过五档过滤与几何结构重算后的树行节点（供 TUI `TreeList` 与 Web 分支树共用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilteredTreeNode {
    pub entry: SessionEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_timestamp: Option<String>,
    pub active_path: bool,
    pub traversal_order: usize,
    pub indent: usize,
    pub display_indent: usize,
    pub show_connector: bool,
    pub connector_displayed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_column: Option<usize>,
    pub is_last: bool,
    pub is_virtual_root_child: bool,
    pub gutters: Vec<GutterInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_parent_id: Option<String>,
}

#[derive(Clone)]
struct FlatNode {
    node: SessionTreeNode,
    traversal_order: usize,
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<GutterInfo>,
    is_virtual_root_child: bool,
}

fn collect_contains_active(
    roots: &[SessionTreeNode],
    current_leaf_id: Option<&str>,
) -> BTreeMap<String, bool> {
    let mut contains_active: BTreeMap<String, bool> = BTreeMap::new();
    let mut all_nodes: Vec<&SessionTreeNode> = Vec::new();
    let mut pre_order_stack: Vec<&SessionTreeNode> = roots.iter().rev().collect();

    while let Some(node) = pre_order_stack.pop() {
        for child in node.children.iter().rev() {
            pre_order_stack.push(child);
        }
        all_nodes.push(node);
    }

    for node in all_nodes.into_iter().rev() {
        let id = node.entry.id().to_owned();
        let mut has = current_leaf_id.is_some_and(|leaf| leaf == id);
        for child in &node.children {
            if contains_active
                .get(child.entry.id())
                .copied()
                .unwrap_or(false)
            {
                has = true;
            }
        }
        contains_active.insert(id, has);
    }
    contains_active
}

fn flatten_full_tree(roots: &[SessionTreeNode], current_leaf_id: Option<&str>) -> Vec<FlatNode> {
    let mut result = Vec::new();
    let contains_active = collect_contains_active(roots, current_leaf_id);
    let multiple_roots = roots.len() > 1;

    type StackItem<'a> = (
        &'a SessionTreeNode,
        usize,
        bool,
        bool,
        bool,
        Vec<GutterInfo>,
        bool,
    );
    let mut stack: Vec<StackItem<'_>> = Vec::new();
    let mut ordered_roots: Vec<&SessionTreeNode> = roots.iter().collect();

    ordered_roots.sort_by_key(|node| {
        (
            !contains_active
                .get(node.entry.id())
                .copied()
                .unwrap_or(false),
            node.entry.timestamp().to_owned(),
        )
    });
    for (i, root) in ordered_roots.iter().enumerate().rev() {
        let is_last = i == ordered_roots.len() - 1;
        stack.push((
            *root,
            usize::from(multiple_roots),
            multiple_roots,
            multiple_roots,
            is_last,
            Vec::new(),
            multiple_roots,
        ));
    }

    while let Some((
        node,
        indent,
        just_branched,
        show_connector,
        is_last,
        gutters,
        is_virtual_root_child,
    )) = stack.pop()
    {
        let traversal_order = result.len();
        let children = &node.children;
        result.push(FlatNode {
            node: SessionTreeNode {
                entry: node.entry.clone(),
                children: Vec::new(),
                label: node.label.clone(),
                label_timestamp: node.label_timestamp.clone(),
            },
            traversal_order,
            indent,
            show_connector,
            is_last,
            gutters: gutters.clone(),
            is_virtual_root_child,
        });

        let multiple_children = children.len() > 1;
        let mut ordered_children: Vec<&SessionTreeNode> = children.iter().collect();

        let child_indent = if multiple_children || (just_branched && indent > 0) {
            indent + 1
        } else {
            indent
        };

        let connector_displayed = show_connector && !is_virtual_root_child;
        let current_display_indent = if multiple_roots {
            indent.saturating_sub(1)
        } else {
            indent
        };
        let connector_position = current_display_indent.saturating_sub(1);
        let child_gutters = if connector_displayed {
            let mut next = gutters;
            next.push(GutterInfo {
                position: connector_position,
                show: !is_last,
            });
            next
        } else {
            gutters
        };

        ordered_children.sort_by_key(|node| {
            (
                !contains_active
                    .get(node.entry.id())
                    .copied()
                    .unwrap_or(false),
                node.entry.timestamp().to_owned(),
            )
        });
        for (i, child) in ordered_children.iter().enumerate().rev() {
            let child_is_last = i == ordered_children.len() - 1;
            stack.push((
                *child,
                child_indent,
                multiple_children,
                multiple_children,
                child_is_last,
                child_gutters.clone(),
                false,
            ));
        }
    }

    result
}

fn searchable_text_of_node(node: &SessionTreeNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(label) = &node.label {
        parts.push(label.clone());
    }
    match &node.entry {
        SessionEntry::Message(m) => {
            parts.push(m.message.role().to_owned());
            match &m.message {
                AgentMessage::User(u) => match &u.content {
                    UserContent::Text(s) => parts.push(s.clone()),
                    UserContent::Blocks(blocks) => {
                        for b in blocks {
                            if let UserContentBlock::Text(t) = b {
                                parts.push(t.text.clone());
                            }
                        }
                    }
                },
                AgentMessage::Assistant(a) => {
                    for b in &a.content {
                        match b {
                            ContentBlock::Text(t) => parts.push(t.text.clone()),
                            ContentBlock::Thinking(th) => parts.push(th.thinking.clone()),
                            ContentBlock::ToolCall(tc) => parts.push(tc.name.clone()),
                        }
                    }
                    if let Some(err) = &a.error_message {
                        parts.push(err.clone());
                    }
                }
                AgentMessage::ToolResult(tr) => {
                    parts.push(tr.tool_name.clone());
                    for b in &tr.content {
                        if let UserContentBlock::Text(t) = b {
                            parts.push(t.text.clone());
                        }
                    }
                }
                AgentMessage::System(_) => {}
                AgentMessage::Custom(c) => {
                    parts.push(c.role.clone());
                    parts.push(serde_json::Value::Object(c.data.clone()).to_string());
                }
            }
        }
        SessionEntry::CustomMessage(cm) => {
            parts.push(cm.custom_type.clone());
            parts.push(cm.content.text_summary());
        }
        SessionEntry::Compaction(c) => {
            parts.push("compaction".to_owned());
            parts.push(c.summary.clone());
        }
        SessionEntry::BranchSummary(bs) => {
            parts.push("branch summary".to_owned());
            parts.push(bs.summary.clone());
        }
        SessionEntry::ModelChange(mc) => {
            parts.push(format!("{}/{}", mc.provider, mc.model_id));
        }
        SessionEntry::ThinkingLevelChange(tl) => {
            parts.push(tl.thinking_level.clone());
        }
        SessionEntry::SessionInfo(si) => {
            if let Some(n) = &si.name {
                parts.push(n.clone());
            }
        }
        SessionEntry::Custom(ce) => {
            parts.push(ce.custom_type.clone());
        }
        SessionEntry::Label(le) => {
            if let Some(l) = &le.label {
                parts.push(l.clone());
            }
        }
        SessionEntry::ContextEdit(ce) => {
            parts.push(ce.target_id.clone());
        }
        SessionEntry::Usage(ue) => {
            parts.push(ue.kind.clone());
        }
    }
    parts.join(" ")
}

/// 构建对齐上游 `TreeList` 的五档过滤几何分支树。
pub fn build_filtered_tree(
    roots: &[SessionTreeNode],
    current_leaf_id: Option<&str>,
    filter_mode: TreeFilterMode,
    search_query: Option<&str>,
    folded_node_ids: Option<&BTreeSet<String>>,
) -> Vec<FilteredTreeNode> {
    let flat_nodes = flatten_full_tree(roots, current_leaf_id);
    if flat_nodes.is_empty() {
        return Vec::new();
    }

    let mut entry_map: BTreeMap<String, &FlatNode> = BTreeMap::new();
    for f in &flat_nodes {
        entry_map.insert(f.node.entry.id().to_owned(), f);
    }

    let mut active_path_ids: BTreeSet<String> = BTreeSet::new();
    let mut cur_id = current_leaf_id.map(ToOwned::to_owned);
    while let Some(id) = cur_id {
        if !active_path_ids.insert(id.clone()) {
            break;
        }
        cur_id = entry_map
            .get(&id)
            .and_then(|f| f.node.entry.parent_id().map(ToOwned::to_owned));
    }

    let search_tokens: Vec<String> = search_query
        .unwrap_or_default()
        .to_lowercase()
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let mut filtered_nodes: Vec<FlatNode> = flat_nodes
        .iter()
        .filter(|flat_node| {
            let entry = &flat_node.node.entry;
            let is_settings_entry = matches!(
                entry,
                SessionEntry::Label(_)
                    | SessionEntry::ContextEdit(_)
                    | SessionEntry::Custom(_)
                    | SessionEntry::ModelChange(_)
                    | SessionEntry::ThinkingLevelChange(_)
                    | SessionEntry::SessionInfo(_)
                    | SessionEntry::Usage(_)
            );

            let passes_filter = match filter_mode {
                TreeFilterMode::UserOnly => {
                    matches!(entry, SessionEntry::Message(m) if m.message.role() == "user")
                }
                TreeFilterMode::NoTools => {
                    !is_settings_entry
                        && !matches!(entry, SessionEntry::Message(m) if m.message.role() == "toolResult")
                }
                TreeFilterMode::LabeledOnly => flat_node.node.label.as_ref().is_some_and(|label| !label.trim().is_empty()),
                TreeFilterMode::All => true,
                TreeFilterMode::Default => !is_settings_entry,
            };

            if !passes_filter {
                return false;
            }

            if !search_tokens.is_empty() {
                let node_text = searchable_text_of_node(&flat_node.node).to_lowercase();
                if !search_tokens.iter().all(|tok| node_text.contains(tok)) {
                    return false;
                }
            }

            true
        })
        .cloned()
        .collect();

    if let Some(folded) = folded_node_ids
        && !folded.is_empty()
    {
        let mut skip_set: BTreeSet<String> = BTreeSet::new();
        for flat_node in &flat_nodes {
            let id = flat_node.node.entry.id();
            if let Some(pid) = flat_node.node.entry.parent_id()
                && (folded.contains(pid) || skip_set.contains(pid))
            {
                skip_set.insert(id.to_owned());
            }
        }
        filtered_nodes.retain(|f| !skip_set.contains(f.node.entry.id()));
    }

    if filtered_nodes.is_empty() {
        return Vec::new();
    }

    let visible_ids: BTreeSet<String> = filtered_nodes
        .iter()
        .map(|f| f.node.entry.id().to_owned())
        .collect();

    let find_visible_ancestor = |node_id: &str| -> Option<String> {
        let mut current = entry_map
            .get(node_id)
            .and_then(|f| f.node.entry.parent_id().map(ToOwned::to_owned));
        let mut visited = BTreeSet::new();
        while let Some(cid) = current {
            if !visited.insert(cid.clone()) {
                break;
            }
            if visible_ids.contains(&cid) {
                return Some(cid);
            }
            current = entry_map
                .get(&cid)
                .and_then(|f| f.node.entry.parent_id().map(ToOwned::to_owned));
        }
        None
    };

    let mut visible_parent: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut visible_children: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
    visible_children.insert(None, Vec::new());

    for flat_node in &filtered_nodes {
        let node_id = flat_node.node.entry.id().to_owned();
        let ancestor_id = find_visible_ancestor(&node_id);
        visible_parent.insert(node_id.clone(), ancestor_id.clone());
        visible_children
            .entry(ancestor_id)
            .or_default()
            .push(node_id);
    }

    let visible_root_ids = visible_children.get(&None).cloned().unwrap_or_default();
    let multiple_roots = visible_root_ids.len() > 1;

    let mut filtered_node_map: BTreeMap<String, FlatNode> = filtered_nodes
        .iter()
        .map(|f| (f.node.entry.id().to_owned(), f.clone()))
        .collect();

    type StackItem = (String, usize, bool, bool, bool, Vec<GutterInfo>, bool);
    let mut stack: Vec<StackItem> = Vec::new();

    for (i, root_id) in visible_root_ids.iter().enumerate().rev() {
        let is_last = i == visible_root_ids.len() - 1;
        stack.push((
            root_id.clone(),
            usize::from(multiple_roots),
            multiple_roots,
            multiple_roots,
            is_last,
            Vec::new(),
            multiple_roots,
        ));
    }

    while let Some((
        node_id,
        indent,
        just_branched,
        show_connector,
        is_last,
        gutters,
        is_virtual_root_child,
    )) = stack.pop()
    {
        if let Some(flat_node) = filtered_node_map.get_mut(&node_id) {
            flat_node.indent = indent;
            flat_node.show_connector = show_connector;
            flat_node.is_last = is_last;
            flat_node.gutters = gutters.clone();
            flat_node.is_virtual_root_child = is_virtual_root_child;
        }

        let children = visible_children
            .get(&Some(node_id))
            .cloned()
            .unwrap_or_default();
        let multiple_children = children.len() > 1;

        let child_indent = if multiple_children || (just_branched && indent > 0) {
            indent + 1
        } else {
            indent
        };

        let connector_displayed = show_connector && !is_virtual_root_child;
        let current_display_indent = if multiple_roots {
            indent.saturating_sub(1)
        } else {
            indent
        };
        let connector_position = current_display_indent.saturating_sub(1);
        let child_gutters = if connector_displayed {
            let mut next = gutters;
            next.push(GutterInfo {
                position: connector_position,
                show: !is_last,
            });
            next
        } else {
            gutters
        };

        for (i, child_id) in children.iter().enumerate().rev() {
            let child_is_last = i == children.len() - 1;
            stack.push((
                child_id.clone(),
                child_indent,
                multiple_children,
                multiple_children,
                child_is_last,
                child_gutters.clone(),
                false,
            ));
        }
    }

    filtered_nodes
        .into_iter()
        .filter_map(|orig| {
            let id = orig.node.entry.id().to_owned();
            let updated = filtered_node_map.remove(&id)?;
            let display_indent = if multiple_roots {
                updated.indent.saturating_sub(1)
            } else {
                updated.indent
            };
            let connector_displayed = updated.show_connector && !updated.is_virtual_root_child;
            let connector_column = connector_displayed.then(|| display_indent.saturating_sub(1));
            Some(FilteredTreeNode {
                entry: updated.node.entry.clone(),
                label: updated.node.label.clone(),
                label_timestamp: updated.node.label_timestamp.clone(),
                active_path: active_path_ids.contains(&id),
                traversal_order: updated.traversal_order,
                indent: updated.indent,
                display_indent,
                show_connector: updated.show_connector,
                connector_displayed,
                connector_column,
                is_last: updated.is_last,
                is_virtual_root_child: updated.is_virtual_root_child,
                gutters: updated.gutters,
                visible_parent_id: visible_parent.get(&id).cloned().flatten(),
            })
        })
        .collect()
}

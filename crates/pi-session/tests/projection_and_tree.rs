use std::collections::BTreeSet;

use pi_agent::AgentMessage;
use pi_ai::{
    StopReason, SystemContent, SystemMessage, TextContent, ToolResultMessage, UserContentBlock,
    UserMessage,
};
use pi_session::{
    CompactionSummaryMessage, ContextEditReplacement, SessionEntry, SessionManager, TreeFilterMode,
    build_filtered_tree,
};

fn assistant_msg(text: &str) -> AgentMessage {
    let mut m = pi_ai::faux::faux_text_message(text);
    m.stop_reason = StopReason::Stop;
    AgentMessage::Assistant(m)
}

#[test]
fn deep_session_tree_build_and_drop_are_stack_safe() {
    std::thread::Builder::new()
        .name("deep-session-tree".to_owned())
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut manager = SessionManager::in_memory("/deep-tree", None, None).unwrap();
            for _ in 0..5_000 {
                manager.append_custom_entry("deep", None).unwrap();
            }

            let roots = manager.get_tree();
            assert_eq!(roots.len(), 1);
            let mut depth = 1;
            let mut node = &roots[0];
            while let Some(child) = node.children.first() {
                depth += 1;
                node = child;
            }
            assert_eq!(depth, 5_000);
            drop(roots);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn compaction_projection_keeps_only_index_zero_compaction_and_applies_context_edits() {
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();

    let sys1 = sm
        .append_message(AgentMessage::System(SystemMessage {
            content: SystemContent::Text("initial system prompt".to_owned()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: 100,
        }))
        .unwrap();
    let _u1 = sm
        .append_message(AgentMessage::User(UserMessage::new("old question 1")))
        .unwrap();
    let _a1 = sm.append_message(assistant_msg("old answer 1")).unwrap();
    let u2 = sm
        .append_message(AgentMessage::User(UserMessage::new("kept question 2")))
        .unwrap();
    let a2 = sm.append_message(assistant_msg("kept answer 2")).unwrap();

    // 第一次压缩：保留从 u2 开始的条目
    let comp1 = sm
        .append_compaction("summary 1", Some(u2.clone()), 1000, None, None, None)
        .unwrap();

    let u3 = sm
        .append_message(AgentMessage::User(UserMessage::new("question 3")))
        .unwrap();
    let a3 = sm.append_message(assistant_msg("answer 3")).unwrap();

    // 第二次压缩：first_kept_entry_id 仍指向 u2（因此 comp1 位于保留区间内！）
    let comp2 = sm
        .append_compaction(
            "summary 2 (latest)",
            Some(u2.clone()),
            2000,
            None,
            None,
            None,
        )
        .unwrap();

    // 对 a2 应用两次 ContextEdit（最新一条胜出，且纯字符串自动包装为文本块数组），对 u3 应用 null 省略
    sm.append_context_edit(
        &a2,
        Some(ContextEditReplacement::from_text("draft replacement")),
    )
    .unwrap();
    sm.append_context_edit(
        &a2,
        Some(ContextEditReplacement::from_text(
            "final replacement for a2",
        )),
    )
    .unwrap();
    sm.append_context_edit(&u3, None).unwrap();

    let proj = sm.build_session_projection();

    // 1. 投影首项必须是最新压缩 comp2，展开为 system_message + CompactionSummaryMessage
    assert_eq!(proj.entries[0].source_entry.id(), comp2);
    assert_eq!(proj.entries[0].messages.len(), 2);
    assert_eq!(proj.entries[0].messages[0].role(), "system");
    let cs = CompactionSummaryMessage::from_agent_message(&proj.entries[0].messages[1]).unwrap();
    assert_eq!(cs.summary, "summary 2 (latest)");

    // 2. 位于保留区间内的旧压缩 comp1 的投影消息必须为空 []
    let comp1_proj = proj
        .entries
        .iter()
        .find(|e| e.source_entry.id() == comp1)
        .expect("comp1 in context entries");
    assert!(
        comp1_proj.messages.is_empty(),
        "older compaction inside retained range must project to empty messages"
    );

    // 3. 压缩前的 sys1 不再重复出现在保留区间内
    assert!(!proj.entries.iter().any(|e| e.source_entry.id() == sys1));

    // 4. u3 被 ContextEdit(None) 隐去，a2 内容被替换为 "final replacement for a2"
    let u3_proj = proj
        .entries
        .iter()
        .find(|e| e.source_entry.id() == u3)
        .unwrap();
    assert!(u3_proj.messages.is_empty());

    let a2_proj = proj
        .entries
        .iter()
        .find(|e| e.source_entry.id() == a2)
        .unwrap();
    assert_eq!(
        a2_proj.messages[0].as_assistant().unwrap().text(),
        "final replacement for a2"
    );

    let a3_proj = proj
        .entries
        .iter()
        .find(|e| e.source_entry.id() == a3)
        .unwrap();
    assert_eq!(
        a3_proj.messages[0].as_assistant().unwrap().text(),
        "answer 3"
    );
}

#[test]
fn create_branched_session_rechains_parents_and_remaps_compaction_first_kept_across_stripped_labels()
 {
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();

    let u1 = sm
        .append_message(AgentMessage::User(UserMessage::new("user 1")))
        .unwrap();
    // 在 u1 后追加一条 LabelEntry（使后续条目成为该 LabelEntry 的直接子节点）
    let lbl1 = sm
        .append_label_change(&u1, Some("bookmark-1".to_owned()))
        .unwrap();
    let a1 = sm.append_message(assistant_msg("assistant 1")).unwrap();
    assert_eq!(sm.get_entry(&a1).unwrap().parent_id(), Some(lbl1.as_str()));

    // 故意构造一条 first_kept_entry_id 指向 lbl1 的 CompactionEntry
    let comp = sm
        .append_compaction("summary", Some(lbl1.clone()), 500, None, None, None)
        .unwrap();
    let u2 = sm
        .append_message(AgentMessage::User(UserMessage::new("user 2")))
        .unwrap();

    sm.create_branched_session(&u2).unwrap();

    // 验证中间的 lbl1 已被剥离，a1 的 parent_id 已重连到 u1，且 comp.first_kept_entry_id 已重定向到 a1！
    let a1_after = sm.get_entry(&a1).unwrap();
    assert_eq!(a1_after.parent_id(), Some(u1.as_str()));

    let comp_after = match sm.get_entry(&comp).unwrap() {
        SessionEntry::Compaction(c) => c,
        _ => panic!("expected compaction"),
    };
    assert_eq!(comp_after.first_kept_entry_id, a1);

    // 验证 u1 的书签标签在派生后的新会话尾部得到了完整保留
    assert_eq!(sm.get_label(&u1), Some("bookmark-1"));
}

#[test]
fn five_filter_modes_and_geometric_connectors_align_with_upstream_tree_list() {
    let mut sm = SessionManager::in_memory("/proj", None, None).unwrap();

    let u1 = sm
        .append_message(AgentMessage::User(UserMessage::new("root user")))
        .unwrap();
    let a1 = sm.append_message(assistant_msg("root assistant")).unwrap();
    let tr1 = sm
        .append_message(AgentMessage::ToolResult(ToolResultMessage {
            tool_call_id: "tc1".to_owned(),
            tool_name: "read".to_owned(),
            content: vec![UserContentBlock::Text(TextContent::new("file content"))],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 1000,
            duration_ms: Some(5),
        }))
        .unwrap();
    let _custom1 = sm.append_custom_entry("internal", None).unwrap();

    // 分支 A（旧分支）：从 tr1 继续
    let u_branch_a = sm
        .append_message(AgentMessage::User(UserMessage::new("branch A user")))
        .unwrap();
    let a_branch_a = sm
        .append_message(assistant_msg("branch A assistant"))
        .unwrap();
    sm.append_label_change(&a_branch_a, Some("tag-a".to_owned()))
        .unwrap();

    // 分支 B（当前活跃分支）：回跳到 a1 分叉出第二条子树
    sm.branch(&a1).unwrap();
    let u_branch_b = sm
        .append_message(AgentMessage::User(UserMessage::new("branch B active user")))
        .unwrap();
    let a_branch_b = sm
        .append_message(assistant_msg("branch B active assistant"))
        .unwrap();

    let roots = sm.get_tree();
    let leaf_id = sm.get_leaf_id();

    // 1. Default 模式：隐藏 custom 与 label，保留 user/assistant/toolResult
    // 且由于当前叶节点在分支 B，a1 的两个子节点中分支 B 排在分支 A 前面！
    let default_tree = build_filtered_tree(&roots, leaf_id, TreeFilterMode::Default, None, None);
    let default_ids: Vec<&str> = default_tree.iter().map(|n| n.entry.id()).collect();
    assert_eq!(
        default_ids,
        vec![
            u1.as_str(),
            a1.as_str(),
            u_branch_b.as_str(),
            a_branch_b.as_str(),
            tr1.as_str(),
            u_branch_a.as_str(),
            a_branch_a.as_str(),
        ]
    );

    // 检查分叉几何属性：u1 和 a1 位于单链 indent=0；
    // a1 分叉出 u_branch_b (is_last=false) 与 tr1 (is_last=true)，两者 indent=1 且 show_connector=true；
    // 分支 B 的下一代 a_branch_b 作为分叉后第一代后代（justBranched && indent > 0）缩进为 indent=2，且携带 position=0, show=true 的竖线 gutter！
    assert_eq!(default_tree[0].indent, 0);
    assert_eq!(default_tree[1].indent, 0);
    assert_eq!(default_tree[2].indent, 1);
    assert!(default_tree[2].show_connector);
    assert!(!default_tree[2].is_last);
    assert!(default_tree[2].active_path);
    assert_eq!(default_tree[3].indent, 2);
    assert_eq!(default_tree[3].gutters.len(), 1);
    assert_eq!(default_tree[3].gutters[0].position, 0);
    assert!(default_tree[3].gutters[0].show);

    // 2. NoTools 模式：隐藏 tr1，u_branch_a 自动重挂到最近可见祖先 a1！
    let no_tools_tree = build_filtered_tree(&roots, leaf_id, TreeFilterMode::NoTools, None, None);
    assert!(!no_tools_tree.iter().any(|n| n.entry.id() == tr1));
    let u_a_node = no_tools_tree
        .iter()
        .find(|n| n.entry.id() == u_branch_a)
        .unwrap();
    assert_eq!(u_a_node.visible_parent_id.as_deref(), Some(a1.as_str()));
    assert_eq!(u_a_node.indent, 1);
    assert!(u_a_node.show_connector);
    assert!(u_a_node.is_last);

    // 3. UserOnly 模式：仅保留 3 条 user 消息
    let user_only_tree = build_filtered_tree(&roots, leaf_id, TreeFilterMode::UserOnly, None, None);
    let user_ids: Vec<&str> = user_only_tree.iter().map(|n| n.entry.id()).collect();
    assert_eq!(
        user_ids,
        vec![u1.as_str(), u_branch_b.as_str(), u_branch_a.as_str()]
    );

    // 4. LabeledOnly 模式：仅保留带标签的 a_branch_a
    let labeled_tree =
        build_filtered_tree(&roots, leaf_id, TreeFilterMode::LabeledOnly, None, None);
    assert_eq!(labeled_tree.len(), 1);
    assert_eq!(labeled_tree[0].entry.id(), a_branch_a);
    assert_eq!(labeled_tree[0].label.as_deref(), Some("tag-a"));

    // 5. All 模式与节点折叠：折叠 tr1 后，其后代 u_branch_a 与 a_branch_a 被隐藏
    let all_tree = build_filtered_tree(&roots, leaf_id, TreeFilterMode::All, None, None);
    assert_eq!(all_tree.len(), 9);

    let mut folded = BTreeSet::new();
    folded.insert(tr1.clone());
    let folded_tree = build_filtered_tree(
        &roots,
        leaf_id,
        TreeFilterMode::Default,
        None,
        Some(&folded),
    );
    assert!(!folded_tree.iter().any(|n| n.entry.id() == u_branch_a));
    assert!(!folded_tree.iter().any(|n| n.entry.id() == a_branch_a));
}

use std::fs;
use std::path::PathBuf;

use pi_agent::AgentMessage;
use pi_ai::{UserContent, UserMessage};
#[cfg(feature = "test-support")]
use pi_session::ContextEditReplacement;
use pi_session::{
    CURRENT_SESSION_VERSION, FileEntry, MAX_SESSION_HEADER_SCAN_BYTES, NewSessionOptions,
    SessionBoundaryDraft, SessionEntry, SessionHeader, SessionManager, SessionStoreError,
    SqliteJournalMode, SqliteLockingMode, SqlitePragmaConfig, SqliteSynchronousMode,
    TailRepairOutcome, find_most_recent_session, find_session_by_id, list_sessions_from_dir,
    read_session_header,
};
use serde_json::json;

fn test_temp_dir(name: &str) -> PathBuf {
    let base = PathBuf::from("E:/tmp/2026-11-04/argon-phase3-development/tests")
        .join(format!("{name}-{}", pi_ai::uuid_v7()));
    fs::create_dir_all(&base).expect("create test temp dir");
    base
}

#[cfg(feature = "test-support")]
fn sample_assistant(text: &str) -> AgentMessage {
    let mut msg = pi_ai::faux::faux_text_message(text);
    msg.stop_reason = pi_ai::StopReason::Stop;
    AgentMessage::Assistant(msg)
}

#[test]
fn lazy_flush_creates_file_only_on_first_user_or_assistant_message() {
    let dir = test_temp_dir("lazy-flush");
    let mut sm = SessionManager::create("/workspace/proj", &dir, None).unwrap();
    let session_file = sm.get_session_file().unwrap().to_path_buf();

    // 仅追加设置与自定义状态条目：不应在磁盘创建 .jsonl 文件
    sm.append_model_change("deepseek", "deepseek-chat").unwrap();
    sm.append_thinking_level_change("high").unwrap();
    sm.append_custom_entry("ext.state", Some(json!({"step": 1})))
        .unwrap();
    assert!(!sm.is_flushed());
    assert!(!session_file.exists());

    // 追加首条 user 消息：立即以排他模式创建文件并一次性刷入 header + 积压条目 + user 消息
    let uid = sm
        .append_message(AgentMessage::User(UserMessage::new("first question")))
        .unwrap();
    assert!(sm.is_flushed());
    assert!(session_file.exists());
    assert_eq!(sm.get_leaf_id(), Some(uid.as_str()));

    drop(sm);
    let reopened = SessionManager::open(&session_file, Some(dir.clone()), None).unwrap();
    assert_eq!(reopened.get_entry_count(), 4);
    assert_eq!(reopened.get_leaf_id(), Some(uid.as_str()));
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(feature = "test-support")]
#[test]
fn exclusive_create_failure_cleans_up_partial_file_and_allows_retry_until_cleanup_fails() {
    let dir = test_temp_dir("exclusive-create-recovery");
    let mut sm = SessionManager::create("/workspace/proj", &dir, None).unwrap();
    let session_file = sm.get_session_file().unwrap().to_path_buf();

    sm.append_model_change("openai", "gpt-5").unwrap();

    // 1. 注入首次排他创建写半截失败：必须删除新创建的半截文件，保持未落盘积压状态，后续重试可成功
    {
        let fault = sm.fault_injection().unwrap();
        fault.lock().unwrap().fail_exclusive_create_after_open = true;
    }
    let err = sm
        .append_message(AgentMessage::User(UserMessage::new("try 1")))
        .unwrap_err();
    assert!(matches!(err, SessionStoreError::Io(_)));
    assert!(!sm.is_flushed());
    assert!(!sm.is_poisoned());
    assert!(!session_file.exists(), "partial file must be cleaned up");
    assert_eq!(sm.get_entry_count(), 1);

    // 重试追加：成功落盘
    let uid = sm
        .append_message(AgentMessage::User(UserMessage::new("try 2")))
        .unwrap();
    assert!(sm.is_flushed());
    assert!(session_file.exists());
    assert_eq!(sm.get_entry_count(), 2);
    assert_eq!(sm.get_leaf_id(), Some(uid.as_str()));

    // 2. 若首次创建失败且清理文件也失败：必须返回 StorageUncertain 并将实例标记为 poisoned
    let dir2 = test_temp_dir("exclusive-create-poison");
    let mut sm2 = SessionManager::create("/workspace/proj", &dir2, None).unwrap();
    {
        let fault2 = sm2.fault_injection().unwrap();
        let mut g = fault2.lock().unwrap();
        g.fail_exclusive_create_after_open = true;
        g.fail_exclusive_create_cleanup = true;
    }
    let err2 = sm2
        .append_message(AgentMessage::User(UserMessage::new("boom")))
        .unwrap_err();
    assert!(matches!(err2, SessionStoreError::StorageUncertain(_)));
    assert!(sm2.is_poisoned());
    let subsequent = sm2
        .append_message(AgentMessage::User(UserMessage::new("after poison")))
        .unwrap_err();
    assert!(matches!(subsequent, SessionStoreError::Poisoned(_)));

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&dir2);
}

#[cfg(feature = "test-support")]
#[test]
fn partial_append_failure_restores_file_tail_and_poisons_when_truncate_fails() {
    let dir = test_temp_dir("partial-append");
    let mut sm = SessionManager::create("/workspace/proj", &dir, None).unwrap();
    let session_file = sm.get_session_file().unwrap().to_path_buf();

    let u1 = sm
        .append_message(AgentMessage::User(UserMessage::new("hello")))
        .unwrap();
    let orig_disk_bytes = fs::read(&session_file).unwrap();

    // 1. 注入写入 12 字节后失败：文件尾部必须截断恢复到 orig_disk_bytes，内存 leaf_id 保持为 u1
    {
        let fault = sm.fault_injection().unwrap();
        fault.lock().unwrap().fail_append_after_bytes = Some(12);
    }
    let err = sm
        .append_message(sample_assistant("partial reply"))
        .unwrap_err();
    assert!(matches!(err, SessionStoreError::Io(_)));
    assert!(!sm.is_poisoned());
    assert_eq!(sm.get_leaf_id(), Some(u1.as_str()));
    assert_eq!(sm.get_entry_count(), 1);
    assert_eq!(fs::read(&session_file).unwrap(), orig_disk_bytes);

    // 正常追加下一条消息：其 parent_id 必须是 u1
    let a1 = sm.append_message(sample_assistant("clean reply")).unwrap();
    let a1_entry = sm.get_entry(&a1).unwrap();
    assert_eq!(a1_entry.parent_id(), Some(u1.as_str()));

    // 2. 注入部分写入失败且恢复截断也失败：返回 StorageUncertain 并标记 poisoned，拒绝一切后续写入
    {
        let fault = sm.fault_injection().unwrap();
        let mut g = fault.lock().unwrap();
        g.fail_append_after_bytes = Some(10);
        g.fail_append_recovery = true;
    }
    let uncertain_err = sm
        .append_custom_entry("audit", Some(json!({"x": 1})))
        .unwrap_err();
    assert!(matches!(
        uncertain_err,
        SessionStoreError::StorageUncertain(_)
    ));
    assert!(sm.is_poisoned());
    assert!(matches!(
        sm.append_custom_entry("audit2", None).unwrap_err(),
        SessionStoreError::Poisoned(_)
    ));
    assert!(matches!(
        sm.commit_boundary_drafts(&[SessionBoundaryDraft::Custom {
            custom_type: "draft".to_owned(),
            data: None
        }])
        .unwrap_err(),
        SessionStoreError::Poisoned(_)
    ));

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(feature = "test-support")]
#[test]
fn commit_boundary_drafts_is_all_or_nothing_across_failures_and_reopen() {
    let dir = test_temp_dir("boundary-drafts-atomic");
    let mut sm = SessionManager::create("/workspace/proj", &dir, None).unwrap();
    let session_file = sm.get_session_file().unwrap().to_path_buf();

    let u1 = sm
        .append_message(AgentMessage::User(UserMessage::new("hello")))
        .unwrap();
    let a1 = sm
        .append_message(sample_assistant("original assistant text"))
        .unwrap();
    let snapshot_before = fs::read_to_string(&session_file).unwrap();

    // 1. 注入重命名前失败：磁盘文件与内存树均保持原状（全无）
    {
        let fault = sm.fault_injection().unwrap();
        fault.lock().unwrap().fail_rewrite_before_rename = true;
    }
    let drafts = vec![
        SessionBoundaryDraft::Custom {
            custom_type: "turn.meta".to_owned(),
            data: Some(json!({"ok": true})),
        },
        SessionBoundaryDraft::ContextEdit {
            target_id: a1.clone(),
            replacement: Some(ContextEditReplacement::from_text("edited assistant text")),
        },
    ];
    let err = sm.commit_boundary_drafts(&drafts).unwrap_err();
    assert!(matches!(err, SessionStoreError::Io(_)));
    assert_eq!(sm.get_entry_count(), 2);
    assert_eq!(sm.get_leaf_id(), Some(a1.as_str()));
    assert_eq!(fs::read_to_string(&session_file).unwrap(), snapshot_before);

    // 2. 正常提交：内存与磁盘原子切换到完整新快照（全有）
    let committed = sm.commit_boundary_drafts(&drafts).unwrap();
    assert_eq!(committed.len(), 2);
    assert_eq!(sm.get_entry_count(), 4);
    assert_eq!(sm.get_leaf_id(), Some(committed.last().unwrap().id()));

    drop(sm);
    let reopened = SessionManager::open(&session_file, Some(dir.clone()), None).unwrap();
    assert_eq!(reopened.get_entry_count(), 4);
    let proj = reopened.build_session_projection();
    let asst_msg = proj.messages.iter().find_map(|m| m.as_assistant()).unwrap();
    assert_eq!(asst_msg.text(), "edited assistant text");
    let _ = u1;
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn single_writer_lock_blocks_concurrent_instance_even_after_atomic_rewrite() {
    let dir = test_temp_dir("single-writer");
    let mut sm1 = SessionManager::create("/workspace/proj", &dir, None).unwrap();
    let session_file = sm1.get_session_file().unwrap().to_path_buf();
    sm1.append_message(AgentMessage::User(UserMessage::new("hi")))
        .unwrap();

    // 触发一次 commit_boundary_drafts 原子重命名替换 JSONL 文件，验证独立 .lock 文件锁依然持有
    sm1.commit_boundary_drafts(&[SessionBoundaryDraft::Custom {
        custom_type: "keep-lock".to_owned(),
        data: None,
    }])
    .unwrap();

    // 第二个写入实例尝试打开同一会话文件：必须被拒绝
    let second_res = SessionManager::open(&session_file, Some(dir.clone()), None);
    assert!(matches!(
        second_res.unwrap_err(),
        SessionStoreError::SingleWriterLocked(_)
    ));

    // 释放第一个实例后，第二个实例可正常打开
    drop(sm1);
    let sm2 = SessionManager::open(&session_file, Some(dir.clone()), None).unwrap();
    assert_eq!(sm2.get_entry_count(), 2);
    drop(sm2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tail_repair_handles_missing_newline_incomplete_tail_intermediate_corrupt_and_invalid_header() {
    let dir = test_temp_dir("tail-repair");

    // 1. 末行有效 JSON 但缺少换行符 -> 自动补写 `\n`
    let file_no_nl = dir.join("no_newline.jsonl");
    let header_line = serde_json::to_string(&FileEntry::Session(SessionHeader {
        version: Some(CURRENT_SESSION_VERSION),
        id: "sess-1".to_owned(),
        timestamp: "2026-11-04T00:00:00.000Z".to_owned(),
        cwd: "/proj".to_owned(),
        parent_session: None,
    }))
    .unwrap();
    let msg_line = serde_json::to_string(&SessionEntry::Custom(pi_session::CustomEntry {
        id: "abcd1234".to_owned(),
        parent_id: None,
        timestamp: "2026-11-04T00:00:01.000Z".to_owned(),
        custom_type: "test".to_owned(),
        data: Some(json!({"v": 1})),
    }))
    .unwrap();
    fs::write(&file_no_nl, format!("{header_line}\n{msg_line}")).unwrap();

    let mut sm1 = SessionManager::open(&file_no_nl, Some(dir.clone()), None).unwrap();
    assert_eq!(
        sm1.last_tail_repair(),
        Some(&TailRepairOutcome::AppendedMissingNewline)
    );
    assert!(fs::read_to_string(&file_no_nl).unwrap().ends_with('\n'));
    sm1.append_custom_entry("test2", None).unwrap();
    drop(sm1);

    // 2. 末行留有半截损坏 JSON，且中间有一行损坏 JSON -> 跳过中间损坏行，截断末行半截 JSON
    let file_corrupt_tail = dir.join("corrupt_tail.jsonl");
    let valid_line_2 = serde_json::to_string(&SessionEntry::Custom(pi_session::CustomEntry {
        id: "ef012345".to_owned(),
        parent_id: Some("abcd1234".to_owned()),
        timestamp: "2026-11-04T00:00:02.000Z".to_owned(),
        custom_type: "test2".to_owned(),
        data: None,
    }))
    .unwrap();
    let raw_with_corrupt = format!(
        "{header_line}\n{msg_line}\n{{corrupt_middle_line\n{valid_line_2}\n{{\"type\":\"custom\",\"id\":\"half"
    );
    fs::write(&file_corrupt_tail, &raw_with_corrupt).unwrap();

    let sm2 = SessionManager::open(&file_corrupt_tail, Some(dir.clone()), None).unwrap();
    assert_eq!(
        sm2.last_tail_repair(),
        Some(&TailRepairOutcome::TruncatedIncompleteTail {
            truncated_bytes: b"{\"type\":\"custom\",\"id\":\"half".len() as u64
        })
    );
    assert_eq!(sm2.get_entry_count(), 2);
    assert!(
        fs::read_to_string(&file_corrupt_tail)
            .unwrap()
            .ends_with(&format!("{valid_line_2}\n"))
    );
    drop(sm2);

    // 3. 非空非法头部文件保护：严禁修改或覆盖原文件
    let invalid_file = dir.join("not_a_session.jsonl");
    let original_garbage = "{\"not\":\"a session\"}\n";
    fs::write(&invalid_file, original_garbage).unwrap();
    let err = SessionManager::open(&invalid_file, Some(dir.clone()), None).unwrap_err();
    assert!(matches!(err, SessionStoreError::InvalidSessionFile(_)));
    assert_eq!(fs::read_to_string(&invalid_file).unwrap(), original_garbage);

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn v1_to_v3_migration_and_bounded_header_scan_and_concurrent_list_work() {
    let dir = test_temp_dir("migration-and-list");

    // 1. 构造 v1 格式会话文件（无 version、无 id/parentId、compaction 使用 firstKeptEntryIndex、v2 hookMessage）
    let v1_path = dir.join("legacy_v1.jsonl");
    let v1_content = [
        r#"{"type":"session","id":"legacy-session-1","timestamp":"2026-11-04T00:00:00.000Z","cwd":"/legacy"}"#,
        r#"{"type":"message","timestamp":"2026-11-04T00:00:01.000Z","message":{"role":"user","content":"hello v1","timestamp":1000}}"#,
        r#"{"type":"message","timestamp":"2026-11-04T00:00:02.000Z","message":{"role":"hookMessage","customType":"my-hook","content":"hook text","display":true,"timestamp":2000}}"#,
        r#"{"type":"compaction","timestamp":"2026-11-04T00:00:03.000Z","summary":"v1 summary","firstKeptEntryIndex":1,"tokensBefore":500}"#,
    ]
    .join("\n")
        + "\n";
    fs::write(&v1_path, v1_content).unwrap();

    let sm = SessionManager::open(&v1_path, Some(dir.clone()), None).unwrap();
    assert_eq!(
        sm.get_header().unwrap().version,
        Some(CURRENT_SESSION_VERSION)
    );
    let entries = sm.get_entries();
    assert_eq!(entries.len(), 3);
    assert!(!entries[0].id().is_empty());
    assert_eq!(entries[1].parent_id(), Some(entries[0].id()));
    // hookMessage 已升级为 custom 角色
    if let SessionEntry::Message(m) = &entries[1] {
        assert_eq!(m.message.role(), "custom");
    } else {
        panic!("expected message entry");
    }
    // CompactionEntry 的 firstKeptEntryIndex(1) 已转换为 entries[0].id()
    if let SessionEntry::Compaction(c) = &entries[2] {
        assert_eq!(c.first_kept_entry_id, entries[0].id());
    } else {
        panic!("expected compaction entry");
    }
    drop(sm);

    // 2. 1 MiB 有界头部扫描：超过 1 MiB 无换行符的单行文件返回 HeaderScanLimitExceeded，列表发现忽略它
    let oversized_path = dir.join("oversized_line.jsonl");
    let huge_line = "x".repeat(MAX_SESSION_HEADER_SCAN_BYTES + 1024);
    fs::write(&oversized_path, huge_line).unwrap();
    let scan_err = read_session_header(&oversized_path).unwrap_err();
    assert!(matches!(
        scan_err,
        SessionStoreError::HeaderScanLimitExceeded(_)
    ));

    // 3. 创建第二个更新的会话并验证 find_session_by_id、find_most_recent_session 与 list_sessions_from_dir
    let mut sm_new = SessionManager::create(
        "/legacy",
        &dir,
        Some(NewSessionOptions {
            id: Some("session-newer-2".to_owned()),
            parent_session: None,
        }),
    )
    .unwrap();
    sm_new
        .append_message(AgentMessage::User(UserMessage {
            content: UserContent::Text("newest prompt".to_owned()),
            timestamp: 9_999_999,
        }))
        .unwrap();
    let newer_path = sm_new.get_session_file().unwrap().to_path_buf();
    drop(sm_new);

    assert_eq!(
        find_session_by_id(&dir, "legacy-session-1", None),
        Some(v1_path.clone())
    );
    assert_eq!(
        find_most_recent_session(&dir, Some("/legacy")),
        Some(newer_path)
    );

    let listed = list_sessions_from_dir(&dir, Some("/legacy"), None).await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "session-newer-2");
    assert_eq!(listed[0].first_message, "newest prompt");
    assert_eq!(listed[1].id, "legacy-session-1");

    // 4. 验证 SQLite U 盘（FAT32/exFAT/可移动驱动器）与固定 NTFS 磁盘的 PRAGMA 策略
    let usb_cfg = SqlitePragmaConfig::for_volume("exFAT", true);
    assert_eq!(usb_cfg.journal_mode, SqliteJournalMode::Truncate);
    assert_eq!(usb_cfg.synchronous, SqliteSynchronousMode::Full);
    assert_eq!(usb_cfg.locking_mode, SqliteLockingMode::Exclusive);

    let ntfs_cfg = SqlitePragmaConfig::for_volume("NTFS", false);
    assert_eq!(ntfs_cfg.journal_mode, SqliteJournalMode::Wal);
    assert_eq!(ntfs_cfg.synchronous, SqliteSynchronousMode::Normal);
    assert_eq!(ntfs_cfg.locking_mode, SqliteLockingMode::Normal);

    let _ = fs::remove_dir_all(&dir);
}

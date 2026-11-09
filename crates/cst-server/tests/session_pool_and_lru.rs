use std::fs;
use std::path::PathBuf;

use cst_pilot::auth::{AuthRuntime, Channel, oa::OaClient};
use cst_pilot::telemetry::TelemetryOptions;
use cst_pilot::update::service::UpdateOptions;
use cst_pilot::{Kit, KitHost, SKILL_NAMES};
use cst_server::discovery::SessionOwnerLock;
use cst_server::mode::CallerIdentity;
use cst_server::session::SessionPool;
use pi_agent::AgentMessage;
use pi_ai::{ModelThinkingLevel, UserContent, UserMessage, now_ms};
use pi_session::SessionEntry;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct KitFixture {
    root: PathBuf,
    workspace_b: PathBuf,
}

impl KitFixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("cst-server-pool-{}", Uuid::new_v4()));
        let root = base.join("kit");
        let workspace_b = base.join("workspace-b");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&workspace_b).unwrap();
        let root = root.canonicalize().unwrap();
        let workspace_b = workspace_b.canonicalize().unwrap();

        for name in SKILL_NAMES {
            let skill_file = root.join(format!("agent/home/skills/{name}/SKILL.md"));
            fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
            fs::write(
                skill_file,
                format!("---\nname: {name}\ndescription: {name} 工具指南\n---\n维护指南。"),
            )
            .unwrap();
        }
        fs::write(
            root.join("agent/home/APPEND_SYSTEM.md"),
            "CST Pilot 固定诊断规则。",
        )
        .unwrap();

        Self { root, workspace_b }
    }

    async fn build_host(&self) -> KitHost {
        let kit = Kit::new(&self.root).unwrap();
        let oa = OaClient::new("https://oa.example").unwrap();
        let auth = AuthRuntime::create(&self.root, Channel::Gui, oa)
            .await
            .unwrap();
        KitHost::new(
            kit,
            auth,
            TelemetryOptions::default(),
            UpdateOptions::Disabled,
        )
        .await
        .unwrap()
    }
}

impl Drop for KitFixture {
    fn drop(&mut self) {
        if let Some(parent) = self.root.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

#[tokio::test]
async fn retained_session_handles_and_clones_prevent_lru_eviction_until_release() {
    let fixture = KitFixture::new();
    let pool = SessionPool::new(fixture.build_host().await, &fixture.root);
    let created = pool.create_with_key("retained", None, None).await.unwrap();
    let id = created.id.clone();
    drop(created);
    let source = pool.open_saved(&id).await.unwrap();
    let retained = source.clone();
    drop(source);

    for i in 0..11 {
        pool.create_with_key(&format!("retained-fill-{i}"), None, None)
            .await
            .unwrap();
    }
    let loaded = pool
        .get_loaded(&id)
        .await
        .expect("held session stays loaded");
    assert!(std::ptr::eq(&*loaded, &*retained));
    let reopened = pool.open_saved(&id).await.unwrap();
    assert!(std::ptr::eq(&*reopened, &*retained));
    drop(loaded);
    drop(reopened);
    drop(retained);

    for i in 0..11 {
        pool.create_with_key(&format!("released-fill-{i}"), None, None)
            .await
            .unwrap();
    }
    pool.evict_excess_idle_slots().await;
    assert!(pool.get_loaded(&id).await.is_none());
    let restored = pool.open_saved(&id).await.unwrap();
    assert_eq!(restored.id, id);
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn final_handle_release_trims_idle_sessions_without_another_open() {
    let fixture = KitFixture::new();
    let pool = SessionPool::with_max_idle(fixture.build_host().await, &fixture.root, 1);
    let first = pool
        .create_with_key("release-first", None, None)
        .await
        .unwrap();
    let second = pool
        .create_with_key("release-second", None, None)
        .await
        .unwrap();
    let id = first.id.clone();
    assert_eq!(pool.active_slot_count().await, 2);
    // Handles may be dropped outside a Tokio runtime.
    std::thread::spawn(move || drop((first, second)))
        .join()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while pool.active_slot_count().await != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("last release schedules LRU cleanup");
    assert!(pool.get_loaded(&id).await.is_none());
    pool.open_saved(&id).await.unwrap();
    pool.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thinking_changes_wait_for_pool_contention_without_reporting_not_found() {
    let fixture = KitFixture::new();
    let pool = std::sync::Arc::new(SessionPool::new(fixture.build_host().await, &fixture.root));
    let source = pool
        .create_with_key("thinking-source", None, None)
        .await
        .unwrap();
    let level = source.session().thinking_level();
    for i in 0..9 {
        pool.create_with_key(&format!("thinking-fill-{i}"), None, None)
            .await
            .unwrap();
    }
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let reader_pool = pool.clone();
    let reader_barrier = barrier.clone();
    let reader = tokio::spawn(async move {
        reader_barrier.wait().await;
        for _ in 0..1000 {
            reader_pool.evict_excess_idle_slots().await;
            tokio::task::yield_now().await;
        }
    });
    barrier.wait().await;
    let mut failures = Vec::new();
    for _ in 0..1000 {
        if let Err(error) = pool.set_session_thinking_level(&source.id, level).await {
            failures.push(error);
        }
        tokio::task::yield_now().await;
    }
    reader.await.unwrap();
    assert!(
        failures.is_empty(),
        "loaded session must remain writable: {failures:?}"
    );
    assert!(pool.get_loaded(&source.id).await.is_some());
    pool.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_eviction_and_reopening_release_owner_locks_before_cold_load() {
    let fixture = KitFixture::new();
    let pool = std::sync::Arc::new(SessionPool::with_max_idle(
        fixture.build_host().await,
        &fixture.root,
        1,
    ));
    let mut ids = Vec::new();
    for i in 0..4 {
        ids.push(
            pool.create_with_key(&format!("churn-{i}"), None, None)
                .await
                .unwrap()
                .id
                .clone(),
        );
    }
    let mut tasks = Vec::new();
    for offset in 0..4 {
        let pool = pool.clone();
        let ids = ids.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..20 {
                let id = &ids[(i + offset) % ids.len()];
                let slot = pool.open_saved(id).await.unwrap();
                assert_eq!(&slot.id, id);
                tokio::task::yield_now().await;
                assert!(slot.is_pinned().await);
                drop(slot);
                pool.evict_excess_idle_slots().await;
            }
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        for task in tasks {
            task.await.unwrap();
        }
    })
    .await
    .expect("concurrent LRU operations complete");
    pool.evict_excess_idle_slots().await;
    assert_eq!(pool.active_slot_count().await, 1);
    pool.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_cold_opens_share_one_slot_for_saved_and_unflushed_sessions() {
    for flushed in [false, true] {
        let fixture = KitFixture::new();
        let pool = std::sync::Arc::new(SessionPool::new(fixture.build_host().await, &fixture.root));
        let source = pool
            .create_with_key("concurrent-source", None, None)
            .await
            .unwrap();
        let id = source.id.clone();
        if flushed {
            source
                .session()
                .with_session_manager_mut(|manager| {
                    manager.append_message(AgentMessage::User(pi_ai::UserMessage {
                        content: pi_ai::UserContent::Text("已保存的消息".to_owned()),
                        timestamp: now_ms(),
                    }))
                })
                .unwrap();
        }
        drop(source);
        for i in 0..10 {
            pool.create_with_key(&format!("fill-{i}"), None, None)
                .await
                .unwrap();
        }
        pool.evict_excess_idle_slots().await;
        assert!(pool.get_loaded(&id).await.is_none());
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let pool = pool.clone();
            let id = id.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                pool.open_saved(&id).await
            }));
        }
        let mut slots = Vec::new();
        for task in tasks {
            slots.push(
                task.await
                    .unwrap()
                    .unwrap_or_else(|error| panic!("flushed={flushed}: {error}")),
            );
        }
        assert!(slots.iter().all(|slot| std::ptr::eq(&**slot, &*slots[0])));
        drop(slots);
        pool.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn unflushed_empty_session_preserves_metadata_across_10_session_lru_eviction_and_restart() {
    let fixture = KitFixture::new();
    let host = fixture.build_host().await;
    let pool = SessionPool::new(host.clone(), &fixture.root);

    // 1. Create an empty session (unflushed), rename it, change its model and thinking level.
    let target_slot = pool
        .create_with_key(
            "req-target",
            Some(("openai".to_owned(), "gpt-4o".to_owned())),
            Some(ModelThinkingLevel::High),
        )
        .await
        .unwrap();
    let target_id = target_slot.id.clone();
    pool.rename_session(&target_slot, "未落盘自定义标题")
        .unwrap();
    assert!(!target_slot.is_flushed());

    // Also create a pinned session with an active SSE subscriber so it must NOT be evicted.
    let pinned_slot = pool
        .create_with_key("req-pinned", None, None)
        .await
        .unwrap();
    let pinned_id = pinned_slot.id.clone();
    let _sse_sub = pinned_slot.events.subscribe_single(
        CallerIdentity::App,
        None,
        CancellationToken::new(),
        None,
    );
    assert!(pinned_slot.is_pinned().await);

    drop(target_slot);
    drop(pinned_slot);

    // 2. Open 10 more idle sessions -> total idle sessions becomes 11 (target + 10 new),
    // which must evict `target_id` (the oldest idle session) while keeping `pinned_id` and the 10 newest idle sessions.
    for i in 0..10 {
        let key = format!("req-extra-{i}");
        pool.create_with_key(&key, None, None).await.unwrap();
    }

    // Wait for release-triggered cleanup before inspecting the idle limit.
    pool.evict_excess_idle_slots().await;
    // Active slots should be 10 idle + 1 pinned = 11.
    assert_eq!(pool.active_slot_count().await, 11);
    assert!(pool.get_loaded(&target_id).await.is_none());
    assert!(pool.get_loaded(&pinned_id).await.is_some());

    // 3. Reopen `target_id` after eviction: must restore title, model, thinking level, and cwd from `unflushed/<id>.json`
    let restored = pool.open_saved(&target_id).await.unwrap();
    assert_eq!(
        restored.session().session_name().as_deref(),
        Some("未落盘自定义标题")
    );
    assert_eq!(restored.session().model().provider, "openai");
    assert_eq!(restored.session().model().id, "gpt-4o");

    // 4. Simulate server restart in a different working directory (`workspace_b`):
    // `list()` and `open_saved()` must still find `target_id` with its original `cwd` and metadata.
    drop(restored);
    drop(_sse_sub);
    pool.shutdown().await.unwrap();

    let host2 = fixture.build_host().await;
    let pool2 = SessionPool::new(host2, &fixture.workspace_b);
    let rows = pool2.list().await.unwrap();
    let row = rows
        .iter()
        .find(|r| r.id == target_id)
        .expect("listed after restart");
    assert_eq!(row.title, "未落盘自定义标题");

    let reopened = pool2.open_saved(&target_id).await.unwrap();
    assert_eq!(
        reopened.session().session_name().as_deref(),
        Some("未落盘自定义标题")
    );
    assert_eq!(reopened.session().model().provider, "openai");
    assert_eq!(reopened.session().model().id, "gpt-4o");
    assert_eq!(
        PathBuf::from(
            reopened
                .session()
                .with_session_manager(|sm| sm.get_cwd().to_owned())
        )
        .canonicalize()
        .unwrap(),
        fixture.root
    );
    pool2.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_before_first_user_message_with_metadata_parent_keeps_source_lock_and_persists_unflushed_snapshot()
 {
    let fixture = KitFixture::new();
    let host = fixture.build_host().await;
    let pool = SessionPool::new(host, &fixture.root);

    // 1. Create source session, set custom name, model, and thinking level (appends metadata entries before first user message).
    let source = pool
        .create_with_key(
            "req-source",
            Some(("openai".to_owned(), "gpt-4o".to_owned())),
            Some(ModelThinkingLevel::Medium),
        )
        .await
        .unwrap();
    let source_id = source.id.clone();
    pool.rename_session(&source, "源故障排查会话").unwrap();

    // Append first user message so the source session flushes `.jsonl` to disk,
    // and verify that the first user message's `parent_id` is `Some(...)` (pointing to metadata entry).
    let user_entry_id = source
        .session()
        .with_session_manager_mut(|sm| {
            sm.append_message(AgentMessage::User(UserMessage {
                content: UserContent::Text("为什么电脑蓝屏".to_owned()),
                timestamp: now_ms(),
            }))
        })
        .unwrap();
    assert!(source.is_flushed());

    let user_parent_id = source.session().with_session_manager(|sm| {
        let entry = sm.get_entry(&user_entry_id).unwrap();
        assert!(matches!(entry, SessionEntry::Message(m) if m.message.role() == "user"));
        entry.parent_id().map(str::to_owned)
    });
    assert!(
        user_parent_id.is_some(),
        "first user message after model/thinking setup has a metadata parent_id"
    );

    // 2. Fork from the first user message's `before` position while `source` is open and holding its single-writer lock.
    let forked = pool.fork(&source_id, &user_entry_id).await.unwrap();
    let forked_id = forked.id.clone();
    assert_ne!(forked_id, source_id);

    // Source session still holds its cross-process session owner lock without interruption.
    assert!(SessionOwnerLock::acquire(pool.agent_dir(), &source_id).is_err());
    assert!(SessionOwnerLock::acquire(pool.agent_dir(), &forked_id).is_err());

    // Forked session branched before the first user message, so it has 0 conversation messages and is unflushed on disk,
    // but `unflushed/<forked_id>.json` was atomically written!
    assert!(!forked.is_flushed());
    assert!(forked.session().state().messages.is_empty());
    let unflushed_snap = pool
        .read_unflushed_sync(&forked_id)
        .expect("unflushed snapshot written for pre-first-message fork");
    assert_eq!(unflushed_snap.name.as_deref(), Some("源故障排查会话"));
    assert_eq!(
        unflushed_snap.model.as_ref().map(|m| m.provider.as_str()),
        Some("openai")
    );
    assert_eq!(
        unflushed_snap.model.as_ref().map(|m| m.model_id.as_str()),
        Some("gpt-4o")
    );
    assert!(unflushed_snap.parent_session.is_some());

    // 3. Evict `forked` via LRU (open 10 more idle sessions) and reopen it: must recover losslessly!
    drop(source);
    drop(forked);
    for i in 0..10 {
        pool.create_with_key(&format!("lru-fill-{i}"), None, None)
            .await
            .unwrap();
    }
    pool.evict_excess_idle_slots().await;
    assert!(pool.get_loaded(&forked_id).await.is_none());

    let reopened_fork = pool.open_saved(&forked_id).await.unwrap();
    assert_eq!(
        reopened_fork.session().session_name().as_deref(),
        Some("源故障排查会话")
    );
    assert_eq!(reopened_fork.session().model().provider, "openai");
    assert_eq!(reopened_fork.session().model().id, "gpt-4o");
    let header_parent = reopened_fork
        .session()
        .with_session_manager(|sm| sm.get_header().and_then(|h| h.parent_session.clone()));
    assert!(header_parent.is_some());

    pool.shutdown().await.unwrap();
}

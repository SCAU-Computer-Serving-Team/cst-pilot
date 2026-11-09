use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use cst_server::session::attachments::{AttachmentError, ImageRef, ImageStore};
use cst_server::session::inbox::{
    Delivery, InboxError, InboxExecutor, InboxSnapshot, ItemDelivery, ItemStatus, QueueEdit,
    SessionInbox,
};
use cst_server::session::receipts::{
    MutationReceipts, ReceiptError, ReceiptStatus, compute_request_signature,
};
use pi_agent::BoxFuture;
use serde_json::json;
use tokio::sync::Notify;
use uuid::Uuid;

struct MockInboxExecutor {
    busy: AtomicBool,
    hold_prompt: Notify,
    prompt_started: Notify,
    events: Mutex<Vec<String>>,
    accepted_ids: Mutex<Vec<String>>,
    fail_before_accept: AtomicBool,
}

impl Default for MockInboxExecutor {
    fn default() -> Self {
        Self {
            busy: AtomicBool::new(false),
            hold_prompt: Notify::new(),
            prompt_started: Notify::new(),
            events: Mutex::new(Vec::new()),
            accepted_ids: Mutex::new(Vec::new()),
            fail_before_accept: AtomicBool::new(false),
        }
    }
}

impl InboxExecutor for MockInboxExecutor {
    fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn prompt(
        &self,
        text: String,
        _images: Vec<ImageRef>,
        _skill: Option<String>,
        message_id: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        let fail = self.fail_before_accept.load(Ordering::SeqCst);
        let events_ptr = &self.events as *const Mutex<Vec<String>> as usize;
        let accepted_ptr = &self.accepted_ids as *const Mutex<Vec<String>> as usize;
        let busy_ptr = &self.busy as *const AtomicBool as usize;
        let started_ptr = &self.prompt_started as *const Notify as usize;
        let hold_ptr = &self.hold_prompt as *const Notify as usize;

        Box::pin(async move {
            // Simulate async attachment preparation before session enters busy state
            tokio::time::sleep(Duration::from_millis(20)).await;
            if fail {
                return Err("模拟发送前失败".to_owned());
            }
            unsafe {
                (*(busy_ptr as *const AtomicBool)).store(true, Ordering::SeqCst);
                (*(accepted_ptr as *const Mutex<Vec<String>>))
                    .lock()
                    .unwrap()
                    .push(message_id.clone());
                (*(events_ptr as *const Mutex<Vec<String>>))
                    .lock()
                    .unwrap()
                    .push(format!("prompt:{message_id}:{text}"));
                (*(started_ptr as *const Notify)).notify_waiters();
                (*(hold_ptr as *const Notify)).notified().await;
                (*(busy_ptr as *const AtomicBool)).store(false, Ordering::SeqCst);
            }
            Ok(())
        })
    }

    fn steer(
        &self,
        text: String,
        _images: Vec<ImageRef>,
        _skill: Option<String>,
        message_id: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        self.events
            .lock()
            .unwrap()
            .push(format!("steer:{message_id}:{text}"));
        self.accepted_ids.lock().unwrap().push(message_id);
        Box::pin(async { Ok(()) })
    }

    fn was_accepted(&self, id: &str) -> bool {
        self.accepted_ids.lock().unwrap().iter().any(|x| x == id)
    }
}

#[tokio::test]
async fn inbox_direct_queue_steer_ordering_and_versioned_mutations() {
    let dir = std::env::temp_dir().join(format!("cst-server-inbox-{}", Uuid::new_v4()));
    let exec = Arc::new(MockInboxExecutor::default());
    let inbox = SessionInbox::new(&dir, "sess-1", exec.clone(), None).unwrap();

    // 1. First idle queue submission becomes `direct` and starts `prompting`
    let first = inbox
        .accept("msg-1", "first prompt", Delivery::Queue, vec![], None)
        .await
        .unwrap();
    assert_eq!(first.delivery, ItemDelivery::Direct);

    // 2. Immediately submit a `steer` and two `queue` items while `msg-1` is preparing/running
    let steer_item = inbox
        .accept("msg-steer", "urgent steer", Delivery::Steer, vec![], None)
        .await
        .unwrap();
    assert_eq!(steer_item.delivery, ItemDelivery::Steer);

    let q2 = inbox
        .accept("msg-2", "second queue", Delivery::Queue, vec![], None)
        .await
        .unwrap();
    let q3 = inbox
        .accept("msg-3", "third queue", Delivery::Queue, vec![], None)
        .await
        .unwrap();
    assert_eq!(q2.delivery, ItemDelivery::Queue);
    assert_eq!(q3.delivery, ItemDelivery::Queue);

    // Wait for `msg-1` to enter busy and `msg-steer` to be delivered after `msg-1`
    exec.prompt_started.notified().await;
    inbox.wake();
    tokio::time::sleep(Duration::from_millis(30)).await;

    {
        let events = exec.events.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                "prompt:msg-1:first prompt".to_owned(),
                "steer:msg-steer:urgent steer".to_owned(),
            ],
            "steer must not overtake initial prompt during attachment preparation"
        );
    }

    // 3. Reorder pending queue items (`[msg-3, msg-2]`) and edit `msg-3` while `msg-1` is still running
    let snap = inbox.snapshot().await.unwrap();
    let reordered = inbox
        .reorder(&["msg-3".to_owned(), "msg-2".to_owned()], snap.version)
        .await
        .unwrap();
    let edited = inbox
        .change(
            "msg-3",
            reordered.version,
            QueueEdit {
                text: Some("third queue edited".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Stale version must be rejected with Conflict
    assert!(matches!(
        inbox
            .change(
                "msg-2",
                snap.version,
                QueueEdit {
                    remove: true,
                    ..Default::default()
                }
            )
            .await,
        Err(InboxError::Conflict(_))
    ));

    // Cancel `msg-2` with the current version
    inbox
        .change(
            "msg-2",
            edited.version,
            QueueEdit {
                remove: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Release `msg-1` and `msg-3`
    exec.hold_prompt.notify_waiters();
    exec.prompt_started.notified().await;
    exec.hold_prompt.notify_waiters();
    inbox.close().await;

    let final_events = exec.events.lock().unwrap().clone();
    assert_eq!(
        final_events,
        vec![
            "prompt:msg-1:first prompt".to_owned(),
            "steer:msg-steer:urgent steer".to_owned(),
            "prompt:msg-3:third queue edited".to_owned(),
        ]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn crashed_delivering_inbox_item_is_marked_uncertain_on_restart_and_never_replayed() {
    let dir = std::env::temp_dir().join(format!("cst-server-uncertain-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();

    // Simulate a leftover inbox file where `msg-crash` was in `delivering` state when the process died
    let crashed = json!({
        "version": 2,
        "nextSequence": 2,
        "paused": false,
        "items": [{
            "id": "msg-crash",
            "sequence": 1,
            "text": "did this run?",
            "delivery": "direct",
            "acceptedText": "did this run?",
            "acceptedDelivery": "queue",
            "images": [],
            "status": "delivering",
            "acceptedAt": 1700000000000u64
        }]
    });
    fs::write(
        dir.join("sess-crash.json"),
        serde_json::to_vec(&crashed).unwrap(),
    )
    .unwrap();

    let exec = Arc::new(MockInboxExecutor::default());
    let inbox = SessionInbox::new(&dir, "sess-crash", exec.clone(), None).unwrap();
    let snap: InboxSnapshot = inbox.snapshot().await.unwrap();
    assert_eq!(snap.items.len(), 1);
    assert_eq!(snap.items[0].status, ItemStatus::Delivering);
    assert_eq!(snap.items[0].uncertain, Some(true));

    // Even if `wake()` is called, the uncertain `delivering` item is never replayed
    inbox.wake();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(exec.events.lock().unwrap().is_empty());

    inbox.close().await;
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mutation_receipts_deduplicate_and_reject_mismatched_or_crashed_started_keys() {
    let dir = std::env::temp_dir().join(format!("cst-server-receipts-{}", Uuid::new_v4()));
    let receipts = MutationReceipts::new(&dir);

    let sig1 = compute_request_signature(
        "POST",
        "/api/sessions",
        &json!({"provider":"deepseek","modelId":"deepseek-flash"}),
    );
    let sig2 = compute_request_signature(
        "POST",
        "/api/sessions",
        &json!({"provider":"openai","modelId":"gpt-4o"}),
    );

    let v1 = receipts
        .once_value("key-1", &sig1, || async { Ok(json!({"id": "s-100"})) })
        .await
        .unwrap();
    assert_eq!(v1, json!({"id": "s-100"}));

    // Same key + same signature returns cached result without running action again
    let v2 = receipts
        .once_value("key-1", &sig1, || async {
            Err(ReceiptError::Action("should not execute".to_owned()))
        })
        .await
        .unwrap();
    assert_eq!(v2, json!({"id": "s-100"}));

    // Same key + different signature returns Conflict
    let conflict = receipts
        .once_value("key-1", &sig2, || async { Ok(json!({})) })
        .await
        .unwrap_err();
    assert!(conflict.is_conflict());

    // Simulate a crashed `status: "started"` receipt on disk
    let lookup = receipts
        .lookup::<serde_json::Value>("key-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lookup.status, ReceiptStatus::Done);

    let crashed_sig = compute_request_signature("DELETE", "/api/sessions/s-100", &json!(null));
    let _ = receipts
        .once_value("key-crashed", &crashed_sig, || async {
            Err(ReceiptError::Action("abort before done".to_owned()))
        })
        .await;
    // Now `key-crashed` is persisted with `status: "started"`; subsequent retry must return Conflict and never replay!
    let replay_attempt = receipts
        .once_value("key-crashed", &crashed_sig, || async {
            Ok(json!({"replayed": true}))
        })
        .await
        .unwrap_err();
    assert!(replay_attempt.is_conflict());

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cancelled_receipt_owner_wakes_waiters_and_never_replays_the_action() {
    let dir = std::env::temp_dir().join(format!("cst-server-cancelled-receipt-{}", Uuid::new_v4()));
    let receipts = MutationReceipts::new(&dir);
    let owner_store = receipts.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(async move {
        owner_store
            .once_value("cancelled-key", "signature", || async move {
                started.send(()).unwrap();
                std::future::pending().await
            })
            .await
    });
    ready.await.unwrap();
    let mut waiter = Box::pin(receipts.once_value("cancelled-key", "signature", || async {
        panic!("an uncertain mutation must never be replayed")
    }));
    assert!(futures_util::poll!(waiter.as_mut()).is_pending());
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    let error = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("cancelled owner must wake existing waiters")
        .unwrap_err();
    assert!(error.is_conflict());
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        receipts.once_value("cancelled-key", "signature", || async {
            panic!("uncertain retry must not execute")
        }),
    )
    .await
    .expect("later retries must settle")
    .unwrap_err();
    assert!(error.is_conflict());
    assert_eq!(
        receipts
            .lookup::<serde_json::Value>("cancelled-key")
            .await
            .unwrap()
            .unwrap()
            .status,
        ReceiptStatus::Started
    );
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn image_store_validates_magic_headers_and_stores_by_sha256() {
    let dir = std::env::temp_dir().join(format!("cst-server-images-{}", Uuid::new_v4()));
    let store = ImageStore::new(&dir);

    let valid_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR";
    let b64_png = base64::engine::general_purpose::STANDARD.encode(valid_png);

    let refs = store
        .accept(
            "sess-img",
            &json!([{
                "mimeType": "image/png",
                "data": b64_png,
            }]),
        )
        .await
        .unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].bytes, valid_png.len());

    let loaded = store.read("sess-img", &refs[0].id).await.unwrap();
    assert_eq!(loaded, valid_png);

    // Wrong magic header for image/png must be rejected
    let bad_b64 = base64::engine::general_purpose::STANDARD.encode(b"not-a-png-file");
    let err = store
        .accept(
            "sess-img",
            &json!([{
                "mimeType": "image/png",
                "data": bad_b64,
            }]),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AttachmentError::InvalidInput(_)));

    store.delete_session_attachments("sess-img").await;
    assert!(matches!(
        store.read("sess-img", &refs[0].id).await,
        Err(AttachmentError::NotFound)
    ));
    let _ = fs::remove_dir_all(&dir);
}

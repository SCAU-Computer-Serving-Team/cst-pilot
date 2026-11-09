use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cst_pilot::telemetry::{
    billing::{BillingLookup, BillingRequest},
    draft::Drafts,
    file_lock::FileLock,
    identity::Installation,
    outbox::{MAX_RECORD_BYTES, Outbox, QueuedRecord},
    owner::{Address, Owner, Proof},
};
use pi_ai::BoxFuture;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("cst-telemetry-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join("agent/home/telemetry").join(name)
    }
    fn put(&self, name: &str, value: &Value) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value.to_string()).unwrap();
    }
    fn outbox(&self) -> Outbox {
        Outbox::new(&self.0, Owner::global().unwrap()).unwrap()
    }
    fn drafts(&self) -> Drafts {
        Drafts::new(&self.0, Owner::global().unwrap()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn record(id: &str, revision: u64) -> QueuedRecord {
    QueuedRecord::new(json!({"v":"0.2","type":"session","recordId":id,"sessionId":"session","revision":revision,"final":false,"endReason":"","startedAt":"2026-11-09T01:00:00+08:00","endedAt":"2026-11-09T01:00:05+08:00","durationMs":5000,"prompts":1,"turns":0,"identity":{"installationId":"old-install","environmentId":"frozen"},"os":{"name":"old machine"},"future":{"unchanged":true}})).unwrap()
}
fn orphan(f: &Fixture, name: &str, record: &QueuedRecord) {
    f.put(&format!("drafts/{name}.json"), &json!({"record":record.value(),"lastTurnEndedAt":1794157202000u64,"ownerPid":std::process::id(),"cstoaRequests":[{"requestId":"request-1","quotaPoints":null}]}));
}

#[test]
fn owner_proof_uses_both_pipe_names_and_is_released_on_drop() {
    let owner = Owner::start().unwrap();
    let proof = owner.proof();
    assert!(proof.valid());
    for _ in 0..8 {
        assert!(proof.alive());
    }
    #[cfg(windows)]
    {
        assert!(
            Proof {
                token: proof.token.clone(),
                address: Address::Pipe(owner.draft_address())
            }
            .alive()
        );
        assert!(
            matches!(&proof.address, Address::Pipe(name) if name == &format!(r"\\.\pipe\cst-pilot-telemetry-{}",proof.token))
        );
    }
    drop(owner);
    assert!(!proof.alive());
    let mut invalid = proof;
    invalid.address = Address::Pipe(r"\\remote\pipe\credential".into());
    assert!(!invalid.valid());
    assert!(!invalid.alive());
}
#[test]
fn timeout_is_conservative_and_wrong_local_token_is_dead() {
    let token = uuid::Uuid::new_v4().to_string();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let _ = stream.write_all(b"wrong token");
    });
    assert!(
        Proof {
            token: token.clone(),
            address: Address::Port(port)
        }
        .alive()
    );
    server.join().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"wrong token").unwrap();
    });
    assert!(
        !Proof {
            token,
            address: Address::Port(port)
        }
        .alive()
    );
    server.join().unwrap();
}
#[test]
fn live_transaction_blocks_but_cancel_is_prompt_and_stale_proof_ignores_pid() {
    let f = Fixture::new();
    let owner = Owner::global().unwrap();
    let path = f.path("test.json");
    let held = FileLock::acquire(&f.0, &path, &owner.proof(), &CancellationToken::new()).unwrap();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let root = f.0.clone();
    let target = path.clone();
    let proof = owner.proof();
    let task = std::thread::spawn(move || {
        FileLock::acquire(&root, &target, &proof, &token)
            .err()
            .unwrap()
    });
    std::thread::sleep(Duration::from_millis(50));
    cancel.cancel();
    assert!(task.join().unwrap().contains("取消"));
    drop(held);
    let dead = Owner::start().unwrap();
    let proof = dead.proof();
    drop(dead);
    f.put(
        "test.json.lock/owner.json",
        &json!({"pid":std::process::id(),"proof":proof}),
    );
    let held = FileLock::acquire(&f.0, &path, &owner.proof(), &CancellationToken::new()).unwrap();
    drop(held);
    assert!(!f.path("test.json.lock").exists());
}
#[test]
fn stale_lock_cleanup_preserves_unknown_contents_and_damaged_owner() {
    let f = Fixture::new();
    let dead = Owner::start().unwrap();
    let proof = dead.proof();
    drop(dead);
    f.put("test.json.lock/owner.json", &json!({"proof":proof}));
    fs::write(f.path("test.json.lock/important.txt"), b"keep").unwrap();
    let owner = Owner::global().unwrap();
    drop(
        FileLock::acquire(
            &f.0,
            &f.path("test.json"),
            &owner.proof(),
            &CancellationToken::new(),
        )
        .unwrap(),
    );
    let stale = fs::read_dir(f.path(""))
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().contains(".stale-"))
        .unwrap()
        .path();
    assert_eq!(fs::read(stale.join("important.txt")).unwrap(), b"keep");
    f.put(
        "blocked.json.lock/owner.json",
        &json!({"pid":std::process::id()}),
    );
    fs::write(f.path("blocked.json.lock/owner.json"), b"{broken").unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(FileLock::acquire(&f.0, &f.path("blocked.json"), &owner.proof(), &cancel).is_err());
    assert_eq!(
        fs::read(f.path("blocked.json.lock/owner.json")).unwrap(),
        b"{broken"
    );
}
#[test]
fn two_queues_replace_latest_and_old_exact_ack_cannot_remove_new_content() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let cancel = CancellationToken::new();
    let a = "https://a.example/v1/sessions";
    let b = "https://b.example/v1/sessions";
    let first = record("a", 1);
    let second = record("a", 2);
    outbox
        .queue_all(&first, &[a.into(), b.into()], &cancel)
        .unwrap();
    outbox.append(&second, Some(a), &cancel).unwrap();
    assert_eq!(outbox.append(&first, Some(a), &cancel).unwrap(), second);
    outbox
        .acknowledge(std::slice::from_ref(&first), Some(a), &cancel)
        .unwrap();
    assert_eq!(outbox.read_all(Some(a)).unwrap(), vec![second.clone()]);
    outbox
        .acknowledge(std::slice::from_ref(&first), Some(b), &cancel)
        .unwrap();
    assert!(outbox.read_all(Some(b)).unwrap().is_empty());
    assert_eq!(outbox.read_all(Some(a)).unwrap(), vec![second]);
    assert_ne!(outbox.path(Some(a)), outbox.path(Some(b)));
}
#[test]
fn concurrent_queue_writers_are_serialized_and_capacity_is_bounded() {
    let f = Fixture::new();
    let cancel = CancellationToken::new();
    let outbox = f.outbox();
    std::thread::scope(|scope| {
        for i in 1..=16 {
            let outbox = outbox.clone();
            let cancel = cancel.clone();
            scope.spawn(move || {
                outbox
                    .append(&record("shared", i), Some("endpoint"), &cancel)
                    .unwrap();
            });
        }
    });
    assert_eq!(outbox.read_all(Some("endpoint")).unwrap()[0].revision(), 16);
    for i in 0..205 {
        outbox
            .append(
                &record(&format!("record-{i}"), 1),
                Some("endpoint"),
                &cancel,
            )
            .unwrap();
    }
    let all = outbox.read_all(Some("endpoint")).unwrap();
    assert_eq!(all.len(), 200);
    assert_eq!(all[0].id(), "record-5");
    assert_eq!(outbox.batch("endpoint").unwrap().len(), 50);
}
#[test]
fn corrupt_lines_are_quarantined_before_rewrite_and_legacy_migrates_to_all_targets() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let cancel = CancellationToken::new();
    fs::create_dir_all(f.path("")).unwrap();
    let legacy =
        QueuedRecord::new(json!({"v":"0.1","type":"session","recordId":"legacy","future":42}))
            .unwrap();
    let bytes = format!(
        "{}\n{{bad\n{}\n",
        legacy.value(),
        record("modern", 1).value()
    );
    fs::write(outbox.path(None), &bytes).unwrap();
    assert_eq!(outbox.read_all(None).unwrap().len(), 2);
    outbox
        .migrate_legacy(&["a".into(), "b".into()], &cancel)
        .unwrap();
    assert!(outbox.read_all(None).unwrap().is_empty());
    assert_eq!(outbox.read_all(Some("a")).unwrap().len(), 2);
    assert_eq!(outbox.batch("a").unwrap(), vec![legacy]);
    let quarantine = fs::read_dir(f.path(""))
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
        .unwrap()
        .path();
    assert_eq!(fs::read(quarantine).unwrap(), b"{bad\n");
}
#[test]
fn per_endpoint_failure_preserves_legacy_and_successful_endpoint_without_duplicates() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let cancel = CancellationToken::new();
    let item = record("a", 1);
    outbox.append(&item, None, &cancel).unwrap();
    fs::create_dir(outbox.path(Some("blocked"))).unwrap();
    assert!(
        outbox
            .migrate_legacy(&["good".into(), "blocked".into()], &cancel)
            .is_err()
    );
    assert_eq!(outbox.read_all(None).unwrap(), vec![item.clone()]);
    assert_eq!(outbox.read_all(Some("good")).unwrap(), vec![item.clone()]);
    fs::remove_dir(outbox.path(Some("blocked"))).unwrap();
    outbox
        .migrate_legacy(&["good".into(), "blocked".into()], &cancel)
        .unwrap();
    assert_eq!(outbox.read_all(Some("good")).unwrap(), vec![item]);
}
#[test]
fn oversized_invalid_and_cancelled_records_do_not_change_existing_queue() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let cancel = CancellationToken::new();
    outbox.append(&record("a", 1), Some("a"), &cancel).unwrap();
    let before = fs::read(outbox.path(Some("a"))).unwrap();
    let mut invalid = record("a", 2).into_value();
    invalid["extra"] = json!("x".repeat(MAX_RECORD_BYTES));
    assert!(QueuedRecord::new(invalid).is_err());
    for revision in [json!(0), json!(-1), json!(9_007_199_254_740_992u64)] {
        let mut value = record("a", 1).into_value();
        value["revision"] = revision;
        assert!(QueuedRecord::new(value).is_err());
    }
    cancel.cancel();
    assert!(outbox.append(&record("a", 2), Some("a"), &cancel).is_err());
    assert_eq!(fs::read(outbox.path(Some("a"))).unwrap(), before);
    let mut medium = record("b", 1).into_value();
    medium["extra"] = json!("x".repeat(500 * 1024));
    outbox
        .append(
            &QueuedRecord::new(medium.clone()).unwrap(),
            Some("batch"),
            &CancellationToken::new(),
        )
        .unwrap();
    medium["recordId"] = json!("c");
    outbox
        .append(
            &QueuedRecord::new(medium).unwrap(),
            Some("batch"),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(outbox.batch("batch").unwrap().len(), 1);
}
#[tokio::test]
async fn active_owner_is_skipped_and_exit_uses_last_activity_and_frozen_environment() {
    let f = Fixture::new();
    let owner = Arc::new(Owner::start().unwrap());
    let drafts = Drafts::new(&f.0, owner.clone()).unwrap();
    let item = record("a", 1);
    let ended = chrono::DateTime::parse_from_rfc3339("2026-11-09T01:00:02+08:00")
        .unwrap()
        .timestamp_millis() as u64;
    drafts
        .save(
            &item,
            ended,
            &[BillingRequest {
                request_id: Some("request-id".into()),
                quota_points: None,
            }],
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(
        f.drafts()
            .recover(
                |_| async { panic!("live owner must be skipped") },
                None,
                CancellationToken::new()
            )
            .await
            .unwrap()
            .is_empty()
    );
    drop(drafts);
    drop(owner);
    let saved = Arc::new(Mutex::new(Vec::new()));
    let seen = saved.clone();
    let recovered = f
        .drafts()
        .recover(
            move |value| {
                seen.lock().unwrap().push(value);
                async { Ok(()) }
            },
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    let value = recovered[0].value();
    assert_eq!(value["revision"], 2);
    assert_eq!(value["final"], true);
    assert_eq!(value["endReason"], "crash");
    assert_eq!(value["durationMs"], 2000);
    assert_eq!(value["identity"], item.value()["identity"]);
    assert_eq!(value["future"], item.value()["future"]);
    assert!(!value.to_string().contains("request-id"));
    assert!(!value.to_string().contains("ownerPid"));
    assert!(!f.drafts().path("a").unwrap().exists());
}
#[tokio::test]
async fn failed_enqueue_preserves_sealed_revision_and_retry_does_not_rebill() {
    let f = Fixture::new();
    orphan(&f, "legacy-session-name", &record("a", 4));
    let first = f
        .drafts()
        .recover(
            |_| async { Err("disk full".into()) },
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(first.is_empty());
    let raw = fs::read(f.path("drafts/legacy-session-name.json")).unwrap();
    let sealed: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(sealed["record"]["revision"], 5);
    assert!(sealed.get("owner").is_none());
    struct Forbidden;
    impl BillingLookup for Forbidden {
        fn lookup<'a>(
            &'a self,
            _: &'a [String],
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
            panic!("sealed revision cannot be re-billed")
        }
    }
    let recovered = f
        .drafts()
        .recover(
            |_| async { Ok(()) },
            Some(&Forbidden),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(recovered[0].revision(), 5);
    assert!(!f.path("drafts/legacy-session-name.json").exists());
}
#[tokio::test]
async fn unmarked_final_drafts_remain_immutable_and_are_not_billed_again() {
    let f = Fixture::new();
    let mut value = record("legacy-final", 4).into_value();
    value["final"] = json!(true);
    value["endReason"] = json!("quit");
    value["cstoaBilling"] = json!({"requests":1,"settledRequests":0,"quotaPoints":null});
    let item = QueuedRecord::new(value).unwrap();
    orphan(&f, "legacy-final", &item);
    struct Forbidden;
    impl BillingLookup for Forbidden {
        fn lookup<'a>(
            &'a self,
            _: &'a [String],
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
            panic!("unmarked final content must stay stable")
        }
    }
    let recovered = f
        .drafts()
        .recover(
            |_| async { Ok(()) },
            Some(&Forbidden),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(recovered, vec![item]);
}

#[tokio::test]
async fn concurrent_recovery_seals_once_and_slow_billing_cannot_overwrite_fast_result() {
    let f = Fixture::new();
    orphan(&f, "old-session", &record("a", 1));
    struct Lookup {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        points: u64,
    }
    impl BillingLookup for Lookup {
        fn lookup<'a>(
            &'a self,
            ids: &'a [String],
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(ids
                    .iter()
                    .map(|id| (id.clone(), Some(self.points)))
                    .collect())
            })
        }
    }
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let lookup = Lookup {
        started: started.clone(),
        release: release.clone(),
        points: 1,
    };
    let slow = f.drafts();
    let saved = Arc::new(Mutex::new(Vec::new()));
    let seen = saved.clone();
    let task = tokio::spawn(async move {
        slow.recover(
            move |record| {
                seen.lock().unwrap().push(record);
                async { Ok(()) }
            },
            Some(&lookup),
            CancellationToken::new(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    let fast = f
        .drafts()
        .recover(|_| async { Ok(()) }, None, CancellationToken::new())
        .await
        .unwrap();
    release.notify_one();
    assert!(task.await.unwrap().is_empty());
    assert_eq!(fast.len(), 1);
    assert!(saved.lock().unwrap().is_empty());
}
#[tokio::test]
async fn damaged_and_unknown_drafts_are_preserved_while_empty_drafts_are_removed() {
    let f = Fixture::new();
    f.put("drafts/bad.json", &json!({"keep":"unknown"}));
    f.put("drafts/unknown.json", &json!({"record":{"future":true}}));
    fs::write(f.path("drafts/bad.json"), b"{bad-sensitive").unwrap();
    let mut empty = record("empty", 1).into_value();
    empty["prompts"] = json!(0);
    f.put("drafts/empty.json", &json!({"record":empty}));
    let mut legacy = record("legacy", 1).into_value();
    legacy["v"] = json!("0.1");
    legacy.as_object_mut().unwrap().remove("revision");
    legacy.as_object_mut().unwrap().remove("final");
    f.put(
        "drafts/legacy.json",
        &json!({"record":legacy,"lastTurnEndedAt":0}),
    );
    let recovered = f
        .drafts()
        .recover(|_| async { Ok(()) }, None, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].version(), "0.1");
    assert!(recovered[0].value().get("final").is_none());
    assert_eq!(recovered[0].value()["durationMs"], 0);
    assert_eq!(
        fs::read(f.path("drafts/bad.json")).unwrap(),
        b"{bad-sensitive"
    );
    assert!(f.path("drafts/unknown.json").exists());
    assert!(!f.path("drafts/empty.json").exists());
}
#[test]
fn hardlinks_in_queues_drafts_and_locks_are_rejected_without_changing_the_other_name() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let cancel = CancellationToken::new();
    let item = record("a", 1);
    outbox.append(&item, Some("a"), &cancel).unwrap();
    let alias = f.0.join("external.jsonl");
    fs::hard_link(outbox.path(Some("a")), &alias).unwrap();
    let bytes = fs::read(&alias).unwrap();
    assert!(outbox.append(&record("a", 2), Some("a"), &cancel).is_err());
    assert_eq!(fs::read(&alias).unwrap(), bytes);
    let drafts = f.drafts();
    drafts.save(&item, 0, &[], &cancel).unwrap();
    let alias = f.0.join("external-draft.json");
    fs::hard_link(drafts.path("a").unwrap(), &alias).unwrap();
    let bytes = fs::read(&alias).unwrap();
    assert!(drafts.save(&record("a", 2), 0, &[], &cancel).is_err());
    assert_eq!(fs::read(&alias).unwrap(), bytes);
    assert!(
        drafts
            .path("../outside")
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("%2F")
    );
}

#[test]
fn draft_writes_are_monotonic_and_removal_compares_exact_content() {
    let f = Fixture::new();
    let drafts = f.drafts();
    let cancel = CancellationToken::new();
    let first = record("same", 1);
    let second = record("same", 2);
    drafts.save(&first, 0, &[], &cancel).unwrap();
    drafts.save(&second, 0, &[], &cancel).unwrap();
    drafts.save(&first, 0, &[], &cancel).unwrap();
    drafts.remove_if(&first, &cancel).unwrap();
    assert!(drafts.path("same").unwrap().exists());
    let mut changed = second.clone().into_value();
    changed["prompts"] = json!(8);
    assert!(
        drafts
            .save(&QueuedRecord::new(changed).unwrap(), 0, &[], &cancel)
            .is_err()
    );
    let mut final_value = second.clone().into_value();
    final_value["revision"] = json!(3);
    final_value["final"] = json!(true);
    final_value["endReason"] = json!("shutdown");
    drafts
        .save(&QueuedRecord::new(final_value).unwrap(), 0, &[], &cancel)
        .unwrap();
    assert!(drafts.save(&record("same", 4), 0, &[], &cancel).is_err());
}
#[tokio::test]
async fn recovered_billing_is_aggregated_without_request_ids_and_sealed_before_enqueue() {
    let f = Fixture::new();
    orphan(&f, "billing", &record("a", 1));
    struct Zero;
    impl BillingLookup for Zero {
        fn lookup<'a>(
            &'a self,
            ids: &'a [String],
            _: CancellationToken,
        ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
            Box::pin(async move { Ok(ids.iter().map(|id| (id.clone(), Some(0))).collect()) })
        }
    }
    let path = f.path("drafts/billing.json");
    let recovered = f
        .drafts()
        .recover(
            move |record| {
                let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert!(saved.get("owner").is_none());
                assert_eq!(saved["record"], *record.value());
                async { Ok(()) }
            },
            Some(&Zero),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        recovered[0].value()["cstoaBilling"],
        json!({"requests":1,"settledRequests":1,"quotaPoints":0})
    );
    assert!(!recovered[0].value().to_string().contains("request-1"));
}
#[cfg(windows)]
#[test]
fn linked_telemetry_parent_cannot_write_or_clean_outside_the_kit() {
    let f = Fixture::new();
    let outside = Fixture::new();
    fs::create_dir_all(f.0.join("agent/home")).unwrap();
    fs::write(outside.0.join("important"), b"keep").unwrap();
    let result = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(f.path(""))
        .arg(&outside.0)
        .output()
        .unwrap();
    assert!(result.status.success());
    let cancel = CancellationToken::new();
    assert!(
        f.outbox()
            .append(&record("a", 1), Some("a"), &cancel)
            .is_err()
    );
    assert!(f.drafts().save(&record("a", 1), 0, &[], &cancel).is_err());
    assert_eq!(fs::read(outside.0.join("important")).unwrap(), b"keep");
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 1);
    fs::remove_dir(f.path("")).unwrap();
}

#[cfg(windows)]
#[test]
fn releasing_a_lock_retries_a_short_external_directory_probe() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    let owner = Owner::global().unwrap();
    let lock = FileLock::acquire(
        &f.0,
        &f.path("probe.json"),
        &owner.proof(),
        &CancellationToken::new(),
    )
    .unwrap();
    let held = fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(3)
        .custom_flags(0x0200_0000 | 0x0020_0000)
        .open(f.path("probe.json.lock"))
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(held);
    });
    drop(lock);
    release.join().unwrap();
    assert!(!f.path("probe.json.lock").exists());
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn process_death_releases_owner_and_copied_pid_does_not_block_recovery_lock() {
    let f = Fixture::new();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "telemetry_owner_child",
            "--nocapture",
        ])
        .env("CST_TELEMETRY_CHILD", &f.0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let output = child.0.stdout.take().unwrap();
    let mut proof = None;
    for line in BufReader::new(output).lines() {
        let line = line.unwrap();
        if let Some(raw) = line.strip_prefix("OWNER:") {
            proof = Some(serde_json::from_str::<Proof>(raw).unwrap());
            break;
        }
    }
    let proof = proof.expect("child ready");
    assert!(proof.alive());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(!proof.alive());
    let owner = Owner::global().unwrap();
    f.put(
        "child.json.lock/owner.json",
        &json!({"pid":std::process::id(),"proof":proof}),
    );
    drop(
        FileLock::acquire(
            &f.0,
            &f.path("child.json"),
            &owner.proof(),
            &CancellationToken::new(),
        )
        .unwrap(),
    );
}
#[test]
#[ignore = "cross-process test helper, launched by the parent test"]
fn telemetry_owner_child() {
    let Some(root) = std::env::var_os("CST_TELEMETRY_CHILD") else {
        return;
    };
    let owner = Owner::global().unwrap();
    let root = PathBuf::from(root).canonicalize().unwrap();
    if std::env::var("CST_TELEMETRY_CHILD_MODE").as_deref() == Ok("identity") {
        let id = Installation::load(&root).expect("identity initialized");
        println!("IDENTITY:{}", id.installation_id);
        return;
    }
    let target = root.join("agent/home/telemetry/child.json");
    let _lock =
        FileLock::acquire(&root, &target, &owner.proof(), &CancellationToken::new()).unwrap();
    println!("OWNER:{}", serde_json::to_string(&owner.proof()).unwrap());
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}
#[test]
fn identity_initialization_uses_the_same_live_file_transaction_lock() {
    let f = Fixture::new();
    let owner = Owner::global().unwrap();
    let path = f.path("identity.json");
    let lock = FileLock::acquire(&f.0, &path, &owner.proof(), &CancellationToken::new()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let root = f.0.clone();
    let task = std::thread::spawn(move || {
        tx.send(Installation::load(&root).map(|id| id.installation_id))
            .unwrap()
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(lock);
    assert!(rx.recv_timeout(Duration::from_secs(3)).unwrap().is_some());
    task.join().unwrap();
}

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;

use cst_pilot::{
    auth::oa::Clock,
    telemetry::{
        config::Endpoint,
        delivery::Delivery,
        diagnostics::{Diagnostics, Event, SnapshotRef},
        environment::Identity,
        heartbeat::{HeartbeatRun, Status},
        identity::Installation,
        outbox::{Outbox, QueuedRecord},
        owner::Owner,
        transport::{
            FailureKind, Method, NativeTransport, Request, Response, SendOutcome, Sender,
            Transport, UploadAuth,
        },
    },
};
use pi_ai::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("cst-telemetry-network-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn auth(&self) -> UploadAuth {
        UploadAuth::Installation(Installation::load(&self.0).unwrap())
    }
    fn outbox(&self) -> Outbox {
        Outbox::new(&self.0, Owner::global().unwrap()).unwrap()
    }
    fn diagnostics(&self) -> Diagnostics {
        Diagnostics::new(&self.0, Owner::global().unwrap()).unwrap()
    }
    fn status(&self) -> Value {
        serde_json::from_slice(&fs::read(self.0.join("agent/home/telemetry/status.json")).unwrap())
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn sleep(&self, _: Duration, cancel: CancellationToken) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        })
    }
}
fn endpoint(url: &str) -> Endpoint {
    Endpoint {
        url: url.into(),
        ca: None,
        unavailable: false,
    }
}
fn record(id: &str, revision: u64) -> QueuedRecord {
    QueuedRecord::new(
        json!({"v":"0.2","type":"session","recordId":id,"revision":revision,"final":false}),
    )
    .unwrap()
}
fn legacy(id: &str) -> QueuedRecord {
    QueuedRecord::new(json!({"v":"0.1","type":"session","recordId":id})).unwrap()
}
fn capability() -> Response {
    response(
        200,
        json!({"protocols":["0.1","0.2"],"capabilities":["session-snapshots","revision-acks","heartbeat"]}),
    )
}
fn response(status: u16, body: Value) -> Response {
    Response {
        status,
        body: serde_json::to_vec(&body).unwrap(),
    }
}
fn accepted(body: &Value) -> Response {
    response(
        202,
        json!({"v":"0.2","acks":body["records"].as_array().unwrap().iter().map(|r|json!({"recordId":r["recordId"],"revision":r["revision"],"status":"stored"})).collect::<Vec<_>>(),"heartbeatAck":{"runId":body["heartbeat"]["runId"],"sequence":body["heartbeat"]["sequence"],"status":"stored"}}),
    )
}
#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<Request>>,
    replies: Mutex<VecDeque<Result<Response, FailureKind>>>,
}
impl Transport for Mock {
    fn request(
        &self,
        request: Request,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<Response, FailureKind>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(request);
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(capability()))
        })
    }
}
impl Mock {
    fn new(replies: Vec<Response>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
        })
    }
}
fn sender(mock: Arc<dyn Transport>) -> Sender {
    Sender::new(mock, Arc::new(TestClock(AtomicU64::new(1_794_200_000_000))))
}

#[tokio::test]
async fn exact_body_signature_and_partial_revision_ack_rules() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let records = (1..=7)
        .map(|n| record(&format!("r{n}"), n))
        .collect::<Vec<_>>();
    let mock = Mock::new(vec![
        capability(),
        response(
            202,
            json!({"v":"0.2","acks":[
        {"recordId":"r1","revision":1,"status":"stored"},{"recordId":"r2","revision":2,"status":"stale"},
        {"recordId":"r3","revision":3,"status":"duplicate"},{"recordId":"r4","revision":4,"status":"rejected","reason":"owner-mismatch"},
        {"recordId":"r5","revision":5,"status":"rejected","reason":"future-reason"},{"recordId":"r6","revision":6,"status":"stored"},{"recordId":"r6","revision":6,"status":"stored"},
        {"recordId":"r7","revision":6,"status":"stored"}]}),
        ),
    ]);
    let result = sender(mock.clone())
        .send(
            &endpoint("https://receiver.test/api"),
            &f.auth(),
            &records,
            None,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        result,
        SendOutcome::Accepted {
            acknowledged: records[..4].to_vec(),
            rejected: vec![records[3].clone()],
            heartbeat_acknowledged: false
        }
    );
    let calls = mock.calls.lock().unwrap();
    assert_eq!(calls[0].method, Method::Get);
    assert!(calls[0].headers.is_empty());
    let req = &calls[1];
    let body = req.body.as_ref().unwrap();
    let h = &req.headers;
    let key = URL_SAFE_NO_PAD.decode(&h["X-Telemetry-Key"]).unwrap();
    let public = VerifyingKey::from_bytes(key[12..].try_into().unwrap()).unwrap();
    let hash = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let text = format!(
        "telemetry-upload\n{}\n{}\n{hash}",
        h["X-Telemetry-Installation"], h["X-Telemetry-Time"]
    );
    let signature =
        Signature::from_slice(&URL_SAFE_NO_PAD.decode(&h["X-Telemetry-Signature"]).unwrap())
            .unwrap();
    public.verify(text.as_bytes(), &signature).unwrap();
}
#[tokio::test]
async fn capability_missing_or_bad_json_never_sends_post_and_statuses_keep_versions() {
    let f = Fixture::new();
    let auth = f.auth();
    let ep = endpoint("https://receiver.test/api");
    for reply in [
        response(
            200,
            json!({"protocols":["0.2"],"capabilities":["session-snapshots"]}),
        ),
        response(500, Value::Null),
    ] {
        let mock = Mock::new(vec![reply]);
        assert_eq!(
            sender(mock.clone())
                .send(
                    &ep,
                    &auth,
                    &[record("r", 1)],
                    None,
                    CancellationToken::new()
                )
                .await,
            SendOutcome::Retry(FailureKind::Capability)
        );
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }
    let mock = Mock::new(vec![Response {
        status: 200,
        body: b"secret non-JSON".to_vec(),
    }]);
    assert_eq!(
        sender(mock)
            .send(
                &ep,
                &auth,
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Retry(FailureKind::Protocol)
    );
    for status in [400, 413, 429, 500, 302] {
        let mock = Mock::new(vec![capability(), response(status, Value::Null)]);
        assert_eq!(
            sender(mock)
                .send(
                    &ep,
                    &auth,
                    &[record("r", 1)],
                    None,
                    CancellationToken::new()
                )
                .await,
            SendOutcome::Retry(FailureKind::Http)
        );
    }
    for status in [400, 413] {
        let mock = Mock::new(vec![response(status, Value::Null)]);
        assert_eq!(
            sender(mock)
                .send(&ep, &auth, &[legacy("r")], None, CancellationToken::new())
                .await,
            SendOutcome::Bad
        );
    }
}
#[tokio::test]
async fn oauth_rejection_falls_back_to_installation_and_stop_is_process_local() {
    let f = Fixture::new();
    let installation = Installation::load(&f.0).unwrap();
    let auth = UploadAuth::Bearer {
        installation,
        access: Zeroizing::new("mock-access".into()),
    };
    let ep = endpoint("https://receiver.test/api");
    let mock = Mock::new(vec![
        capability(),
        response(401, Value::Null),
        capability(),
        response(
            202,
            json!({"v":"0.2","acks":[{"recordId":"r","revision":1,"status":"stored"}]}),
        ),
    ]);
    assert!(matches!(
        sender(mock.clone())
            .send_with_fallback(
                &ep,
                &auth,
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Accepted { .. }
    ));
    {
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls[1].headers["Authorization"], "Bearer mock-access");
        assert!(calls[3].headers["Authorization"].starts_with("Installation "));
        assert_eq!(
            calls[1].headers["X-Telemetry-Installation"],
            calls[3].headers["X-Telemetry-Installation"]
        );
    }
    for phase in [true, false] {
        let replies = if phase {
            vec![response(404, Value::Null)]
        } else {
            vec![capability(), response(410, Value::Null)]
        };
        let mock = Mock::new(replies);
        let sender = sender(mock.clone());
        for _ in 0..2 {
            assert_eq!(
                sender
                    .send(
                        &ep,
                        &auth,
                        &[record("r", 1)],
                        None,
                        CancellationToken::new()
                    )
                    .await,
                SendOutcome::Stop
            );
        }
        assert_eq!(mock.calls.lock().unwrap().len(), if phase { 1 } else { 2 });
    }
}
struct Gate {
    calls: Mutex<Vec<Value>>,
    entered: Semaphore,
    release: Semaphore,
    posts: AtomicUsize,
    fail_url: Option<String>,
    outbox: Option<(Outbox, String)>,
}
impl Gate {
    fn new() -> Self {
        Self {
            calls: Mutex::default(),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            posts: AtomicUsize::new(0),
            fail_url: None,
            outbox: None,
        }
    }
}
impl Transport for Gate {
    fn request(
        &self,
        request: Request,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Response, FailureKind>> {
        Box::pin(async move {
            if request.method == Method::Get {
                return Ok(capability());
            }
            let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
            self.calls.lock().unwrap().push(body.clone());
            if self.fail_url.as_deref() == Some(&request.endpoint.url) {
                return Err(FailureKind::Network);
            }
            if self.posts.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.add_permits(1);
                tokio::select! {_ = cancel.cancelled()=>return Err(FailureKind::Timeout),permit=self.release.acquire()=>permit.unwrap().forget()}
            }
            if let Some((outbox, url)) = &self.outbox
                && body["records"][0]["revision"] == 1
            {
                outbox
                    .append(&record("old", 2), Some(url), &CancellationToken::new())
                    .unwrap();
            }
            Ok(accepted(&body))
        })
    }
}
#[tokio::test]
async fn independent_queues_single_flight_and_inflight_old_ack_cannot_remove_new_revision() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let good = endpoint("https://good.test/api");
    let bad = endpoint("https://bad.test/api");
    outbox
        .append(
            &record("old", 1),
            Some(&good.url),
            &CancellationToken::new(),
        )
        .unwrap();
    outbox
        .append(&record("old", 1), Some(&bad.url), &CancellationToken::new())
        .unwrap();
    let mut gate = Gate::new();
    gate.fail_url = Some(bad.url.clone());
    gate.outbox = Some((outbox.clone(), good.url.clone()));
    let gate = Arc::new(gate);
    let delivery = Delivery::new(outbox.clone(), sender(gate.clone()), f.diagnostics());
    let task = tokio::spawn({
        let d = delivery.clone();
        let auth = f.auth();
        let eps = vec![good.clone(), bad.clone()];
        async move { d.flush(auth, &eps, CancellationToken::new()).await }
    });
    gate.entered.acquire().await.unwrap().forget();
    delivery
        .flush(
            f.auth(),
            std::slice::from_ref(&good),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(gate.posts.load(Ordering::SeqCst), 1);
    gate.release.add_permits(1);
    task.await.unwrap();
    assert!(outbox.read_all(Some(&good.url)).unwrap().is_empty());
    assert_eq!(outbox.read_all(Some(&bad.url)).unwrap().len(), 1);
    let calls = gate.calls.lock().unwrap();
    assert!(calls.iter().any(|v| v["records"][0]["revision"] == 2));
}
#[tokio::test]
async fn partial_ack_retains_unconfirmed_and_oversized_head_is_quarantined() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let ep = endpoint("https://receiver.test/api");
    let mut value = record("big", 1).into_value();
    value["padding"] = Value::String("x".repeat(901 * 1024));
    outbox
        .append(
            &QueuedRecord::new(value).unwrap(),
            Some(&ep.url),
            &CancellationToken::new(),
        )
        .unwrap();
    for id in ["a", "b"] {
        outbox
            .append(&record(id, 1), Some(&ep.url), &CancellationToken::new())
            .unwrap();
    }
    let mock = Mock::new(vec![
        capability(),
        response(
            202,
            json!({"v":"0.2","acks":[{"recordId":"a","revision":1,"status":"stored"}]}),
        ),
    ]);
    Delivery::new(outbox.clone(), sender(mock), f.diagnostics())
        .flush(
            f.auth(),
            std::slice::from_ref(&ep),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        outbox.read_all(Some(&ep.url)).unwrap(),
        vec![record("b", 1)]
    );
    assert!(
        fs::read_dir(f.0.join("agent/home/telemetry"))
            .unwrap()
            .any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".oversize-"))
    );
    let state = f.status();
    let status = state["endpoints"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(status["failure"]["kind"], "protocol");
    assert_eq!(status["acks"][0]["recordId"], "a");
}
#[tokio::test]
async fn legacy_migration_and_http_bad_delete_only_matching_endpoint() {
    let f = Fixture::new();
    let outbox = f.outbox();
    let a = endpoint("https://a.test/api");
    let b = endpoint("https://b.test/api");
    let record = legacy("legacy");
    outbox
        .append(&record, None, &CancellationToken::new())
        .unwrap();
    let mock = Mock::new(vec![response(413, Value::Null)]);
    let d = Delivery::new(outbox.clone(), sender(mock), f.diagnostics());
    d.migrate(&[a.clone(), b.clone()], CancellationToken::new())
        .await
        .unwrap();
    assert!(outbox.read_all(None).unwrap().is_empty());
    d.flush(f.auth(), std::slice::from_ref(&a), CancellationToken::new())
        .await;
    assert!(outbox.read_all(Some(&a.url)).unwrap().is_empty());
    assert_eq!(outbox.read_all(Some(&b.url)).unwrap(), vec![record]);
}
#[tokio::test]
async fn slow_heartbeat_coalesces_latest_close_and_does_not_touch_queues() {
    let f = Fixture::new();
    let ep = endpoint("https://receiver.test/api");
    let gate = Arc::new(Gate::new());
    let clock = Arc::new(TestClock(AtomicU64::new(1000)));
    let sender = Sender::new(gate.clone(), clock.clone());
    let run = HeartbeatRun::new(
        Identity {
            installation_id: Some("installation".into()),
            environment_id: None,
            environment_source: "unavailable".into(),
        },
        sender,
        f.diagnostics(),
    );
    run.register("r");
    run.revision("r", 7);
    run.activity("r", Some(true));
    let task = tokio::spawn({
        let run = run.clone();
        let auth = f.auth();
        let ep = ep.clone();
        async move {
            run.pulse(auth, &[ep], false, false, CancellationToken::new())
                .await
        }
    });
    gate.entered.acquire().await.unwrap().forget();
    clock.0.store(31_000, Ordering::SeqCst);
    run.activity("r", Some(false));
    run.pulse(
        f.auth(),
        std::slice::from_ref(&ep),
        false,
        false,
        CancellationToken::new(),
    )
    .await;
    clock.0.store(32_000, Ordering::SeqCst);
    run.pulse(
        f.auth(),
        std::slice::from_ref(&ep),
        true,
        false,
        CancellationToken::new(),
    )
    .await;
    gate.release.add_permits(1);
    task.await.unwrap();
    run.pulse(f.auth(), &[ep], false, true, CancellationToken::new())
        .await;
    let calls = gate.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["heartbeat"]["status"], "busy");
    assert_eq!(calls[1]["heartbeat"]["status"], "closed");
    assert_eq!(calls[1]["heartbeat"]["sequence"], 3);
    assert_eq!(calls[1]["heartbeat"]["snapshots"][0]["revision"], 7);
    assert!(!f.0.join("agent/home/telemetry/pending.jsonl").exists());
    assert_eq!(
        f.status()["endpoints"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["heartbeat"]["sequence"],
        3
    );
}
#[tokio::test]
async fn heartbeat_interval_and_ninety_second_pending_expiry() {
    let f = Fixture::new();
    let ep = endpoint("https://receiver.test/api");
    let gate = Arc::new(Gate::new());
    let clock = Arc::new(TestClock(AtomicU64::new(1000)));
    let run = HeartbeatRun::new(
        Identity {
            installation_id: None,
            environment_id: None,
            environment_source: "unavailable".into(),
        },
        Sender::new(gate.clone(), clock.clone()),
        f.diagnostics(),
    );
    let task = tokio::spawn({
        let run = run.clone();
        let auth = f.auth();
        let ep = ep.clone();
        async move {
            run.pulse(auth, &[ep], false, false, CancellationToken::new())
                .await
        }
    });
    gate.entered.acquire().await.unwrap().forget();
    clock.0.store(2000, Ordering::SeqCst);
    run.pulse(
        f.auth(),
        std::slice::from_ref(&ep),
        false,
        false,
        CancellationToken::new(),
    )
    .await;
    clock.0.store(31_000, Ordering::SeqCst);
    run.pulse(f.auth(), &[ep], false, false, CancellationToken::new())
        .await;
    clock.0.store(122_000, Ordering::SeqCst);
    gate.release.add_permits(1);
    task.await.unwrap();
    assert_eq!(gate.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn heartbeat_ack_mismatch_and_unavailable_ca_are_local_fixed_categories() {
    let f = Fixture::new();
    let ep = endpoint("https://receiver.test/api");
    let mock = Mock::new(vec![
        capability(),
        response(
            202,
            json!({"v":"0.2","heartbeatAck":{"runId":"wrong","sequence":1,"status":"stored"}}),
        ),
    ]);
    let run = HeartbeatRun::new(
        Identity {
            installation_id: None,
            environment_id: None,
            environment_source: "unavailable".into(),
        },
        sender(mock.clone()),
        f.diagnostics(),
    );
    run.pulse(
        f.auth(),
        std::slice::from_ref(&ep),
        false,
        true,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        f.status()["endpoints"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["failure"]["kind"],
        "protocol"
    );
    let ep = Endpoint {
        unavailable: true,
        ..ep
    };
    let before = mock.calls.lock().unwrap().len();
    assert_eq!(
        sender(mock.clone())
            .send(
                &ep,
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Retry(FailureKind::Tls)
    );
    assert_eq!(before, mock.calls.lock().unwrap().len());
}
#[test]
fn diagnostics_are_bounded_masked_and_compatible_without_record_body() {
    let f = Fixture::new();
    let d = f.diagnostics();
    let token = CancellationToken::new();
    for n in 0..55 {
        d.record(
            Event::Saved(SnapshotRef {
                record_id: format!("r{n}"),
                revision: Some(n + 1),
                final_state: Some(false),
            }),
            1000,
            &token,
        );
    }
    for n in 0..10 {
        d.record(
            Event::Failure {
                endpoint: Some(format!("https://private-endpoint-{n}.test/secret")),
                kind: FailureKind::Network,
            },
            1000,
            &token,
        );
    }
    let state = f.status();
    assert_eq!(state["saved"].as_array().unwrap().len(), 50);
    assert_eq!(state["saved"][0]["final"], false);
    assert_eq!(state["endpoints"].as_object().unwrap().len(), 8);
    let text = state.to_string();
    assert!(!text.contains("https:"));
    assert!(!text.contains("private-endpoint"));
    assert!(!text.contains("secret"));
}
#[tokio::test]
async fn cancellation_drops_network_and_releases_delivery_singleflight_without_parent_cancel() {
    let f = Fixture::new();
    let ep = endpoint("https://receiver.test/api");
    let outbox = f.outbox();
    outbox
        .append(&record("r", 1), Some(&ep.url), &CancellationToken::new())
        .unwrap();
    let gate = Arc::new(Gate::new());
    let d = Delivery::new(outbox.clone(), sender(gate.clone()), f.diagnostics());
    let parent = CancellationToken::new();
    let task = tokio::spawn({
        let d = d.clone();
        let auth = f.auth();
        let ep = ep.clone();
        let cancel = parent.clone();
        async move { d.flush(auth, &[ep], cancel).await }
    });
    gate.entered.acquire().await.unwrap().forget();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!parent.is_cancelled());
    assert_eq!(outbox.read_all(Some(&ep.url)).unwrap().len(), 1);
    d.flush(f.auth(), std::slice::from_ref(&ep), parent).await;
    assert!(outbox.read_all(Some(&ep.url)).unwrap().is_empty());
}

#[tokio::test]
async fn upload_auth_only_uses_unexpired_oauth_and_requires_persistent_identity() {
    use pi_ai::{ApiKeyCredential, Credential, InMemoryCredentialStore, OAuthCredential};
    let f = Fixture::new();
    let store = InMemoryCredentialStore::new();
    let installation = Installation::load(&f.0).unwrap();
    store
        .set(
            "cstoa",
            Credential::ApiKey(ApiKeyCredential {
                key: Some("not-an-upload-token".into()),
                ..Default::default()
            }),
        )
        .await;
    assert!(matches!(
        UploadAuth::from_store(Some(installation.clone()), &store, "cstoa", 1000),
        Some(UploadAuth::Installation(_))
    ));
    for (access, expires, bearer) in [
        ("valid-access", 1001, true),
        ("expired", 1000, false),
        ("invalid\r\nheader", 2000, false),
    ] {
        store
            .set(
                "cstoa",
                Credential::OAuth(OAuthCredential {
                    refresh: "must-not-refresh".into(),
                    access: access.into(),
                    expires,
                    extra: BTreeMap::new(),
                }),
            )
            .await;
        assert_eq!(
            matches!(
                UploadAuth::from_store(Some(installation.clone()), &store, "cstoa", 1000),
                Some(UploadAuth::Bearer { .. })
            ),
            bearer
        );
    }
    assert!(UploadAuth::from_store(None, &store, "cstoa", 1000).is_none());
}

#[tokio::test]
async fn capability_and_post_share_one_five_second_deadline() {
    struct Delayed(AtomicUsize);
    impl Transport for Delayed {
        fn request(
            &self,
            request: Request,
            _: CancellationToken,
        ) -> BoxFuture<'_, Result<Response, FailureKind>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(3)).await;
                Ok(if request.method == Method::Get {
                    capability()
                } else {
                    accepted(
                        &serde_json::from_slice::<Value>(request.body.as_ref().unwrap()).unwrap(),
                    )
                })
            })
        }
    }
    let f = Fixture::new();
    let delayed = Arc::new(Delayed(AtomicUsize::new(0)));
    let start = std::time::Instant::now();
    assert_eq!(
        sender(delayed.clone())
            .send(
                &endpoint("https://receiver.test/api"),
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Retry(FailureKind::Timeout)
    );
    assert_eq!(delayed.0.load(Ordering::SeqCst), 2);
    assert!(start.elapsed() < Duration::from_millis(6500));
}

#[tokio::test]
async fn heartbeat_snapshot_references_are_capped_and_never_contain_record_payloads() {
    let f = Fixture::new();
    let gate = Arc::new(Gate::new());
    gate.release.add_permits(1);
    let run = HeartbeatRun::new(
        Identity {
            installation_id: None,
            environment_id: None,
            environment_source: "unavailable".into(),
        },
        sender(gate.clone()),
        f.diagnostics(),
    );
    for n in 0..520 {
        let id = format!("r{n}");
        run.register(&id);
        run.revision(&id, n + 1);
    }
    run.pulse(
        f.auth(),
        &[endpoint("https://receiver.test/api")],
        false,
        true,
        CancellationToken::new(),
    )
    .await;
    let calls = gate.calls.lock().unwrap();
    assert_eq!(
        calls[0]["heartbeat"]["snapshots"].as_array().unwrap().len(),
        512
    );
    assert!(calls[0]["records"].as_array().unwrap().is_empty());
    assert!(
        calls[0]["heartbeat"]["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r.as_object().unwrap().len() == 2)
    );
}

struct Server {
    child: Child,
    url: String,
}
impl Server {
    fn start(tls: bool) -> Self {
        let script = r#"const fs=require('node:fs');const tls=process.argv[1]==='tls';const opts=tls?{key:fs.readFileSync(process.argv[2]),cert:fs.readFileSync(process.argv[3])}:{};const server=require(tls?'node:https':'node:http').createServer(opts,(req,res)=>{if(req.url==='/redirect'){res.writeHead(302,{location:'/must-not-follow'});res.end();return;}if(req.url==='/oversize'){res.writeHead(200);res.write('x'.repeat(65000));res.end('y'.repeat(1000));return;}if(req.url==='/slow'){setTimeout(()=>res.end('{}'),10000);return;}if(req.url==='/must-not-follow'){res.writeHead(500);res.end('followed');return;}let body='';req.on('data',b=>body+=b);req.on('end',()=>{if(req.method==='GET'){res.end(JSON.stringify({protocols:['0.2'],capabilities:['session-snapshots','revision-acks','heartbeat']}));return;}const b=JSON.parse(body);res.writeHead(202);res.end(JSON.stringify({v:'0.2',acks:b.records.map(r=>({recordId:r.recordId,revision:r.revision,status:'stored'}))}));});});server.on('tlsClientError',()=>{});server.listen(0,'127.0.0.1',()=>console.log(server.address().port));"#;
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/telemetry-tls");
        let mut child = Command::new("node")
            .arg("-e")
            .arg(script)
            .arg(if tls { "tls" } else { "http" })
            .arg(base.join("server.key.pem"))
            .arg(base.join("server.cert.pem"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().parse::<u16>().expect("local server port");
        Self {
            child,
            url: format!("{}://127.0.0.1:{port}", if tls { "https" } else { "http" }),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
#[tokio::test]
async fn real_http_roundtrip_redirect_rejection_body_limit_and_total_timeout() {
    let f = Fixture::new();
    let server = Server::start(false);
    let sender = Sender::default();
    let ep = endpoint(&format!("{}/api", server.url));
    assert!(matches!(
        sender
            .send(
                &ep,
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Accepted { .. }
    ));
    let native = NativeTransport;
    let request = |path: &str| Request {
        endpoint: endpoint(&format!("{}{path}", server.url)),
        method: Method::Get,
        headers: BTreeMap::new(),
        body: None,
    };
    let redirect = native
        .request(request("/redirect"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(redirect.status, 302);
    assert_eq!(
        native
            .request(request("/oversize"), CancellationToken::new())
            .await
            .err(),
        Some(FailureKind::Protocol)
    );
    let start = std::time::Instant::now();
    assert_eq!(
        sender
            .send(
                &endpoint(&format!("{}/slow", server.url)),
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Retry(FailureKind::Timeout)
    );
    assert!(start.elapsed() < Duration::from_secs(7));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        sender
            .send(&ep, &f.auth(), &[record("r", 1)], None, cancel)
            .await,
        SendOutcome::Retry(FailureKind::Timeout)
    );
}
#[tokio::test]
async fn dedicated_ca_is_scoped_and_hostname_validation_remains_enabled() {
    let f = Fixture::new();
    let server = Server::start(true);
    let sender = Sender::default();
    let ep = endpoint(&format!("{}/api", server.url));
    let cert = include_str!("fixtures/telemetry-tls/ca.cert.pem");
    assert_eq!(
        sender
            .send(
                &ep,
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Retry(FailureKind::Tls)
    );
    let pinned = Endpoint {
        ca: Some(cert.into()),
        ..ep.clone()
    };
    assert!(matches!(
        sender
            .send(
                &pinned,
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Accepted { .. }
    ));
    assert_eq!(
        sender
            .send(
                &ep,
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Retry(FailureKind::Tls)
    );
    let wrong_host = Endpoint {
        url: pinned.url.replace("127.0.0.1", "localhost"),
        ..pinned
    };
    assert_eq!(
        sender
            .send(
                &wrong_host,
                &f.auth(),
                &[record("r", 1)],
                None,
                CancellationToken::new()
            )
            .await,
        SendOutcome::Retry(FailureKind::Tls)
    );
    let _status: Status = Status::Idle;
}

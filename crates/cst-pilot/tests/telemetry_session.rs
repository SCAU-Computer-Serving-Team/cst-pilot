use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use cst_pilot::{
    Kit, SKILL_NAMES,
    auth::{
        AuthRuntime, Channel,
        oa::{OaClient, SystemClock},
    },
    telemetry::{
        TelemetryOptions, TelemetryService,
        billing::BillingLookup,
        environment::{EnvironmentSource, SystemProbe},
        outbox::Outbox,
        owner::Owner,
        transport::{FailureKind, Method, Request, Response, Transport},
    },
};
use pi_ai::{
    BoxFuture, InMemoryCredentialStore,
    faux::{FauxResponseStep, create_faux_provider, faux_text_message},
};
use pi_runtime::CreateAgentSessionOptions;
use pi_session::SessionManager;
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const ENDPOINT: &str = "http://127.0.0.1:9411/telemetry";
struct Fixture(PathBuf);
impl Fixture {
    fn new(enabled: bool) -> Self {
        let path = std::env::temp_dir().join(format!("cst-telemetry-sdk-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("agent/home")).unwrap();
        let f = Self(path.canonicalize().unwrap());
        fs::write(
            f.file("telemetry.json"),
            json!({"enabled":enabled,"endpoints":[ENDPOINT]}).to_string(),
        )
        .unwrap();
        f
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join("agent/home").join(name)
    }
    fn resources(&self) {
        fs::write(self.file("APPEND_SYSTEM.md"), "仅测试。").unwrap();
        for name in SKILL_NAMES {
            let p = self.file(&format!("skills/{name}/SKILL.md"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(
                p,
                format!("---\nname: {name}\ndescription: 测试\n---\n测试。"),
            )
            .unwrap();
        }
    }
    fn outbox(&self) -> Outbox {
        Outbox::new(&self.0, Owner::global().unwrap()).unwrap()
    }
    fn draft(&self, id: &str) -> Value {
        serde_json::from_slice(
            &fs::read(self.file(&format!("telemetry/drafts/{id}.json"))).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    delay: Duration,
}
impl EnvironmentSource for Probe {
    fn probe(&self) -> SystemProbe {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        SystemProbe {
            machine_guid: Some("FAKE-ONLY".into()),
            name: Some("Test OS".into()),
            is_pe: Some(false),
            ..Default::default()
        }
    }
}
#[derive(Default)]
struct Network {
    calls: AtomicUsize,
    bodies: Mutex<Vec<Value>>,
    fail: AtomicBool,
    block: AtomicBool,
}
impl Transport for Network {
    fn request(
        &self,
        request: Request,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Response, FailureKind>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.block.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err(FailureKind::Timeout);
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(FailureKind::Network);
            }
            let body = if request.method == Method::Get {
                json!({"protocols":["0.2"],"capabilities":["session-snapshots","revision-acks","heartbeat"]})
            } else {
                let body: Value = serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
                self.bodies.lock().unwrap().push(body.clone());
                json!({"v":"0.2","acks":body["records"].as_array().unwrap().iter().map(|r|json!({"recordId":r["recordId"],"revision":r["revision"],"status":"stored"})).collect::<Vec<_>>(),"heartbeatAck":{"runId":body["heartbeat"]["runId"],"sequence":body["heartbeat"]["sequence"],"status":"stored"}})
            };
            Ok(Response {
                status: if request.method == Method::Get {
                    200
                } else {
                    202
                },
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    }
}
#[derive(Default)]
struct Lookup {
    calls: AtomicUsize,
    block: AtomicBool,
}
impl BillingLookup for Lookup {
    fn lookup<'a>(
        &'a self,
        ids: &'a [String],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.block.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err("cancelled".into());
            }
            Ok(ids.iter().map(|id| (id.clone(), Some(0))).collect())
        })
    }
}
fn options(probe: Arc<Probe>, network: Arc<Network>) -> TelemetryOptions {
    TelemetryOptions {
        environment: probe,
        transport: network,
        clock: Arc::new(SystemClock),
        kit_version: "test-kit".into(),
    }
}
async fn service(
    f: &Fixture,
    probe: Arc<Probe>,
    network: Arc<Network>,
    lookup: Arc<Lookup>,
) -> Arc<TelemetryService> {
    TelemetryService::load(
        &f.0,
        Arc::new(InMemoryCredentialStore::new()),
        lookup,
        options(probe, network),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn exact_float_roundtrip_keeps_same_revision_draft_and_ack_content_stable() {
    use cst_pilot::telemetry::{draft::Drafts, outbox::QueuedRecord};
    let f = Fixture::new(true);
    let record = QueuedRecord::new(json!({"v":"0.2","type":"session","recordId":"float-test","revision":1,"final":true,"cost":9.999999999999999e-6})).unwrap();
    let cancel = CancellationToken::new();
    let drafts = Drafts::new(&f.0, Owner::global().unwrap()).unwrap();
    drafts.save(&record, 1, &[], &cancel).unwrap();
    drafts.save(&record, 1, &[], &cancel).unwrap();
    let outbox = f.outbox();
    outbox.append(&record, Some(ENDPOINT), &cancel).unwrap();
    assert_eq!(
        outbox.read_all(Some(ENDPOINT)).unwrap(),
        vec![record.clone()]
    );
    outbox
        .acknowledge(&[record], Some(ENDPOINT), &cancel)
        .unwrap();
    assert!(outbox.read_all(Some(ENDPOINT)).unwrap().is_empty());
}

#[tokio::test]
async fn event_snapshots_freeze_before_probe_and_deduplicate_unchanged_periodic_stats() {
    let f = Fixture::new(true);
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let probe = Arc::new(Probe {
        delay: Duration::from_millis(400),
        ..Default::default()
    });
    let before = Instant::now();
    let service = service(&f, probe.clone(), network, Arc::default()).await;
    let s = service.instance("session", "new", "web");
    s.update(|c| c.input("rpc", pi_ai::now_ms()));
    let first = s.checkpoint();
    s.update(|c| c.input("rpc", pi_ai::now_ms()));
    assert!(before.elapsed() < Duration::from_millis(300));
    first.await.unwrap();
    let draft = f.draft(&s.record_id);
    assert_eq!(draft["record"]["prompts"], 1);
    assert_eq!(draft["record"]["os"]["name"], "Test OS");
    assert_eq!(draft["record"]["os"]["isPE"], false);
    assert!(draft["record"]["os"].get("isPe").is_none());
    s.submit().await.unwrap();
    let a = f.draft(&s.record_id)["record"]["revision"]
        .as_u64()
        .unwrap();
    s.submit().await.unwrap();
    assert_eq!(f.draft(&s.record_id)["record"]["revision"], a);
    assert_eq!(
        f.outbox().read_all(Some(ENDPOINT)).unwrap()[0].value()["prompts"],
        2
    );
    s.finalize("delete", None, Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(service.active_instances(), 0);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn final_billing_uses_new_revision_and_final_retry_never_mutates_sealed_content() {
    let f = Fixture::new(true);
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let lookup = Arc::new(Lookup::default());
    let service = service(&f, Arc::default(), network, lookup.clone()).await;
    let s = service.instance("session", "resume", "web");
    s.update(|c| {
        c.input("rpc", pi_ai::now_ms());
        c.before_request("cstoa", "high", pi_ai::now_ms());
        c.after_response(
            200,
            &BTreeMap::from([("x-oneapi-request-id".into(), "private-request-id".into())]),
            pi_ai::now_ms(),
        );
    });
    s.submit().await.unwrap();
    let intermediate = f.outbox().read_all(Some(ENDPOINT)).unwrap()[0].clone();
    assert_eq!(
        intermediate.value()["cstoaBilling"]["quotaPoints"],
        Value::Null
    );
    let path = f.outbox().path(Some(ENDPOINT));
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        s.finalize("quit", Some(9), Instant::now() + Duration::from_secs(1))
            .await
            .is_err()
    );
    let sealed = f.draft(&s.record_id)["record"].clone();
    assert_eq!(sealed["final"], true);
    assert_eq!(sealed["cstoaBilling"]["quotaPoints"], 0);
    assert!(sealed["revision"].as_u64().unwrap() > intermediate.revision() + 1);
    let count = lookup.calls.load(Ordering::SeqCst);
    fs::remove_dir(&path).unwrap();
    s.finalize("delete", Some(1), Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    let final_record = f.outbox().read_all(Some(ENDPOINT)).unwrap()[0]
        .value()
        .clone();
    assert_eq!(final_record, sealed);
    assert_eq!(lookup.calls.load(Ordering::SeqCst), count);
    assert!(!final_record.to_string().contains("private-request-id"));
    assert!(
        !f.file(&format!("telemetry/drafts/{}.json", s.record_id))
            .exists()
    );
}
#[tokio::test]
async fn a_failing_checkpoint_does_not_poison_later_serial_operations() {
    let f = Fixture::new(true);
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let service = service(&f, Arc::default(), network, Arc::default()).await;
    service.run_id().await.unwrap();
    let s = service.instance("session", "new", "web");
    s.update(|c| c.input("rpc", pi_ai::now_ms()));
    fs::create_dir_all(f.file("telemetry/drafts")).unwrap();
    let p = f.file(&format!("telemetry/drafts/{}.json", s.record_id));
    fs::write(&p, b"damaged-original").unwrap();
    assert!(s.checkpoint().await.is_err());
    assert_eq!(fs::read(&p).unwrap(), b"damaged-original");
    fs::remove_file(p).unwrap();
    s.submit().await.unwrap();
    assert_eq!(f.draft(&s.record_id)["record"]["prompts"], 1);
    s.finalize("quit", None, Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}
#[tokio::test]
async fn empty_instances_never_create_a_session_record() {
    let f = Fixture::new(true);
    let network = Arc::new(Network::default());
    let service = service(&f, Arc::default(), network.clone(), Arc::default()).await;
    let s = service.instance("empty", "new", "web");
    s.submit().await.unwrap();
    s.finalize("quit", None, Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(service.active_instances(), 0);
    assert!(f.outbox().read_all(Some(ENDPOINT)).unwrap().is_empty());
    assert!(
        network
            .bodies
            .lock()
            .unwrap()
            .iter()
            .all(|b| b["records"].as_array().unwrap().is_empty())
    );
}
#[tokio::test]
async fn canonical_home_shares_run_probe_and_five_second_close_budget_across_sessions() {
    let f = Fixture::new(true);
    let probe = Arc::new(Probe::default());
    let network = Arc::new(Network::default());
    network.block.store(true, Ordering::SeqCst);
    let lookup = Arc::new(Lookup::default());
    lookup.block.store(true, Ordering::SeqCst);
    let a = service(&f, probe.clone(), network.clone(), lookup.clone()).await;
    let b = service(&f, probe.clone(), network, lookup.clone()).await;
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.run_id().await.unwrap(), b.run_id().await.unwrap());
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    for id in ["one", "two"] {
        let s = a.instance(id, "new", "web");
        s.update(|c| {
            c.input("rpc", pi_ai::now_ms());
            c.before_request("cstoa", "", pi_ai::now_ms());
            c.after_response(
                200,
                &BTreeMap::from([("x-oneapi-request-id".into(), id.into())]),
                pi_ai::now_ms(),
            );
        });
    }
    let first = a.begin_close();
    assert_eq!(first, b.begin_close());
    let before = Instant::now();
    tokio::join!(a.close_process("quit"), b.close_process("quit"));
    assert!(before.elapsed() < Duration::from_millis(5500));
    a.drain().await;
    assert_eq!(a.active_instances(), 0);
    assert_eq!(f.outbox().read_all(Some(ENDPOINT)).unwrap().len(), 2);
}

async fn auth(f: &Fixture) -> AuthRuntime {
    AuthRuntime::create(
        &f.0,
        Channel::Gui,
        OaClient::new("http://127.0.0.1:9").unwrap(),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn disabled_sdk_telemetry_has_no_probe_owner_identity_files_observers_or_requests() {
    let f = Fixture::new(false);
    f.resources();
    let auth = auth(&f).await;
    let faux = create_faux_provider();
    auth.runtime().register_provider(faux.provider.clone());
    let probe = Arc::new(Probe::default());
    let network = Arc::new(Network::default());
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_services(
            CreateAgentSessionOptions {
                model: Some(faux.model.clone()),
                session_manager: Some(
                    SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
                ),
                ..Default::default()
            },
            auth,
            options(probe.clone(), network.clone()),
        )
        .await
        .unwrap();
    assert!(session.telemetry().is_none());
    faux.set_responses(vec![FauxResponseStep::Static(faux_text_message("reply"))]);
    session
        .created
        .session
        .prompt("private prompt", None)
        .await
        .unwrap();
    session.created.session.reload().await.unwrap();
    session.shutdown().await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(network.calls.load(Ordering::SeqCst), 0);
    assert!(!f.file("telemetry").exists());
}
#[tokio::test]
async fn real_sdk_two_rounds_and_reload_keep_one_record_and_do_not_export_message_bodies() {
    let f = Fixture::new(true);
    f.resources();
    let auth = auth(&f).await;
    let faux = create_faux_provider();
    auth.runtime().register_provider(faux.provider.clone());
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_services(
            CreateAgentSessionOptions {
                model: Some(faux.model.clone()),
                session_manager: Some(
                    SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
                ),
                ..Default::default()
            },
            auth,
            options(Arc::default(), network.clone()),
        )
        .await
        .unwrap();
    let service = session.telemetry().unwrap();
    faux.set_responses(vec![
        FauxResponseStep::Static(faux_text_message("private assistant one")),
        FauxResponseStep::Static(faux_text_message("private assistant two")),
    ]);
    session
        .created
        .session
        .prompt("private user one", None)
        .await
        .unwrap();
    service.drain().await;
    let first = f.outbox().read_all(Some(ENDPOINT)).unwrap()[0].clone();
    assert_eq!(first.value()["prompts"], 1);
    assert_eq!(first.value()["turns"], 1);
    session.created.session.reload().await.unwrap();
    assert_eq!(service.active_instances(), 1);
    session
        .created
        .session
        .prompt("private user two", None)
        .await
        .unwrap();
    service.drain().await;
    let second = f.outbox().read_all(Some(ENDPOINT)).unwrap()[0].clone();
    assert_eq!(first.id(), second.id());
    assert_eq!(second.value()["prompts"], 2);
    assert_eq!(second.value()["turns"], 2);
    assert!(second.revision() > first.revision());
    session.shutdown().await.unwrap();
    assert_eq!(service.active_instances(), 0);
    let last = f.outbox().read_all(Some(ENDPOINT)).unwrap()[0].clone();
    assert_eq!(last.value()["final"], true);
    assert_eq!(last.value()["endReason"], "quit");
    assert!(!last.value().to_string().contains("private "));
}
#[tokio::test]
async fn real_sdk_session_switch_and_tool_results_use_separate_instance_records() {
    use pi_ai::{
        StopReason,
        faux::{faux_assistant_message, faux_tool_call},
    };
    let f = Fixture::new(true);
    f.resources();
    let auth = auth(&f).await;
    let faux = create_faux_provider();
    auth.runtime().register_provider(faux.provider.clone());
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_services(
            CreateAgentSessionOptions {
                model: Some(faux.model.clone()),
                session_manager: Some(
                    SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
                ),
                ..Default::default()
            },
            auth,
            options(Arc::default(), network),
        )
        .await
        .unwrap();
    let service = session.telemetry().unwrap();
    faux.set_responses(vec![FauxResponseStep::Static(faux_assistant_message(vec![faux_tool_call("runbook",json!({"title":"测试","level":"safe","items":[{"shell":"cmd","summary":"测试说明","command":"echo private-command"}]}).as_object().unwrap().clone(),Some("tool-test".into()))],Some(StopReason::ToolUse))),FauxResponseStep::Static(faux_text_message("first")),FauxResponseStep::Static(faux_text_message("second"))]);
    session
        .created
        .session
        .prompt("first private prompt", None)
        .await
        .unwrap();
    service.drain().await;
    let first = f.outbox().read_all(Some(ENDPOINT)).unwrap()[0].clone();
    assert_eq!(first.value()["turns"], 2);
    assert_eq!(first.value()["tools"][0]["name"], "runbook");
    assert_eq!(first.value()["tools"][0]["calls"], 1);
    assert_eq!(first.value()["tools"][0]["entriesTotal"], 1);
    assert!(session.created.session.new_session(None).await.unwrap());
    session
        .created
        .session
        .prompt("second private prompt", None)
        .await
        .unwrap();
    service.drain().await;
    session.finalize_telemetry("delete").await;
    session.shutdown().await.unwrap();
    let records = f.outbox().read_all(Some(ENDPOINT)).unwrap();
    assert_eq!(records.len(), 2);
    let old = records.iter().find(|r| r.id() == first.id()).unwrap();
    let new = records.iter().find(|r| r.id() != first.id()).unwrap();
    assert_eq!(old.value()["endReason"], "new");
    assert_eq!(old.value()["prompts"], 1);
    assert_eq!(new.value()["endReason"], "delete");
    assert_eq!(new.value()["prompts"], 1);
    assert_ne!(old.value()["sessionId"], new.value()["sessionId"]);
    assert!(
        !serde_json::to_string(&records.iter().map(|r| r.value()).collect::<Vec<_>>())
            .unwrap()
            .contains("private")
    );
}

#[tokio::test]
async fn sdk_creation_does_not_wait_for_slow_environment_and_drop_seals_recoverable_crash() {
    let f = Fixture::new(true);
    f.resources();
    let auth = auth(&f).await;
    let faux = create_faux_provider();
    auth.runtime().register_provider(faux.provider.clone());
    let probe = Arc::new(Probe {
        delay: Duration::from_secs(1),
        ..Default::default()
    });
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let before = Instant::now();
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_services(
            CreateAgentSessionOptions {
                model: Some(faux.model.clone()),
                session_manager: Some(
                    SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
                ),
                ..Default::default()
            },
            auth,
            options(probe, network),
        )
        .await
        .unwrap();
    assert!(before.elapsed() < Duration::from_millis(800));
    faux.set_responses(vec![FauxResponseStep::Static(faux_text_message("done"))]);
    session.created.session.prompt("test", None).await.unwrap();
    let service = session.telemetry().unwrap();
    drop(session);
    service.drain().await;
    wait_for(|| service.active_instances() == 0).await;
    let records = f.outbox().read_all(Some(ENDPOINT)).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].value()["endReason"], "crash");
}

#[tokio::test]
async fn actual_sdk_http_response_id_is_looked_up_locally_and_only_totals_are_uploaded() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let f = Fixture::new(true);
    f.resources();
    fs::write(
        f.file("auth.json"),
        json!({"cstoa":{"type":"api_key","key":"synthetic-model-key"}}).to_string(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = format!("http://{}", listener.local_addr().unwrap());
    let billing_calls = Arc::new(AtomicUsize::new(0));
    let calls = billing_calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let length = String::from_utf8_lossy(&bytes[..end])
                        .lines()
                        .find_map(|s| {
                            let (k, v) = s.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
                assert!(bytes.len() < 1024 * 1024);
            }
            let text = String::from_utf8(bytes).unwrap();
            let path = text.lines().next().unwrap();
            let (kind, body) = if path.contains("/models ") {
                (
                    "application/json",
                    json!({"data":[{"id":"test-chat"}]}).to_string(),
                )
            } else if path.contains("/billing ") {
                assert!(text.contains("local-only-request-id"));
                calls.fetch_add(1, Ordering::SeqCst);
                ("application/json",json!({"result":true,"data":{"unit":"quota_point","requests":[{"requestId":"local-only-request-id","quotaPoints":0}]}}).to_string())
            } else {
                assert!(path.contains("/chat/completions "));
                (
                    "text/event-stream",
                    format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"id":"chat","object":"chat.completion.chunk","created":1,"model":"test-chat","choices":[{"index":0,"delta":{"role":"assistant","content":"private reply"},"finish_reason":null}]}),
                        json!({"id":"chat","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}})
                    ),
                )
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nx-oneapi-request-id: local-only-request-id\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Server(server);
    let auth = AuthRuntime::create(&f.0, Channel::Gui, OaClient::new(&host).unwrap())
        .await
        .unwrap();
    assert!(auth.refresh(true).await.errors.is_empty());
    let model = auth.runtime().get_model("cstoa", "test-chat").unwrap();
    let network = Arc::new(Network::default());
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_services(
            CreateAgentSessionOptions {
                model: Some(model),
                session_manager: Some(
                    SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap(),
                ),
                ..Default::default()
            },
            auth,
            options(Arc::default(), network.clone()),
        )
        .await
        .unwrap();
    session
        .created
        .session
        .prompt("private input", None)
        .await
        .unwrap();
    wait_for(|| billing_calls.load(Ordering::SeqCst) > 0).await;
    session.shutdown().await.unwrap();
    let service = session.telemetry().unwrap();
    service.flush(true).await;
    wait_for(|| {
        network.bodies.lock().unwrap().iter().any(|b| {
            b["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["final"] == true)
        })
    })
    .await;
    let bodies = network.bodies.lock().unwrap().clone();
    let records = bodies
        .iter()
        .flat_map(|b| b["records"].as_array().unwrap())
        .collect::<Vec<_>>();
    let final_record = records.iter().rev().find(|r| r["final"] == true).unwrap();
    assert_eq!(final_record["cstoaBilling"]["requests"], 1);
    assert_eq!(final_record["cstoaBilling"]["settledRequests"], 1);
    assert_eq!(final_record["cstoaBilling"]["quotaPoints"], 0);
    assert_eq!(final_record["models"][0]["totalTokens"], 5);
    assert_eq!(final_record["models"][0]["cost"], Value::Null);
    assert!(
        !serde_json::to_string(&bodies)
            .unwrap()
            .contains("local-only-request-id")
    );
    assert!(!serde_json::to_string(&bodies).unwrap().contains("private"));
}

#[tokio::test]
async fn sdk_fork_and_resume_finalize_each_runtime_instance_without_recounting_history() {
    use pi_agent::AgentMessage;
    use pi_session::{ForkSessionOptions, SessionEntry};
    let f = Fixture::new(true);
    f.resources();
    let auth = auth(&f).await;
    let faux = create_faux_provider();
    auth.runtime().register_provider(faux.provider.clone());
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let manager = SessionManager::create(f.0.to_string_lossy(), f.file("sessions"), None).unwrap();
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_services(
            CreateAgentSessionOptions {
                model: Some(faux.model.clone()),
                session_manager: Some(manager),
                ..Default::default()
            },
            auth,
            options(Arc::default(), network),
        )
        .await
        .unwrap();
    let service = session.telemetry().unwrap();
    faux.set_responses((0..3).map(|_| FauxResponseStep::Static(faux_text_message("answer"))));
    session.created.session.prompt("first", None).await.unwrap();
    service.drain().await;
    let original = session.created.session.session_file().unwrap();
    let target=session.created.session.with_session_manager(|sm|sm.get_entries().into_iter().find(|entry|matches!(entry,SessionEntry::Message(m) if matches!(m.message,AgentMessage::User(_)))).unwrap().id().to_owned());
    assert!(
        session
            .created
            .session
            .fork(&target, ForkSessionOptions::default())
            .await
            .unwrap()
    );
    session
        .created
        .session
        .prompt("forked", None)
        .await
        .unwrap();
    service.drain().await;
    assert!(
        session
            .created
            .session
            .switch_session(original)
            .await
            .unwrap()
    );
    session
        .created
        .session
        .prompt("resumed", None)
        .await
        .unwrap();
    session.shutdown().await.unwrap();
    service.drain().await;
    let records = f.outbox().read_all(Some(ENDPOINT)).unwrap();
    assert_eq!(records.len(), 3);
    for r in &records {
        assert_eq!(r.value()["prompts"], 1);
        assert_eq!(r.value()["turns"], 1);
        assert_eq!(r.value()["final"], true);
    }
    for reason in ["fork", "resume", "quit"] {
        assert!(records.iter().any(|r| r.value()["endReason"] == reason));
    }
}

#[tokio::test]
async fn startup_recovery_freezes_old_environment_and_hints_retry_without_closing() {
    let f = Fixture::new(true);
    fs::create_dir_all(f.file("telemetry/drafts")).unwrap();
    fs::write(f.file("telemetry/drafts/old.json"),json!({"record":{"v":"0.2","type":"session","recordId":"old","sessionId":"old-session","revision":3,"final":false,"prompts":1,"startedAt":"2026-11-08T00:00:00+08:00","endReason":"","identity":{"environmentId":"old-machine"},"os":{"name":"old-os"}},"lastTurnEndedAt":1794067201000u64}).to_string()).unwrap();
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let service = service(&f, Arc::default(), network.clone(), Arc::default()).await;
    wait_for(|| {
        f.outbox()
            .read_all(Some(ENDPOINT))
            .is_ok_and(|v| v.iter().any(|r| r.id() == "old"))
    })
    .await;
    let recovered = f
        .outbox()
        .read_all(Some(ENDPOINT))
        .unwrap()
        .into_iter()
        .find(|r| r.id() == "old")
        .unwrap();
    assert_eq!(recovered.revision(), 4);
    assert_eq!(recovered.value()["endReason"], "crash");
    assert_eq!(
        recovered.value()["identity"]["environmentId"],
        "old-machine"
    );
    let s = service.instance("active", "new", "web");
    s.update(|c| c.input("rpc", pi_ai::now_ms()));
    network.fail.store(false, Ordering::SeqCst);
    service.hint();
    service.hint();
    wait_for(|| {
        network.bodies.lock().unwrap().iter().any(|b| {
            b["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["recordId"] == s.record_id)
        })
    })
    .await;
    assert_eq!(service.active_instances(), 1);
    let snapshot = f.draft(&s.record_id);
    assert_eq!(snapshot["record"]["final"], false);
    service.close_process("quit").await;
}

#[tokio::test]
async fn sdk_compaction_observer_keeps_reason_tokens_and_survives_reload_without_duplicates() {
    use pi_agent::AgentMessage;
    use pi_ai::UserMessage;
    let f = Fixture::new(true);
    f.resources();
    fs::write(
        f.file("settings.json"),
        json!({"compaction":{"enabled":false,"reserveTokens":1024,"keepRecentTokens":100}})
            .to_string(),
    )
    .unwrap();
    let auth = auth(&f).await;
    let faux = create_faux_provider();
    auth.runtime().register_provider(faux.provider.clone());
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let mut manager = SessionManager::in_memory(f.0.to_string_lossy(), None, None).unwrap();
    for _ in 0..8 {
        manager
            .append_message(AgentMessage::User(UserMessage::new("history ".repeat(500))))
            .unwrap();
        manager
            .append_message(AgentMessage::Assistant(faux_text_message(
                "answer ".repeat(500),
            )))
            .unwrap();
    }
    let session = Kit::new(&f.0)
        .unwrap()
        .create_session_with_services(
            CreateAgentSessionOptions {
                model: Some(faux.model.clone()),
                session_manager: Some(manager),
                ..Default::default()
            },
            auth,
            options(Arc::default(), network),
        )
        .await
        .unwrap();
    faux.set_responses(
        (0..6).map(|_| FauxResponseStep::Static(faux_text_message("## Goal\nSummarized."))),
    );
    session
        .created
        .session
        .prompt("current", None)
        .await
        .unwrap();
    session.created.session.reload().await.unwrap();
    session.created.session.compact(None).await.unwrap();
    session.shutdown().await.unwrap();
    let records = f.outbox().read_all(Some(ENDPOINT)).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].value()["compactions"], 1);
    assert!(records[0].value()["compactionTokens"].as_u64().unwrap() > 0);
    assert_eq!(records[0].value()["compactionOverflows"], 0);
    assert_eq!(records[0].value()["prompts"], 1);
}

#[tokio::test]
async fn shared_thirty_second_timer_submits_active_instances_without_foreground_events() {
    let f = Fixture::new(true);
    fs::write(f.0.join("VERSION"), "v2.3.4\n").unwrap();
    let network = Arc::new(Network::default());
    network.fail.store(true, Ordering::SeqCst);
    let mut settings = options(Arc::default(), network);
    settings.kit_version.clear();
    let service = TelemetryService::load(
        &f.0,
        Arc::new(InMemoryCredentialStore::new()),
        Arc::new(Lookup::default()),
        settings,
    )
    .await
    .unwrap()
    .unwrap();
    service.run_id().await.unwrap();
    let s = service.instance("timer", "new", "web");
    s.update(|c| c.input("rpc", pi_ai::now_ms()));
    let wait = async {
        loop {
            if f.outbox()
                .read_all(Some(ENDPOINT))
                .is_ok_and(|records| !records.is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(35), wait)
        .await
        .unwrap();
    let record = f.outbox().read_all(Some(ENDPOINT)).unwrap().remove(0);
    assert_eq!(record.value()["prompts"], 1);
    assert_eq!(record.value()["final"], false);
    assert_eq!(record.value()["kitVersion"], "v2.3.4");
    s.finalize("quit", None, Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    service.close_process("quit").await;
}

#[test]
fn runtime_shutdown_drops_pending_submission_tasks_without_retaining_the_home_service() {
    let f = Fixture::new(true);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let weak = runtime.block_on(async {
        let network = Arc::new(Network::default());
        network.block.store(true, Ordering::SeqCst);
        let service = service(
            &f,
            Arc::new(Probe {
                delay: Duration::from_millis(200),
                ..Default::default()
            }),
            network,
            Arc::default(),
        )
        .await;
        let weak = Arc::downgrade(&service);
        let s = service.instance("shutdown", "new", "web");
        s.update(|c| c.input("rpc", pi_ai::now_ms()));
        drop(s.checkpoint());
        tokio::task::yield_now().await;
        weak
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    assert!(weak.upgrade().is_none());
}

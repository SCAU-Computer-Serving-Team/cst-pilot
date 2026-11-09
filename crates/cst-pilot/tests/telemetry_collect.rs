use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use cst_pilot::auth::{
    AuthRuntime, Channel,
    oa::{OaClient, SystemClock},
};
use cst_pilot::telemetry::{
    PreparedTelemetry,
    billing::{Billing, BillingLookup, OaBilling},
    collect::Collector,
    config::{Config, endpoint_url},
    environment::{Environment, EnvironmentSource, SystemProbe},
    identity::Installation,
};
use cst_pilot::tools::ssrf::{HttpRequest, HttpResponse, HttpTransport};
use pi_ai::{AssistantMessage, BoxFuture, StopReason};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("cst-telemetry-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("agent/home")).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join("agent/home").join(name)
    }
    fn write(&self, name: &str, value: Value) {
        let path = self.file(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value.to_string()).unwrap();
    }
    fn config(&self) -> Config {
        self.write(
            "telemetry.json",
            json!({"enabled":true,"endpoints":[{"url":"http://127.0.0.1:9411/v1/sessions"}]}),
        );
        Config::load(&self.0).unwrap().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Probe(AtomicUsize);
impl EnvironmentSource for Probe {
    fn probe(&self) -> SystemProbe {
        self.0.fetch_add(1, Ordering::SeqCst);
        SystemProbe {
            machine_guid: Some(" TEST-GUID ".into()),
            name: Some("Windows 11 Pro".into()),
            version: "10.0.26100".into(),
            display_version: Some("24H2".into()),
            arch: Some("x64".into()),
            is_pe: Some(false),
            admin: Some(true),
        }
    }
}
fn probe() -> Probe {
    Probe(AtomicUsize::new(0))
}
fn collector(f: &Fixture) -> Collector {
    Collector::new(
        "session-test".into(),
        "new".into(),
        "web".into(),
        1000,
        Environment::capture(None, &probe()),
        "v0.5".into(),
        &f.config(),
    )
}
fn message(provider: &str) -> AssistantMessage {
    let model = pi_ai::builtin_providers()
        .iter()
        .flat_map(|provider| provider.get_models())
        .next()
        .unwrap();
    let mut message = AssistantMessage::new_pending(&model);
    message.provider = provider.into();
    message.model = "test-model".into();
    message.stop_reason = StopReason::Stop;
    message.usage.input = 10;
    message.usage.output = 5;
    message.usage.cache_read = 20;
    message.usage.total_tokens = 35;
    message.usage.cost.total = 0.25;
    message
}
#[test]
fn disabled_missing_or_empty_configuration_does_not_probe_or_initialize() {
    let f = Fixture::new();
    let probe = probe();
    for value in [
        None,
        Some(
            json!({"enabled":false,"endpoints":[{"url":"https://example.com","caFile":"secret.crt"}]}),
        ),
        Some(json!({"endpoints":[]})),
        Some(json!({"enabled":true,"endpoints":["http://192.168.1.1"]})),
    ] {
        if let Some(value) = value {
            f.write("telemetry.json", value);
        }
        assert!(PreparedTelemetry::load(&f.0, &probe).unwrap().is_none());
        assert_eq!(probe.0.load(Ordering::SeqCst), 0);
        assert!(!f.file("telemetry").exists());
    }
    fs::write(f.file("telemetry.json"), "{invalid").unwrap();
    assert!(PreparedTelemetry::load(&f.0, &probe).unwrap().is_none());
    assert_eq!(
        fs::read_to_string(f.file("telemetry.json")).unwrap(),
        "{invalid"
    );
}
#[test]
fn endpoints_are_deduplicated_validated_and_bad_ca_stays_unavailable() {
    let f = Fixture::new();
    for url in [
        "ftp://example.com",
        "http://example.com",
        "https://name:password@example.com",
        "https://example.com/#fragment",
    ] {
        assert!(endpoint_url(url).is_none());
    }
    assert!(endpoint_url("http://[::1]:12345/v1/sessions").is_some());
    f.write(
        "telemetry.json",
        json!({"endpoint":"https://legacy.example/v1/sessions"}),
    );
    assert_eq!(Config::load(&f.0).unwrap().unwrap().endpoints.len(), 1);
    f.write("telemetry.json",json!({"endpoints":["https://example.com","https://example.com/",{"url":"https://other.example","caFile":"../private.crt"},{"url":"https://third.example","caFile":"missing.crt"}],"authProvider":"cstoa"}));
    f.write(
        "extensions/telemetry/currency.json",
        json!({"cstoa":"CNY","invalid":"EUR"}),
    );
    let config = Config::load(&f.0).unwrap().unwrap();
    assert_eq!(config.endpoints.len(), 3);
    assert!(config.endpoints[1].unavailable);
    assert!(config.endpoints[2].unavailable);
    assert_eq!(config.currency_of("cstoa"), "CNY");
    assert_eq!(config.currency_of("custom"), "USD");
    assert!(!f.file("telemetry").exists());
}
#[test]
fn identity_is_persistent_and_damaged_file_is_never_replaced() {
    let f = Fixture::new();
    let id = Installation::load(&f.0).unwrap();
    let saved = fs::read(f.file("telemetry/identity.json")).unwrap();
    let again = Installation::load(&f.0).unwrap();
    assert_eq!(id.installation_id, again.installation_id);
    assert_eq!(id.environment_id("one"), again.environment_id("one"));
    assert_ne!(id.environment_id("one"), id.environment_id("two"));
    assert_eq!(fs::read(f.file("telemetry/identity.json")).unwrap(), saved);
    fs::write(f.file("telemetry/identity.json"), "{damaged-secret").unwrap();
    assert!(Installation::load(&f.0).is_none());
    assert_eq!(
        fs::read_to_string(f.file("telemetry/identity.json")).unwrap(),
        "{damaged-secret"
    );
}
#[test]
fn concurrent_initialization_publishes_exactly_one_identity() {
    let f = Fixture::new();
    let ids = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| Installation::load(&f.0).unwrap().installation_id))
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(ids.iter().all(|id| id == &ids[0]));
}
#[test]
fn node_crypto_golden_matches_hmac_ed25519_spki_and_exact_body() {
    let f = Fixture::new();
    f.write("telemetry/identity.json",json!({"installationId":"550e8400-e29b-41d4-a716-446655440000","identityKey":STANDARD.encode([7u8;32])}));
    let id = Installation::load(&f.0).unwrap();
    assert_eq!(
        id.environment_id(" TEST-GUID "),
        "429f8261b637f14bd52ca537bc59b9da134713557411e79d3a7602a02f0532e1"
    );
    let headers = id.headers(b"{\"records\":[]}", 1794182400000);
    assert_eq!(
        headers["X-Telemetry-Key"],
        "MCowBQYDK2VwAyEAsvdqqOTSH3BS-MOyEIUZXyty9qly-MaJ8mEG_hhYBsQ"
    );
    assert_eq!(
        headers["X-Telemetry-Signature"],
        "ZotYP3vZ7W7N0veojRGjD2z2sj8b4-LglVmzuSucrGKhL5j01QsijRqIDH73Sl9MK9Uqe0h893ZRcK5mEtnBCw"
    );
    assert_ne!(
        headers["X-Telemetry-Signature"],
        id.headers(b"{\"records\":[] }", 1794182400000)["X-Telemetry-Signature"]
    );
}
#[test]
fn snapshot_excludes_guid_secrets_and_preserves_unknown_or_pe_identity() {
    let f = Fixture::new();
    f.config();
    let probe = probe();
    let ready = PreparedTelemetry::load(&f.0, &probe).unwrap().unwrap();
    assert_eq!(probe.0.load(Ordering::SeqCst), 1);
    let text = serde_json::to_string(&ready.environment).unwrap();
    for secret in ["TEST-GUID", "identityKey", "machineGuid"] {
        assert!(!text.contains(secret));
    }
    assert!(ready.environment.identity.environment_id.is_some());
    struct Pe;
    impl EnvironmentSource for Pe {
        fn probe(&self) -> SystemProbe {
            SystemProbe {
                is_pe: Some(true),
                machine_guid: Some("should-not-hash".into()),
                ..Default::default()
            }
        }
    }
    let pe = Environment::capture(ready.installation.as_ref(), &Pe);
    assert!(pe.identity.environment_id.is_none());
    assert_eq!(pe.identity.environment_source, "unavailable");
    assert_eq!(
        pe.identity.installation_id,
        ready.environment.identity.installation_id
    );
    assert_eq!(ready.environment.runtime.version, env!("CARGO_PKG_VERSION"));
}
#[test]
fn identity_and_configuration_refuse_hardlinks_and_junctions() {
    let f = Fixture::new();
    fs::create_dir_all(f.file("telemetry")).unwrap();
    let original = f.0.join("original.json");
    fs::write(&original, "private-original").unwrap();
    fs::hard_link(&original, f.file("telemetry/identity.json")).unwrap();
    assert!(Installation::load(&f.0).is_none());
    assert_eq!(fs::read_to_string(original).unwrap(), "private-original");
    fs::hard_link(f.file("telemetry/identity.json"), f.file("telemetry.json")).unwrap();
    assert!(Config::load(&f.0).is_err());
    #[cfg(windows)]
    {
        let second = Fixture::new();
        let outside = Fixture::new();
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(second.file("telemetry"))
            .arg(&outside.0)
            .output()
            .unwrap();
        assert!(status.status.success());
        assert!(Installation::load(&second.0).is_none());
        assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 1);
    }
}
#[test]
fn models_tokens_cost_errors_and_context_are_cumulative() {
    let f = Fixture::new();
    let mut state = collector(&f);
    state.input("extension", 1100);
    state.input("interactive", 1100);
    state.input("rpc", 1150);
    state.turn_start(1200);
    state.before_request("cstoa", "high", 1200);
    state.after_response(
        200,
        &BTreeMap::from([("X-OneAPI-Request-ID".into(), "request-local-only".into())]),
        1250,
    );
    let mut m = message("cstoa");
    m.thinking_level = Some(pi_ai::ModelThinkingLevel::Low);
    state.turn_end(Some(&m), Some((80, 32000)), 1300);
    m = message("custom");
    m.stop_reason = StopReason::Error;
    m.error_message = Some("exact provider error\nnot truncated".into());
    state.turn_start(1400);
    state.before_request("custom", "off", 1400);
    state.after_response(429, &BTreeMap::new(), 1410);
    state.turn_end(Some(&m), Some((50, 99999)), 1450);
    state.turn_start(1500);
    state.before_request("custom", "off", 1500);
    state.turn_end(Some(&m), None, 1530);
    state.compact(100, true);
    state.compact_failed();
    let record = state.snapshot(2, false, "", 2000, Some(5));
    assert_eq!(record.metrics.prompts, 2);
    assert_eq!(record.metrics.turns, 3);
    assert_eq!(record.metrics.active_ms, 180);
    assert_eq!(record.metrics.network_errors, 1);
    assert_eq!(record.metrics.provider_errors["429"], 1);
    assert_eq!(record.metrics.context_peak, 80);
    assert_eq!(record.metrics.context_window, 32000);
    assert_eq!(record.metrics.compaction_tokens, 100);
    assert_eq!(record.metrics.compaction_overflows, 1);
    assert_eq!(record.metrics.compaction_failures, 1);
    let oa = record
        .metrics
        .models
        .iter()
        .find(|m| m.provider == "cstoa")
        .unwrap();
    assert_eq!(oa.cost, None);
    assert_eq!(oa.thinking_level, "high");
    assert_eq!(oa.total_tokens, 35);
    let custom = record
        .metrics
        .models
        .iter()
        .find(|m| m.provider == "custom")
        .unwrap();
    assert_eq!(custom.cost, Some(0.5));
    assert_eq!(custom.incomplete_usage_turns, 2);
    assert_eq!(record.cstoa_billing.as_ref().unwrap().quota_points, None);
    let text = serde_json::to_string(&record).unwrap();
    assert!(!text.contains("request-local-only"));
    assert!(text.contains("exact provider error\\nnot truncated"));
    assert_eq!(record.metrics.errors[0].count, 2);
}
#[test]
fn tool_metrics_count_once_and_omit_all_business_arguments_and_body() {
    let f = Fixture::new();
    let mut state = collector(&f);
    let input = json!({"scope":"usage","path":"private-path","query":"private-query","recoveryPassword":"private-password"});
    state.tool_start("1", "disk", &input, 1100);
    state.tool_start("1", "disk", &input, 1100);
    let result=cst_pilot::diagnostics::result::diagnostic_result(json!({"usage":{"method":"node-walk","degradedFrom":"wiztree"},"degraded":true,"outputTruncated":true,"private":"private-result-body"})).unwrap();
    state.tool_end("1", &result, true, Some(40), 1200);
    state.tool_end("1", &result, true, None, 1300);
    state.tool_start(
        "2",
        "runbook_deliver",
        &json!({"runbookId":"private-id"}),
        1400,
    );
    state.tool_end(
        "2",
        &cst_pilot::diagnostics::result::diagnostic_result(json!({"runbook":{"items":3}})).unwrap(),
        false,
        None,
        1410,
    );
    state.tool_start("3", "sys", &json!({"scope":"private-scope-argument"}), 1420);
    state.tool_end("3", &result, false, None, 1430);
    let record = state.snapshot(1, true, "quit", 2000, None);
    let disk = record
        .metrics
        .tools
        .iter()
        .find(|t| t.name == "disk")
        .unwrap();
    assert_eq!(disk.calls, 1);
    assert_eq!(disk.failures, 1);
    assert_eq!(disk.degraded, 1);
    assert_eq!(disk.truncated, 1);
    assert_eq!(disk.total_ms, 40);
    assert_eq!(disk.max_ms, 40);
    assert!(disk.result_bytes > 0);
    let rb = record
        .metrics
        .tools
        .iter()
        .find(|t| t.name == "runbook_deliver")
        .unwrap();
    assert_eq!(rb.entries_total, Some(3));
    assert_eq!(
        record
            .metrics
            .tools
            .iter()
            .find(|t| t.name == "sys")
            .unwrap()
            .scope
            .as_deref(),
        Some("overview")
    );
    assert_eq!(record.metrics.tool_failures, 1);
    let text = serde_json::to_string(&record).unwrap();
    for secret in [
        "private-path",
        "private-query",
        "private-password",
        "private-result-body",
        "private-id",
        "private-scope-argument",
    ] {
        assert!(!text.contains(secret), "{secret}");
    }
}
#[test]
fn error_groups_and_model_tool_cardinality_are_bounded() {
    let f = Fixture::new();
    let mut state = collector(&f);
    for i in 0..40 {
        let mut m = message("test");
        m.model = format!("model-{i}");
        m.stop_reason = StopReason::Error;
        m.error_message = Some(format!("error-{i}"));
        state.turn_start(2000);
        state.before_request("test", "off", 2000);
        state.turn_end(Some(&m), None, 2010);
    }
    let mut frequent = message("test");
    frequent.stop_reason = StopReason::Error;
    frequent.error_message = Some("frequent-secret-original".repeat(1000));
    for _ in 0..5 {
        state.turn_start(2020);
        state.turn_end(Some(&frequent), None, 2030);
    }
    for i in 0..520 {
        state.tool_start(&format!("tool-{i}"), &format!("name-{i}"), &json!({}), 2100);
        state.tool_end(
            &format!("tool-{i}"),
            &cst_pilot::diagnostics::result::diagnostic_result(json!({})).unwrap(),
            true,
            None,
            2200,
        );
    }
    let record = state.snapshot(1, false, "", 3000, None);
    assert_eq!(record.metrics.models.len(), 32);
    assert_eq!(record.metrics.tools.len(), 512);
    assert_eq!(record.metrics.tool_failures, 520);
    assert_eq!(record.metrics.errors.len(), 10);
    assert_eq!(record.metrics.errors[0].count, 5);
    assert_eq!(
        record.metrics.errors[0].message.len(),
        frequent.error_message.unwrap().len()
    );
}
#[test]
fn crashes_use_saved_last_activity_and_keep_frozen_environment() {
    let f = Fixture::new();
    let mut state = collector(&f);
    assert!(!state.has_activity());
    state.turn_start(1100);
    state.turn_end(Some(&message("cstoa")), None, 1200);
    let first = state.snapshot(2, false, "", 1500, None);
    let before = serde_json::to_value(&first.environment).unwrap();
    let crash = first.crash(state.last_activity);
    assert_eq!(crash.revision, 3);
    assert!(crash.final_state);
    assert_eq!(crash.end_reason, "crash");
    assert_eq!(crash.duration_ms, 200);
    assert_eq!(serde_json::to_value(crash.environment).unwrap(), before);
    assert!(chrono::DateTime::parse_from_rfc3339(&crash.ended_at).is_ok());
}
struct Lookup {
    batches: Mutex<Vec<usize>>,
    block_after: usize,
}
impl BillingLookup for Lookup {
    fn lookup<'a>(
        &'a self,
        ids: &'a [String],
        _: CancellationToken,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Option<u64>>, String>> {
        let mut batches = self.batches.lock().unwrap();
        batches.push(ids.len());
        let block = batches.len() > self.block_after;
        drop(batches);
        Box::pin(async move {
            if block {
                std::future::pending::<()>().await;
            }
            Ok(ids.iter().map(|id| (id.clone(), Some(0))).collect())
        })
    }
}
#[tokio::test]
async fn billing_deduplicates_requests_batches_fifty_and_preserves_unknowns() {
    let billing = Billing::default();
    for i in 0..102 {
        let at = billing.start();
        billing.finish(at, Some(&format!("id-{i}")));
    }
    let duplicate = billing.start();
    billing.finish(duplicate, Some("id-0"));
    billing.start();
    assert_eq!(billing.summary().unwrap().requests, 103);
    assert_eq!(billing.summary().unwrap().quota_points, None);
    let lookup = Lookup {
        batches: Mutex::new(Vec::new()),
        block_after: usize::MAX,
    };
    billing
        .settle(&lookup, CancellationToken::new(), Duration::from_secs(1))
        .await;
    assert_eq!(*lookup.batches.lock().unwrap(), vec![50, 50, 2]);
    let summary = billing.summary().unwrap();
    assert_eq!(summary.settled_requests, 102);
    assert_eq!(summary.quota_points, Some(0));
    let restored = Billing::restore(billing.snapshot());
    assert_eq!(summary, restored.summary().unwrap());
}
#[tokio::test]
async fn billing_total_deadline_and_cancel_do_not_cancel_parent_or_erase_settled() {
    let billing = Billing::default();
    for i in 0..65 {
        let at = billing.start();
        billing.finish(at, Some(&format!("id-{i}")));
    }
    let lookup = Lookup {
        batches: Mutex::new(Vec::new()),
        block_after: 1,
    };
    let parent = CancellationToken::new();
    let start = std::time::Instant::now();
    billing
        .settle(&lookup, parent.clone(), Duration::from_millis(60))
        .await;
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(!parent.is_cancelled());
    assert_eq!(billing.summary().unwrap().settled_requests, 50);
    parent.cancel();
    billing
        .settle(&lookup, parent, Duration::from_millis(60))
        .await;
    assert_eq!(lookup.batches.lock().unwrap().len(), 2);
}
#[derive(Default)]
struct MockHttp {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<(u16, Value)>>,
    switch: Mutex<Option<(PathBuf, Value)>>,
}
impl MockHttp {
    fn reply(&self, status: u16, value: Value) {
        self.replies.lock().unwrap().push_back((status, value));
    }
}
impl HttpTransport for MockHttp {
    fn request(
        &self,
        request: HttpRequest,
        _: CancellationToken,
    ) -> BoxFuture<'_, Result<HttpResponse, String>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front();
        let switch = self.switch.lock().unwrap().take();
        Box::pin(async move {
            if let Some((path, value)) = switch {
                fs::write(path, value.to_string()).unwrap();
            }
            let (status, body) = reply.ok_or("no mock")?;
            Ok(HttpResponse {
                status,
                body: body.to_string(),
            })
        })
    }
}
#[tokio::test]
async fn oa_billing_uses_current_valid_credentials_and_checks_unit_ids_and_rejections() {
    let f = Fixture::new();
    let http = Arc::new(MockHttp::default());
    f.write("auth.json",json!({"cstoa":{"type":"oauth","access":"mock-expired","refresh":"do-not-use","expires":1}}));
    let auth = AuthRuntime::create(
        &f.0,
        Channel::Tui,
        OaClient::with_transport("https://oa.example", http.clone(), Arc::new(SystemClock))
            .unwrap(),
    )
    .await
    .unwrap();
    let lookup = OaBilling::from_auth(&auth);
    let ids = vec!["id-1".into(), "id-2".into(), "duplicate".into()];
    assert!(
        lookup
            .lookup(&ids, CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(http.requests.lock().unwrap().is_empty());
    f.write(
        "auth.json",
        json!({"cstoa":{"type":"api_key","key":"mock-key"}}),
    );
    let saved = fs::read(f.file("auth.json")).unwrap();
    http.reply(200,json!({"result":true,"data":{"unit":"quota_point","requests":[{"requestId":"id-1","quotaPoints":0},{"requestId":"id-2","quotaPoints":8},{"requestId":"unrequested-secret-id","quotaPoints":999},{"requestId":"duplicate","quotaPoints":3},{"requestId":"duplicate","quotaPoints":4}]}}));
    let values = lookup.lookup(&ids, CancellationToken::new()).await.unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values["id-1"], Some(0));
    assert_eq!(values["id-2"], Some(8));
    {
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests[0].url, "https://oa.example/api/agent/llm/billing");
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(key, value)| key == "Authorization" && value == "Bearer mock-key")
        );
    }
    for (status, value) in [
        (
            200,
            json!({"result":true,"data":{"unit":"CNY","requests":[]}}),
        ),
        (401, json!({"error":"mock-key-sensitive-error"})),
        (302, json!({"location":"https://evil.example"})),
    ] {
        http.reply(status, value);
        let result = lookup.lookup(&ids, CancellationToken::new()).await;
        assert!(
            result.as_ref().is_ok_and(|v| v.is_empty())
                || result.as_ref().is_err_and(|e| !e.contains("mock-key"))
        );
    }
    assert_eq!(fs::read(f.file("auth.json")).unwrap(), saved);
    http.reply(200,json!({"result":true,"data":{"unit":"quota_point","requests":[{"requestId":"id-1","quotaPoints":99}]}}));
    *http.switch.lock().unwrap() = Some((
        f.file("auth.json"),
        json!({"cstoa":{"type":"api_key","key":"different-account"}}),
    ));
    assert!(
        lookup
            .lookup(&ids, CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
}

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use pi_agent::AgentMessage;
use pi_ai::*;
use pi_runtime::*;
use pi_session::{CacheWarmingAction, SessionManager, UsageEntry};

fn cached_model() -> Model {
    let mut m = pi_session::unconfigured_model();
    m.provider = "warm-mock".into();
    m.id = "physical".into();
    m.api = known_api::OPENAI_COMPLETIONS.into();
    m.base_url = "https://mock.invalid/v1".into();
    m.context_window = 100_000;
    m.max_tokens = 1024;
    m.prompt_cache = Some(ModelPromptCache {
        short: Some(60.0),
        long: Some(120.0),
    });
    m.cost = ModelCost {
        input: 20.0,
        output: 10.0,
        cache_read: 1.0,
        cache_write: 20.0,
        ..Default::default()
    };
    m
}

async fn eventually(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !check() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("mock work must settle without advancing real time");
}

struct Harness {
    warmer: CacheWarmer,
    request: CacheWarmRequest,
    clock: ManualTestClock,
    current: Arc<AtomicBool>,
    mode: Arc<Mutex<CacheWarmingMode>>,
    calls: Arc<Mutex<Vec<(Model, TranscriptContext, SimpleStreamOptions)>>>,
    pending: Arc<Mutex<Vec<AssistantMessageEventStream>>>,
    hold: Arc<AtomicBool>,
    entries: Arc<Mutex<Vec<UsageEntry>>>,
    errors: Arc<Mutex<Vec<String>>>,
    fail_storage: Arc<AtomicBool>,
}

impl Harness {
    async fn new(decide: Option<DecideWarmingFn>, tokens: u64) -> Self {
        let model = cached_model();
        let rt = ModelRuntime::create(CreateModelRuntimeOptions {
            include_builtins: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let pending = Arc::new(Mutex::new(Vec::new()));
        let hold = Arc::new(AtomicBool::new(false));
        let (capture, waiters, held) = (calls.clone(), pending.clone(), hold.clone());
        rt.register_provider(
            create_provider(CreateProviderOptions {
                id: model.provider.clone(),
                name: None,
                base_url: None,
                headers: None,
                auth: ProviderAuth::api_key(ApiKeyAuth::keyless("warm-mock", "mock-key")),
                models: vec![AnyModel::Chat(model.clone())],
                fetch_models: None,
                filter_models: None,
                classifiers: None,
                api: Some(ProviderApiConfig::Single(ProviderStreams::new(
                    Arc::new(|_, _, _| panic!("simple stream expected")),
                    Arc::new(move |m, c, o| {
                        let o = o.cloned().unwrap();
                        capture.lock().unwrap().push((m.clone(), c.clone(), o));
                        let stream = create_assistant_message_event_stream();
                        if held.load(Ordering::SeqCst) {
                            waiters.lock().unwrap().push(stream.clone());
                        } else {
                            let message = warm_message(m);
                            stream.push(AssistantMessageEvent::Done {
                                reason: StopReason::Stop,
                                message: message.clone(),
                            });
                            stream.end(Some(message));
                        }
                        stream
                    }),
                ))),
            })
            .unwrap(),
        );
        let mut sm = SessionManager::in_memory(".", None, None).unwrap();
        let mut last = AssistantMessage::new_pending(&model);
        last.stop_reason = StopReason::Stop;
        last.usage.input = tokens;
        sm.append_message(AgentMessage::Assistant(last)).unwrap();
        let branch = sm.get_branch(None);
        let entries = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let fail_storage = Arc::new(AtomicBool::new(false));
        let mode = Arc::new(Mutex::new(CacheWarmingMode::Streaming));
        let (saved, failures, mode_ref, fail) = (
            entries.clone(),
            errors.clone(),
            mode.clone(),
            fail_storage.clone(),
        );
        let clock = ManualTestClock::new(0);
        let warmer = CacheWarmer::new(
            rt,
            Arc::new(move || branch.clone()),
            Arc::new(move |kind, provider, model, usage, note, is_current| {
                if !is_current() {
                    return Ok(None);
                }
                if fail.load(Ordering::SeqCst) {
                    return Err("injected disk failure".into());
                }
                let entry = UsageEntry {
                    id: uuid_v7(),
                    parent_id: None,
                    timestamp: String::new(),
                    kind,
                    provider,
                    model,
                    usage,
                    note,
                };
                saved.lock().unwrap().push(entry.clone());
                Ok(Some(entry))
            }),
            Arc::new(move |err| failures.lock().unwrap().push(err)),
            Arc::new(move || *mode_ref.lock().unwrap()),
            decide.unwrap_or_else(|| Arc::new(|event| Box::pin(async move { event.action }))),
            Some(Arc::new(clock.clone())),
        );
        let mut selected = model.clone();
        selected.id = "virtual-selected".into();
        selected.provider = "virtual".into();
        let request = CacheWarmRequest {
            session_id: "session".into(),
            session_generation: 2,
            selection_generation: 3,
            request_seq: 4,
            selected_model: selected,
            physical_model: model,
            transcript: TranscriptContext::default(),
            options: SimpleStreamOptions {
                base: StreamOptions {
                    cache_retention: Some(CacheRetention::Short),
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        Self {
            warmer,
            request,
            clock,
            current: Arc::new(AtomicBool::new(true)),
            mode,
            calls,
            pending,
            hold,
            entries,
            errors,
            fail_storage,
        }
    }
    fn start(&self) {
        let current = self.current.clone();
        self.warmer.start(
            self.request.clone(),
            Arc::new(move || current.load(Ordering::SeqCst)),
        );
    }
    async fn tick(&self) {
        self.clock.advance_ms(50_000);
        eventually(|| {
            !self.entries.lock().unwrap().is_empty()
                || !self.pending.lock().unwrap().is_empty()
                || {
                    let status = self.warmer.status();
                    status.state == "inactive" && status.decision.is_some()
                }
                || !self.errors.lock().unwrap().is_empty()
        })
        .await;
    }
}

fn warm_message(m: &Model) -> AssistantMessage {
    let mut msg = AssistantMessage::new_pending(m);
    msg.stop_reason = StopReason::Stop;
    msg.response_model = Some("physical-response-version".into());
    msg.usage.cache_read = 10_000;
    msg.usage.output = 1;
    msg
}

#[test]
fn ttl_retention_obeys_request_environment_and_safe_replay_rules() {
    let mut m = cached_model();
    let mut opts = SimpleStreamOptions::default();
    opts.base.env = Some(std::collections::BTreeMap::from([(
        "PI_CACHE_RETENTION".into(),
        "long".into(),
    )]));
    assert_eq!(get_prompt_cache_ttl_ms(&m, Some(&opts)), Some(120_000));
    opts.base.cache_retention = Some(CacheRetention::None);
    assert_eq!(get_prompt_cache_ttl_ms(&m, Some(&opts)), None);
    assert_eq!(get_cache_warming_delay_ms(10_000), None);
    assert_eq!(get_cache_warming_delay_ms(10_001), Some(1));
    assert_eq!(get_cache_warming_delay_ms(60_000), Some(50_000));
    m.api = "anthropic-messages".into();
    opts.reasoning = Some(ThinkingLevel::Medium);
    assert!(!is_replayable(&m, Some(&opts)));
    m.compat.get_or_insert_default().force_adaptive_thinking = Some(true);
    assert!(is_replayable(&m, Some(&opts)));
    opts.base.cache_retention = Some(CacheRetention::Short);
    m.prompt_cache.as_mut().unwrap().short = Some(f64::MAX);
    assert_eq!(get_prompt_cache_ttl_ms(&m, Some(&opts)), None);
}

#[tokio::test]
async fn warmer_timer_is_armed_at_schedule_time_not_when_spawn_first_polls() {
    let h = Harness::new(None, 10_000).await;
    h.start();
    // No yield before the virtual clock reaches the scheduled deadline.
    h.tick().await;
    assert_eq!(h.calls.lock().unwrap().len(), 1);
    let entries = h.entries.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model, "physical-response-version");
    let calls = h.calls.lock().unwrap();
    assert_eq!(calls[0].0.id, "physical");
    assert_eq!(calls[0].2.base.max_tokens, Some(1));
    assert_eq!(calls[0].2.base.max_retries, Some(0));
    h.warmer.cancel();
}

#[tokio::test]
async fn warmer_economics_and_fixed_streaming_idle_windows() {
    let h = Harness::new(None, 10_000).await;
    h.start();
    let d = h.warmer.status().decision.unwrap();
    assert!((d.warm_cost - 0.01001).abs() < 1e-10);
    assert!((d.miss_cost - 0.19).abs() < 1e-10);
    assert_eq!(d.action, CacheWarmingAction::Warm);
    // Every refresh is quick; the safety origin must never move to the last refresh.
    for n in 1..=72 {
        h.clock.advance_ms(50_000);
        eventually(|| h.entries.lock().unwrap().len() >= n).await;
    }
    eventually(|| h.warmer.status().state == "inactive").await;
    assert_eq!(h.calls.lock().unwrap().len(), 72);
    assert!(h.warmer.status().reason.unwrap().contains("one-hour"));

    let h = Harness::new(
        Some(Arc::new(|_| Box::pin(async { CacheWarmingAction::Warm }))),
        10_000,
    )
    .await;
    *h.mode.lock().unwrap() = CacheWarmingMode::Idle;
    h.start();
    h.warmer.on_agent_settled();
    let d = h.warmer.status().decision.unwrap();
    assert_eq!(d.continuation_probability, 0.15);
    assert_eq!(d.action, CacheWarmingAction::Stop);
    for n in 1..=36 {
        h.clock.advance_ms(50_000);
        eventually(|| h.entries.lock().unwrap().len() >= n).await;
    }
    eventually(|| h.warmer.status().state == "inactive").await;
    assert_eq!(h.calls.lock().unwrap().len(), 36);
    assert!(h.warmer.status().reason.unwrap().contains("30-minute"));
}

#[tokio::test]
async fn warmer_discards_late_results_and_reports_storage_failure() {
    let h = Harness::new(None, 10_000).await;
    h.hold.store(true, Ordering::SeqCst);
    h.start();
    h.tick().await;
    h.current.store(false, Ordering::SeqCst);
    let pending = h.pending.lock().unwrap().pop().unwrap();
    let message = warm_message(&h.request.physical_model);
    pending.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: message.clone(),
    });
    pending.end(Some(message));
    eventually(|| h.warmer.status().state == "inactive").await;
    assert!(h.entries.lock().unwrap().is_empty());
    h.warmer.cancel();

    let h = Harness::new(None, 10_000).await;
    h.fail_storage.store(true, Ordering::SeqCst);
    h.start();
    h.tick().await;
    eventually(|| !h.errors.lock().unwrap().is_empty()).await;
    assert!(h.warmer.status().reason.unwrap().contains("storage error"));
    assert!(h.entries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn warmer_cancel_drops_pending_extension_decision() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let entered = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let (e, d) = (entered.clone(), dropped.clone());
    let h = Harness::new(
        Some(Arc::new(move |_| {
            let (e, d) = (e.clone(), d.clone());
            Box::pin(async move {
                let _guard = Dropped(d);
                e.store(true, Ordering::SeqCst);
                std::future::pending().await
            })
        })),
        10_000,
    )
    .await;
    h.start();
    h.clock.advance_ms(50_000);
    eventually(|| entered.load(Ordering::SeqCst)).await;
    h.warmer.cancel();
    eventually(|| dropped.load(Ordering::SeqCst)).await;
    assert!(h.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn warmer_rechecks_context_and_deadline_after_extension_await() {
    for stale_context in [false, true] {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Notify::new());
        let (e, r) = (entered.clone(), release.clone());
        let h = Harness::new(
            Some(Arc::new(move |_| {
                let (e, r) = (e.clone(), r.clone());
                Box::pin(async move {
                    e.store(true, Ordering::SeqCst);
                    r.notified().await;
                    CacheWarmingAction::Warm
                })
            })),
            10_000,
        )
        .await;
        h.start();
        h.clock.advance_ms(50_000);
        eventually(|| entered.load(Ordering::SeqCst)).await;
        if stale_context {
            h.current.store(false, Ordering::SeqCst);
        } else {
            h.clock.advance_ms(5_001);
        }
        release.notify_one();
        eventually(|| h.warmer.status().state == "inactive").await;
        // No physical send is admitted after either check fails.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(h.calls.lock().unwrap().is_empty());
        h.warmer.cancel();
    }
}

#[tokio::test]
async fn warmer_stale_request_cannot_replace_newer_request_or_clear_its_schedule() {
    let h = Harness::new(None, 10_000).await;
    let mut newer = h.request.clone();
    newer.request_seq += 1;
    newer.transcript = pi_ai::normalize_context(&Context {
        messages: vec![Message::User(UserMessage::new("new request"))],
        ..Default::default()
    });
    h.warmer.start(newer, Arc::new(|| true));
    h.start(); // older observer arrives later
    h.tick().await;
    assert!(
        serde_json::to_string(h.calls.lock().unwrap()[0].1.messages())
            .unwrap()
            .contains("new request")
    );
    h.warmer.cancel();
}

#[tokio::test]
async fn warmer_unknown_economics_default_stop_extensions_can_force_and_modes_cancel() {
    let h = Harness::new(None, 0).await;
    h.start();
    h.tick().await;
    assert!(h.calls.lock().unwrap().is_empty());
    assert!(h.warmer.status().reason.unwrap().contains("economics"));
    let h = Harness::new(
        Some(Arc::new(|_| Box::pin(async { CacheWarmingAction::Warm }))),
        0,
    )
    .await;
    // Verify callbacks are invoked without holding internal callback/state locks.
    let copy = h.warmer.clone();
    h.warmer.set_on_warmed(Some(Arc::new(move |_| {
        copy.set_on_warmed(None);
    })));
    h.start();
    h.tick().await;
    assert_eq!(
        h.entries.lock().unwrap()[0].note.as_deref(),
        Some("extension override")
    );
    *h.mode.lock().unwrap() = CacheWarmingMode::Off;
    h.warmer.on_mode_changed();
    assert_eq!(
        h.warmer.status().reason.as_deref(),
        Some("cache warming disabled")
    );
    assert!(
        h.calls.lock().unwrap()[0]
            .2
            .base
            .cancel_token
            .as_ref()
            .unwrap()
            .is_cancelled()
    );

    let h = Harness::new(None, 10_000).await;
    h.start();
    h.warmer.on_agent_settled();
    assert_eq!(
        h.warmer.status().reason.as_deref(),
        Some("agent run settled")
    );
}

#[tokio::test]
async fn warmer_decision_panics_fall_back_without_leaving_refreshing_state() {
    for synchronous in [false, true] {
        let h = Harness::new(
            Some(Arc::new(move |_| {
                assert!(!synchronous, "injected sync extension panic");
                Box::pin(async { panic!("injected async extension panic") })
            })),
            10_000,
        )
        .await;
        h.start();
        h.tick().await;
        assert_eq!(h.entries.lock().unwrap().len(), 1);
        assert_eq!(h.warmer.status().state, "scheduled");
        h.warmer.cancel();
    }
}
